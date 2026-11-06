//! Карта кода решения (спецификация M5.6 §5, §7, §9, §12–§13; хранилище — [`zam_store::code`]).
//!
//! - `POST /code/sync {solution, commit?, branch?, machine?, manifest: [{path, hash}]}` →
//!   `{need, removed, etag}`: файлы карты вне манифеста удаляются, `need` — пути без строки или с
//!   другим хэшем. Повтор пути (после нормализации) — 422 `manifest[i]: повтор пути`.
//! - `PUT /code/files {solution, commit?, branch?, machine?, files: [FileUpload]}` (до 500, а
//!   карта целиком — до `code.max_files`) → `{accepted, stale_refs, warnings}`; сигнатуры
//!   маскируются (`zam_markdown::mask_text`) и хранятся маскированными; правило карты для ссылок
//!   памяти — только ссылки на символы, в базе владельца и в Knowledge участника (файловые
//!   ссылки судят хуки рабочей копии).
//! - `GET /code/status`, `GET /code/map` (Markdown или JSON в бюджете токенов, `focus`, `area`,
//!   `files` — не больше N файлов),
//!   `GET /code/files` (таблица файлов по рангу, постранично), `GET /code/symbols`,
//!   `GET /code/symbols/{id | путь#qual}`, `POST /code/impact` — чтение решения; ответы карты,
//!   символов и влияния несут `tokens` (`zam_store::count_tokens`). Слабый `ETag`
//!   (`W/"…"`)/`If-None-Match` — у `status` и у `map` без `focus` (etag снимка, `rank_state` и
//!   параметры): возраст снимка в тексте меняется, а содержимое — нет.
//!
//! Права проверяются после чтения тела: у `sync`/`files`/`impact` решение — поле тела (предел
//! тела ограничивает цену чужого запроса).
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
//! **Карта читается в две фазы:** файлы по рангу (`code_map_files`), затем символы только
//! первых файлов (`code_map_symbols`) — порциями, пока упаковка показывает все прочитанные.
//! Центральные символы — `code_central` (без чтения карты), знания о пути — `path_owners`
//! (без предела индекса хуков). Вызовы хранилища собраны в тонкий слой (раздел «хранилище»).
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use zam_code::{
    FileUpload, Focus, Graph, ManifestItem, MapFile, MapSym, NAME_MAX_CHARS, Packed, ParseState,
    QUAL_MAX_CHARS, SIG_MAX_CHARS, SymbolHash, SymbolKind, UPLOAD_FILES_MAX, edges_among,
    focus_restart, rank_on_graph, render_map_rest, split_ref,
};
use zam_core::Id;
use zam_store::code::{
    CodeSnapshot, CodeStatus, MapFileRow, MapSymRow, RankInputs, RankState, SymbolHit, UploadMeta,
    code_central, code_clear, code_files_page, code_impact, code_map_files, code_map_symbols,
    code_put_files, code_rank_inputs, code_snapshot, code_status, code_store_ranks, code_symbol,
    code_symbols_search, code_sync_manifest,
};
use zam_store::file_refs::PATH_MAX_CHARS;
use zam_store::{
    ChangedFile, Db, RefItem, SpaceKind, check_commit, check_hash, count_tokens,
    normalize_ref_path, path_owners, symbols_uploaded,
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
/// Файлов, которые перебирает поиск центральных символов (по убыванию ранга).
const CENTRAL_FILES_MAX: usize = 5000;
/// Первая порция файлов карты, символы которых читаются (вторая фаза), — не меньше.
const MAP_FILES_FIRST: usize = 64;
/// Запас на элемент манифеста сверх пути (`{"path":…,"hash":<64 hex>}` и экранирование), байт.
const MANIFEST_ITEM_OVERHEAD: usize = 100;
/// Ранжирований с фокусом одновременно (CPU: PageRank по всей карте на запрос).
const FOCUS_SLOTS: usize = 2;
/// Тяжёлых операций карты одновременно на весь сервер (§12): чтений входов ранжирования вне
/// кэша и фоновых пересчётов рангов (чтение входов и PageRank). Входы карты 50 000 файлов —
/// ≈ 180 МиБ: без общего предела N решений в `pending` читали бы и считали N раз параллельно
/// (финальное ревью ветки M5.6, I1).
const HEAVY_SLOTS: usize = 2;
/// Срок ранжирования с фокусом не меньше, мс.
const FOCUS_TIMEOUT_MIN_MS: u64 = 200;
/// Входов ранжирования в кэше фокуса (по `(владелец, GUID, etag снимка)`).
const FOCUS_CACHE: usize = 2;
/// Память кэша фокуса не больше, байт ([`RankInputs::heap_bytes`]): 50 000 символов — ≈ 9 МиБ,
/// карта 50 000 файлов × 20 символов × 40 ссылок — ≈ 180 МиБ (замер ревью задачи 4, O2).
/// Входы больше предела не кэшируются (фокус читает их заново, одно чтение на решение).
const FOCUS_CACHE_BYTES: usize = 256 * 1024 * 1024;
/// Знаний в карточке символа и во влиянии не больше (сверх — `knowledge_left`).
const KNOWLEDGE_MAX: usize = 200;
/// Предел параметра `files` у `GET /code/map`.
const MAP_FILES_PARAM_MAX: usize = 1000;
/// Файлов на странице `GET /code/files` по умолчанию и не больше.
const FILES_PAGE_DEFAULT: usize = 50;
const FILES_PAGE_MAX: usize = 200;
/// Ветка и машина клиента не длиннее, символов.
const META_MAX_CHARS: usize = 256;
/// `qual` в строке дайджеста не длиннее, символов (строка ≤ 80 токенов, §10).
const DIGEST_QUAL_MAX: usize = 48;

// ---- состояние сервера ----

/// Ключ решения: `(владелец, GUID)`.
type SolutionKey = (Id, String);

/// Состояние карты кода на сервере (`AppState::code`): пересчёты рангов (single-flight на
/// решение), слоты тяжёлых операций и ранжирования с фокусом, кэш входов ранжирования.
pub struct CodeRuntime {
    /// Решения, ранги которых сейчас пересчитываются.
    ranking: Mutex<HashSet<SolutionKey>>,
    /// Слоты ранжирования с фокусом ([`FOCUS_SLOTS`]).
    focus_slots: Arc<tokio::sync::Semaphore>,
    /// Слоты тяжёлых операций на весь сервер ([`HEAVY_SLOTS`], [`heavy_slot`]): слот держит
    /// задача чтения входов вне кэша или фоновый пересчёт рангов целиком. Ожидающий слота
    /// пересчёт ничего не держит в памяти; запрос ждёт его в пределах `code.rank_timeout_ms`.
    heavy_slots: Arc<tokio::sync::Semaphore>,
    /// Входы ранжирования последних решений: `((владелец, GUID), etag снимка, входы, байт)`.
    inputs: Mutex<VecDeque<CachedInputs>>,
    /// Идущее чтение входов решения — не больше одного на `(владелец, GUID)`: `(etag, чтение)`.
    /// Запрос с тем же `etag` ждёт его (single-flight), с другим (карта меняется, длинная
    /// синхронизация) — ждёт конца и читает сам. Срок запроса чтение не отменяет — дочитанное
    /// ляжет в кэш для следующего запроса.
    loading: Mutex<HashMap<SolutionKey, (String, InputsLoad)>>,
    /// Тестовые точки (фича `test-hooks`).
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub hooks: CodeHooks,
}

impl Default for CodeRuntime {
    fn default() -> Self {
        Self {
            ranking: Mutex::default(),
            focus_slots: Arc::new(tokio::sync::Semaphore::new(FOCUS_SLOTS)),
            heavy_slots: Arc::new(tokio::sync::Semaphore::new(HEAVY_SLOTS)),
            inputs: Mutex::default(),
            loading: Mutex::default(),
            #[cfg(feature = "test-hooks")]
            hooks: CodeHooks::default(),
        }
    }
}

/// Запись кэша входов: `((владелец, GUID), etag снимка, входы, байт)`.
type CachedInputs = (SolutionKey, String, Arc<RankInputs>, usize);

/// Чтение входов ранжирования, которого могут ждать несколько запросов.
type InputsLoad = futures_util::future::Shared<
    futures_util::future::BoxFuture<'static, Result<Arc<RankInputs>, String>>,
>;

impl CodeRuntime {
    /// Входы из кэша по `(владелец, GUID, etag снимка)`.
    fn cached(&self, key: &SolutionKey, etag: &str) -> Option<Arc<RankInputs>> {
        self.inputs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(k, e, _, _)| k == key && e == etag)
            .map(|(_, _, i, _)| i.clone())
    }

    /// Кладёт входы в кэш: последние [`FOCUS_CACHE`] решений не больше [`FOCUS_CACHE_BYTES`]
    /// вместе, у решения — одна запись; входы больше предела не кладутся.
    fn remember(&self, key: SolutionKey, etag: String, inputs: Arc<RankInputs>) {
        let bytes = inputs.heap_bytes();
        let mut cache = self.inputs.lock().unwrap_or_else(PoisonError::into_inner);
        cache.retain(|(k, _, _, _)| *k != key);
        if bytes > FOCUS_CACHE_BYTES {
            return;
        }
        cache.push_front((key, etag, inputs, bytes));
        cache.truncate(FOCUS_CACHE);
        let mut total = 0usize;
        let keep = cache
            .iter()
            .take_while(|(_, _, _, b)| {
                total += b;
                total <= FOCUS_CACHE_BYTES
            })
            .count();
        cache.truncate(keep);
    }

    /// Очищает кэш входов и счётчики чтений (тестовая точка): чтения пересчёта рангов до неё
    /// (тот же путь [`load_inputs`]) в счёт теста не идут.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub fn forget_inputs(&self) {
        use std::sync::atomic::Ordering;
        self.inputs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        self.hooks.inputs_reads.store(0, Ordering::SeqCst);
        let active = self.hooks.inputs_active.load(Ordering::SeqCst);
        self.hooks.inputs_active_max.store(active, Ordering::SeqCst);
    }
}

