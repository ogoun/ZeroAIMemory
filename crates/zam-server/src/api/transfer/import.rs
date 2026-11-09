//! `POST /spaces/{space}/import?own=&on_conflict=skip|revision&axioms=demote|keep&dry_run=`
//! (M6 §12б.5) — импорт архива экспорта (или zip папки `.md`) в пространство.
//!
//! **Порядок.** Остановка сервера — 503 → права (владелец пространства или администратор;
//! участник решения — 403, токен `ro` — 403, Human по токену — 404) → `Content-Length` больше
//! `import.max_bytes` — 413 без чтения → слот ([`IMPORT_SLOTS`] на сервер, занято — 503 с
//! `Retry-After`) → аудит `space.import` (не записан — 500, тело не читается) → тело потоком в
//! `<data_dir>/.incoming/<id>.part` с пределом (413) → проверка всего архива
//! ([`zam_store::import::validate_archive`], 422) → id назначения и переписывание ссылок →
//! распаковка вложений в блобы владельца → запись пачками ([`zam_store::import::apply_plan`]).
//! Файл `.part` удаляется при любом исходе ([`Part`]), в том числе при обрыве соединения.
//!
//! **Свой или чужой.** Архив свой, если `source_user_id` описи — владелец базы или задан
//! `own=true`. Тогда для коллизий id проверяются базы других пользователей сервера и точки
//! Qdrant других тенантов ([`zam_index::VectorIndex::foreign_targets`]).
//!
//! **Аудит** — без текста: `space.import {space, user, owner, own, on_conflict, axioms,
//! dry_run}` до тела и `space.import.done {space, user, owner, complete, reason?, created,
//! revised, skipped, conflicts, bytes}` по окончании (`reason`: `client` — обрыв тела,
//! `too_large`, `rejected`, `stop`, `error`). Причина сбоя приёма тела — кодом ([`Recv`]), а не
//! по тексту ошибки.
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Body;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, header};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use zam_core::{Id, TargetKind, TargetRef};
use zam_store::import::{
    AxiomPolicy, ImportError, ImportLimits, ImportOptions, ImportReport, MAX_RATIO_DEFAULT,
    OnConflict, apply_plan, extract_blobs, plan_ids, remap_refs, validate_archive,
};
use zam_store::{BlobStore, DocIntake};

use crate::api::blobs::{admin_user, sniff_blob};
use crate::api::query;
use crate::api::scope::{Mode, Role, space_scope};
use crate::app::AppState;
use crate::auth::{Client, Principal};
use crate::error::ApiError;

/// Действие аудита до тела.
pub const AUDIT_ACTION: &str = "space.import";
/// Действие аудита по окончании.
pub const AUDIT_DONE: &str = "space.import.done";
/// Импортов одновременно на сервер: запись идёт одним писателем базы, а проверка архива
/// держит его тексты в памяти.
pub const IMPORT_SLOTS: usize = 1;
/// `Retry-After` при занятом слоте, с.
const RETRY_AFTER_S: u64 = 30;
/// Каталог временных файлов тела в каталоге данных.
const INCOMING: &str = ".incoming";
/// Клиент не присылает тело дольше — приём обрывается (слот не держится вечно).
const STALL: Duration = Duration::from_secs(10 * 60);
/// Шаг ожидания куска тела: между шагами проверяется остановка сервера.
const TICK: Duration = Duration::from_millis(500);

#[derive(Deserialize)]
pub struct ImportQuery {
    own: Option<bool>,
    on_conflict: Option<String>,
    axioms: Option<String>,
    dry_run: Option<bool>,
    /// `X-Zam-User` администратора (как у экспорта).
    user: Option<String>,
}

fn options_of(q: &ImportQuery) -> Result<ImportOptions, ApiError> {
    let on_conflict = match q.on_conflict.as_deref().map(str::trim) {
        None | Some("") | Some("skip") => OnConflict::Skip,
        Some("revision") => OnConflict::Revision,
        Some(x) => {
            return Err(ApiError::BadRequest(format!(
                "on_conflict: skip или revision, не «{x}»"
            )));
        }
    };
    let axioms = match q.axioms.as_deref().map(str::trim) {
        None | Some("") => None,
        Some("demote") => Some(AxiomPolicy::Demote),
        Some("keep") => Some(AxiomPolicy::Keep),
        Some(x) => {
            return Err(ApiError::BadRequest(format!(
                "axioms: demote или keep, не «{x}»"
            )));
        }
    };
    Ok(ImportOptions {
        own: q.own.unwrap_or(false),
        on_conflict,
        axioms,
        dry_run: q.dry_run.unwrap_or(false),
        ..ImportOptions::default()
    })
}

