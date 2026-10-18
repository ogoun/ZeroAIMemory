//! Сервер ZeroAIMemory.
pub mod api;
pub mod app;
pub mod bootstrap;
pub mod config;
pub mod error;
pub mod logging;

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
    std::fs::create_dir_all(cfg.data_dir.join("logs"))?;
    let _guard = logging::init(&cfg.log, &cfg.data_dir.join("logs"))?;
    let bind = cfg.server.bind;
    let (app, state, report) = build(cfg, env_password).await?;
    if let Some(t) = &report.initial_token {
        print_initial_token(t);
    }
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(addr = %listener.local_addr()?, version = state.version, "zam-server запущен");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
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
