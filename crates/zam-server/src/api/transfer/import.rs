//! Импорт архива экспорта (или zip папки `.md`) в пространство (M6 §12б.5) — **заданием**:
//! `POST /spaces/{space}/import?own=&on_conflict=skip|revision&axioms=demote|keep&dry_run=`
//! принимает тело и отвечает 202 `{job, status}`; проверка и запись идут в фоне,
//! `GET /spaces/{space}/imports/{job}` отдаёт этап, счётчики и итог.
//!
//! **Приём** (в обработчике). Остановка сервера — 503 → права (владелец пространства или
//! администратор; участник решения — 403, токен `ro` — 403, Human по токену — 404) →
//! `Content-Length` больше `import.max_bytes` — 413 без чтения → слот приёма (по одному на
//! пользователя, [`RECV_SLOTS`] на сервер; занято — 503 с `Retry-After`) → аудит `space.import`
//! (не записан — 500, тело не читается) → тело потоком в `<data_dir>/.incoming/<id>.part` с
//! пределом (413), сторожем простоя ([`STALL`]), общим сроком (`max(30 мин, max_bytes / 1 МиБ/с)`)
//! и минимальной скоростью ([`MIN_SPEED`] после [`SPEED_GRACE`]; 408), остановкой (503). Сбой
//! приёма — ответ сразу и аудит `space.import.done`.
//!
//! **Задание** (`tokio::spawn`, владеет `.part`, аудитом и итогом; обрыв клиента его не
//! прерывает). Очередь записи — одно задание на сервер ([`AppState::imports`], этап `queued`) →
//! `validating` (весь архив, 422) → id назначения и переписывание ссылок → `extracting`
//! (вложения в блобы владельца) → `writing` (пачки, счётчики) → итог. Остановка сервера —
//! задание кончается `Stopped` (503 в итоге); повтор безопасен: id назначения детерминированы
//! ([`zam_store::import::import_id`]), записанное пропускается. Если записана хоть одна пачка,
//! рендер, индексация и конвейер документов оповещаются и при ошибке. `dry_run` — тоже задание.
//!
//! **Задания живут в памяти** процесса: итог хранится не меньше [`JOB_KEEP`]; после
//! перезапуска сервера опрос отвечает 404 (повторите импорт — записанное пропустится). Опрос —
//! тому же пользователю (`bound_user` или пользователь `X-Zam-User` задания) или
//! администратору; остальным — 404.
//!
//! **Коллизии id.** Для всех кандидатов id назначения проверяются базы других пользователей
//! сервера и точки Qdrant других тенантов ([`zam_index::VectorIndex::foreign_targets`]).
//!
//! **Аудит** — без текста: `space.import {space, user, owner, own, on_conflict, axioms,
//! dry_run}` до тела и `space.import.done {space, user, owner, job?, complete, reason?,
//! created, revised, skipped, conflicts, bytes}` по окончании (`reason`: `client`, `too_large`,
//! `stall`, `deadline`, `slow`, `rejected`, `stop`, `error`). Причина сбоя — кодом ([`Recv`]),
//! не по тексту.
use std::collections::{HashMap, HashSet};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Body;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use zam_core::{Id, TargetKind, TargetRef};
use zam_store::import::{
    AxiomPolicy, ImportError, ImportLimits, ImportOptions, ImportReport, MAX_RATIO_DEFAULT,
    OnConflict, Plan, Progress, apply_plan, extract_blobs, plan_ids, remap_refs, validate_archive,
};
use zam_store::{BlobStore, Db, DocIntake};

use crate::api::blobs::{admin_user, pick_mime, sniff_blob};
use crate::api::query;
use crate::api::scope::{Mode, Role, space_scope};
use crate::app::AppState;
use crate::auth::{Client, Principal};
use crate::error::ApiError;

