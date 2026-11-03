//! Карта кода решения (спецификация M5.6 §5, §7, §9, §12–§13; хранилище — [`zam_store::code`]).
//!
//! - `POST /code/sync {solution, commit?, branch?, machine?, manifest: [{path, hash}]}` →
//!   `{need, removed, etag}`: файлы карты вне манифеста удаляются, `need` — пути без строки или с
//!   другим хэшем. Повтор пути (после нормализации) — 422 `manifest[i]: повтор пути`.
//! - `PUT /code/files {solution, commit?, branch?, machine?, files: [FileUpload]}` (до 500) →
//!   `{accepted, stale_refs, warnings}`; сигнатуры маскируются (`zam_markdown::mask_text`) и
//!   хранятся маскированными; правило устарелости ссылок памяти — база владельца и Knowledge
//!   участника (как `POST /files/changed`).
//! - `GET /code/status`, `GET /code/map` (Markdown или JSON в бюджете токенов, `focus`, `area`),
//!   `GET /code/symbols`, `GET /code/symbols/{id | путь#qual}`, `POST /code/impact` — чтение
//!   решения; ответы карты, символов и влияния несут `tokens` (`zam_store::count_tokens`).
//!   `ETag`/`If-None-Match` — у `status` и у `map` без `focus` (etag снимка, `rank_state` и
//!   параметры).
//! - `DELETE /code?solution=` — только владелец решения в своей сессии WebUI, аудит `code.clear`.
//!
//! **Права:** запись — владелец и участник `rw` (токен `ro`, участник `ro` — 403), чтение — любой с
//! правом чтения решения, отзыв — 403 `/problems/solution-access`; Knowledge, Rules, Human —
//! 422 (карта есть только у решений); `code.enabled = false` — 404 у всех маршрутов. Маршруты —
//! вне `memory_routes`: карта — производные данные, не запись памяти.
//!
//! **Ранги** пересчитываются лениво ([`ensure_ranked`]): при `rank_state = pending` фоновая
//! задача (одна на решение, `AppState::code_ranking`) читает входы, считает PageRank в
//! `spawn_blocking` и сохраняет ранги; запрос ждёт её не дольше `code.rank_timeout_ms`, иначе
//! отвечает старыми рангами и `warnings: ["rank pending"]`.
//!
//! **Id символов нестабильны** между синхронизациями (файл перезагружается — символы получают
//! новые id): постоянный адрес символа — `путь#qual` (`GET /code/symbols/{путь#qual}`), id —
//! только внутри одного ответа.
//!
//! Вызовы хранилища собраны в тонкий слой (раздел «хранилище» ниже): интерфейс
//! `zam_store::code` меняется ревью задачи 3.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use jiff::Timestamp;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use zam_code::{
    FileUpload, Focus, Graph, ManifestItem, MapFile, MapSym, NAME_MAX_CHARS, Packed, ParseState,
    QUAL_MAX_CHARS, SIG_MAX_CHARS, SymbolHash, SymbolKind, UPLOAD_FILES_MAX, edges_among,
    focus_restart, rank_on_graph, render_map, split_ref,
};
use zam_core::Id;
use zam_store::code::{
    CodeSnapshot, CodeStatus, MapFileRow, RankInputs, RankState, UploadMeta, code_clear,
    code_impact, code_map_rows, code_put_files, code_rank_inputs, code_status, code_store_ranks,
    code_symbol, code_symbols_search, code_sync_manifest,
};
use zam_store::{
    ChangedFile, Db, RefItem, SpaceKind, check_commit, check_hash, count_tokens, files_changed,
    normalize_ref_path, refs_index,
};

use super::entries::unprocessable;
use super::files::{not_modified, report_dbs};
use super::scope::{MemScope, Mode, Role, SpaceRef, space_scope};
use super::search::SOLUTION_HEADER;
use super::{parse_body, query};
use crate::app::AppState;
use crate::auth::{Client, Principal};
use crate::error::ApiError;

/// Предупреждение: ранги ещё считаются, карта — по старым рангам.
pub const RANK_PENDING: &str = "rank pending";
/// Рёбер в `format=json` не больше (обзор WebUI, §11).
const EDGES_MAX: usize = 120;
/// Наибольший бюджет карты, токенов (§7).
const BUDGET_MAX: usize = 20_000;
/// `focus.text` не длиннее, символов (§9).
const FOCUS_TEXT_MAX: usize = 2000;
/// Путей и имён в `focus` не больше.
const FOCUS_ITEMS_MAX: usize = 100;
/// Символов в выдаче поиска по умолчанию.
const SYMBOLS_LIMIT_DEFAULT: usize = 20;
/// Центральных символов в `GET /code/status`.
const STATUS_CENTRAL: usize = 10;
/// Центральных символов в строке дайджеста.
const DIGEST_CENTRAL: usize = 3;
/// Символов из одного файла среди центральных (§7).
const CENTRAL_PER_FILE: usize = 3;
/// Ветка и машина клиента не длиннее, символов.
const META_MAX_CHARS: usize = 256;
/// `qual` в строке дайджеста не длиннее, символов (строка ≤ 80 токенов, §10).
const DIGEST_QUAL_MAX: usize = 48;

// ---- общие проверки ----

/// `code.enabled = false` — 404 у всех маршрутов карты.
fn enabled(st: &AppState) -> Result<(), ApiError> {
    if st.cfg.code.enabled {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

fn not_solution() -> ApiError {
    unprocessable(
        "solution: карта кода есть только у пространств решений (не Knowledge, Rules, Human)",
    )
}

/// Тело `/code/*` с пределом `code.max_request_bytes`: больше — 413 с пределом в `detail`.
fn code_body<T: DeserializeOwned>(
    st: &AppState,
    b: Result<Bytes, BytesRejection>,
) -> Result<T, ApiError> {
    match b {
        Ok(b) => parse_body(&b),
        Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            Err(ApiError::PayloadTooLarge(format!(
                "тело запроса больше предела code.max_request_bytes = {} байт",
                st.cfg.code.max_request_bytes
            )))
        }
        Err(e) => Err(ApiError::BadRequest(format!("некорректное тело: {e}"))),
    }
}

/// Область решения `raw` (GUID или `Id` пространства) для режима `mode`; пространство не
/// решения — 422, неизвестное — 404, права — как у прочих маршрутов решения.
pub async fn code_scope(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    raw: &str,
    mode: Mode,
) -> Result<MemScope, ApiError> {
    if let SpaceRef::Fixed(_) = SpaceRef::parse(raw)? {
        return Err(not_solution());
    }
    let sc = space_scope(st, p, headers, raw, mode).await?;
    if sc.space.kind != SpaceKind::Solution || sc.space.solution_guid.is_none() {
        return Err(not_solution());
    }
    Ok(sc)
}

/// GUID решения области ([`code_scope`] гарантирует его наличие).
fn guid(sc: &MemScope) -> String {
    sc.space.solution_guid.clone().unwrap_or_default()
}

/// Решение запроса чтения: параметр `solution`, иначе `X-Zam-Solution`; нет обоих — 400.
fn solution_param<'a>(raw: Option<&'a str>, headers: &'a HeaderMap) -> Result<&'a str, ApiError> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            headers
                .get(SOLUTION_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
        })
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::BadRequest("нужен solution или X-Zam-Solution".into()))
}

