//! Сервер ZeroAIMemory.
pub mod agent_setup;
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
pub mod mcp;
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

/// Файл первого токена администратора в каталоге данных (stderr — не терминал).
pub const INITIAL_TOKEN_FILE: &str = "first-admin-token.txt";
/// Строка stderr с путём к файлу первого токена (её читают и тестовые обвязки).
pub const INITIAL_TOKEN_FILE_LINE: &str = "ZeroAIMemoryTokenFile=";

/// Сообщает первый токен администратора (мимо tracing) — **стенд Debian 13, L1**:
/// - stderr — терминал: токен в рамке, файл не пишется;
/// - stderr не терминал (служба systemd → journald, перенаправление в файл): токен в журнал не
///   попадает — он записывается в `<data_dir>/first-admin-token.txt` с правами только владельца
///   (0600 на unix, DACL владельца, SYSTEM и администраторов на Windows — как `master.key`
///   Vault), а в stderr — путь к файлу и совет удалить его после прочтения. Не удалось записать
///   файл — токен печатается в рамке с предупреждением: потерять доступ хуже
///   **[решение исполнителя]**.
fn announce_initial_token(
    token: &str,
    data_root: &std::path::Path,
    tty: bool,
    out: &mut impl std::io::Write,
) -> std::io::Result<()> {
    if !tty {
        let path = data_root.join(INITIAL_TOKEN_FILE);
        match write_token_file(&path, token) {
            Ok(()) => {
                writeln!(
                    out,
                    "==== ZeroAIMemory: первый токен администратора записан в файл (показывается один раз) ===="
                )?;
                writeln!(out, "{INITIAL_TOKEN_FILE_LINE}{}", path.display())?;
                writeln!(
                    out,
                    "Прочитайте токен, сохраните его и удалите файл: в журнал службы токен не пишется."
                )?;
                return writeln!(
                    out,
                    "=========================================================================================="
                );
            }
            Err(e) => writeln!(
                out,
                "предупреждение: файл {} не записан ({e}); токен — ниже, в stderr",
                path.display()
            )?,
        }
    }
    writeln!(
        out,
        "==== ZeroAIMemory: первый токен администратора (показывается один раз) ===="
    )?;
    writeln!(out, "ZeroAIMemoryToken={token}")?;
    writeln!(
        out,
        "=========================================================================="
    )
}

/// Пишет файл токена заново (остаток прошлого старта удаляется) сразу с правами только
/// владельца, затем `sync_all`.
fn write_token_file(path: &std::path::Path, token: &str) -> std::io::Result<()> {
    use std::io::Write;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut f = vault::keys::create_private(path)?;
    f.write_all(format!("{token}\n").as_bytes())?;
    f.sync_all()
}

/// Запускает сервер до Ctrl+C / SIGTERM.
pub async fn run(cfg: ServerConfig, env_password: Option<String>) -> anyhow::Result<()> {
    let bind = cfg.server.bind;
    let log_cfg = cfg.log.clone();
    // Порядок: пароль, лок каталога, логи, базы; bind до выпуска токена, печать после.
    let prepared = app::prepare(cfg, env_password.as_deref())?;
    let data_root = prepared.data_dir.root().to_path_buf();
    let _guard = logging::init(&log_cfg, &prepared.data_dir.logs())?;
    // bind до выпуска токена: при занятом порту в БД ничего не появляется.
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let (app, state, report) = app::finish(prepared).await?;
    if let Some(t) = &report.initial_token {
        use std::io::IsTerminal;
        let err = std::io::stderr();
        let _ = announce_initial_token(t, &data_root, err.is_terminal(), &mut err.lock());
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

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "zam_ПервыйТокенТеста0123456789";

    /// stderr не терминал (служба, journald): токен — в файле с правами только владельца, в
    /// выводе — путь к файлу, но не сам токен (стенд Debian 13, L1).
    #[test]
    fn non_tty_writes_private_file_without_token_in_output() {
        let d = test_util::dir_in_target("initial-token-file");
        // остаток прошлого старта заменяется
        std::fs::write(d.join(INITIAL_TOKEN_FILE), "старое").unwrap();
        let mut out = Vec::new();
        announce_initial_token(TOKEN, &d, false, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(!out.contains(TOKEN), "{out}");
        assert!(!out.contains("ZeroAIMemoryToken="), "{out}");
        let file = d.join(INITIAL_TOKEN_FILE);
        assert!(
            out.lines()
                .any(|l| l == format!("{INITIAL_TOKEN_FILE_LINE}{}", file.display())),
            "{out}"
        );
        assert!(out.contains("удалите файл"), "{out}");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!("{TOKEN}\n")
        );
        // только владелец: 0600 на unix, DACL без чужих читателей на Windows
        assert_eq!(vault::keys::key_file_exposure(&file).unwrap(), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    /// stderr — терминал: рамка с токеном, файл не пишется.
    #[test]
    fn tty_prints_frame_without_file() {
        let d = test_util::dir_in_target("initial-token-tty");
        let mut out = Vec::new();
        announce_initial_token(TOKEN, &d, true, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.lines()
                .any(|l| l == format!("ZeroAIMemoryToken={TOKEN}")),
            "{out}"
        );
        assert!(!d.join(INITIAL_TOKEN_FILE).exists());
    }

    /// Файл не записан (каталога нет) — токен не теряется: рамка с предупреждением.
    #[test]
    fn file_failure_falls_back_to_frame() {
        let d = test_util::dir_in_target("initial-token-fail").join("нет-каталога");
        let mut out = Vec::new();
        announce_initial_token(TOKEN, &d, false, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains("предупреждение") && out.contains(TOKEN),
            "{out}"
        );
    }
}
