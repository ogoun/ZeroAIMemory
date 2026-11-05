//! Маршруты HTTP API.
pub mod admin;
pub mod agent_setup;
pub mod big_body;
pub mod blobs;
pub mod cache;
pub mod code;
pub mod doc_locator;
pub mod downloads;
pub mod entries;
pub mod feedback;
pub mod files;
pub mod glossary;
pub mod graph;
pub mod health;
pub mod ideas;
pub mod index;
pub mod insights;
pub mod nodes;
pub mod previews;
pub mod questions;
pub mod read;
pub mod recall;
pub mod releases;
pub mod render;
pub mod reports;
pub mod scope;
pub mod search;
pub mod secrets;
pub mod sessions;
pub mod solutions;
pub mod spaces;
pub mod tasks;
pub mod triggers;
pub mod user_settings;
pub mod vault;

use axum::Router;
use axum::body::Bytes;
use axum::extract::DefaultBodyLimit;
use axum::extract::Query;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::routing::{delete, get, patch, post};
use serde::de::DeserializeOwned;
use zam_core::Id;

use crate::app::AppState;
use crate::auth::admin as login;
use crate::auth::user as user_auth;
use crate::config::{LimitsSection, ServerConfig};
use crate::error::ApiError;

/// Запас на JSON-обёртку тела записи заметки, байт.
const JSON_OVERHEAD: usize = 64 * 1024;

/// Предел тела запроса записи заметки. Текст заметки проверяется отдельно
/// (`note_max_bytes`); экранирование JSON (`\n`, `\r`, `\"`) может удвоить
/// размер, поэтому тело — до двух пределов заметки плюс запас.
fn note_body_limit(l: &LimitsSection) -> DefaultBodyLimit {
    DefaultBodyLimit::max(note_body_max(l))
}

/// Предел тела с текстом заметки, байт: два предела заметки плюс запас (тот же у `/mcp`).
pub(crate) fn note_body_max(l: &LimitsSection) -> usize {
    l.note_max_bytes
        .saturating_mul(2)
        .saturating_add(JSON_OVERHEAD)
}

/// Запас на заголовки частей и поле `node` в multipart загрузки вложения, байт.
const MULTIPART_OVERHEAD: usize = 1024 * 1024;

/// Предел тела `POST|GET /spaces/{space}/entries` для слоя маршрута: multipart отчёта (часть
/// `meta` — до [`note_body_max`] плюс [`reports::META_OVERHEAD`], файлы — потоком по
/// `reports.max_total_bytes` и `limits.blob_max_bytes`, 413) и запас на заголовки частей. JSON
/// обработчик читает сам с пределом [`entries_json_max`].
fn entries_body_limit(c: &ServerConfig) -> DefaultBodyLimit {
    let files = usize::try_from(c.reports.max_total_bytes).unwrap_or(usize::MAX);
    DefaultBodyLimit::max(
        note_body_max(&c.limits)
            .saturating_add(reports::META_OVERHEAD)
            .saturating_add(files)
            .saturating_add(MULTIPART_OVERHEAD),
    )
}

/// Предел JSON-тела `POST /spaces/{space}/entries`, байт: запись журнала — до [`note_body_max`]
/// (больше — 413, как раньше), отчёт MCP — ещё файлы в base64 (`⌈mcp_max_bytes · 4/3⌉`) и запас.
pub(crate) fn entries_json_max(c: &ServerConfig) -> usize {
    let b64 = c.reports.mcp_max_bytes.div_ceil(3).saturating_mul(4);
    note_body_max(&c.limits)
        .saturating_add(usize::try_from(b64).unwrap_or(usize::MAX))
        .saturating_add(JSON_OVERHEAD)
}

/// Предел тела `POST /blobs`: само вложение проверяется потоком по
/// `blob_max_bytes` (413), слой отсекает лишь заведомо большие тела.
fn blob_body_limit(l: &LimitsSection) -> DefaultBodyLimit {
    DefaultBodyLimit::max(l.blob_max_bytes.saturating_add(MULTIPART_OVERHEAD))
}