fn path_of(raw: &str, what: &str) -> Result<String, ApiError> {
    normalize_ref_path(raw).map_err(|e| unprocessable(format!("{what}: {e}")))
}

/// Строка: непустая, не длиннее `max` символов, без управляющих (`tab` — табуляция допустима).
fn check_text(s: &str, max: usize, what: &str, tab: bool) -> Result<(), ApiError> {
    if s.is_empty() {
        return Err(unprocessable(format!("{what}: пустая строка")));
    }
    if s.chars().count() > max {
        return Err(unprocessable(format!("{what}: длиннее {max} символов")));
    }
    if s.chars().any(|c| c.is_control() && !(tab && c == '\t')) {
        return Err(unprocessable(format!("{what}: управляющие символы")));
    }
    Ok(())
}

/// `n` шестнадцатеричных символов (нижний регистр) или `None`.
fn hex_of(s: &str, lens: &[usize]) -> Option<String> {
    let s = s.trim();
    (lens.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| s.to_ascii_lowercase())
}

/// Атрибуция загрузки (`commit` — 4–64 hex, ветка и машина — до 256 символов без управляющих).
fn upload_meta(
    commit: Option<&str>,
    branch: Option<&str>,
    machine: Option<&str>,
    user_id: Id,
) -> Result<UploadMeta, ApiError> {
    let commit = commit
        .map(|c| {
            check_commit(c).ok_or_else(|| unprocessable("commit: sha коммита, 4–64 hex-символа"))
        })
        .transpose()?;
    let text = |v: Option<&str>, what: &str| -> Result<Option<String>, ApiError> {
        v.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| check_text(s, META_MAX_CHARS, what, false).map(|()| s.to_string()))
            .transpose()
    };
    Ok(UploadMeta {
        commit,
        branch: text(branch, "branch")?,
        machine: text(machine, "machine")?,
        user_id,
        now: Timestamp::now(),
    })
}

/// Поля объекта вне `allowed` — 422 `<what>.<поле>: неизвестное поле`.
fn known_fields(o: &Map<String, Value>, allowed: &[&str], what: &str) -> Result<(), ApiError> {
    match o.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(unprocessable(format!("{what}.{k}: неизвестное поле"))),
        None => Ok(()),
    }
}

fn str_field<'a>(o: &'a Map<String, Value>, key: &str, what: &str) -> Result<&'a str, ApiError> {
    match o.get(key) {
        Some(Value::String(s)) => Ok(s),
        None | Some(Value::Null) => Err(unprocessable(format!("{what}.{key}: обязательное поле"))),
        Some(_) => Err(unprocessable(format!("{what}.{key}: ожидается строка"))),
    }
}

/// `ETag` в кавычках.
fn quoted(etag: &str) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(&format!("\"{etag}\"")).map_err(|e| ApiError::Internal(e.to_string()))
}

/// Ответ с `ETag` (или 304 при совпадении `If-None-Match`).
fn with_etag(
    headers: &HeaderMap,
    etag: &str,
    body: impl FnOnce() -> Value,
) -> Result<Response, ApiError> {
    let mut r = if not_modified(headers, etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        Json(body()).into_response()
    };
    let h = r.headers_mut();
    h.insert(header::ETAG, quoted(etag)?);
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    Ok(r)
}

fn etag_of(parts: &[&str]) -> String {
    blake3::hash(parts.join("\n").as_bytes()).to_hex()[..32].to_string()
}

/// Токены выдачи — счётчиком сервера по компактному JSON.
fn tokens_of(v: &Value) -> usize {
    count_tokens(&v.to_string())
}

// ---- хранилище (тонкий слой над `zam_store::code`) ----

/// Состояние карты; нет карты — `None`.
async fn store_status(sc: &MemScope, guid: &str) -> Result<Option<CodeStatus>, ApiError> {
    Ok(code_status(&sc.db, guid).await?)
}

/// Файлы и символы карты по сохранённому рангу (область `area`).
async fn store_rows(
    sc: &MemScope,
    guid: &str,
    area: Option<&str>,
) -> Result<Vec<MapFileRow>, ApiError> {
    Ok(code_map_rows(&sc.db, guid, area).await?)
}

/// Пересчёт рангов: входы → PageRank в `spawn_blocking` → сохранение (`false` — карта
/// изменилась после чтения входов).
async fn store_rank(db: Db, guid: String) -> Result<bool, String> {
    let inputs = code_rank_inputs(&db, &guid)
        .await
        .map_err(|e| e.to_string())?;
    let (inputs, ranked) = tokio::task::spawn_blocking(move || {
        let r = zam_code::rank(&inputs.files, None);
        (inputs, r)
    })
    .await
    .map_err(|e| format!("ранжирование прервано (panic = {})", e.is_panic()))?;
    code_store_ranks(&db, &guid, &inputs, &ranked)
        .await
        .map_err(|e| e.to_string())
}

// ---- ранги ----

/// Снимает отметку «пересчёт идёт» решения при завершении фоновой задачи.
struct RankingGuard {
    set: Arc<Mutex<HashSet<(Id, String)>>>,
    key: (Id, String),
}

impl Drop for RankingGuard {
    fn drop(&mut self) {
        self.set
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.key);
    }
}

/// Ранги решения актуальны или пересчитываются: при `rank_state = pending` запускает фоновый
/// пересчёт (один на решение) и ждёт его не дольше `code.rank_timeout_ms`. Не успел, пересчёт
/// уже идёт, карта изменилась во время счёта или сбой — `["rank pending"]` (карта отдаётся по
/// старым рангам).
pub async fn ensure_ranked(st: &AppState, sc: &MemScope, guid: &str) -> Vec<String> {
    match code_status(&sc.db, guid).await {
        Ok(Some(s)) if s.snapshot.rank_state == RankState::Pending => rank_now(st, sc, guid).await,
        Ok(_) => Vec::new(),
        Err(e) => {
            tracing::warn!(error = %e, "состояние карты кода не прочитано");
            vec![RANK_PENDING.into()]
        }
    }
}