/// Временный файл тела: удаляется при уничтожении (любой исход, в том числе паника и обрыв).
struct Part(PathBuf);

impl Drop for Part {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.0)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.0.display(), error = %e, "временный файл импорта не удалён");
        }
    }
}

/// Причина сбоя приёма тела (кодом, без разбора текста).
#[derive(Debug)]
enum Recv {
    /// Больше `import.max_bytes`.
    TooLarge,
    /// Поток тела оборвался (клиент ушёл, ошибка сети).
    Client(String),
    /// Клиент не присылает тело дольше [`STALL`].
    Stall,
    /// Сервер останавливается.
    Stop,
    /// Запись временного файла.
    Io(std::io::Error),
}

impl Recv {
    fn reason(&self) -> &'static str {
        match self {
            Recv::TooLarge => "too_large",
            Recv::Client(_) => "client",
            Recv::Stall => "stall",
            Recv::Stop => "stop",
            Recv::Io(_) => "error",
        }
    }

    fn into_api(self, limit: u64) -> ApiError {
        match self {
            Recv::TooLarge => ApiError::PayloadTooLarge(format!(
                "архив больше предела import.max_bytes = {limit} байт"
            )),
            Recv::Client(m) => ApiError::BadRequest(format!("тело запроса оборвано: {m}")),
            Recv::Stall => ApiError::RequestTimeout(format!(
                "тело запроса не приходит дольше {} с",
                STALL.as_secs()
            )),
            Recv::Stop => ApiError::ShuttingDown {
                retry_after_s: RETRY_AFTER_S,
            },
            Recv::Io(e) => ApiError::Internal(format!("временный файл импорта: {e}")),
        }
    }
}

/// Тело в файл `part` не больше `limit` байт; возвращает размер. Кусок не пришёл дольше
/// `stall` — [`Recv::Stall`]; сервер останавливается (`stop`) — [`Recv::Stop`].
async fn receive(
    body: Body,
    part: &std::path::Path,
    limit: u64,
    stall: Duration,
    stop: &AtomicBool,
) -> Result<u64, Recv> {
    if let Some(dir) = part.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(Recv::Io)?;
    }
    let mut f = tokio::fs::File::create(part).await.map_err(Recv::Io)?;
    let mut s = body.into_data_stream();
    let mut n: u64 = 0;
    let mut last = Instant::now();
    loop {
        if stop.load(Ordering::SeqCst) {
            return Err(Recv::Stop);
        }
        let next = match tokio::time::timeout(TICK.min(stall), s.next()).await {
            Ok(x) => x,
            Err(_) if last.elapsed() >= stall => return Err(Recv::Stall),
            Err(_) => continue,
        };
        let Some(chunk) = next else { break };
        last = Instant::now();
        let chunk = chunk.map_err(|e| Recv::Client(e.to_string()))?;
        n += chunk.len() as u64;
        if n > limit {
            return Err(Recv::TooLarge);
        }
        f.write_all(&chunk).await.map_err(Recv::Io)?;
    }
    f.flush().await.map_err(Recv::Io)?;
    f.sync_all().await.map_err(Recv::Io)?;
    Ok(n)
}

/// Итог для аудита `space.import.done`.
#[derive(Default)]
struct Outcome {
    bytes: u64,
    reason: Option<&'static str>,
    report: Option<ImportReport>,
}

fn import_err(e: ImportError, out: &mut Outcome) -> ApiError {
    match e {
        ImportError::Rejected(m) => {
            out.reason = Some("rejected");
            ApiError::Unprocessable(m)
        }
        ImportError::Stopped => {
            out.reason = Some("stop");
            ApiError::ShuttingDown {
                retry_after_s: RETRY_AFTER_S,
            }
        }
        ImportError::Store(e) => {
            out.reason = Some("error");
            e.into()
        }
    }
}