/// Маршруты под `/api/v1` (без `/mcp`: его инструменты зовут этот роутер изнутри).
pub fn router(state: &AppState) -> Router<AppState> {
    memory_routes(&state.cfg)
        // записи в память с `X-Zam-Session` — счётчик сессии (одна точка, R1)
        .route_layer(from_fn_with_state(
            state.clone(),
            sessions::count_memory_writes,
        ))
        .merge(service_routes(&state.cfg))
}

/// Маршруты записи в память (и чтения тех же ресурсов): узлы, вложения, записи журналов,
/// связи, Vault, кэш выжимок (`POST /cache`, `DELETE /cache/{id}`, `promote`; чтение кэша —
/// в [`service_routes`]). Изменяющие запросы с `X-Zam-Session` считает
/// [`sessions::count_memory_writes`]; оценка записи (`/feedback`) — не запись в память.
/// Любой новый путь записи в память — сюда (решение контроллера R12); MCP ([`crate::mcp`])
/// вызывает эти маршруты через внутренний `Router`.
fn memory_routes(cfg: &ServerConfig) -> Router<AppState> {
    let limits = &cfg.limits;
    let note_limit = note_body_limit(limits);
    Router::new()
        .route(
            "/api/v1/nodes",
            post(nodes::write::create).layer(note_limit),
        )
        .route(
            "/api/v1/nodes/{id}",
            get(nodes::read::by_id)
                .put(nodes::write::replace)
                .patch(nodes::write::patch)
                .delete(nodes::write::remove)
                .layer(note_limit),
        )
        .route("/api/v1/nodes/{id}/move", post(nodes::write::move_to))
        .route("/api/v1/nodes/{id}/restore", post(nodes::write::restore))
        .route("/api/v1/nodes/{id}/revert", post(nodes::history::revert))
        .route(
            "/api/v1/spaces/{space}/entries",
            get(entries::list)
                .post(entries::create)
                .layer(entries_body_limit(cfg)),
        )
        .route(
            "/api/v1/entries/{id}",
            get(entries::get)
                .patch(entries::patch)
                .delete(entries::remove)
                .layer(note_limit),
        )
        .route("/api/v1/spaces/{space}/glossary", get(glossary::get))
        .route("/api/v1/entries/{id}/restore", post(entries::restore))
        .route("/api/v1/entries/{id}/answer", post(questions::answer))
        .route("/api/v1/entries/{id}/claim", post(tasks::claim))
        .route("/api/v1/entries/{id}/propose", post(ideas::propose))
        .route("/api/v1/entries/{id}/decide", post(ideas::decide))
        .route("/api/v1/entries/{id}/adopt", post(ideas::adopt))
        .route("/api/v1/entries/{id}/release", post(tasks::release))
        .route(
            "/api/v1/entries/{id}/supersede",
            post(insights::supersede).layer(note_limit),
        )
        .route("/api/v1/entries/{id}/retract", post(insights::retract))
        .route(
            "/api/v1/spaces/{space}/vault",
            get(vault::list).post(vault::create),
        )
        .route(
            "/api/v1/vault/{id}",
            get(vault::get).patch(vault::patch).delete(vault::remove),
        )
        .route("/api/v1/edges", post(graph::create))
        .route("/api/v1/edges/{id}", delete(graph::remove))
        .route(
            "/api/v1/blobs",
            post(blobs::upload).layer(blob_body_limit(limits)),
        )
        .route(
            "/api/v1/nodes/{id}/attachments/{sha256}/{filename}",
            delete(blobs::remove),
        )
        .route("/api/v1/cache", post(cache::put))
        .route("/api/v1/cache/{id}", delete(cache::remove))
        .route("/api/v1/cache/{id}/promote", post(cache::promote))
}

/// Предел тела `PUT /code/files`, `POST /code/impact` — `code.max_request_bytes`; у
/// `POST /code/sync` свой — манифест на `code.max_files` путей ([`code::sync_body_max`]). Больше —
/// 413 с пределом в `detail`.
fn code_body_limit(c: &ServerConfig) -> DefaultBodyLimit {
    DefaultBodyLimit::max(usize::try_from(c.code.max_request_bytes).unwrap_or(usize::MAX))
}

