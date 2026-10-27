//! Сервер ZeroAIMemory.
pub mod api;
pub mod app;
pub mod attrib;
pub mod auth;
pub mod bootstrap;
pub mod certainty;
pub mod config;
pub mod cuda;
pub mod error;
pub mod index_state;
pub mod logging;
pub mod maintenance;
pub mod ml;
pub mod models;
pub mod qdrant;
pub mod reindex;
pub mod rerank;
pub mod vault;
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
    // действующие веса поиска и пороги чанкинга (секретов нет): по ним eval записывает
    // итоговую конфигурацию прогона
    tracing::info!(
        search = %serde_json::to_string(&state.cfg.search).unwrap_or_default(),
        index = %serde_json::to_string(&state.cfg.index).unwrap_or_default(),
        "параметры поиска и индексации"
    );
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

/// Открывает все базы (с миграциями), регистрирует решения M1–M2 в реестре (§11.6),
/// дожидается разовой перечанковки R1 и возвращает число пользователей. Если токенизатор модели доступен, R1 идёт её точным счётчиком
/// и порогами под её окно (как у сервера), иначе — приближённым (`ApproxCounter`).
pub async fn migrate(cfg: ServerConfig) -> anyhow::Result<usize> {
    let dir = zam_store::DataDir::open(&cfg.data_dir)?;
    let tc_cfg = cfg.clone();
    tokio::task::spawn_blocking(move || ml::setup_chunker(&tc_cfg, None)).await?;
    let system = zam_store::SystemStore::open(&dir)?;
    let users = zam_store::UserDbs::new(dir.users_dir());
    let list = system.list_users().await?;
    for u in &list {
        users.get(u.id).await?;
    }
    bootstrap::register_solutions(&system, &users).await;
    for u in &list {
        users.wait_rechunk(u.id).await;
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
    // Windows: Ctrl+Break — штатная остановка процесса без окна консоли (его группе процессов
    // шлёт `GenerateConsoleCtrlEvent`: `cargo xtask bench --scenario first-start`, менеджеры
    // служб); Ctrl+C такой группе не доставляется.
    #[cfg(windows)]
    let term = async {
        match tokio::signal::windows::ctrl_break() {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(any(unix, windows)))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
}
