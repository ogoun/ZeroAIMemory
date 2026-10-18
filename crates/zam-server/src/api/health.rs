//! `/health` (жив ли процесс) и `/ready` (доступна ли `system.db`); без аутентификации.
use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

use crate::app::{AppState, GIT_SHA};
use crate::error::ApiError;

pub async fn health(State(st): State<AppState>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": st.version,
        "git": GIT_SHA,
        "uptime_s": st.started_at.elapsed().as_secs(),
    }))
}

pub async fn ready(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    st.system.ping().await.map_err(|e| {
        tracing::warn!(error = %e, "system.db недоступна");
        ApiError::Unavailable("system.db недоступна".into())
    })?;
    Ok(Json(json!({ "status": "ready" })))
}