/// Id архива, занятые вне базы `owner`: в базах других пользователей и точками Qdrant других
/// тенантов (только для своего архива).
async fn taken_ids(
    st: &AppState,
    owner: Id,
    ids: &[Id],
    warnings: &mut Vec<String>,
) -> Result<HashSet<Id>, ApiError> {
    let mut taken = HashSet::new();
    if ids.is_empty() {
        return Ok(taken);
    }
    let list = json!(ids.iter().map(Id::to_string).collect::<Vec<_>>()).to_string();
    for u in st.system.list_users().await? {
        if u.id == owner {
            continue;
        }
        let db = st.users.get(u.id).await?;
        let l = list.clone();
        let found: Vec<Id> = db
            .read(move |c| {
                let mut out = Vec::new();
                for sql in [
                    "SELECT id FROM nodes WHERE id IN (SELECT value FROM json_each(?1))",
                    "SELECT id FROM entries WHERE id IN (SELECT value FROM json_each(?1))",
                    "SELECT id FROM attachment_docs WHERE id IN (SELECT value FROM json_each(?1))",
                ] {
                    let mut s = c.prepare(sql)?;
                    for r in s.query_map([&l], |r| r.get::<_, Id>(0))? {
                        out.push(r?);
                    }
                }
                Ok(out)
            })
            .await?;
        taken.extend(found);
    }
    if let Some(v) = st.index.vector.as_ref() {
        let targets: Vec<TargetRef> = ids
            .iter()
            .flat_map(|&id| {
                [
                    TargetRef {
                        kind: TargetKind::Node,
                        id,
                    },
                    TargetRef {
                        kind: TargetKind::Entry,
                        id,
                    },
                ]
            })
            .collect();
        match v.foreign_targets(owner, &targets).await {
            Ok(found) => taken.extend(found.into_iter().map(|t| t.id)),
            Err(e) => {
                tracing::warn!(error = %e, "импорт: точки Qdrant не проверены");
                warnings.push(
                    "точки Qdrant других пользователей не проверены (векторный индекс \
                     недоступен): коллизии id искались только в базах сервера"
                        .into(),
                );
            }
        }
    }
    Ok(taken)
}

pub async fn import(
    State(st): State<AppState>,
    p: Principal,
    client: Client,
    mut headers: HeaderMap,
    Path(space): Path<String>,
    q: Result<Query<ImportQuery>, QueryRejection>,
    body: Body,
) -> Result<Json<ImportReport>, ApiError> {
    if st.stopping.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(ApiError::ShuttingDown {
            retry_after_s: RETRY_AFTER_S,
        });
    }
    let q = query(q)?;
    admin_user(&p, &mut headers, q.user.as_deref())?;
    let mut opts = options_of(&q)?;
    let sc = space_scope(&st, &p, &headers, &space, Mode::Write).await?;
    // импорт пишет много записей от имени владельца базы: участнику решения — нет
    if matches!(sc.role, Role::Member(_)) {
        return Err(ApiError::Denied(
            "импорт в решение — только владелец решения или администратор".into(),
        ));
    }
    let limit = st.cfg.import.max_bytes;
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > limit) {
        return Err(Recv::TooLarge.into_api(limit));
    }
    let _slot = st
        .import_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::Busy {
            retry_after_s: RETRY_AFTER_S,
        })?;
    let mut attrib = crate::attrib::attribution(&p, &headers, None)?;
    attrib.area = None;
    opts.with_reports = p.is_human();
    opts.stop = Some(st.stopping.clone());
    let audit = Audit {
        actor: p.actor(),
        user: sc.user.id,
        owner: sc.owner,
        space: sc.space.id,
        ip: client.ip.to_string(),
    };
    audit
        .start(&st, &q, &opts)
        .await
        .map_err(|e| ApiError::Internal(format!("аудит импорта: {e}")))?;
    let mut out = Outcome::default();
    let res = run(&st, &sc, body, limit, opts, attrib, &mut out).await;
    if let Err(e) = audit.done(&st, &out, res.is_ok()).await {
        tracing::warn!(space = %sc.space.id, error = %e, "аудит импорта не записан");
    }
    res.map(Json)
}

