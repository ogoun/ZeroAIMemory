//! Оценка полезности записи (§10.5; задача 5 M4).
//!
//! `POST /entries/{id}/feedback {value: "useful"|"useless", session_id?}` → 204. Нужно право
//! чтения цели (как у `GET /entries/{id}`: посторонний — 404, участник без одобрения — 403
//! `solution-access`, удалённая запись — 404); участник с доступом `ro` оценивать может.
//! Событие дописывается в `usage` базы, где лежит запись (у общего решения — база владельца),
//! с пользователем запроса, сессией (`session_id` из тела или `X-Zam-Session`) и источником:
//! токен — `agent`, WebUI — `webui`. Счётчики отдаёт `GET /entries/{id}` полем `usage`.
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use zam_core::{TargetKind, TargetRef};
use zam_store::{UsageEvent, UsageKind, UsageSource, record_usage};

use super::json_body;
use super::scope::{Mode, entry_scope, live_entry};
use crate::app::AppState;
use crate::attrib::session_touch;
use crate::auth::Principal;
use crate::error::ApiError;

/// Оценка.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Value {
    Useful,
    Useless,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackReq {
    value: Value,
    session_id: Option<String>,
}

/// `POST /entries/{id}/feedback`.
pub async fn entry(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<StatusCode, ApiError> {
    let req: FeedbackReq = json_body(body)?;
    let session = session_touch(&p, &headers, req.session_id.as_deref())?.map(|s| s.id);
    let sc = entry_scope(&st, &p, &headers, &id, Mode::Read).await?;
    live_entry(&sc)?;
    let event = UsageEvent {
        user_id: sc.user.id,
        session_id: session,
        target: TargetRef {
            kind: TargetKind::Entry,
            id: sc.entry.id,
        },
        event: match req.value {
            Value::Useful => UsageKind::Useful,
            Value::Useless => UsageKind::Useless,
        },
        source: Some(match p {
            Principal::Token(_) => UsageSource::Agent,
            Principal::Admin { .. } => UsageSource::Webui,
        }),
    };
    record_usage(&sc.db, &event).await?;
    Ok(StatusCode::NO_CONTENT)
}
