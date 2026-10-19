//! Сервер ZeroAIMemory.
pub mod api;
pub mod app;
pub mod attrib;
pub mod auth;
pub mod bootstrap;
pub mod config;
pub mod error;
pub mod index_state;
pub mod logging;
pub mod ml;
pub mod qdrant;
pub mod web;

#[cfg(test)]
mod test_util;

use std::net::SocketAddr;

pub use app::{AppState, BootReport, build};
use config::ServerConfig;

/// Печатает первый токен в stderr (мимо tracing), ровно в рамке.
fn print_initial_token(token: &str) {
    eprintln!("==== ZeroAIMemory: первый токен администратора (показывается один раз) ====");
    eprintln!("ZeroAIMemoryToken={token}");
    eprintln!("==========================================================================");
}

/// Запускает сервер до Ctrl+C / SIGTERM.
pub async fn run(cfg: ServerConfig, env_password: Option<String>) -> anyhow::Result<()> {
    let bind = cfg.server.bind;
    let log_cfg = cfg.log.clone();
    // Порядок: пароль, лок каталога, логи, базы; bind до выпуска токена, печать после.
    let prepared = app::prepare(cfg, env_password.as_deref())?;
    let _guard = logging::init(&log_cfg, &prepared.data_dir.logs())?;
    // bind до выпуска токена: при занятом порту в БД ничего не появляется.
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let (app, state, report) = app::finish(prepared).await?;
    if let Some(t) = &report.initial_token {
        print_initial_token(t);
    }
    tracing::info!(addr = %listener.local_addr()?, version = state.version, "zam-server запущен");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    state.shutdown().await;
    tracing::info!("zam-server остановлен");
    Ok(())
}

/// Открывает все базы (с миграциями) и возвращает число пользователей.
pub async fn migrate(cfg: ServerConfig) -> anyhow::Result<usize> {
    let dir = zam_store::DataDir::open(&cfg.data_dir)?;
    let system = zam_store::SystemStore::open(&dir)?;
    let users = zam_store::UserDbs::new(dir.users_dir());
    let list = system.list_users().await?;
    for u in &list {
        users.get(u.id).await?;
    }
    Ok(list.len())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
}