/// Приём, проверка и запись (см. модуль); `out` — итог для аудита.
async fn run(
    st: &AppState,
    sc: &crate::api::scope::MemScope,
    body: Body,
    limit: u64,
    mut opts: ImportOptions,
    attrib: zam_store::Attribution,
    out: &mut Outcome,
) -> Result<ImportReport, ApiError> {
    let part = Part(
        st.data_dir
            .root()
            .join(INCOMING)
            .join(format!("{}.part", Id::new())),
    );
    out.bytes = match receive(body, &part.0, limit, STALL, &st.stopping).await {
        Ok(n) => n,
        Err(e) => {
            out.reason = Some(e.reason());
            return Err(e.into_api(limit));
        }
    };
    let limits = ImportLimits {
        max_bytes: limit,
        max_files: st.cfg.import.max_files,
        max_ratio: MAX_RATIO_DEFAULT,
    };
    let path = part.0.clone();
    let plan = tokio::task::spawn_blocking(move || {
        let f = std::fs::File::open(&path).map_err(|e| ImportError::Store(e.into()))?;
        let mut z = zip::ZipArchive::new(std::io::BufReader::new(f))
            .map_err(|e| ImportError::Rejected(format!("тело — не архив zip: {e}")))?;
        validate_archive(&mut z, &limits)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("проверка архива: {e}")))?
    .map_err(|e| import_err(e, out))?;
    let mut plan = plan;
    let own = plan.is_own(sc.owner, opts.own);
    opts.own = own;
    let mut warnings = Vec::new();
    let taken = if own {
        taken_ids(st, sc.owner, &plan.source_ids(), &mut warnings).await?
    } else {
        HashSet::new()
    };
    let map = plan_ids(&sc.db, sc.space.id, &plan, own, &taken).await?;
    let remap = remap_refs(&mut plan, &map);
    plan.warnings.extend(warnings);
    if !opts.dry_run {
        let store = BlobStore::new(&st.data_dir.blobs_dir(), sc.owner);
        let (path, p2, s2) = (part.0.clone(), plan.clone(), store.clone());
        tokio::task::spawn_blocking(move || {
            let f = std::fs::File::open(&path).map_err(|e| ImportError::Store(e.into()))?;
            let mut z = zip::ZipArchive::new(std::io::BufReader::new(f))
                .map_err(|e| ImportError::Rejected(format!("тело — не архив zip: {e}")))?;
            extract_blobs(&mut z, &p2, &s2)
        })
        .await
        .map_err(|e| ApiError::Internal(format!("распаковка вложений: {e}")))?
        .map_err(|e| import_err(e, out))?;
        // вид документа вложения — по файлу блоба, как при загрузке
        let dl = st.cfg.docs.limits();
        let pairs: HashSet<(String, String, u64)> = plan
            .notes
            .iter()
            .flat_map(|n| n.attachments.iter())
            .map(|a| (a.sha256.clone(), a.name.clone(), a.size))
            .collect();
        for (sha, name, size) in pairs {
            let kind = sniff_blob(store.path_of(&sha), name.clone(), dl.clone()).await;
            plan.intakes
                .insert((sha, name), DocIntake::classify(kind, size, dl.max_bytes));
        }
    }
    let mut report = apply_plan(&sc.db, sc.space.id, sc.owner, plan, &opts, &attrib)
        .await
        .map_err(|e| import_err(e, out))?;
    for u in remap.unresolved {
        if !report.unresolved.contains(&u) {
            report.unresolved.push(u);
        }
    }
    if own {
        report.id_remapped = map.remapped.len() as u64;
        report.remapped = map
            .remapped
            .iter()
            .map(|(a, b)| zam_store::import::Remapped {
                from: a.to_string(),
                to: b.to_string(),
            })
            .collect();
    }
    if !opts.dry_run {
        st.render_gen.bump_space(sc.owner, sc.space.id);
        st.index.notify();
        st.docs.notify();
    }
    tracing::info!(
        space = %sc.space.id,
        created = report.created,
        revised = report.revised,
        skipped = report.skipped,
        conflicts = report.conflicts.len(),
        dry_run = report.dry_run,
        "импорт пространства"
    );
    out.report = Some(report.clone());
    Ok(report)
}

/// Аудит импорта без текста.
struct Audit {
    actor: String,
    user: Id,
    owner: Id,
    space: Id,
    ip: String,
}

impl Audit {
    async fn record(
        &self,
        st: &AppState,
        action: &str,
        details: Value,
    ) -> Result<(), zam_store::StoreError> {
        st.system
            .audit(
                &self.actor,
                Some(self.user),
                action,
                Some(&self.space.to_string()),
                Some(&self.ip),
                details,
            )
            .await
    }