/// Остальные маршруты: служебные, чтение, поиск, сессии, администрирование, карта кода.
fn service_routes(cfg: &ServerConfig) -> Router<AppState> {
    let code_limit = code_body_limit(cfg);
    Router::new()
        // карта кода (M5.6): производные данные, не запись памяти — `memory_writes` не растёт
        .route("/api/v1/code", delete(code::clear))
        .route(
            "/api/v1/code/sync",
            post(code::sync).layer(DefaultBodyLimit::max(code::sync_body_max(&cfg.code))),
        )
        .route(
            "/api/v1/code/files",
            get(code::files).put(code::put_files).layer(code_limit),
        )
        .route("/api/v1/code/status", get(code::status))
        .route("/api/v1/code/map", get(code::map))
        .route("/api/v1/code/symbols", get(code::symbols))
        .route("/api/v1/code/symbols/{id}", get(code::symbol))
        .route("/api/v1/code/impact", post(code::impact).layer(code_limit))
        .route("/api/v1/health", get(health::health))
        .route("/api/v1/ready", get(health::ready))
        .route("/api/v1/auth/login", post(login::login))
        .route("/api/v1/auth/logout", post(login::logout))
        .route("/api/v1/auth/me", get(login::me))
        .route("/api/v1/auth/password", post(user_auth::change_password))
        .route(
            "/api/v1/auth/tokens",
            get(user_auth::list_tokens).post(user_auth::issue_token),
        )
        .route("/api/v1/auth/tokens/{id}", delete(user_auth::revoke_token))
        .route(
            "/api/v1/auth/settings",
            get(user_settings::get_own).patch(user_settings::patch_own),
        )
        .route("/api/v1/spaces", get(spaces::list))
        .route("/api/v1/spaces/{space}/tree", get(spaces::tree))
        .route(
            "/api/v1/solutions",
            get(solutions::list).post(solutions::register),
        )
        .route(
            "/api/v1/solutions/{guid}",
            get(solutions::get).patch(solutions::rename),
        )
        .route(
            "/api/v1/solutions/{guid}/access",
            get(solutions::access_list),
        )
        .route(
            "/api/v1/solutions/{guid}/access/{user}/approve",
            post(solutions::approve),
        )
        .route(
            "/api/v1/solutions/{guid}/access/{user}/reject",
            post(solutions::reject),
        )
        .route(
            "/api/v1/solutions/{guid}/access/{user}/revoke",
            post(solutions::revoke),
        )
        .route("/api/v1/nodes/by-path", get(nodes::read::by_path))
        .route("/api/v1/nodes/{id}/outline", get(nodes::read::outline))
        .route(
            "/api/v1/nodes/{id}/revisions",
            get(nodes::history::revisions),
        )
        .route(
            "/api/v1/nodes/{id}/revisions/{rev}",
            get(nodes::history::revision),
        )
        .route("/api/v1/nodes/{id}/diff", get(nodes::history::diff))
        .route("/api/v1/nodes/{id}/backlinks", get(graph::node_backlinks))
        .route("/api/v1/nodes/{id}/links", get(graph::node_links))
        .route("/api/v1/trash", get(nodes::write::trash))
        .route("/api/v1/entries/{id}/feedback", post(feedback::entry))
        .route("/api/v1/entries/{id}/revisions", get(entries::revisions))
        .route(
            "/api/v1/entries/{id}/backlinks",
            get(graph::entry_backlinks),
        )
        .route("/api/v1/entries/{id}/links", get(graph::entry_links))
        .route("/api/v1/vault/{id}/revisions", get(vault::revisions))
        .route("/api/v1/graph", get(graph::graph))
        .route("/api/v1/releases/{id}/compare", get(releases::compare))
        .route(
            "/api/v1/render",
            post(render::preview).layer(DefaultBodyLimit::max(render::RENDER_BODY_MAX)),
        )
        .route("/api/v1/render/highlight.css", get(render::highlight))
        .route("/api/v1/blobs/{sha256}/{filename}", get(blobs::download))
        // файлы отчёта (M5.5): только пара этой записи, только сессиям WebUI
        .route(
            "/api/v1/entries/{id}/attachments/{sha256}/{filename}",
            get(reports::view::attachment),
        )
        .route("/api/v1/previews/{sha256}", get(previews::preview))
        .route("/api/v1/nodes/{id}/attachments", get(blobs::list))
        .route("/api/v1/search", post(search::search))
        .route("/api/v1/read", post(read::read))
        // подсказки пишут только `usage` и `session_seen`, не память (R12)
        .route("/api/v1/recall", post(recall::recall))
        .route("/api/v1/cache", get(cache::list))
        .route("/api/v1/cache/{id}", get(cache::get))
        .route("/api/v1/triggers", get(triggers::list))
        // ссылки на файлы (M4.5 §6): состояние ссылок, не текст памяти — вне `memory_routes`
        .route("/api/v1/file-refs", get(files::index))
        // тела с хэшами символов (M5.6 §8) — до `code.max_request_bytes`, как загрузка карты
        .route(
            "/api/v1/files/changed",
            post(files::changed).layer(code_body_limit(cfg)),
        )
        .route(
            "/api/v1/files/verified",
            post(files::verified).layer(code_body_limit(cfg)),
        )
        .route(
            "/api/v1/sessions",
            get(sessions::list).post(sessions::create),
        )
        .route("/api/v1/sessions/unattributed", get(sessions::unattributed))
        .route(
            "/api/v1/sessions/{id}",
            get(sessions::get).patch(sessions::patch),
        )
        .route("/api/v1/agent-setup", get(agent_setup::setup))
        .route("/api/v1/agent-guide", get(agent_setup::guide))
        .route(
            "/api/v1/admin/agent-setup/preview",
            post(agent_setup::preview),
        )
        // клиент `zam` для установки — без аутентификации (§15.5)
        .route("/downloads/zam/{platform}", get(downloads::zam))
        .route("/api/v1/index/status", get(index::status))
        .route("/api/v1/admin/reindex", post(index::reindex))
        .route(
            "/api/v1/admin/users",
            get(admin::list_users).post(admin::create_user),
        )
        .route(
            "/api/v1/admin/users/{id}",
            patch(admin::patch_user).delete(admin::delete_user),
        )
        .route(
            "/api/v1/admin/users/{id}/tokens",
            get(admin::list_tokens).post(admin::issue_token),
        )
        .route(
            "/api/v1/admin/users/{id}/password",
            post(admin::set_password).delete(admin::clear_password),
        )
        .route(
            "/api/v1/admin/users/{id}/settings",
            get(user_settings::get_user).patch(user_settings::patch_user),
        )
        .route("/api/v1/admin/tokens/{id}", delete(admin::revoke_token))
        .route("/api/v1/admin/audit", get(admin::list_audit))
        .route(
            "/api/v1/admin/settings",
            get(admin::get_settings).patch(admin::patch_settings),
        )
}

