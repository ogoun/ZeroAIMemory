//! Маршруты HTTP API.
pub mod admin;
pub mod health;
pub mod nodes;
pub mod render;
pub mod scope;
pub mod solutions;
pub mod spaces;

use axum::Router;
use axum::body::Bytes;
use axum::extract::DefaultBodyLimit;
use axum::extract::Query;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch, post};
use serde::de::DeserializeOwned;
use zam_core::Id;

use crate::app::AppState;
use crate::auth::admin as login;
use crate::config::LimitsSection;
use crate::error::ApiError;

/// Запас на JSON-обёртку тела записи заметки, байт.
const JSON_OVERHEAD: usize = 64 * 1024;

/// Предел тела запроса записи заметки. Текст заметки проверяется отдельно
/// (`note_max_bytes`); экранирование JSON (`\n`, `\r`, `\"`) может удвоить
/// размер, поэтому тело — до двух пределов заметки плюс запас.
fn note_body_limit(l: &LimitsSection) -> DefaultBodyLimit {
    DefaultBodyLimit::max(
        l.note_max_bytes
            .saturating_mul(2)
            .saturating_add(JSON_OVERHEAD),
    )
}

/// Маршруты под `/api/v1`.
pub fn router(limits: &LimitsSection) -> Router<AppState> {
    let note_limit = note_body_limit(limits);
    Router::new()
        .route("/api/v1/health", get(health::health))
        .route("/api/v1/ready", get(health::ready))
        .route("/api/v1/auth/login", post(login::login))
        .route("/api/v1/auth/logout", post(login::logout))
        .route("/api/v1/auth/me", get(login::me))
        .route("/api/v1/spaces", get(spaces::list))
        .route("/api/v1/spaces/{space}/tree", get(spaces::tree))
        .route("/api/v1/solutions", post(solutions::register))
        .route("/api/v1/solutions/{guid}", get(solutions::get))
        .route(
            "/api/v1/nodes",
            post(nodes::write::create).layer(note_limit),
        )
        .route("/api/v1/nodes/by-path", get(nodes::read::by_path))
        .route(
            "/api/v1/nodes/{id}",
            get(nodes::read::by_id)
                .put(nodes::write::replace)
                .patch(nodes::write::patch)
                .delete(nodes::write::remove)
                .layer(note_limit),
        )
        .route("/api/v1/nodes/{id}/outline", get(nodes::read::outline))
        .route("/api/v1/nodes/{id}/move", post(nodes::write::move_to))
        .route("/api/v1/nodes/{id}/restore", post(nodes::write::restore))
        .route(
            "/api/v1/nodes/{id}/revisions",
            get(nodes::history::revisions),
        )
        .route(
            "/api/v1/nodes/{id}/revisions/{rev}",
            get(nodes::history::revision),
        )
        .route("/api/v1/nodes/{id}/diff", get(nodes::history::diff))
        .route("/api/v1/nodes/{id}/revert", post(nodes::history::revert))
        .route("/api/v1/trash", get(nodes::write::trash))
        .route(
            "/api/v1/render",
            post(render::preview).layer(DefaultBodyLimit::max(render::RENDER_BODY_MAX)),
        )
        .route("/api/v1/render/highlight.css", get(render::highlight))
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
        .route("/api/v1/admin/tokens/{id}", delete(admin::revoke_token))
        .route("/api/v1/admin/audit", get(admin::list_audit))
}

// ---- разбор входных данных (общий для обработчиков) ----

pub(crate) fn parse_id(s: &str) -> Result<Id, ApiError> {
    s.parse()
        .map_err(|_| ApiError::BadRequest("некорректный идентификатор".into()))
}

pub(crate) fn parse_body<T: DeserializeOwned>(b: &Bytes) -> Result<T, ApiError> {
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
