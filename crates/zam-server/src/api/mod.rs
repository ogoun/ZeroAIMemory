//! Маршруты HTTP API.
pub mod health;
pub mod spaces;

use axum::Router;
use axum::routing::{get, post};

use crate::app::AppState;
use crate::auth::admin;

/// Маршруты под `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/health", get(health::health))
        .route("/api/v1/ready", get(health::ready))
        .route("/api/v1/auth/login", post(admin::login))
        .route("/api/v1/auth/logout", post(admin::logout))
        .route("/api/v1/auth/me", get(admin::me))
        .route("/api/v1/spaces", get(spaces::list))
}
