//! Маршруты HTTP API.
pub mod health;

use axum::Router;
use axum::routing::get;

use crate::app::AppState;

/// Маршруты под `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/health", get(health::health))
        .route("/api/v1/ready", get(health::ready))
}