async fn rank_now(st: &AppState, sc: &MemScope, guid: &str) -> Vec<String> {
    let key = (sc.owner, guid.to_string());
    if !st
        .code_ranking
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key.clone())
    {
        return vec![RANK_PENDING.into()];
    }
    let guard = RankingGuard {
        set: st.code_ranking.clone(),
        key,
    };
    let (db, g) = (sc.db.clone(), guid.to_string());
    let task = tokio::spawn(async move {
        let _guard = guard;
        let r = store_rank(db, g).await;
        if let Err(e) = &r {
            tracing::warn!(error = %e, "ранги карты кода не пересчитаны");
        }
        r
    });
    let wait = Duration::from_millis(st.cfg.code.rank_timeout_ms);
    // 0 — не ждать вовсе (таймер tokio округляет нулевой срок до следующего тика)
    if wait.is_zero() {
        return vec![RANK_PENDING.into()];
    }
    match tokio::time::timeout(wait, task).await {
        Ok(Ok(Ok(true))) => Vec::new(),
        // карта изменилась, сбой или задача прервана — по старым рангам
        Ok(_) => vec![RANK_PENDING.into()],
        // не дождались: задача продолжает в фоне и снимет отметку сама
        Err(_) => vec![RANK_PENDING.into()],
    }
}

/// Пересчитывает ранги, если нужно, и перечитывает состояние.
async fn ranked_status(
    st: &AppState,
    sc: &MemScope,
    guid: &str,
) -> Result<(Option<CodeStatus>, Vec<String>), ApiError> {
    let s = store_status(sc, guid).await?;
    if s.as_ref()
        .is_none_or(|s| s.snapshot.rank_state != RankState::Pending)
    {
        return Ok((s, Vec::new()));
    }
    let warnings = rank_now(st, sc, guid).await;
    Ok((store_status(sc, guid).await?, warnings))
}

// ---- тексты ----

/// Русская форма числительного: 1 файл, 2 файла, 5 файлов.
fn plural<'a>(n: u64, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    match (n % 10, n % 100) {
        (_, 11..=14) => many,
        (1, _) => one,
        (2..=4, _) => few,
        _ => many,
    }
}

fn files_text(n: u64) -> String {
    format!("{n} {}", plural(n, "файл", "файла", "файлов"))
}

fn symbols_text(n: u64) -> String {
    format!("{n} {}", plural(n, "символ", "символа", "символов"))
}

/// Возраст снимка: «только что», «12 мин назад», «3 ч назад», «2 дн назад».
fn age_text(secs: i64) -> String {
    match secs {
        ..60 => "только что".into(),
        60..3600 => format!("{} мин назад", secs / 60),
        3600..86_400 => format!("{} ч назад", secs / 3600),
        _ => format!("{} дн назад", secs / 86_400),
    }
}

fn age_s(s: &CodeSnapshot) -> i64 {
    (Timestamp::now().as_second() - s.updated_at.as_second()).max(0)
}

/// Снимок: `ветка@коммит (машина)` — что известно.
fn snapshot_text(s: &CodeSnapshot) -> Option<String> {
    let commit = s.commit.as_deref().map(|c| &c[..c.len().min(7)]);
    let at = match (s.branch.as_deref(), commit) {
        (Some(b), Some(c)) => Some(format!("{b}@{c}")),
        (Some(b), None) => Some(b.to_string()),
        (None, Some(c)) => Some(c.to_string()),
        (None, None) => None,
    };
    match (at, s.machine.as_deref()) {
        (Some(a), Some(m)) => Some(format!("{a}, {m}")),
        (Some(a), None) => Some(a),
        (None, Some(m)) => Some(m.to_string()),
        (None, None) => None,
    }
}

/// Заголовок карты (§5: преобладающий снимок и доля файлов из других).
fn map_header(s: Option<&CodeStatus>, area: Option<&str>, focused: bool) -> String {
    let Some(s) = s else {
        return "# Карта кода пуста — синхронизируйте: `zam code sync`".into();
    };
    let snap = &s.snapshot;
    let mut h = format!(
        "# Карта кода: {}, {}; снимок {}",
        files_text(snap.files),
        symbols_text(snap.symbols),
        age_text(age_s(snap))
    );
    if let Some(t) = snapshot_text(snap) {
        h.push_str(&format!(" ({t})"));
    }
    if s.other_share > 0.0 {
        h.push_str(&format!(
            "; из других снимков — {:.0} %",
            (s.other_share * 100.0).max(1.0)
        ));
    }
    if let Some(a) = area {
        h.push_str(&format!("; область {a}"));
    }
    if focused {
        h.push_str("; с фокусом");
    }
    h
}

/// Центральный символ (§7): первые по рангу, не больше трёх из одного файла.
fn central_of(rows: &[MapFileRow], n: usize) -> Vec<Value> {
    let mut all: Vec<(f64, usize, usize)> = rows
        .iter()
        .enumerate()
        .flat_map(|(i, f)| {
            f.symbols
                .iter()
                .enumerate()
                .map(move |(j, s)| (s.rank, i, j))
        })
        .collect();
    all.sort_by(|a, b| b.0.total_cmp(&a.0).then((a.1, a.2).cmp(&(b.1, b.2))));
    let mut per: HashMap<usize, usize> = HashMap::new();
    let mut out = Vec::new();
    for (_, i, j) in all {
        if out.len() >= n {
            break;
        }
        let c = per.entry(i).or_default();
        if *c < CENTRAL_PER_FILE {
            *c += 1;
            let (f, s) = (&rows[i], &rows[i].symbols[j]);
            out.push(
                json!({"id": s.id, "path": f.path, "qual": s.qual, "kind": s.kind,
                            "sig": s.sig, "line": s.line, "rank": s.rank}),
            );
        }
    }
    out
}

/// Строка дайджеста (§10): размер карты, возраст снимка, центральные символы.
fn digest_line(s: &CodeStatus, central: &[Value]) -> String {
    let snap = &s.snapshot;
    let mut line = format!(
        "ZeroAIMemory: карта кода: {}, {}, снимок {}",
        files_text(snap.files),
        symbols_text(snap.symbols),
        age_text(age_s(snap))
    );
    if let Some(t) = snapshot_text(snap) {
        line.push_str(&format!(" ({t})"));
    }
    let names: Vec<String> = central
        .iter()
        .take(DIGEST_CENTRAL)
        .filter_map(|c| c["qual"].as_str())
        .map(|q| q.chars().take(DIGEST_QUAL_MAX).collect())
        .collect();
    if !names.is_empty() {
        line.push_str(&format!("; центральные: {}", names.join(", ")));
    }
    line.push_str(" — `memory_code`/`zam code map`");
    line
}

/// Строка дайджеста карты решения (для `/recall`, задача 6); карты нет или она пуста — `None`.
pub async fn code_line(sc: &MemScope, guid: &str) -> Option<String> {
    let s = store_status(sc, guid).await.ok()??;
    if s.snapshot.files == 0 {
        return None;
    }
    let rows = store_rows(sc, guid, None).await.ok()?;
    Some(digest_line(&s, &central_of(&rows, DIGEST_CENTRAL)))
}