/// Входы прочитаны при неизменном снимке: `etag` до и после чтения совпал — входы кладутся в
/// кэш под ним (иначе карта менялась во время чтения, и кэш не заполняется).
async fn remember_if_unchanged(
    rt: &CodeRuntime,
    db: &Db,
    key: SolutionKey,
    before: &str,
    inputs: &Arc<RankInputs>,
) {
    if let Ok(Some(after)) = code_snapshot(db, &key.1).await
        && after.etag == before
    {
        rt.remember(key, after.etag, inputs.clone());
    }
}

/// Тестовые точки карты кода: задержки фонового пересчёта (после чтения входов), ранжирования
/// с фокусом и чтения входов для кэша; счётчики пересчётов и чтений.
#[cfg(feature = "test-hooks")]
#[doc(hidden)]
#[derive(Default)]
pub struct CodeHooks {
    pub rank_delay_ms: std::sync::atomic::AtomicU64,
    pub focus_delay_ms: std::sync::atomic::AtomicU64,
    pub rank_runs: std::sync::atomic::AtomicUsize,
    /// Пересчётов рангов на шаге «входы прочитаны → PageRank» сейчас и наибольшее их число.
    pub rank_active: std::sync::atomic::AtomicUsize,
    pub rank_active_max: std::sync::atomic::AtomicUsize,
    /// Задержка перед чтением входов для кэша фокуса, мс, и число таких чтений.
    pub inputs_delay_ms: std::sync::atomic::AtomicU64,
    pub inputs_reads: std::sync::atomic::AtomicUsize,
    /// Паника в следующем чтении входов (сбрасывается этим чтением).
    pub inputs_panic: std::sync::atomic::AtomicBool,
    /// Наибольшее число одновременных чтений входов одного решения.
    pub inputs_active: std::sync::atomic::AtomicUsize,
    pub inputs_active_max: std::sync::atomic::AtomicUsize,
    /// Занятых слотов тяжёлых операций ([`HEAVY_SLOTS`]) сейчас и наибольшее их число.
    pub heavy_active: std::sync::atomic::AtomicUsize,
    pub heavy_active_max: std::sync::atomic::AtomicUsize,
}

/// Слот тяжёлой операции карты ([`HEAVY_SLOTS`]); освобождается при сбросе.
struct HeavyPermit {
    _permit: tokio::sync::OwnedSemaphorePermit,
    #[cfg(feature = "test-hooks")]
    rt: Arc<CodeRuntime>,
}