/// Действие аудита до тела.
pub const AUDIT_ACTION: &str = "space.import";
/// Действие аудита по окончании.
pub const AUDIT_DONE: &str = "space.import.done";
/// Приёмов тела одновременно на сервер (у пользователя — один).
pub const RECV_SLOTS: usize = 2;
/// Заданий, проверяющих и пишущих одновременно (очередь): запись идёт одним писателем базы,
/// а план держит тексты архива в памяти.
pub const WRITE_SLOTS: usize = 1;
/// Итог задания хранится не меньше.
pub const JOB_KEEP: Duration = Duration::from_secs(3600);
/// `Retry-After` при занятом слоте, с.
const RETRY_AFTER_S: u64 = 30;
/// Каталог временных файлов тела в каталоге данных.
const INCOMING: &str = ".incoming";
/// Клиент не присылает тело дольше — приём обрывается (слот не держится вечно).
const STALL: Duration = Duration::from_secs(10 * 60);
/// Шаг ожидания куска тела: между шагами проверяется остановка сервера.
const TICK: Duration = Duration::from_millis(500);
/// Общий срок приёма — не меньше.
const DEADLINE_MIN: Duration = Duration::from_secs(30 * 60);
/// Скорость, по которой считается общий срок приёма, байт/с.
const DEADLINE_SPEED: u64 = 1 << 20;
/// Минимальная средняя скорость приёма после [`SPEED_GRACE`], байт/с.
const MIN_SPEED: u64 = 16 << 10;
/// Первые секунды приёма скорость не проверяется.
const SPEED_GRACE: Duration = Duration::from_secs(60);

/// Общий срок приёма тела до `max_bytes`.
fn recv_deadline(max_bytes: u64) -> Duration {
    DEADLINE_MIN.max(Duration::from_secs(max_bytes / DEADLINE_SPEED))
}

/// Убирает `<data_dir>/.incoming/*.part`, оставшиеся от прерванного процесса (запуск сервера).
pub fn clean_incoming(root: &FsPath) {
    let dir = root.join(INCOMING);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "part")
            && let Err(err) = std::fs::remove_file(&p)
        {
            tracing::warn!(path = %p.display(), error = %err, "старый .part импорта не удалён");
        }
    }
}

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

// ---------- задания ----------

/// Состояние задания.
#[derive(Clone)]
struct Job {
    user: Id,
    space: String,
    dry_run: bool,
    stage: &'static str,
    progress: Arc<Progress>,
    report: Option<ImportReport>,
    /// Ошибка задания — problem+json (`status`, `type`, `detail`).
    error: Option<Value>,
    finished: Option<Instant>,
}

/// Импорты сервера: слоты приёма, очередь записи, задания (в памяти).
pub struct ImportRuntime {
    recv: Arc<tokio::sync::Semaphore>,
    recv_users: Arc<std::sync::Mutex<HashSet<Id>>>,
    write: Arc<tokio::sync::Semaphore>,
    jobs: std::sync::Mutex<HashMap<Id, Job>>,
}

impl Default for ImportRuntime {
    fn default() -> Self {
        ImportRuntime {
            recv: Arc::new(tokio::sync::Semaphore::new(RECV_SLOTS)),
            recv_users: Arc::default(),
            write: Arc::new(tokio::sync::Semaphore::new(WRITE_SLOTS)),
            jobs: std::sync::Mutex::default(),
        }
    }
}

impl ImportRuntime {
    fn jobs(&self) -> std::sync::MutexGuard<'_, HashMap<Id, Job>> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Свободных слотов приёма (тесты и диагностика).
    pub fn recv_available(&self) -> usize {
        self.recv.available_permits()
    }

    /// Свободных слотов записи (тесты и диагностика).
    pub fn write_available(&self) -> usize {
        self.write.available_permits()
    }

    /// Занять слот записи (тест очереди).
    #[doc(hidden)]
    pub fn hold_write(&self) -> tokio::sync::OwnedSemaphorePermit {
        self.write
            .clone()
            .try_acquire_owned()
            .expect("слот записи свободен")
    }

    /// Занять слот приёма (тест занятости).
    #[doc(hidden)]
    pub fn hold_recv(&self) -> tokio::sync::OwnedSemaphorePermit {
        self.recv
            .clone()
            .try_acquire_owned()
            .expect("слот приёма свободен")
    }

    /// Идущие задания (не кончились).
    pub fn running(&self) -> usize {
        self.jobs()
            .values()
            .filter(|j| j.finished.is_none())
            .count()
    }

    fn insert(&self, id: Id, job: Job) {
        let mut j = self.jobs();
        j.retain(|_, x| x.finished.is_none_or(|t| t.elapsed() < JOB_KEEP));
        j.insert(id, job);
    }