// ---- POST /code/sync ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncReq {
    solution: String,
    #[serde(default)]
    commit: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    machine: Option<String>,
    manifest: Vec<Value>,
}

/// Манифест: нормализованные пути, хэши, без повторов; ошибки — 422 `manifest[i]…`.
fn parse_manifest(m: &[Value], max: u32) -> Result<Vec<ManifestItem>, ApiError> {
    if m.len() > max as usize {
        return Err(unprocessable(format!(
            "manifest: не больше {max} файлов (code.max_files)"
        )));
    }
    let mut seen: HashSet<String> = HashSet::with_capacity(m.len());
    let mut out = Vec::with_capacity(m.len());
    for (i, v) in m.iter().enumerate() {
        let what = format!("manifest[{i}]");
        let o = v
            .as_object()
            .ok_or_else(|| unprocessable(format!("{what}: ожидается объект {{path, hash}}")))?;
        known_fields(o, &["path", "hash"], &what)?;
        let path = path_of(str_field(o, "path", &what)?, &format!("{what}.path"))?;
        let hash = check_hash(str_field(o, "hash", &what)?).ok_or_else(|| {
            unprocessable(format!("{what}.hash: blake3 содержимого, 64 hex-символа"))
        })?;
        if !seen.insert(path.clone()) {
            return Err(unprocessable(format!("{what}: повтор пути «{path}»")));
        }
        out.push(ManifestItem { path, hash });
    }
    Ok(out)
}

/// `POST /code/sync` → `{need, removed, etag}`.
pub async fn sync(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    enabled(&st)?;
    let req: SyncReq = code_body(&st, body)?;
    let sc = code_scope(&st, &p, &headers, &req.solution, Mode::Write).await?;
    let meta = upload_meta(
        req.commit.as_deref(),
        req.branch.as_deref(),
        req.machine.as_deref(),
        sc.user.id,
    )?;
    let manifest = parse_manifest(&req.manifest, st.cfg.code.max_files)?;
    let out = code_sync_manifest(&sc.db, &guid(&sc), &meta, &manifest).await?;
    Ok(Json(
        json!({ "need": out.need, "removed": out.removed, "etag": out.etag }),
    ))
}

// ---- PUT /code/files ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutReq {
    solution: String,
    #[serde(default)]
    commit: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    machine: Option<String>,
    files: Vec<Value>,
}

/// Проверка файла загрузки `i` (вход недоверенный, §13) и маскировка сигнатур; возвращает
/// число сигнатур, в которых замаскированы секреты.
fn check_upload(
    f: &mut FileUpload,
    i: usize,
    c: &crate::config::CodeSection,
) -> Result<usize, ApiError> {
    let what = format!("files[{i}]");
    f.path = path_of(&f.path, &format!("{what}.path"))?;
    f.hash = check_hash(&f.hash)
        .ok_or_else(|| unprocessable(format!("{what}.hash: blake3 содержимого, 64 hex-символа")))?;
    if let Some(oid) = &f.git_oid {
        f.git_oid = Some(hex_of(oid, &[40, 64]).ok_or_else(|| {
            unprocessable(format!("{what}.git_oid: oid git, 40 или 64 hex-символа"))
        })?);
    }
    if f.symbols.len() > c.max_symbols_per_file as usize {
        return Err(unprocessable(format!(
            "{what}.symbols: не больше {} символов в файле (code.max_symbols_per_file)",
            c.max_symbols_per_file
        )));
    }
    if f.refs.len() > c.max_refs_per_file as usize {
        return Err(unprocessable(format!(
            "{what}.refs: не больше {} ссылок в файле (code.max_refs_per_file)",
            c.max_refs_per_file
        )));
    }
    let mut masked = 0;
    for (j, s) in f.symbols.iter_mut().enumerate() {
        let w = format!("{what}.symbols[{j}]");
        check_text(&s.name, NAME_MAX_CHARS, &format!("{w}.name"), false)?;
        check_text(&s.qual, QUAL_MAX_CHARS, &format!("{w}.qual"), false)?;
        s.body_hash = hex_of(&s.body_hash, &[16])
            .ok_or_else(|| unprocessable(format!("{w}.body_hash: 16 hex-символов")))?;
        if s.line == 0 || s.end_line < s.line {
            return Err(unprocessable(format!(
                "{w}.line: строки с 1, end_line не меньше line"
            )));
        }
        if let Some(sig) = s.sig.take().filter(|x| !x.is_empty()) {
            check_text(&sig, SIG_MAX_CHARS, &format!("{w}.sig"), true)?;
            let m = zam_markdown::mask_text(&sig);
            if m != sig {
                masked += 1;
            }
            s.sig = Some(m);
        }
    }
    for (k, r) in f.refs.iter().enumerate() {
        let w = format!("{what}.refs[{k}]");
        check_text(&r.name, NAME_MAX_CHARS, &format!("{w}.name"), false)?;
        if r.count == 0 {
            return Err(unprocessable(format!("{w}.count: больше 0")));
        }
    }
    Ok(masked)
}

/// Файлы загрузки: не больше 500, каждый проверен ([`check_upload`]).
fn parse_uploads(
    files: Vec<Value>,
    c: &crate::config::CodeSection,
) -> Result<(Vec<FileUpload>, usize), ApiError> {
    if files.len() > UPLOAD_FILES_MAX {
        return Err(unprocessable(format!(
            "files: не больше {UPLOAD_FILES_MAX} файлов за запрос"
        )));
    }
    let mut masked = 0;
    let mut out = Vec::with_capacity(files.len());
    for (i, v) in files.into_iter().enumerate() {
        let mut f: FileUpload =
            serde_json::from_value(v).map_err(|e| unprocessable(format!("files[{i}]: {e}")))?;
        masked += check_upload(&mut f, i, c)?;
        out.push(f);
    }
    Ok((out, masked))
}

