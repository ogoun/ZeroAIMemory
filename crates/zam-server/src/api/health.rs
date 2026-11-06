//! `/health` (жив ли процесс) и `/ready` (доступна ли `system.db`); без аутентификации.
use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

use crate::app::{AppState, GIT_SHA};
use crate::error::ApiError;

/// Возможности сборки в `/health` (`features`) для загрузчика замера M6 (`xtask eval`, модуль
/// `caps`): у выпусков до M6 поля нет, и замер узнаёт их по версии и коммиту тега. `"m6"` —
/// сборка ветки M6; задача M6, которая вводит ключ конфигурации или возможность, добавляет сюда
/// её строку (`budget`, `snippet`, `focus`, `digest`, `hint_thresholds`, `kind_boost`,
/// `attach_weight`, `snapshot` — таблица `KEYS` в `xtask/src/eval/caps.rs`).
pub const FEATURES: &[&str] = &["m6", "budget", "snippet"];

pub async fn health(State(st): State<AppState>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": st.version,
        "git": GIT_SHA,
        "features": FEATURES,
        "token_counter": zam_store::budget_counter_kind().as_str(),
        "tokenizer_id": zam_store::budget_tokenizer_id(),
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
