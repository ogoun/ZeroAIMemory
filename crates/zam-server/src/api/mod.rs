//! Маршруты HTTP API.
pub mod admin;
pub mod health;
pub mod spaces;

use axum::Router;
use axum::routing::{delete, get, patch, post};

use crate::app::AppState;
use crate::auth::admin as login;

/// Маршруты под `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/health", get(health::health))
        .route("/api/v1/ready", get(health::ready))
        .route("/api/v1/auth/login", post(login::login))
        .route("/api/v1/auth/logout", post(login::logout))
        .route("/api/v1/auth/me", get(login::me))
        .route("/api/v1/spaces", get(spaces::list))
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