/// `PUT /code/files` → `{accepted, stale_refs, warnings}`.
pub async fn put_files(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    enabled(&st)?;
    let req: PutReq = code_body(&st, body)?;
    let sc = code_scope(&st, &p, &headers, &req.solution, Mode::Write).await?;
    let meta = upload_meta(
        req.commit.as_deref(),
        req.branch.as_deref(),
        req.machine.as_deref(),
        sc.user.id,
    )?;
    let (files, masked) = parse_uploads(req.files, &st.cfg.code)?;
    let g = guid(&sc);
    // ссылки памяти Knowledge участника (другая база) — правило `POST /files/changed`;
    // неполный или пропущенный разбор — по хэшу файла
    let changed: Vec<ChangedFile> = files
        .iter()
        .map(|f| ChangedFile {
            path: f.path.clone(),
            hash: f.hash.clone(),
            symbols: (f.parse_state == ParseState::Ok).then(|| {
                f.symbols
                    .iter()
                    .map(|s| SymbolHash {
                        qual: s.qual.clone(),
                        body_hash: s.body_hash.clone(),
                    })
                    .collect()
            }),
        })
        .collect();
    let mut dbs = report_dbs(&st, &p, &sc).await?.into_iter();
    let (_, own_spaces) = dbs
        .next()
        .ok_or_else(|| ApiError::Internal("нет базы владельца решения".into()))?;
    let out = code_put_files(&sc.db, &g, &meta, files, &own_spaces).await?;
    let mut stale_refs = out.stale_refs;
    for (db, spaces) in dbs {
        stale_refs += files_changed(&db, &g, &spaces, &changed, meta.now).await?;
    }
    let mut warnings: Vec<String> = Vec::new();
    if masked > 0 {
        warnings.push(format!("в {masked} сигнатурах замаскированы секреты"));
    }
    Ok(Json(json!({
        "accepted": out.accepted,
        "stale_refs": stale_refs,
        "warnings": warnings,
    })))
}

// ---- GET /code/status ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolutionQuery {
    solution: Option<String>,
}

/// `GET /code/status?solution=` → состояние снимка с `ETag`.
pub async fn status(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<SolutionQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    enabled(&st)?;
    let q = query(q)?;
    let raw = solution_param(q.solution.as_deref(), &headers)?;
    let sc = code_scope(&st, &p, &headers, raw, Mode::Read).await?;
    let g = guid(&sc);
    let (s, warnings) = ranked_status(&st, &sc, &g).await?;
    let Some(s) = s else {
        let etag = etag_of(&["status", "none"]);
        return with_etag(&headers, &etag, || {
            json!({"solution": g, "exists": false, "files": 0, "symbols": 0, "by_lang": [],
                   "skipped": [], "skipped_total": 0, "central": [], "rank_state": null,
                   "digest": null, "warnings": warnings})
        });
    };
    let snap = &s.snapshot;
    let etag = etag_of(&["status", &snap.etag, snap.rank_state.as_str()]);
    if not_modified(&headers, &etag) {
        return with_etag(&headers, &etag, || Value::Null);
    }
    let rows = store_rows(&sc, &g, None).await?;
    let central = central_of(&rows, STATUS_CENTRAL);
    let digest = (snap.files > 0).then(|| digest_line(&s, &central));
    let skipped: Vec<Value> = s
        .skipped
        .iter()
        .map(|(path, reason)| json!({"path": path, "reason": reason}))
        .collect();
    let body = json!({
        "solution": g,
        "exists": true,
        "commit": snap.commit,
        "branch": snap.branch,
        "machine": snap.machine,
        "updated_at": snap.updated_at.to_string(),
        "age_s": age_s(snap),
        "files": snap.files,
        "symbols": snap.symbols,
        "by_lang": s.by_lang,
        "skipped": skipped,
        "skipped_total": s.skipped_total,
        "other_share": s.other_share,
        "rank_state": snap.rank_state,
        "ranked_at": snap.ranked_at.map(|t| t.to_string()),
        "etag": snap.etag,
        "central": central,
        "digest": digest,
        "warnings": warnings,
    });
    with_etag(&headers, &etag, || body)
}

// ---- GET /code/map ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapQuery {
    solution: Option<String>,
    budget_tokens: Option<usize>,
    focus: Option<String>,
    area: Option<String>,
    format: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FocusReq {
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    idents: Vec<String>,
    #[serde(default)]
    text: Option<String>,
}

/// `focus` — JSON `{paths?, idents?, text?}` (§9); пути нормализуются, `text` ≤ 2000 символов.
pub fn parse_focus(raw: &str) -> Result<Focus, ApiError> {
    let f: FocusReq = serde_json::from_str(raw)
        .map_err(|e| unprocessable(format!("focus: JSON {{paths?, idents?, text?}}: {e}")))?;
    if f.paths.len() > FOCUS_ITEMS_MAX || f.idents.len() > FOCUS_ITEMS_MAX {
        return Err(unprocessable(format!(
            "focus: не больше {FOCUS_ITEMS_MAX} путей и {FOCUS_ITEMS_MAX} имён"
        )));
    }
    let paths = f
        .paths
        .iter()
        .enumerate()
        .map(|(i, p)| path_of(p, &format!("focus.paths[{i}]")))
        .collect::<Result<_, _>>()?;
    for (i, n) in f.idents.iter().enumerate() {
        check_text(n, QUAL_MAX_CHARS, &format!("focus.idents[{i}]"), false)?;
    }
    if let Some(t) = &f.text
        && t.chars().count() > FOCUS_TEXT_MAX
    {
        return Err(unprocessable(format!(
            "focus.text: не длиннее {FOCUS_TEXT_MAX} символов"
        )));
    }
    Ok(Focus {
        paths,
        idents: f.idents,
        text: f.text,
    })
}

/// Бюджет карты: по умолчанию `code.map_tokens`, 1..=20000 (иначе 422).
pub fn map_budget(st: &AppState, b: Option<usize>) -> Result<usize, ApiError> {
    let b = b.unwrap_or(st.cfg.code.map_tokens);
    if (1..=BUDGET_MAX).contains(&b) {
        Ok(b)
    } else {
        Err(unprocessable(format!(
            "budget_tokens: от 1 до {BUDGET_MAX}"
        )))
    }
}

/// Собранная карта: строки по рангу (с фокусом — персонализированному), упаковка, рёбра.
struct MapOut {
    header: String,
    rows: Vec<MapFileRow>,
    packed: Packed,
    /// `(src id, dst id, вес)` между показанными файлами, до [`EDGES_MAX`].
    edges: Vec<(i64, i64, f64)>,
    warnings: Vec<String>,
}

/// Переставляет строки по рангу файлов `fr` (по id): ранг символа = ранг файла × доля.
fn apply_focus(rows: &mut [MapFileRow], fr: &HashMap<i64, f64>) {
    for f in rows.iter_mut() {
        f.rank = fr.get(&f.id).copied().unwrap_or(0.0);
        for s in f.symbols.iter_mut() {
            s.rank = f.rank * s.share;
        }
        f.symbols
            .sort_by(|a, b| b.rank.total_cmp(&a.rank).then(a.line.cmp(&b.line)));
    }
    rows.sort_by(|a, b| b.rank.total_cmp(&a.rank).then_with(|| a.path.cmp(&b.path)));
}

/// Ранги файлов по фокусу (`None` — совпадений нет, глобальный ранг).
fn focus_ranks(inputs: &RankInputs, f: &Focus) -> Option<HashMap<i64, f64>> {
    let restart = focus_restart(&inputs.files, f)?;
    let r = rank_on_graph(&Graph::new(&inputs.files), Some(&restart));
    Some(inputs.files.iter().map(|x| x.id).zip(r).collect())
}

