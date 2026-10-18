//! Состояние приложения и сборка роутера.
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use tower_http::compression::CompressionLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;
use zam_store::{DataDir, SystemStore, UserDbs};

use crate::config::{AdminSecret, ServerConfig};
use crate::error::not_found;
use crate::{api, bootstrap};

/// Git-хэш сборки (см. build.rs).
pub const GIT_SHA: &str = env!("ZAM_GIT_SHA");

/// Общее состояние; дёшево клонируется.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<ServerConfig>,
    /// Владелец лока каталога данных: живёт, пока жив любой клон состояния.
    pub data_dir: Arc<DataDir>,
    pub admin_hash: Arc<String>,
    pub system: SystemStore,
    pub users: UserDbs,
    pub started_at: Instant,
    pub version: &'static str,
}

// Вручную: хэш пароля и пути не должны попадать в отладочный вывод.
impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

/// Результат первого запуска.
#[derive(Default)]
pub struct BootReport {
    /// Открытый текст первого токена администратора; `Some` только при создании.
    pub initial_token: Option<String>,
}

impl std::fmt::Debug for BootReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootReport")
            .field("initial_token", &self.initial_token.as_ref().map(|_| "***"))
            .finish()
    }
}

/// Собирает состояние и роутер. Глобальный subscriber не трогает.
pub async fn build(
    cfg: ServerConfig,
    env_password: Option<String>,
) -> anyhow::Result<(Router, AppState, BootReport)> {
    let AdminSecret::Hash(admin_hash) = cfg.admin_secret(env_password.as_deref())?;
    let data_dir = Arc::new(DataDir::open(&cfg.data_dir)?);
    let system = SystemStore::open(&data_dir)?;
    let initial_token = bootstrap::ensure_admin_user(&system, &cfg.admin.login).await?;
    let state = AppState {
        users: UserDbs::new(data_dir.users_dir()),
        data_dir,
        system,
        admin_hash: Arc::new(admin_hash),
        started_at: Instant::now(),
        version: env!("CARGO_PKG_VERSION"),
        cfg: Arc::new(cfg),
    };
    let router = router(state.clone());
    Ok((router, state, BootReport { initial_token }))
}

fn router(state: AppState) -> Router {
    let base = state.cfg.server.base_path.clone();
    let api = api::router().fallback(not_found).with_state(state);
    let app = if base.is_empty() {
        api
    } else {
        Router::new().nest(&base, api).fallback(not_found)
    };
    // Только метод, путь (без query: в ней может быть токен), статус и задержка.
    app.layer(
        TraceLayer::new_for_http()
            .make_span_with(|req: &axum::http::Request<_>| {
                tracing::info_span!("http", method = %req.method(), path = %req.uri().path())
            })
            .on_request(())
            .on_response(DefaultOnResponse::new().level(Level::INFO)),
    )
    .layer(CompressionLayer::new())
}