    /// `space.import` до тела; тестовая точка `import_audit` (фича `test-hooks`) — сбой.
    async fn start(
        &self,
        st: &AppState,
        q: &ImportQuery,
        o: &ImportOptions,
    ) -> Result<(), zam_store::StoreError> {
        crate::backup::fail_point(&st.cfg.data_dir, "import_audit")
            .map_err(|e| zam_store::StoreError::Io(std::io::Error::other(e.to_string())))?;
        self.record(
            st,
            AUDIT_ACTION,
            json!({
                "space": self.space, "user": self.user, "owner": self.owner, "own": o.own,
                "on_conflict": q.on_conflict.as_deref().unwrap_or("skip"),
                "axioms": q.axioms.as_deref(), "dry_run": o.dry_run,
            }),
        )
        .await
    }

    /// `space.import.done` по окончании.
    async fn done(
        &self,
        st: &AppState,
        out: &Outcome,
        ok: bool,
    ) -> Result<(), zam_store::StoreError> {
        let mut d = json!({
            "space": self.space, "user": self.user, "owner": self.owner,
            "complete": ok, "bytes": out.bytes,
        });
        if let Value::Object(o) = &mut d {
            if !ok {
                o.insert("reason".into(), json!(out.reason.unwrap_or("error")));
            }
            if let Some(r) = &out.report {
                o.insert("created".into(), json!(r.created));
                o.insert("revised".into(), json!(r.revised));
                o.insert("skipped".into(), json!(r.skipped));
                o.insert("conflicts".into(), json!(r.conflicts.len()));
                o.insert("dry_run".into(), json!(r.dry_run));
            }
        }
        self.record(st, AUDIT_DONE, d).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(c: Option<&str>, a: Option<&str>) -> ImportQuery {
        ImportQuery {
            own: None,
            on_conflict: c.map(str::to_string),
            axioms: a.map(str::to_string),
            dry_run: None,
            user: None,
        }
    }

    #[test]
    fn options_parsing() {
        let o = options_of(&q(None, None)).unwrap();
        assert_eq!(o.on_conflict, OnConflict::Skip);
        assert_eq!(o.axioms, None);
        let o = options_of(&q(Some("revision"), Some("keep"))).unwrap();
        assert_eq!(o.on_conflict, OnConflict::Revision);
        assert_eq!(o.axioms, Some(AxiomPolicy::Keep));
        assert!(options_of(&q(Some("merge"), None)).is_err());
        assert!(options_of(&q(None, Some("drop"))).is_err());
    }

    #[test]
    fn recv_reasons_are_codes() {
        assert_eq!(Recv::TooLarge.reason(), "too_large");
        assert_eq!(Recv::Client("x".into()).reason(), "client");
        assert_eq!(Recv::Io(std::io::Error::other("x")).reason(), "error");
    }

    /// Клиент молчит — приём обрывается по `stall`, а не ждёт вечно; остановка — сразу.
    #[tokio::test]
    async fn receive_gives_up_on_stall_and_stop() {
        let dir = std::env::current_dir()
            .unwrap()
            .join("target")
            .join("t18b2-recv");
        let p = dir.join("x.part");
        let silent = Body::from_stream(futures_util::stream::pending::<
            Result<axum::body::Bytes, std::io::Error>,
        >());
        // без защиты приём ждал бы вечно: предел теста — 10 с
        let limit = Duration::from_secs(10);
        let t0 = Instant::now();
        let stop = AtomicBool::new(false);
        let r = tokio::time::timeout(
            limit,
            receive(silent, &p, 1 << 20, Duration::from_millis(100), &stop),
        )
        .await
        .expect("приём ждал молчащего клиента дольше 10 с");
        assert!(matches!(r, Err(Recv::Stall)), "{r:?}");
        assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
        let silent = Body::from_stream(futures_util::stream::pending::<
            Result<axum::body::Bytes, std::io::Error>,
        >());
        let stop = AtomicBool::new(true);
        let r = tokio::time::timeout(limit, receive(silent, &p, 1 << 20, STALL, &stop))
            .await
            .expect("приём не заметил остановку сервера");
        assert!(matches!(r, Err(Recv::Stop)), "{r:?}");
        assert_eq!(Recv::Stall.reason(), "stall");
        drop(Part(p));
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn part_removed_on_drop() {
        let dir = std::env::current_dir()
            .unwrap()
            .join("target")
            .join("t18b2-part");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.part");
        std::fs::write(&p, b"x").unwrap();
        drop(Part(p.clone()));
        assert!(!p.exists());
        let _ = std::fs::remove_dir(&dir);
    }
}