/// Карта в бюджете: строки карты, персонализация `focus` (не дольше `code.rank_timeout_ms`,
/// иначе — глобальный ранг и предупреждение), упаковка счётчиком сервера, рёбра (`edges`).
#[allow(clippy::too_many_arguments)]
async fn build_map(
    st: &AppState,
    sc: &MemScope,
    guid: &str,
    status: Option<&CodeStatus>,
    budget: usize,
    focus: Option<&Focus>,
    area: Option<&str>,
    edges: bool,
) -> Result<MapOut, ApiError> {
    let mut warnings = Vec::new();
    let mut rows = match status {
        Some(_) => store_rows(sc, guid, area).await?,
        None => Vec::new(),
    };
    let mut inputs: Option<RankInputs> = None;
    if let (Some(f), Some(_)) = (focus, status) {
        let inp = code_rank_inputs(&sc.db, guid).await?;
        let f = f.clone();
        let task = tokio::task::spawn_blocking(move || {
            let r = focus_ranks(&inp, &f);
            (inp, r)
        });
        let wait = Duration::from_millis(st.cfg.code.rank_timeout_ms);
        match tokio::time::timeout(wait, task).await {
            Ok(Ok((inp, fr))) => {
                if let Some(fr) = fr {
                    apply_focus(&mut rows, &fr);
                }
                inputs = Some(inp);
            }
            Ok(Err(e)) => return Err(ApiError::aborted("ранжирование с фокусом", &e)),
            Err(_) => warnings.push("focus rank timeout".to_string()),
        }
    }
    let header = map_header(status, area, focus.is_some());
    let files: Vec<MapFile> = rows
        .iter()
        .map(|f| MapFile {
            path: f.path.clone(),
            syms: f
                .symbols
                .iter()
                .map(|s| MapSym {
                    kind: s.kind,
                    qual: s.qual.clone(),
                    sig: s.sig.clone(),
                    line: s.line,
                })
                .collect(),
        })
        .collect();
    let h = header.clone();
    let packed = tokio::task::spawn_blocking(move || render_map(&h, &files, budget, &count_tokens))
        .await
        .map_err(|e| ApiError::aborted("упаковка карты", &e))?;
    let mut out_edges = Vec::new();
    if edges && packed.files_shown > 1 {
        let inp = match inputs {
            Some(i) => i,
            None => code_rank_inputs(&sc.db, guid).await?,
        };
        let shown: Vec<i64> = rows.iter().take(packed.files_shown).map(|f| f.id).collect();
        out_edges = tokio::task::spawn_blocking(move || {
            let at: HashMap<i64, usize> = inp
                .files
                .iter()
                .enumerate()
                .map(|(i, f)| (f.id, i))
                .collect();
            let idx: Vec<usize> = shown.iter().filter_map(|id| at.get(id).copied()).collect();
            edges_among(&inp.files, &idx)
                .into_iter()
                .take(EDGES_MAX)
                .map(|(a, b, w)| (inp.files[a].id, inp.files[b].id, w))
                .collect()
        })
        .await
        .map_err(|e| ApiError::aborted("рёбра карты", &e))?;
    }
    Ok(MapOut {
        header,
        rows,
        packed,
        edges: out_edges,
        warnings,
    })
}

/// Карта решения в бюджете Markdown (для MCP `memory_code action=map`, задача 7): ранги
/// пересчитываются при необходимости ([`ensure_ranked`]).
pub async fn map_text(
    st: &AppState,
    sc: &MemScope,
    guid: &str,
    budget: usize,
    focus: Option<&Focus>,
    area: Option<&str>,
) -> Result<Packed, ApiError> {
    let (s, _) = ranked_status(st, sc, guid).await?;
    Ok(
        build_map(st, sc, guid, s.as_ref(), budget, focus, area, false)
            .await?
            .packed,
    )
}

/// Показанные файлы карты JSON: первые `files_shown`, символы — первые `symbols_shown` подряд.
fn shown_json(rows: &[MapFileRow], p: &Packed) -> Vec<Value> {
    let mut left = p.symbols_shown;
    rows.iter()
        .take(p.files_shown)
        .map(|f| {
            let take = left.min(f.symbols.len());
            left -= take;
            let syms: Vec<Value> = f.symbols[..take]
                .iter()
                .map(|s| {
                    json!({"id": s.id, "kind": s.kind, "qual": s.qual, "sig": s.sig,
                                "line": s.line, "rank": s.rank})
                })
                .collect();
            json!({"id": f.id, "path": f.path, "lang": f.lang, "rank": f.rank, "symbols": syms})
        })
        .collect()
}

/// `GET /code/map?solution=&budget_tokens=&focus=&area=&format=md|json`.
pub async fn map(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<MapQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    enabled(&st)?;
    let q = query(q)?;
    let raw = solution_param(q.solution.as_deref(), &headers)?;
    let sc = code_scope(&st, &p, &headers, raw, Mode::Read).await?;
    let budget = map_budget(&st, q.budget_tokens)?;
    let focus = q
        .focus
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_focus)
        .transpose()?;
    let area = q
        .area
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|a| path_of(a, "area"))
        .transpose()?;
    let json_fmt = match q.format.as_deref().map(str::trim) {
        None | Some("") | Some("md") => false,
        Some("json") => true,
        Some(_) => return Err(unprocessable("format: md или json")),
    };
    let g = guid(&sc);
    let (s, mut warnings) = ranked_status(&st, &sc, &g).await?;
    let etag = focus.is_none().then(|| {
        let (snap, state) = s.as_ref().map_or(("none", "none"), |s| {
            (s.snapshot.etag.as_str(), s.snapshot.rank_state.as_str())
        });
        etag_of(&[
            "map",
            snap,
            state,
            &budget.to_string(),
            area.as_deref().unwrap_or(""),
            if json_fmt { "json" } else { "md" },
        ])
    });
    if let Some(e) = &etag
        && not_modified(&headers, e)
    {
        return with_etag(&headers, e, || Value::Null);
    }
    let m = build_map(
        &st,
        &sc,
        &g,
        s.as_ref(),
        budget,
        focus.as_ref(),
        area.as_deref(),
        json_fmt,
    )
    .await?;
    warnings.extend(m.warnings.iter().cloned());
    let rank_state = s.as_ref().map(|s| s.snapshot.rank_state);
    let pk = &m.packed;
    let body = if json_fmt {
        let edges: Vec<Value> = m
            .edges
            .iter()
            .map(|(a, b, w)| json!({"src": a, "dst": b, "weight": w}))
            .collect();
        json!({
            "format": "json",
            "header": m.header,
            "files": shown_json(&m.rows, pk),
            "edges": edges,
            "files_shown": pk.files_shown,
            "symbols_shown": pk.symbols_shown,
            "files_left": pk.files_left,
            "symbols_left": pk.symbols_left,
            "tokens": pk.tokens,
            "rank_state": rank_state,
            "warnings": warnings,
        })
    } else {
        json!({
            "format": "md",
            "text": pk.text,
            "files_shown": pk.files_shown,
            "symbols_shown": pk.symbols_shown,
            "files_left": pk.files_left,
            "symbols_left": pk.symbols_left,
            "tokens": pk.tokens,
            "rank_state": rank_state,
            "warnings": warnings,
        })
    };
    match etag {
        Some(e) => with_etag(&headers, &e, || body),
        None => Ok(Json(body).into_response()),
    }
}