#[cfg(feature = "test-hooks")]
impl Drop for HeavyPermit {
    fn drop(&mut self) {
        self.rt
            .hooks
            .heavy_active
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Ждёт слот тяжёлой операции (очередь — по порядку обращения).
async fn heavy_slot(rt: &Arc<CodeRuntime>) -> Result<HeavyPermit, String> {
    let permit = rt
        .heavy_slots
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| e.to_string())?;
    #[cfg(feature = "test-hooks")]
    {
        use std::sync::atomic::Ordering;
        let h = &rt.hooks;
        let now = h.heavy_active.fetch_add(1, Ordering::SeqCst) + 1;
        h.heavy_active_max.fetch_max(now, Ordering::SeqCst);
    }
    Ok(HeavyPermit {
        _permit: permit,
        #[cfg(feature = "test-hooks")]
        rt: rt.clone(),
    })
}

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

/// Предел тела `POST /code/sync`, байт: манифест на `code.max_files` путей предельной длины
/// (`max_files × (PATH_MAX_CHARS + 100)`), но не меньше `code.max_request_bytes`.
pub fn sync_body_max(c: &crate::config::CodeSection) -> usize {
    let request = usize::try_from(c.max_request_bytes).unwrap_or(usize::MAX);
    (c.max_files as usize)
        .saturating_mul(PATH_MAX_CHARS + MANIFEST_ITEM_OVERHEAD)
        .max(request)
}

/// Тело `/code/*` с пределом `limit` байт (слой маршрута): больше — 413 с пределом в `detail`.
fn code_body<T: DeserializeOwned>(
    limit: usize,
    b: Result<Bytes, BytesRejection>,
) -> Result<T, ApiError> {
    parse_body(&code_bytes(limit, b)?)
}

/// Тело запроса карты: превышение предела — 413 с пределом, иная ошибка чтения — 400.
fn code_bytes(limit: usize, b: Result<Bytes, BytesRejection>) -> Result<Bytes, ApiError> {
    match b {
        Ok(b) => Ok(b),
        Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => Err(ApiError::PayloadTooLarge(
            format!("тело запроса больше предела {limit} байт (code.max_request_bytes)"),
        )),
        Err(e) => Err(ApiError::BadRequest(format!("некорректное тело: {e}"))),
    }
}

/// Ошибка чтения тела `POST /code/sync`: превышение предела — 413 с пределом, иное (обрыв
/// соединения, ошибка кодирования) — 400 (ревью задачи 4).
fn body_read_error(e: axum::Error, limit: usize) -> ApiError {
    let inner = e.into_inner();
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(inner.as_ref());
    while let Some(err) = cur {
        if err.is::<http_body_util::LengthLimitError>() {
            return ApiError::PayloadTooLarge(format!(
                "тело запроса больше предела {limit} байт (манифест: code.max_files путей)"
            ));
        }
        cur = err.source();
    }
    ApiError::BadRequest(format!("некорректное тело: {inner}"))
}

/// Предел тела `PUT /code/files` и `POST /code/impact`, байт.
fn request_max(st: &AppState) -> usize {
    usize::try_from(st.cfg.code.max_request_bytes).unwrap_or(usize::MAX)
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

/// `qual` без `/`: иначе ссылка `путь#qual` разбиралась бы как путь (`split_ref`).
fn check_qual(q: &str, what: &str) -> Result<(), ApiError> {
    if q.contains('/') {
        return Err(unprocessable(format!("{what}: «/» в qual недопустим")));
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
        now: zam_store::time::now(),
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

/// Слабый `ETag` (`W/"…"`): ответ семантически тот же при том же снимке, рангах и параметрах,
/// хотя возраст снимка в тексте («5 мин назад») меняется.
fn quoted(etag: &str) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(&format!("W/\"{etag}\"")).map_err(|e| ApiError::Internal(e.to_string()))
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

/// Файлы карты по сохранённому рангу (область `area`), без символов — первая фаза карты.
async fn store_files(
    sc: &MemScope,
    guid: &str,
    area: Option<&str>,
) -> Result<Vec<MapFileRow>, ApiError> {
    Ok(code_map_files(&sc.db, guid, area).await?)
}

/// Символы файлов `ids` по рангу — вторая фаза карты.
async fn store_symbols(
    sc: &MemScope,
    guid: &str,
    ids: &[i64],
) -> Result<HashMap<i64, Vec<MapSymRow>>, ApiError> {
    Ok(code_map_symbols(&sc.db, guid, ids)
        .await?
        .into_iter()
        .collect())
}

/// Центральные символы (§7) без чтения всей карты.
async fn store_central(sc: &MemScope, guid: &str, n: usize) -> Result<Vec<SymbolHit>, ApiError> {
    Ok(code_central(&sc.db, guid, n, CENTRAL_FILES_MAX).await?)
}

/// Строка снимка (лёгкое чтение `rank_state`, `etag`).
async fn store_snapshot(sc: &MemScope, guid: &str) -> Result<Option<CodeSnapshot>, ApiError> {
    Ok(code_snapshot(&sc.db, guid).await?)
}

/// Пересчёт рангов под слотом тяжёлой операции ([`rank_inputs`]): снимок ещё `pending` → входы
/// → PageRank в `spawn_blocking` → сохранение (`false` — карта изменилась после чтения входов).
/// Ранги уже посчитаны (другой запрос успел) — `true` без счёта.
async fn store_rank(rt: Arc<CodeRuntime>, owner: Id, db: Db, guid: String) -> Result<bool, String> {
    let snapshot = code_snapshot(&db, &guid).await.map_err(|e| e.to_string())?;
    if snapshot
        .as_ref()
        .is_none_or(|s| s.rank_state != RankState::Pending)
    {
        return Ok(true);
    }
    // слот — до чтения входов и до конца счёта: ожидающий пересчёт ничего не держит в памяти
    let Some((_slot, inputs)) = rank_inputs(&rt, &db, (owner, guid.clone())).await? else {
        return Ok(true);
    };
    #[cfg(feature = "test-hooks")]
    rt.hooks
        .rank_runs
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    #[cfg(feature = "test-hooks")]
    {
        use std::sync::atomic::Ordering;
        let h = &rt.hooks;
        let now = h.rank_active.fetch_add(1, Ordering::SeqCst) + 1;
        h.rank_active_max.fetch_max(now, Ordering::SeqCst);
        let ms = h.rank_delay_ms.load(Ordering::SeqCst);
        if ms > 0 {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
    }
    let ranked = tokio::task::spawn_blocking(move || {
        let r = zam_code::rank(&inputs.files, None);
        (inputs, r)
    })
    .await;
    #[cfg(feature = "test-hooks")]
    rt.hooks
        .rank_active
        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    let (inputs, ranked) =
        ranked.map_err(|e| format!("ранжирование прервано (panic = {})", e.is_panic()))?;
    code_store_ranks(&db, &guid, &inputs, &ranked, zam_store::time::now())
        .await
        .map(|written| written.is_some())
        .map_err(|e| e.to_string())
}

// ---- ранги ----

/// Снимает отметку «пересчёт идёт» решения при завершении фоновой задачи.
struct RankingGuard {
    rt: Arc<CodeRuntime>,
    key: SolutionKey,
}

impl Drop for RankingGuard {
    fn drop(&mut self) {
        self.rt
            .ranking
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
    match code_snapshot(&sc.db, guid).await {
        Ok(Some(s)) if s.rank_state == RankState::Pending => rank_now(st, sc, guid).await,
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
        .code
        .ranking
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key.clone())
    {
        return vec![RANK_PENDING.into()];
    }
    let guard = RankingGuard {
        rt: st.code.clone(),
        key,
    };
    let (rt, owner, db, g) = (st.code.clone(), sc.owner, sc.db.clone(), guid.to_string());
    let task = tokio::spawn(async move {
        let _guard = guard;
        let r = store_rank(rt, owner, db, g).await;
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
    let pending = store_snapshot(sc, guid)
        .await?
        .is_some_and(|s| s.rank_state == RankState::Pending);
    let warnings = if pending {
        rank_now(st, sc, guid).await
    } else {
        Vec::new()
    };
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
    (zam_store::time::now().as_second() - s.updated_at.as_second()).max(0)
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

/// Центральные символы в JSON ответа.
fn central_json(hits: &[SymbolHit]) -> Vec<Value> {
    hits.iter()
        .map(|s| {
            json!({"id": s.id, "path": s.path, "qual": s.qual, "kind": s.kind,
                   "sig": s.sig, "line": s.line, "rank": s.rank})
        })
        .collect()
}

/// Предел строки дайджеста карты, токенов (§10).
pub const DIGEST_LINE_TOKENS: usize = 80;
/// Предел имени ветки в строке дайджеста, символов (длиннее — обрезка с `…`).
const DIGEST_BRANCH_MAX: usize = 40;

/// Возраст снимка в строке дайджеста (Minor 7 ревью задачи 4): «N мин» (меньше часа), «N ч»
/// (меньше 48 ч), иначе «N дн.».
fn digest_age(secs: i64) -> String {
    match secs.max(0) {
        s @ ..3600 => format!("{} мин", s / 60),
        s @ ..172_800 => format!("{} ч", s / 3600),
        s => format!("{} дн.", s / 86_400),
    }
}

/// Снимок строки дайджеста: `ветка@коммит7` — что известно, без машины (Minor 7).
fn digest_snapshot(s: &CodeSnapshot) -> Option<String> {
    let commit = s.commit.as_deref().map(|c| &c[..c.len().min(7)]);
    // ветка — до 256 символов: в строке ≤ 80 токенов — до [`DIGEST_BRANCH_MAX`] (ревью
    // задачи 6, M1)
    let branch = s.branch.as_deref().map(|b| {
        if b.chars().count() > DIGEST_BRANCH_MAX {
            let mut t: String = b.chars().take(DIGEST_BRANCH_MAX).collect();
            t.push('…');
            t
        } else {
            b.to_string()
        }
    });
    match (branch.as_deref(), commit) {
        (Some(b), Some(c)) => Some(format!("{b}@{c}")),
        (Some(b), None) => Some(b.to_string()),
        (None, Some(c)) => Some(c.to_string()),
        (None, None) => None,
    }
}

/// Строка дайджеста (§10): размер карты, возраст снимка, центральные символы — до
/// [`DIGEST_CENTRAL`]; строка не помещается в [`DIGEST_LINE_TOKENS`] по `fits` (сервер —
/// [`digest_fits`], M6 §4.2) — центральных 2, 1, 0, а без центральных всё ещё не помещается —
/// и без скобки снимка.
fn digest_line(s: &CodeStatus, central: &[Value], fits: &dyn Fn(&str) -> bool) -> String {
    let snap = &s.snapshot;
    let bare = format!(
        "ZeroAIMemory: карта кода: {}, {}, снимок {}",
        files_text(snap.files),
        symbols_text(snap.symbols),
        digest_age(age_s(snap))
    );
    let mut head = bare.clone();
    if let Some(t) = digest_snapshot(snap) {
        head.push_str(&format!(" ({t})"));
    }
    let names: Vec<String> = central
        .iter()
        .take(DIGEST_CENTRAL)
        .filter_map(|c| c["qual"].as_str())
        .map(|q| q.chars().take(DIGEST_QUAL_MAX).collect())
        .collect();
    let line = |n: usize| {
        let mut l = head.clone();
        if n > 0 {
            l.push_str(&format!("; центральные: {}", names[..n].join(", ")));
        }
        l.push_str(" — `memory_code`/`zam code map`");
        l
    };
    (0..=names.len())
        .rev()
        .map(line)
        .find(|l| fits(l))
        .unwrap_or_else(|| format!("{bare} — `memory_code`/`zam code map`"))
}

/// Строка дайджеста помещается в [`DIGEST_LINE_TOKENS`] счётчиком сервера (`fits`, M6 §4.2).
fn digest_fits(line: &str) -> bool {
    zam_store::fits_budget(line, DIGEST_LINE_TOKENS)
}

/// Строка дайджеста карты решения `guid` в базе `db` (для `/recall`: `session_start` и
/// `recovery`, задача 6); карты нет, она пуста или не читается — `None`.
pub async fn code_line(db: &Db, guid: &str) -> Option<String> {
    let s = code_status(db, guid).await.ok()??;
    if s.snapshot.files == 0 {
        return None;
    }
    let central = code_central(db, guid, DIGEST_CENTRAL, CENTRAL_FILES_MAX)
        .await
        .ok()?;
    Some(digest_line(&s, &central_json(&central), &digest_fits))
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
    manifest: Vec<ManifestIn>,
}

/// Элемент манифеста как он пришёл (разбор сразу в строки, без промежуточного `Value`: тело
/// манифеста — до десятков мегабайт).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestIn {
    path: String,
    hash: String,
}

/// Манифест: нормализованные пути, хэши, без повторов; ошибки — 422 `manifest[i]…`.
fn parse_manifest(m: Vec<ManifestIn>, max: u32) -> Result<Vec<ManifestItem>, ApiError> {
    if m.len() > max as usize {
        return Err(unprocessable(format!(
            "manifest: не больше {max} файлов (code.max_files)"
        )));
    }
    let mut seen: HashSet<String> = HashSet::with_capacity(m.len());
    let mut out = Vec::with_capacity(m.len());
    for (i, v) in m.into_iter().enumerate() {
        let what = format!("manifest[{i}]");
        let path = path_of(&v.path, &format!("{what}.path"))?;
        let hash = check_hash(&v.hash).ok_or_else(|| {
            unprocessable(format!("{what}.hash: blake3 содержимого, 64 hex-символа"))
        })?;
        if !seen.insert(path.clone()) {
            return Err(unprocessable(format!(
                "{what}: повтор пути «{}»",
                zam_store::shown_ref(&path)
            )));
        }
        out.push(ManifestItem { path, hash });
    }
    Ok(out)
}

/// `POST /code/sync` → `{need, removed, stale_refs, etag}`. С заголовком `X-Zam-Solution`
/// права проверяются до чтения тела (манифест — до десятков мегабайт), и `solution` тела
/// обязан совпадать с ним.
pub async fn sync(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<Json<Value>, ApiError> {
    enabled(&st)?;
    let early = match headers
        .get(SOLUTION_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw) => Some(code_scope(&st, &p, &headers, raw, Mode::Write).await?),
        None => None,
    };
    let limit = sync_body_max(&st.cfg.code);
    let bytes = axum::body::to_bytes(body, limit)
        .await
        .map_err(|e| body_read_error(e, limit))?;
    let req: SyncReq = parse_body(&bytes)?;
    drop(bytes);
    let sc = match early {
        Some(sc) => {
            let body_sc = SpaceRef::parse(&req.solution)?;
            let same = match body_sc {
                SpaceRef::Solution(g) => sc.space.solution_guid.as_deref() == Some(g.as_str()),
                SpaceRef::Id(id) => sc.space.id == id,
                SpaceRef::Fixed(_) => false,
            };
            if !same {
                return Err(unprocessable("solution: не совпадает с X-Zam-Solution"));
            }
            sc
        }
        None => code_scope(&st, &p, &headers, &req.solution, Mode::Write).await?,
    };
    let meta = upload_meta(
        req.commit.as_deref(),
        req.branch.as_deref(),
        req.machine.as_deref(),
        sc.user.id,
    )?;
    let manifest = parse_manifest(req.manifest, st.cfg.code.max_files)?;
    let g = guid(&sc);
    let mut dbs = report_dbs(&st, &p, &sc).await?.into_iter();
    let (_, own_spaces) = dbs
        .next()
        .ok_or_else(|| ApiError::Internal("нет базы владельца решения".into()))?;
    let others: Vec<(Db, Vec<Id>)> = dbs.collect();
    // удаляемые пути — до синхронизации: ссылки на их символы в Knowledge участника (другая
    // база) — `symbol_removed`, как у владельца
    let gone: Vec<ChangedFile> = if others.is_empty() {
        Vec::new()
    } else {
        let keep: HashSet<&str> = manifest.iter().map(|m| m.path.as_str()).collect();
        store_files(&sc, &g, None)
            .await?
            .into_iter()
            .filter(|f| !keep.contains(f.path.as_str()))
            .map(|f| ChangedFile {
                path: f.path,
                hash: zam_code::MISSING_HASH.to_string(),
                symbols: None,
            })
            .collect()
    };
    let out = code_sync_manifest(&sc.db, &g, &meta, &manifest, &own_spaces).await?;
    let mut stale_refs = out.stale_refs;
    if !gone.is_empty() {
        for (db, spaces) in others {
            stale_refs += symbols_uploaded(&db, &g, &spaces, &gone, meta.now).await?;
        }
    }
    Ok(Json(json!({
        "need": out.need,
        "removed": out.removed,
        "stale_refs": stale_refs,
        "etag": out.etag,
    })))
}

// ---- PUT /code/files ----

/// Тело `PUT /code/files`. Файлы — сырые срезы тела (`&RawValue`): каждый разбирается сразу в
/// [`FileUpload`] с индексом в ошибке, без промежуточного `Value` — кратного перерасхода памяти
/// на теле до `code.max_request_bytes` (финальное ревью ветки M5.6, M3).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutReq<'a> {
    solution: String,
    #[serde(default)]
    commit: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    machine: Option<String>,
    #[serde(borrow)]
    files: Vec<&'a serde_json::value::RawValue>,
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
        check_qual(&s.qual, &format!("{w}.qual"))?;
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
    files: Vec<&serde_json::value::RawValue>,
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
            serde_json::from_str(v.get()).map_err(|e| unprocessable(format!("files[{i}]: {e}")))?;
        masked += check_upload(&mut f, i, c)?;
        out.push(f);
    }
    Ok((out, masked))
}

/// Карта решения после загрузки — не больше `max` файлов (`code.max_files`): текущие файлы и
/// новые пути загрузки; иначе 422 с пределом. Новые пути считаются, только если быстрая оценка
/// «файлы + загрузка» предел превышает.
async fn check_map_size(
    sc: &MemScope,
    guid: &str,
    files: &[FileUpload],
    max: u32,
) -> Result<(), ApiError> {
    let have = store_snapshot(sc, guid).await?.map_or(0, |s| s.files);
    if have + files.len() as u64 <= u64::from(max) {
        return Ok(());
    }
    let known: HashSet<String> = store_files(sc, guid, None)
        .await?
        .into_iter()
        .map(|f| f.path)
        .collect();
    let new: HashSet<&str> = files
        .iter()
        .map(|f| f.path.as_str())
        .filter(|p| !known.contains(*p))
        .collect();
    if known.len() + new.len() > max as usize {
        return Err(unprocessable(format!(
            "files: в карте решения не больше {max} файлов (code.max_files): сейчас {}, новых {}",
            known.len(),
            new.len()
        )));
    }
    Ok(())
}

/// `PUT /code/files` → `{accepted, stale_refs, warnings}`.
pub async fn put_files(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    enabled(&st)?;
    let body = code_bytes(request_max(&st), body)?;
    let req: PutReq<'_> = parse_body(&body)?;
    let sc = code_scope(&st, &p, &headers, &req.solution, Mode::Write).await?;
    let meta = upload_meta(
        req.commit.as_deref(),
        req.branch.as_deref(),
        req.machine.as_deref(),
        sc.user.id,
    )?;
    let (files, masked) = parse_uploads(req.files, &st.cfg.code)?;
    let g = guid(&sc);
    check_map_size(&sc, &g, &files, st.cfg.code.max_files).await?;
    // ссылки на символы в Knowledge участника (другая база) — то же правило карты, что у
    // `code_put_files`: файловые ссылки и хэш файла не трогаются (CRLF-близнец другой машины не
    // мигает ими, ревью задачи 3, I2); неполный или пропущенный разбор — `None` (не судится)
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
        stale_refs += symbols_uploaded(&db, &g, &spaces, &changed, meta.now).await?;
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
    let central = central_json(&store_central(&sc, &g, STATUS_CENTRAL).await?);
    let digest = (snap.files > 0).then(|| digest_line(&s, &central, &digest_fits));
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
    /// Файлов карты не больше (`1..=`[`MAP_FILES_PARAM_MAX`]): «Обзор» WebUI берёт 40 —
    /// без лишних символов в ответе, рёбра — только между ними (ревью задачи 8).
    files: Option<usize>,
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

/// Переставляет файлы по рангу `fr` (по id, ранг с фокусом); ранги символов — при чтении
/// символов ([`rerank_symbols`]).
fn apply_focus(rows: &mut [MapFileRow], fr: &HashMap<i64, f64>) {
    for f in rows.iter_mut() {
        f.rank = fr.get(&f.id).copied().unwrap_or(0.0);
    }
    rows.sort_by(|a, b| b.rank.total_cmp(&a.rank).then_with(|| a.path.cmp(&b.path)));
}

/// Ранги символов файла с фокусом: ранг файла × вычисляемая доля, по убыванию.
fn rerank_symbols(f: &mut MapFileRow) {
    for s in f.symbols.iter_mut() {
        s.rank = f.rank * s.share;
    }
    f.symbols.sort_by(|a, b| {
        b.rank
            .total_cmp(&a.rank)
            .then(a.line.cmp(&b.line))
            .then(a.id.cmp(&b.id))
    });
}

/// Прочитанные файлы карты (первые `loaded`) для упаковки.
fn pack_input(rows: &[MapFileRow], loaded: usize) -> Vec<MapFile> {
    rows[..loaded]
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
        .collect()
}

/// Ранги файлов по фокусу (`None` — совпадений нет, глобальный ранг).
fn focus_ranks(inputs: &RankInputs, f: &Focus) -> Option<HashMap<i64, f64>> {
    let restart = focus_restart(&inputs.files, f)?;
    let r = rank_on_graph(&Graph::new(&inputs.files), Some(&restart));
    Some(inputs.files.iter().map(|x| x.id).zip(r).collect())
}

/// Входы ранжирования решения: из кэша (тот же `etag` снимка — та же карта) или задачей
/// чтения — одной на решение ([`CodeRuntime::loading`]). Чтение под другим `etag` (карта
/// менялась) запрос дожидается и затем читает сам: во время длинной синхронизации полных чтений
/// решения не больше одного одновременно. Отмена ожидания (срок запроса) чтения не отменяет:
/// дочитанное ляжет в кэш.
async fn cached_inputs(
    st: &AppState,
    sc: &MemScope,
    guid: &str,
    etag: &str,
) -> Result<Arc<RankInputs>, ApiError> {
    load_inputs(&st.code, &sc.db, (sc.owner, guid.to_string()), etag)
        .await
        .map_err(ApiError::Internal)
}

/// Чтений под чужим `etag`, которые запрос дожидается, не больше: дальше — итог последнего из
/// них (входы чуть старее снимка: рёбра и фокус по исчезнувшим файлам просто не находятся).
/// Иначе во время длинной синхронизации запрос без своего срока (рёбра JSON-карты) ждал бы
/// чтение за чтением (ревью задачи 9A, M4).
const INPUTS_FOREIGN_WAITS: usize = 3;

/// Входы решения `key` под `etag`: кэш или чтение через [`CodeRuntime::loading`] — общий путь
/// запросов с фокусом, рёбер карты и фонового пересчёта рангов ([`store_rank`]).
async fn load_inputs(
    rt: &Arc<CodeRuntime>,
    db: &Db,
    key: SolutionKey,
    etag: &str,
) -> Result<Arc<RankInputs>, String> {
    let mut waits = 0;
    loop {
        if let Some(i) = rt.cached(&key, etag) {
            return Ok(i);
        }
        match inputs_load(rt, db, &key, etag, false) {
            Load::Started(load) | Load::Same(load) => return load.await,
            // чужой `etag`: дождаться и проверить кэш снова
            Load::Foreign(load) => {
                let r = load.await;
                waits += 1;
                if waits >= INPUTS_FOREIGN_WAITS
                    && let Ok(i) = r
                {
                    return Ok(i);
                }
            }
        }
    }
}

/// Слот тяжёлой операции и входы фонового пересчёта рангов решения `key`; снимок перечитывается
/// под слотом (уже не `pending` — `None`). Входы — из кэша, чтением под этим же слотом
/// ([`inputs_load`] с `covered`) или, если чтение решения уже идёт (запрос с фокусом, рёбра
/// карты), — дождавшись его **без** слота: чужому чтению нужен свой слот, и два пересчёта,
/// ждущие таких чтений со слотами в руках, заняли бы оба слота навсегда.
async fn rank_inputs(
    rt: &Arc<CodeRuntime>,
    db: &Db,
    key: SolutionKey,
) -> Result<Option<(HeavyPermit, Arc<RankInputs>)>, String> {
    let mut waits = 0;
    loop {
        let slot = heavy_slot(rt).await?;
        let etag = match code_snapshot(db, &key.1).await.map_err(|e| e.to_string())? {
            Some(s) if s.rank_state == RankState::Pending => s.etag,
            _ => return Ok(None),
        };
        if let Some(i) = rt.cached(&key, &etag) {
            return Ok(Some((slot, i)));
        }
        match inputs_load(rt, db, &key, &etag, true) {
            Load::Started(load) => return load.await.map(|i| Some((slot, i))),
            Load::Same(load) | Load::Foreign(load) => {
                drop(slot);
                let r = load.await;
                waits += 1;
                if waits >= INPUTS_FOREIGN_WAITS
                    && let Ok(i) = r
                {
                    return Ok(Some((heavy_slot(rt).await?, i)));
                }
            }
        }
    }
}

/// Снимает запись чтения из [`CodeRuntime::loading`] и при панике задачи (ревью задачи 4):
/// иначе упавшее чтение осталось бы «вечным», и решение не читалось бы до смены `etag`.
struct LoadingGuard {
    rt: Arc<CodeRuntime>,
    key: SolutionKey,
    etag: String,
}

impl Drop for LoadingGuard {
    fn drop(&mut self) {
        let mut loading = self
            .rt
            .loading
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if loading.get(&self.key).is_some_and(|(e, _)| *e == self.etag) {
            loading.remove(&self.key);
        }
        #[cfg(feature = "test-hooks")]
        self.rt
            .hooks
            .inputs_active
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Задача чтения входов решения из [`inputs_load`].
enum Load {
    /// Новая — запущена этим вызовом под его `etag`.
    Started(InputsLoad),
    /// Уже шла под тем же `etag` (общая).
    Same(InputsLoad),
    /// Уже шла под другим `etag` — её надо дождаться.
    Foreign(InputsLoad),
}

/// Идущая задача чтения входов решения `key` ([`Load`]). Новая задача читает под своим слотом
/// тяжёлой операции ([`heavy_slot`]); `covered` — слот уже держит вызывающий (фоновый пересчёт,
/// [`rank_inputs`]), и задача его не ждёт.
fn inputs_load(
    rt: &Arc<CodeRuntime>,
    db: &Db,
    key: &SolutionKey,
    etag: &str,
    covered: bool,
) -> Load {
    use futures_util::FutureExt;
    let mut loading = rt.loading.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((e, f)) = loading.get(key) {
        return if e == etag {
            Load::Same(f.clone())
        } else {
            Load::Foreign(f.clone())
        };
    }
    let guard = LoadingGuard {
        rt: rt.clone(),
        key: key.clone(),
        etag: etag.to_string(),
    };
    #[cfg(feature = "test-hooks")]
    {
        use std::sync::atomic::Ordering;
        let h = &rt.hooks;
        let now = h.inputs_active.fetch_add(1, Ordering::SeqCst) + 1;
        h.inputs_active_max.fetch_max(now, Ordering::SeqCst);
    }
    let db = db.clone();
    // задача снимает себя из `loading` охранником под тем же замком — после вставки ниже
    let task = tokio::spawn(async move {
        let g = guard;
        let _slot = match covered {
            true => None,
            false => Some(heavy_slot(&g.rt).await?),
        };
        #[cfg(feature = "test-hooks")]
        {
            use std::sync::atomic::Ordering;
            let h = &g.rt.hooks;
            h.inputs_reads.fetch_add(1, Ordering::SeqCst);
            let ms = h.inputs_delay_ms.load(Ordering::SeqCst);
            if ms > 0 {
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
            if h.inputs_panic.swap(false, Ordering::SeqCst) {
                panic!("тестовая паника чтения входов");
            }
        }
        let r = code_rank_inputs(&db, &g.key.1)
            .await
            .map(Arc::new)
            .map_err(|e| e.to_string());
        if let Ok(inputs) = &r {
            remember_if_unchanged(&g.rt, &db, g.key.clone(), &g.etag, inputs).await;
        }
        drop(g);
        r
    });
    let load: InputsLoad = async move {
        task.await
            .unwrap_or_else(|e| Err(format!("чтение входов прервано (panic = {})", e.is_panic())))
    }
    .boxed()
    .shared();
    loading.insert(key.clone(), (etag.to_string(), load.clone()));
    Load::Started(load)
}

/// Ранги файлов с фокусом: входы (кэш или общее чтение), слот ([`FOCUS_SLOTS`]) и PageRank —
/// под одним сроком `max(code.rank_timeout_ms, 200)`. Срок вышел — `Err(())` (карта без
/// фокуса); чтение входов при этом продолжается и заполнит кэш, слот занят счётом до его конца.
async fn focus_file_ranks(
    st: &AppState,
    sc: &MemScope,
    guid: &str,
    etag: &str,
    focus: &Focus,
) -> Result<Result<Option<HashMap<i64, f64>>, ()>, ApiError> {
    let wait = Duration::from_millis(st.cfg.code.rank_timeout_ms.max(FOCUS_TIMEOUT_MIN_MS));
    let work = async {
        let inputs = cached_inputs(st, sc, guid, etag).await?;
        let permit = st
            .code
            .focus_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        let f = focus.clone();
        #[cfg(feature = "test-hooks")]
        let delay = st
            .code
            .hooks
            .focus_delay_ms
            .load(std::sync::atomic::Ordering::SeqCst);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            #[cfg(feature = "test-hooks")]
            if delay > 0 {
                std::thread::sleep(Duration::from_millis(delay));
            }
            focus_ranks(&inputs, &f)
        })
        .await
        .map_err(|e| ApiError::aborted("ранжирование с фокусом", &e))
    };
    match tokio::time::timeout(wait, work).await {
        Ok(r) => r.map(Ok),
        Err(_) => Ok(Err(())),
    }
}

/// Карта в бюджете: файлы по рангу (с `focus` — персонализированному, не дольше
/// `max(code.rank_timeout_ms, 200)`, иначе глобальный ранг и `focus rank timeout`), символы
/// первых файлов порциями, упаковка счётчиком сервера, рёбра (`edges`).
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
    files_max: Option<usize>,
) -> Result<MapOut, ApiError> {
    let mut warnings = Vec::new();
    // фаза 1: файлы по рангу (без символов)
    let mut rows = match status {
        Some(_) => store_files(sc, guid, area).await?,
        None => Vec::new(),
    };
    let mut focused = false;
    if let (Some(f), Some(s)) = (focus, status) {
        match focus_file_ranks(st, sc, guid, &s.snapshot.etag, f).await? {
            Ok(Some(fr)) => {
                apply_focus(&mut rows, &fr);
                focused = true;
            }
            Ok(None) => {}
            Err(()) => warnings.push("focus rank timeout".to_string()),
        }
    }
    let header = map_header(status, area, focused);
    // фаза 2: символы первых файлов; не все показаны — готово, иначе порция ×4
    let total = rows.len();
    // `files`: читаются и показываются только первые; остальные — числом в хвосте
    let cap = files_max.map_or(total, |n| n.min(total));
    let mut want = cap.min((budget / 8).max(MAP_FILES_FIRST));
    let mut loaded = 0;
    let packed = loop {
        let ids: Vec<i64> = rows[loaded..want].iter().map(|f| f.id).collect();
        let mut syms = store_symbols(sc, guid, &ids).await?;
        for f in &mut rows[loaded..want] {
            f.symbols = syms.remove(&f.id).unwrap_or_default();
            if focused {
                rerank_symbols(f);
            }
        }
        loaded = want;
        // непрочитанные файлы — только числом в хвосте «… ещё N файлов, M символов»
        let rest = (
            total - loaded,
            rows[loaded..]
                .iter()
                .map(|f| f.symbol_count as usize)
                .sum::<usize>(),
        );
        let (h, files) = (header.clone(), pack_input(&rows, loaded));
        let packed = tokio::task::spawn_blocking(move || {
            render_map_rest(
                &h,
                &files,
                rest,
                zam_store::budget_cap(budget),
                &count_tokens,
            )
        })
        .await
        .map_err(|e| ApiError::aborted("упаковка карты", &e))?;
        if packed.files_shown < loaded || loaded == cap {
            break packed;
        }
        want = cap.min(loaded.saturating_mul(4));
    };
    let mut out_edges = Vec::new();
    if let (true, true, Some(s)) = (edges, packed.files_shown > 1, status) {
        let inp = cached_inputs(st, sc, guid, &s.snapshot.etag).await?;
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

/// Карта решения в бюджете Markdown и предупреждения (`rank pending`, `focus rank timeout`) —
/// для MCP `memory_code action=map` (задача 7): ранги пересчитываются при необходимости
/// ([`ensure_ranked`]).
pub async fn map_text(
    st: &AppState,
    sc: &MemScope,
    guid: &str,
    budget: usize,
    focus: Option<&Focus>,
    area: Option<&str>,
) -> Result<(Packed, Vec<String>), ApiError> {
    let (s, mut warnings) = ranked_status(st, sc, guid).await?;
    let m = build_map(st, sc, guid, s.as_ref(), budget, focus, area, false, None).await?;
    warnings.extend(m.warnings);
    Ok((m.packed, warnings))
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

/// `GET /code/map?solution=&budget_tokens=&focus=&area=&files=&format=md|json`.
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
    let files_max = q.files;
    if files_max.is_some_and(|n| !(1..=MAP_FILES_PARAM_MAX).contains(&n)) {
        return Err(unprocessable(format!(
            "files: от 1 до {MAP_FILES_PARAM_MAX}"
        )));
    }
    let files_tag = files_max.map(|n| n.to_string()).unwrap_or_default();
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
            &files_tag,
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
        files_max,
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

// ---- GET /code/files ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesQuery {
    solution: Option<String>,
    lang: Option<String>,
    q: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
}

/// `GET /code/files?solution=&lang=&q=&offset=&limit=` → `{files: [{id, path, lang, rank,
/// symbols}], total, offset, limit, warnings}`: таблица файлов карты по рангу (WebUI, задача 8),
/// фильтр языка и подстроки пути (без учёта регистра), `limit` 1..=200.
pub async fn files(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<FilesQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    enabled(&st)?;
    let q = query(q)?;
    let raw = solution_param(q.solution.as_deref(), &headers)?;
    let sc = code_scope(&st, &p, &headers, raw, Mode::Read).await?;
    let lang = q
        .lang
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            zam_code::Lang::parse(l).ok_or_else(|| {
                let all: Vec<&str> = zam_code::Lang::ALL.iter().map(|l| l.as_str()).collect();
                unprocessable(format!("lang: один из {}", all.join("|")))
            })
        })
        .transpose()?;
    let needle =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| check_text(s, QUAL_MAX_CHARS, "q", false).map(|()| s.to_lowercase()))
            .transpose()?;
    let limit = q.limit.unwrap_or(FILES_PAGE_DEFAULT);
    if !(1..=FILES_PAGE_MAX).contains(&limit) {
        return Err(unprocessable(format!("limit: от 1 до {FILES_PAGE_MAX}")));
    }
    let offset = q.offset.unwrap_or(0);
    let g = guid(&sc);
    let warnings = ensure_ranked(&st, &sc, &g).await;
    let (rows, total) = code_files_page(&sc.db, &g, lang, needle.as_deref(), offset, limit).await?;
    let page: Vec<Value> = rows
        .iter()
        .map(|f| {
            json!({"id": f.id, "path": f.path, "lang": f.lang, "rank": f.rank,
                   "symbols": f.symbol_count})
        })
        .collect();
    Ok(Json(json!({
        "files": page,
        "total": total,
        "offset": offset,
        "limit": limit,
        "warnings": warnings,
    })))
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

/// Знания решения о пути `path` в базах `dbs` ([`report_dbs`]: база владельца и Knowledge
/// участника), без предела записей на путь: ссылки на символы первыми.
async fn knowledge_of(
    dbs: &[(Db, Vec<Id>)],
    guid: &str,
    path: &str,
) -> Result<Vec<RefItem>, ApiError> {
    let mut items = Vec::new();
    for (db, spaces) in dbs {
        items.extend(path_owners(db, guid, path, spaces).await?);
    }
    // символьные первыми и после слияния баз (сортировка устойчива)
    items.sort_by_key(|i| i.symbol.is_none());
    Ok(items)
}

/// Знания выдачи — не больше [`KNOWLEDGE_MAX`]; возвращает, сколько не вошло.
fn cap_knowledge(items: &mut Vec<Value>) -> usize {
    let left = items.len().saturating_sub(KNOWLEDGE_MAX);
    items.truncate(KNOWLEDGE_MAX);
    left
}

/// Знание выдачи: `{path, id, kind, title, state, space, symbol?}`; `space` — `Id`
/// пространства записи (как в `GET /spaces`; ссылка WebUI на знание из Knowledge участника).
fn knowledge_json(path: &str, it: &RefItem) -> Value {
    let mut v = json!({"path": path, "id": it.id, "kind": it.kind, "title": it.title,
                       "state": it.state, "space": it.space.map(|s| s.to_string())});
    if let Some(s) = &it.symbol {
        v["symbol"] = json!(s);
    }
    v
}

/// Схлопывание знаний (ревью задачи 8): у записи со ссылками и на файл, и на его символ
/// остаётся строка символа — файловая той же записи и пути снимается; повтор строки — один раз.
/// Порядок сохраняется.
fn collapse_knowledge(items: Vec<(String, RefItem)>) -> Vec<(String, RefItem)> {
    let with_symbol: HashSet<(String, String)> = items
        .iter()
        .filter(|(_, it)| it.symbol.is_some())
        .map(|(p, it)| (p.clone(), it.id.clone()))
        .collect();
    let mut seen: HashSet<(String, String, Option<String>)> = HashSet::new();
    items
        .into_iter()
        .filter(|(p, it)| it.symbol.is_some() || !with_symbol.contains(&(p.clone(), it.id.clone())))
        .filter(|(p, it)| seen.insert((p.clone(), it.id.clone(), it.symbol.clone())))
        .collect()
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
        .ok_or_else(|| {
            let r = zam_store::shown_ref(&format!("{path}#{qual}"));
            ApiError::CodeSymbolNotFound(format!("символа «{r}» нет в карте"))
        })
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
        .ok_or_else(|| ApiError::CodeSymbolNotFound(format!("символа {id} нет в карте")))?;
    let path = card.symbol.path.clone();
    let dbs = report_dbs(&st, &p, &sc).await?;
    let own: Vec<(String, RefItem)> = knowledge_of(&dbs, &g, &path)
        .await?
        .into_iter()
        .filter(|it| it.symbol.as_deref().is_none_or(|s| s == card.symbol.qual))
        .map(|it| (path.clone(), it))
        .collect();
    let mut knowledge: Vec<Value> = collapse_knowledge(own)
        .iter()
        .map(|(p, it)| knowledge_json(p, it))
        .collect();
    let knowledge_left = cap_knowledge(&mut knowledge);
    let mut sym = json!(card.symbol);
    sym["body_hash"] = json!(card.body_hash);
    sym["exported"] = json!(card.exported);
    sym["ref"] = json!(format!("{}#{}", card.symbol.path, card.symbol.qual));
    let mut body = json!({
        "symbol": sym,
        "callers": card.callers,
        "uses": card.uses,
        "knowledge": knowledge,
        "knowledge_left": knowledge_left,
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
        let symbols = super::files::symbol_hashes_of(o.get("symbols"), max_symbols, false, &what)?;
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
    let req: ImpactReq = code_body(request_max(&st), body)?;
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
    let dbs = report_dbs(&st, &p, &sc).await?;
    let mut found: Vec<(String, RefItem)> = Vec::new();
    for (path, _) in &files {
        let all = rows.paths_only.contains(path);
        let quals = affected.get(path.as_str());
        if !all && quals.is_none() {
            continue;
        }
        for it in knowledge_of(&dbs, &g, path).await? {
            let take = all
                || it
                    .symbol
                    .as_deref()
                    .is_none_or(|s| quals.is_some_and(|q| q.contains(s)));
            if take {
                found.push((path.clone(), it));
            }
        }
    }
    let mut knowledge: Vec<Value> = collapse_knowledge(found)
        .iter()
        .map(|(p, it)| knowledge_json(p, it))
        .collect();
    let mut body = json!({
        "changed": rows.changed,
        "removed": rows.removed,
        "consumers": rows.consumers,
        "paths_only": rows.paths_only,
        "knowledge_left": cap_knowledge(&mut knowledge),
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

    /// Minor 7 ревью задачи 4: возраст «N мин / N ч / N дн.», без машины, центральные
    /// сокращаются 3 → 2 → 1 → 0, пока строка длиннее предела.
    #[test]
    fn digest_line_age_snapshot_and_central_cut() {
        assert_eq!(digest_age(59), "0 мин");
        assert_eq!(digest_age(3599), "59 мин");
        assert_eq!(digest_age(3600), "1 ч");
        assert_eq!(digest_age(47 * 3600 + 59), "47 ч");
        assert_eq!(digest_age(48 * 3600), "2 дн.");
        let snap = |branch: Option<&str>, commit: Option<&str>| CodeSnapshot {
            solution: "g".into(),
            commit: commit.map(str::to_string),
            branch: branch.map(str::to_string),
            machine: Some("box".into()),
            updated_at: zam_store::time::now(),
            files: 2,
            symbols: 5,
            etag: String::new(),
            rank_state: RankState::Ok,
            ranked_at: None,
        };
        let status = |s: CodeSnapshot| CodeStatus {
            snapshot: s,
            by_lang: Vec::new(),
            skipped: Vec::new(),
            skipped_total: 0,
            other_share: 0.0,
        };
        let c = |q: &str| json!({"qual": q});
        let central = [c("Store::open"), c("Db"), c("route")];
        let chars = |t: &str| t.chars().count();
        let s = status(snap(Some("main"), Some("abcdef1234")));
        assert_eq!(
            digest_line(&s, &central, &|_| true),
            "ZeroAIMemory: карта кода: 2 файла, 5 символов, снимок 0 мин (main@abcdef1); \
             центральные: Store::open, Db, route — `memory_code`/`zam code map`"
        );
        // без ветки и коммита — без скобок; машина не показывается
        let s0 = status(snap(None, None));
        let l = digest_line(&s0, &central[..1], &|_| true);
        assert!(l.contains("снимок 0 мин; центральные: Store::open"), "{l}");
        assert!(!l.contains("box"), "{l}");
        // предел по счётчику: длина строки с 3, 2, 1 центральным
        let full = chars(&digest_line(&s, &central, &|_| true));
        let cut = |limit: usize| {
            digest_line(&s, &central, &|t: &str| {
                (chars(t) * DIGEST_LINE_TOKENS).div_ceil(limit) <= DIGEST_LINE_TOKENS
            })
        };
        assert!(cut(full).contains("route"));
        let two = cut(full - 1);
        assert!(two.contains("Store::open, Db —"), "{two}");
        let none = cut(10);
        assert!(!none.contains("центральные"), "{none}");
        assert!(!none.contains("(main"), "без скобки снимка: {none}");
        assert!(none.ends_with(" — `memory_code`/`zam code map`"), "{none}");
        // ревью задачи 6, M1: ветка в 256 символов — обрезка до 40 с «…», строка ≤ 80 токенов
        let long = "b".repeat(256);
        let sl = status(snap(Some(&long), Some("abcdef1234")));
        let l = digest_line(&sl, &central, &digest_fits);
        assert!(l.contains(&format!("({}…@abcdef1)", "b".repeat(40))), "{l}");
        assert!(digest_fits(&l), "{l}");
    }
}
