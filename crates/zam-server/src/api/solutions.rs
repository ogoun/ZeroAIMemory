//! Решения (§7.1, §12): `POST /solutions {id, name}` и `GET /solutions/{guid}`.
//!
//! GUID — UUID любой версии в форме с дефисами; хранится в нижнем регистре.
//! Регистрация идемпотентна: 201 при создании, 200, если решение уже есть
//! (имя существующего не меняется). Регистрирует токен `rw` или администратор.
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use zam_store::{Space, create_solution, get_solution};

use super::json_body;
use super::scope::{canonical_guid, require_rw, user_db};
use crate::app::AppState;
use crate::auth::Principal;
use crate::error::ApiError;

/// Предел длины имени решения, символов.
const NAME_MAX_CHARS: usize = 128;

#[derive(Deserialize)]
pub struct Register {
    id: String,
    name: String,
}

fn guid(raw: &str) -> Result<String, ApiError> {
    canonical_guid(raw.trim())
        .ok_or_else(|| ApiError::BadRequest("id решения — UUID с дефисами".into()))
}

pub async fn register(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<Space>), ApiError> {
    require_rw(&p)?;
    let req: Register = json_body(body)?;
    let guid = guid(&req.id)?;
    let name = req.name.trim();
    if name.is_empty()
        || name.chars().count() > NAME_MAX_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(ApiError::BadRequest(format!(
            "имя решения: непустое, до {NAME_MAX_CHARS} символов, без управляющих"
        )));
    }
    let (_, db) = user_db(&st, &p, &headers).await?;
    let (space, created) = create_solution(&db, &guid, name).await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(space)))
}

pub async fn get(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(raw): Path<String>,
) -> Result<Json<Space>, ApiError> {
    let guid = canonical_guid(raw.trim()).ok_or(ApiError::NotFound)?;
    let (_, db) = user_db(&st, &p, &headers).await?;
    get_solution(&db, &guid)
        .await?
        .map(Json)
        .ok_or(ApiError::NotFound)
}