// ---- GET /code/symbols, /code/symbols/{id} ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolsQuery {
    solution: Option<String>,
    q: Option<String>,
    kind: Option<String>,
    path: Option<String>,
    limit: Option<usize>,
}

/// `GET /code/symbols?solution=&q=&kind=&path=&limit=` → `{symbols, tokens, warnings}`.
pub async fn symbols(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<SymbolsQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    enabled(&st)?;
    let q = query(q)?;
    let raw = solution_param(q.solution.as_deref(), &headers)?;
    let sc = code_scope(&st, &p, &headers, raw, Mode::Read).await?;
    let text = q.q.as_deref().map(str::trim).unwrap_or_default();
    check_text(text, QUAL_MAX_CHARS, "q", false)?;
    let kind = q
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(|k| {
            SymbolKind::parse(k).ok_or_else(|| {
                let all: Vec<&str> = SymbolKind::ALL.iter().map(|k| k.as_str()).collect();
                unprocessable(format!("kind: один из {}", all.join("|")))
            })
        })
        .transpose()?;
    let limit = q.limit.unwrap_or(SYMBOLS_LIMIT_DEFAULT);
    if !(1..=zam_store::code::SYMBOLS_LIMIT_MAX).contains(&limit) {
        return Err(unprocessable(format!(
            "limit: от 1 до {}",
            zam_store::code::SYMBOLS_LIMIT_MAX
        )));
    }
    let path = q
        .path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|a| path_of(a, "path"))
        .transpose()?;
    let g = guid(&sc);
    let warnings = ensure_ranked(&st, &sc, &g).await;
    let hits = code_symbols_search(&sc.db, &g, text, kind, path.as_deref(), limit).await?;
    let symbols = json!(hits);
    Ok(Json(json!({
        "tokens": tokens_of(&symbols),
        "symbols": symbols,
        "warnings": warnings,
    })))
}

/// Знания решения о путях (`refs_index` базы владельца и Knowledge участника): путь → записи,
/// символьные ссылки первыми. Хранилище отдаёт до 5 записей на путь в базе.
async fn knowledge_index(
    st: &AppState,
    p: &Principal,
    sc: &MemScope,
    guid: &str,
) -> Result<HashMap<String, Vec<RefItem>>, ApiError> {
    let mut by: HashMap<String, Vec<RefItem>> = HashMap::new();
    for (db, spaces) in report_dbs(st, p, sc).await? {
        for pr in refs_index(&db, guid, &spaces).await? {
            let items = by.entry(pr.path).or_default();
            items.extend(pr.items);
            // символьные первыми и после слияния баз (сортировка устойчива)
            items.sort_by_key(|i| i.symbol.is_none());
        }
    }
    Ok(by)
}

fn knowledge_json(path: &str, it: &RefItem) -> Value {
    let mut v = json!({"path": path, "id": it.id, "kind": it.kind, "title": it.title,
                       "state": it.state});
    if let Some(s) = &it.symbol {
        v["symbol"] = json!(s);
    }
    v
}

/// Символ по `{id}` (число) или по постоянному адресу `путь#qual` (URL-кодированному).
async fn find_symbol(sc: &MemScope, guid: &str, raw: &str) -> Result<i64, ApiError> {
    if let Ok(id) = raw.parse::<i64>() {
        return Ok(id);
    }
    let (path, qual) = match split_ref(raw) {
        (p, Some(q)) => (path_of(p, "symbol")?, q.to_string()),
        _ => {
            return Err(ApiError::BadRequest("символ: id или путь#qual".into()));
        }
    };
    let hits = code_symbols_search(&sc.db, guid, &qual, None, Some(&path), 1).await?;
    hits.into_iter()
        .find(|h| h.path == path && h.qual == qual)
        .map(|h| h.id)
        .ok_or(ApiError::NotFound)
}

/// `GET /code/symbols/{id | путь#qual}?solution=` → карточка: определение, кто ссылается, на
/// что ссылается файл, привязанные знания (символьные первыми, ссылки на другие символы файла —
/// нет).
pub async fn symbol(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    q: Result<Query<SolutionQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    enabled(&st)?;
    let q = query(q)?;
    let raw = solution_param(q.solution.as_deref(), &headers)?;
    let sc = code_scope(&st, &p, &headers, raw, Mode::Read).await?;
    let g = guid(&sc);
    let id = find_symbol(&sc, &g, raw_id.trim()).await?;
    let card = code_symbol(&sc.db, &g, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let index = knowledge_index(&st, &p, &sc, &g).await?;
    let path = card.symbol.path.clone();
    let knowledge: Vec<Value> = index
        .get(&path)
        .into_iter()
        .flatten()
        .filter(|it| it.symbol.as_deref().is_none_or(|s| s == card.symbol.qual))
        .map(|it| knowledge_json(&path, it))
        .collect();
    let mut sym = json!(card.symbol);
    sym["body_hash"] = json!(card.body_hash);
    sym["exported"] = json!(card.exported);
    sym["ref"] = json!(format!("{}#{}", card.symbol.path, card.symbol.qual));
    let mut body = json!({
        "symbol": sym,
        "callers": card.callers,
        "uses": card.uses,
        "knowledge": knowledge,
    });
    body["tokens"] = json!(tokens_of(&body));
    Ok(Json(body))
}

// ---- POST /code/impact ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactReq {
    solution: String,
    files: Vec<Value>,
}

type ImpactFile = (String, Option<Vec<SymbolHash>>);