    fn stage(&self, id: Id, stage: &'static str) {
        if let Some(j) = self.jobs().get_mut(&id) {
            j.stage = stage;
        }
    }

    fn finish(&self, id: Id, res: Result<ImportReport, Value>) {
        if let Some(j) = self.jobs().get_mut(&id) {
            j.finished = Some(Instant::now());
            match res {
                Ok(r) => {
                    j.stage = "done";
                    j.report = Some(r);
                }
                Err(e) => {
                    j.stage = "failed";
                    j.error = Some(e);
                }
            }
        }
    }

    fn get(&self, id: Id) -> Option<Job> {
        self.jobs().get(&id).cloned()
    }
}

/// Слот приёма пользователя: разрешение сервера и отметка; освобождается при уничтожении.
struct RecvSlot {
    _permit: tokio::sync::OwnedSemaphorePermit,
    users: Arc<std::sync::Mutex<HashSet<Id>>>,
    user: Id,
}

impl Drop for RecvSlot {
    fn drop(&mut self) {
        self.users
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.user);
    }
}

fn recv_slot(rt: &ImportRuntime, user: Id) -> Result<RecvSlot, ApiError> {
    let busy = || ApiError::Busy {
        retry_after_s: RETRY_AFTER_S,
    };
    let mut users = rt.recv_users.lock().unwrap_or_else(|e| e.into_inner());
    if users.contains(&user) {
        return Err(busy());
    }
    let permit = rt.recv.clone().try_acquire_owned().map_err(|_| busy())?;
    users.insert(user);
    Ok(RecvSlot {
        _permit: permit,
        users: rt.recv_users.clone(),
        user,
    })
}

/// Ошибка API → problem+json для итога задания.
async fn problem(e: ApiError) -> Value {
    let r = e.into_response();
    let status = r.status().as_u16();
    let body = axum::body::to_bytes(r.into_body(), 64 << 10)
        .await
        .unwrap_or_default();
    let mut v: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
    if let Value::Object(o) = &mut v {
        o.insert("status".into(), json!(status));
    }
    v
}

// ---------- приём тела ----------

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
    /// Приём дольше общего срока.
    Deadline,
    /// Средняя скорость ниже [`MIN_SPEED`].
    Slow,
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
            Recv::Deadline => "deadline",
            Recv::Slow => "slow",
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
            Recv::Deadline => ApiError::RequestTimeout(format!(
                "приём архива дольше {} с",
                recv_deadline(limit).as_secs()
            )),
            Recv::Slow => ApiError::RequestTimeout(format!(
                "архив приходит медленнее {} КиБ/с",
                MIN_SPEED >> 10
            )),
            Recv::Stop => ApiError::ShuttingDown {
                retry_after_s: RETRY_AFTER_S,
            },
            Recv::Io(e) => ApiError::Internal(format!("временный файл импорта: {e}")),
        }
    }
}

/// Пределы приёма.
struct RecvLimits {
    max_bytes: u64,
    stall: Duration,
    deadline: Duration,
    min_speed: u64,
    grace: Duration,
}

/// Тело в файл `part` (см. [`RecvLimits`]); возвращает размер. Сервер останавливается
/// (`stop`) — [`Recv::Stop`].
async fn receive(
    body: Body,
    part: &FsPath,
    lim: &RecvLimits,
    stop: &AtomicBool,
) -> Result<u64, Recv> {
    if let Some(dir) = part.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(Recv::Io)?;
    }
    let mut f = tokio::fs::File::create(part).await.map_err(Recv::Io)?;
    let mut s = body.into_data_stream();
    let mut n: u64 = 0;
    let t0 = Instant::now();
    let mut last = t0;
    loop {
        if stop.load(Ordering::SeqCst) {
            return Err(Recv::Stop);
        }
        let el = t0.elapsed();
        if el >= lim.deadline {
            return Err(Recv::Deadline);
        }
        if el >= lim.grace && n < lim.min_speed.saturating_mul(el.as_secs()) {
            return Err(Recv::Slow);
        }
        let next = match tokio::time::timeout(TICK.min(lim.stall), s.next()).await {
            Ok(x) => x,
            Err(_) if last.elapsed() >= lim.stall => return Err(Recv::Stall),
            Err(_) => continue,
        };
        let Some(chunk) = next else { break };
        last = Instant::now();
        let chunk = chunk.map_err(|e| Recv::Client(e.to_string()))?;
        n += chunk.len() as u64;
        if n > lim.max_bytes {
            return Err(Recv::TooLarge);
        }
        f.write_all(&chunk).await.map_err(Recv::Io)?;
    }
    f.flush().await.map_err(Recv::Io)?;
    f.sync_all().await.map_err(Recv::Io)?;
    Ok(n)
}