// ---- разбор входных данных (общий для обработчиков) ----

pub(crate) fn parse_id(s: &str) -> Result<Id, ApiError> {
    s.parse()
        .map_err(|_| ApiError::BadRequest("некорректный идентификатор".into()))
}

/// Тело JSON (тип может заимствовать из `b`, например `&RawValue`); ошибка — 400.
pub(crate) fn parse_body<'a, T: serde::Deserialize<'a>>(b: &'a Bytes) -> Result<T, ApiError> {
    serde_json::from_slice(b).map_err(|e| ApiError::BadRequest(format!("некорректное тело: {e}")))
}

/// Тело запроса с пределом размера: превышение → 413 problem+json
/// (вместо текстового отказа axum).
pub(crate) fn json_body<T: DeserializeOwned>(
    b: Result<Bytes, BytesRejection>,
) -> Result<T, ApiError> {
    match b {
        Ok(b) => parse_body(&b),
        Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => Err(ApiError::PayloadTooLarge(
            "тело запроса превышает предел".into(),
        )),
        Err(e) => Err(ApiError::BadRequest(format!("некорректное тело: {e}"))),
    }
}

/// Параметры запроса; ошибка разбора → 400 problem+json.
pub(crate) fn query<T>(q: Result<Query<T>, QueryRejection>) -> Result<T, ApiError> {
    q.map(|Query(v)| v)
        .map_err(|e| ApiError::BadRequest(format!("некорректный запрос: {e}")))
}