/// Файлы влияния: `{path, symbols?: [{qual, body_hash}]}`, до 500; повтор пути — первый.
fn parse_impact(files: &[Value], max_symbols: u32) -> Result<Vec<ImpactFile>, ApiError> {
    if files.len() > UPLOAD_FILES_MAX {
        return Err(unprocessable(format!(
            "files: не больше {UPLOAD_FILES_MAX} файлов за запрос"
        )));
    }
    let mut out: Vec<ImpactFile> = Vec::with_capacity(files.len());
    for (i, v) in files.iter().enumerate() {
        let what = format!("files[{i}]");
        let o = v
            .as_object()
            .ok_or_else(|| unprocessable(format!("{what}: ожидается объект {{path, symbols?}}")))?;
        known_fields(o, &["path", "symbols"], &what)?;
        let path = path_of(str_field(o, "path", &what)?, &format!("{what}.path"))?;
        let symbols = match o.get("symbols") {
            None | Some(Value::Null) => None,
            Some(Value::Array(a)) => {
                if a.len() > max_symbols as usize {
                    return Err(unprocessable(format!(
                        "{what}.symbols: не больше {max_symbols} символов (code.max_symbols_per_file)"
                    )));
                }
                let mut syms = Vec::with_capacity(a.len());
                for (j, s) in a.iter().enumerate() {
                    let w = format!("{what}.symbols[{j}]");
                    let so = s.as_object().ok_or_else(|| {
                        unprocessable(format!("{w}: ожидается объект {{qual, body_hash}}"))
                    })?;
                    known_fields(so, &["qual", "body_hash"], &w)?;
                    let qual = str_field(so, "qual", &w)?;
                    check_text(qual, QUAL_MAX_CHARS, &format!("{w}.qual"), false)?;
                    let body_hash = hex_of(str_field(so, "body_hash", &w)?, &[16])
                        .ok_or_else(|| unprocessable(format!("{w}.body_hash: 16 hex-символов")))?;
                    syms.push(SymbolHash {
                        qual: qual.to_string(),
                        body_hash,
                    });
                }
                Some(syms)
            }
            Some(_) => return Err(unprocessable(format!("{what}.symbols: ожидается массив"))),
        };
        if !out.iter().any(|(p, _)| *p == path) {
            out.push((path, symbols));
        }
    }
    Ok(out)
}

/// `POST /code/impact {solution, files}` → изменённые и удалённые символы, потребители на 1
/// шаг, знания: о путях без символов — все, о файлах с изменениями — файловые и ссылки на
/// изменённые и удалённые символы.
pub async fn impact(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    enabled(&st)?;
    let req: ImpactReq = code_body(&st, body)?;
    let sc = code_scope(&st, &p, &headers, &req.solution, Mode::Read).await?;
    let files = parse_impact(&req.files, st.cfg.code.max_symbols_per_file)?;
    let g = guid(&sc);
    let rows = code_impact(&sc.db, &g, &files).await?;
    let mut affected: HashMap<&str, HashSet<&str>> = HashMap::new();
    for s in rows.changed.iter().chain(&rows.removed) {
        affected
            .entry(s.path.as_str())
            .or_default()
            .insert(s.qual.as_str());
    }
    let index = knowledge_index(&st, &p, &sc, &g).await?;
    let mut knowledge = Vec::new();
    for (path, _) in &files {
        let all = rows.paths_only.contains(path);
        let quals = affected.get(path.as_str());
        if !all && quals.is_none() {
            continue;
        }
        for it in index.get(path).into_iter().flatten() {
            let take = all
                || it
                    .symbol
                    .as_deref()
                    .is_none_or(|s| quals.is_some_and(|q| q.contains(s)));
            if take {
                knowledge.push(knowledge_json(path, it));
            }
        }
    }
    let mut body = json!({
        "changed": rows.changed,
        "removed": rows.removed,
        "consumers": rows.consumers,
        "paths_only": rows.paths_only,
        "knowledge": knowledge,
        "warnings": [],
    });
    body["tokens"] = json!(tokens_of(&body));
    Ok(Json(body))
}

// ---- DELETE /code ----

/// Текст 403 очистки карты не владельцем.
pub const CLEAR_OWNER_ONLY: &str = "очистить карту кода может только владелец решения в WebUI";

/// Очистка — только владельцу решения в его сессии WebUI: пользователь в своей базе
/// (`Role::Owner`) или администратор в своей; участники, администратор от имени другого и любой
/// токен — 403.
fn clear_allowed(st: &AppState, p: &Principal, sc: &MemScope) -> Result<(), ApiError> {
    let own = match p {
        Principal::User { user_id, .. } => sc.owner == *user_id && sc.role == Role::Owner,
        Principal::Admin { .. } => {
            sc.owner == st.admin_id && matches!(sc.role, Role::Owner | Role::Admin)
        }
        Principal::Token(_) => false,
    };
    if own {
        Ok(())
    } else {
        Err(ApiError::Denied(CLEAR_OWNER_ONLY.into()))
    }
}

/// `DELETE /code?solution=` → `{removed}`; аудит `code.clear`.
pub async fn clear(
    State(st): State<AppState>,
    p: Principal,
    client: Client,
    headers: HeaderMap,
    q: Result<Query<SolutionQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    enabled(&st)?;
    let q = query(q)?;
    let raw = solution_param(q.solution.as_deref(), &headers)?;
    let sc = code_scope(&st, &p, &headers, raw, Mode::Write).await?;
    clear_allowed(&st, &p, &sc)?;
    let g = guid(&sc);
    let removed = code_clear(&sc.db, &g).await?;
    st.system
        .audit(
            &p.actor(),
            Some(sc.user.id),
            "code.clear",
            Some(&g),
            Some(&client.ip.to_string()),
            json!({ "files": removed }),
        )
        .await?;
    tracing::info!(guid = %g, files = removed, "карта кода очищена");
    Ok(Json(json!({ "removed": removed })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plural_and_age() {
        assert_eq!(files_text(1), "1 файл");
        assert_eq!(files_text(4), "4 файла");
        assert_eq!(files_text(11), "11 файлов");
        assert_eq!(symbols_text(22), "22 символа");
        assert_eq!(age_text(5), "только что");
        assert_eq!(age_text(125), "2 мин назад");
        assert_eq!(age_text(7200), "2 ч назад");
        assert_eq!(age_text(3 * 86_400), "3 дн назад");
    }

    #[test]
    fn focus_parsed_and_checked() {
        let f =
            parse_focus(r#"{"paths": ["./src\\a.rs"], "idents": ["Store"], "text": "x"}"#).unwrap();
        assert_eq!(f.paths, ["src/a.rs"]);
        let err = |s: &str| match parse_focus(s) {
            Err(ApiError::Unprocessable(m)) => m,
            other => panic!("ожидалась 422: {other:?}"),
        };
        assert!(err(r#"{"paths": ["../x"]}"#).starts_with("focus.paths[0]"));
        assert!(err(r#"{"other": 1}"#).starts_with("focus:"));
        assert!(err(r#"{"idents": ["a\nb"]}"#).starts_with("focus.idents[0]"));
    }

    #[test]
    fn text_checks() {
        assert!(check_text("a\tb", 10, "x", true).is_ok());
        assert!(check_text("a\tb", 10, "x", false).is_err());
        assert!(check_text("абв", 3, "x", false).is_ok());
        assert!(check_text("абвг", 3, "x", false).is_err());
        assert_eq!(
            hex_of("ABCDEF0123456789", &[16]).as_deref(),
            Some("abcdef0123456789")
        );
        assert!(hex_of("xyz", &[16]).is_none());
    }
}