// ---------- обработчики ----------

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

/// Id-кандидаты, занятые вне базы `owner`: в базах других пользователей и точками Qdrant
/// других тенантов.
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

/// Что заданию нужно от области запроса.
struct Target {
    db: Db,
    owner: Id,
    space: Id,
}

pub async fn import(
    State(st): State<AppState>,
    p: Principal,
    client: Client,
    mut headers: HeaderMap,
    Path(space): Path<String>,
    q: Result<Query<ImportQuery>, QueryRejection>,
    body: Body,
) -> Result<Response, ApiError> {
    if st.stopping.load(Ordering::SeqCst) {
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
    let slot = recv_slot(&st.imports, sc.user.id)?;
    let mut attrib = crate::attrib::attribution(&p, &headers, None)?;
    attrib.area = None;
    opts.with_reports = p.is_human();
    opts.stop = Some(st.stopping.clone());
    // точка теста повтора после обрыва: две записи в пачке, остановка после первой пачки
    if crate::backup::fail_point(&st.cfg.data_dir, "import_one_batch").is_err() {
        opts.batch = 2;
        opts.stop_after = Some(1);
    }
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
    let part = Part(
        st.data_dir
            .root()
            .join(INCOMING)
            .join(format!("{}.part", Id::new())),
    );
    let lim = RecvLimits {
        max_bytes: limit,
        stall: STALL,
        deadline: recv_deadline(limit),
        min_speed: MIN_SPEED,
        grace: SPEED_GRACE,
    };
    let mut out = Outcome::default();
    match receive(body, &part.0, &lim, &st.stopping).await {
        Ok(n) => out.bytes = n,
        Err(e) => {
            out.reason = Some(e.reason());
            let err = e.into_api(limit);
            if let Err(e) = audit.done(&st, &out, false, None).await {
                tracing::warn!(space = %sc.space.id, error = %e, "аудит импорта не записан");
            }
            return Err(err);
        }
    }
    drop(slot);
    let job = Id::new();
    let progress = Arc::new(Progress::default());
    opts.progress = Some(progress.clone());
    st.imports.insert(
        job,
        Job {
            user: sc.user.id,
            space: space.clone(),
            dry_run: opts.dry_run,
            stage: "queued",
            progress,
            report: None,
            error: None,
            finished: None,
        },
    );
    let target = Target {
        db: sc.db.clone(),
        owner: sc.owner,
        space: sc.space.id,
    };
    let st2 = st.clone();
    tokio::spawn(async move {
        let res = run(
            &st2,
            job,
            &target,
            part,
            limit,
            opts.clone(),
            attrib,
            &mut out,
        )
        .await;
        // записана хоть одна пачка — оповещения и на пути ошибки
        if opts
            .progress
            .as_ref()
            .is_some_and(|p| p.written.load(Ordering::SeqCst))
            && !opts.dry_run
        {
            st2.render_gen.bump_space(target.owner, target.space);
            st2.index.notify();
            st2.docs.notify();
        }
        if let Err(e) = audit.done(&st2, &out, res.is_ok(), Some(job)).await {
            tracing::warn!(space = %target.space, error = %e, "аудит импорта не записан");
        }
        let res = match res {
            Ok(r) => Ok(r),
            Err(e) => {
                tracing::warn!(job = %job, "импорт не завершён");
                Err(problem(e).await)
            }
        };
        st2.imports.finish(job, res);
    });
    let status = format!("/spaces/{space}/imports/{job}");
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, format!("/api/v1{status}"))],
        Json(json!({"job": job, "status": status})),
    )
        .into_response())
}

