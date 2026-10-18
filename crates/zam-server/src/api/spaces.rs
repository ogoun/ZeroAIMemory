//! `GET /spaces` — пространства пользователя запроса (§6.3).
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use serde_json::{Value, json};
use zam_store::list_spaces;

use crate::app::AppState;
use crate::auth::{Principal, target_user};
use crate::error::ApiError;

pub async fn list(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let user = target_user(&st, &p, &headers).await?;
    let spaces = list_spaces(&st.users.get(user.id).await?).await?;
    let access = match &p {
        Principal::Token(t) => t.access.as_str(),
        Principal::Admin { .. } => "admin",
    };
    Ok(Json(json!({
        "user": { "id": user.id, "name": user.name },
        "access": access,
        "spaces": spaces,
    })))
}