/// `GET /spaces/{space}/imports/{job}` — этап, счётчики, итог или ошибка задания.
pub async fn status(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path((space, job)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let id: Id = job.parse().map_err(|_| ApiError::NotFound)?;
    let j = st.imports.get(id).ok_or(ApiError::NotFound)?;
    let user = match p.bound_user() {
        Some(u) => Some(u),
        None => crate::auth::target_user_id(&st, &p, &headers).ok(),
    };
    // чужое задание — как несуществующее
    if j.space != space || !(p.is_admin() || user == Some(j.user)) {
        return Err(ApiError::NotFound);
    }
    let state = match (j.finished, &j.error) {
        (None, _) => "running",
        (Some(_), None) => "done",
        (Some(_), Some(_)) => "failed",
    };
    Ok(Json(json!({
        "job": id,
        "state": state,
        "stage": j.stage,
        "dry_run": j.dry_run,
        "progress": {
            "total": j.progress.total.load(Ordering::SeqCst),
            "done": j.progress.done.load(Ordering::SeqCst),
        },
        "report": j.report,
        "error": j.error,
    })))
}

/// Слот записи (очередь): ждём, пока сервер не остановится.
async fn write_slot(st: &AppState) -> Result<tokio::sync::OwnedSemaphorePermit, ImportError> {
    loop {
        if st.stopping.load(Ordering::SeqCst) {
            return Err(ImportError::Stopped);
        }
        match tokio::time::timeout(TICK, st.imports.write.clone().acquire_owned()).await {
            Ok(Ok(p)) => return Ok(p),
            Ok(Err(_)) => return Err(ImportError::Stopped),
            Err(_) => continue,
        }
    }
}

/// Задание: очередь, проверка, распаковка, запись (см. модуль); `out` — итог для аудита.
#[allow(clippy::too_many_arguments)]
async fn run(
    st: &AppState,
    job: Id,
    t: &Target,
    part: Part,
    limit: u64,
    mut opts: ImportOptions,
    attrib: zam_store::Attribution,
    out: &mut Outcome,
) -> Result<ImportReport, ApiError> {
    let _write = write_slot(st).await.map_err(|e| import_err(e, out))?;
    st.imports.stage(job, "validating");
    let limits = ImportLimits {
        max_bytes: limit,
        max_files: st.cfg.import.max_files,
        max_ratio: MAX_RATIO_DEFAULT,
        max_text_bytes: st.cfg.import.max_text_bytes,
    };
    let path = part.0.clone();
    let mut plan: Plan = tokio::task::spawn_blocking(move || {
        let f = std::fs::File::open(&path).map_err(|e| ImportError::Store(e.into()))?;
        let mut z = zip::ZipArchive::new(std::io::BufReader::new(f))
            .map_err(|e| ImportError::Rejected(format!("тело — не архив zip: {e}")))?;
        validate_archive(&mut z, &limits)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("проверка архива: {e}")))?
    .map_err(|e| import_err(e, out))?;
    let own = plan.is_own(t.owner, opts.own);
    opts.own = own;
    let mut warnings = Vec::new();
    let taken = taken_ids(
        st,
        t.owner,
        &plan.candidate_ids(t.owner, t.space, own),
        &mut warnings,
    )
    .await?;
    let map = plan_ids(&t.db, t.owner, t.space, &plan, own, &taken).await?;
    let remap = remap_refs(&mut plan, &map);
    plan.warnings.extend(warnings);
    // размер — фактический из архива, MIME — по имени, как при загрузке
    let sizes = plan.blobs.clone();
    let fix = |f: &mut zam_store::import::PlanFile| {
        if let Some((_, n)) = f.file.as_ref().and_then(|x| sizes.get(x)) {
            f.size = *n;
        }
        f.mime = Some(pick_mime(&f.name, None));
    };
    for n in &mut plan.notes {
        n.attachments.iter_mut().for_each(fix);
    }
    for e in &mut plan.entries {
        e.files.iter_mut().for_each(fix);
    }
    if !opts.dry_run {
        st.imports.stage(job, "extracting");
        let store = BlobStore::new(&st.data_dir.blobs_dir(), t.owner);
        let blobs = std::mem::take(&mut plan.blobs);
        let (path, s2) = (part.0.clone(), store.clone());
        tokio::task::spawn_blocking(move || {
            let f = std::fs::File::open(&path).map_err(|e| ImportError::Store(e.into()))?;
            let mut z = zip::ZipArchive::new(std::io::BufReader::new(f))
                .map_err(|e| ImportError::Rejected(format!("тело — не архив zip: {e}")))?;
            extract_blobs(&mut z, &blobs, &s2)
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
    drop(part);
    st.imports.stage(job, "writing");
    let mut report = apply_plan(&t.db, t.space, t.owner, plan, &opts, &attrib)
        .await
        .map_err(|e| import_err(e, out))?;
    for u in remap.unresolved {
        if !report.unresolved.contains(&u) {
            report.unresolved.push(u);
        }
    }
    report.id_remapped = map.remapped.len() as u64;
    report.remapped = map
        .remapped
        .iter()
        .map(|(a, b)| zam_store::import::Remapped {
            from: a.clone(),
            to: b.to_string(),
        })
        .collect();
    tracing::info!(
        space = %t.space,
        job = %job,
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
        job: Option<Id>,
    ) -> Result<(), zam_store::StoreError> {
        let mut d = json!({
            "space": self.space, "user": self.user, "owner": self.owner,
            "complete": ok, "bytes": out.bytes,
        });
        if let Value::Object(o) = &mut d {
            if let Some(j) = job {
                o.insert("job".into(), json!(j));
            }
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
        assert_eq!(Recv::Deadline.reason(), "deadline");
        assert_eq!(Recv::Slow.reason(), "slow");
        assert_eq!(Recv::Io(std::io::Error::other("x")).reason(), "error");
        assert_eq!(
            recv_deadline(2 << 30),
            Duration::from_secs(2048).max(DEADLINE_MIN)
        );
        assert_eq!(recv_deadline(64 << 30), Duration::from_secs(65536));
    }

    fn lim(stall: Duration, deadline: Duration, min_speed: u64, grace: Duration) -> RecvLimits {
        RecvLimits {
            max_bytes: 1 << 20,
            stall,
            deadline,
            min_speed,
            grace,
        }
    }

    fn silent() -> Body {
        Body::from_stream(futures_util::stream::pending::<
            Result<axum::body::Bytes, std::io::Error>,
        >())
    }

    /// Клиент молчит — приём обрывается по `stall`, общему сроку или скорости, а не ждёт вечно;
    /// остановка — сразу.
    #[tokio::test]
    async fn receive_gives_up_on_stall_and_stop() {
        let dir = std::env::current_dir()
            .unwrap()
            .join("target")
            .join("t18b2-recv");
        let p = dir.join("x.part");
        // без защиты приём ждал бы вечно: предел теста — 10 с
        let limit = Duration::from_secs(10);
        let long = Duration::from_secs(3600);
        let run = async |l: RecvLimits, stop: bool| {
            let t0 = Instant::now();
            let stop = AtomicBool::new(stop);
            let r = tokio::time::timeout(limit, receive(silent(), &p, &l, &stop))
                .await
                .expect("приём ждал молчащего клиента дольше 10 с");
            assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
            r
        };
        let r = run(lim(Duration::from_millis(100), long, 0, long), false).await;
        assert!(matches!(r, Err(Recv::Stall)), "{r:?}");
        let r = run(lim(long, Duration::from_millis(300), 0, long), false).await;
        assert!(matches!(r, Err(Recv::Deadline)), "{r:?}");
        let r = run(lim(long, long, 1 << 20, Duration::from_secs(1)), false).await;
        assert!(matches!(r, Err(Recv::Slow)), "{r:?}");
        let r = run(lim(long, long, 0, long), true).await;
        assert!(matches!(r, Err(Recv::Stop)), "{r:?}");
        drop(Part(p));
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn part_removed_on_drop_and_on_start() {
        let dir = std::env::current_dir()
            .unwrap()
            .join("target")
            .join("t18b2-part");
        std::fs::create_dir_all(dir.join(INCOMING)).unwrap();
        let p = dir.join("x.part");
        std::fs::write(&p, b"x").unwrap();
        drop(Part(p.clone()));
        assert!(!p.exists());
        // запуск сервера убирает старые .part
        let old = dir.join(INCOMING).join("old.part");
        let keep = dir.join(INCOMING).join("keep.txt");
        std::fs::write(&old, b"x").unwrap();
        std::fs::write(&keep, b"x").unwrap();
        clean_incoming(&dir);
        assert!(!old.exists() && keep.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
