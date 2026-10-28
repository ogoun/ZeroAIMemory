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
pub mod service;
pub mod vault;
pub mod web;

#[cfg(test)]
mod test_util;

use std::net::SocketAddr;

pub use app::{AppState, BootReport, ShutdownSignal, StopSource, build};
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
///   Vault), а в stderr — путь к файлу и совет удалить его после прочтения;
/// - stderr не терминал и файл не записан (каталог только для чтения, нет места, файл не
///   удалить): токен **не печатается** (иначе снова journald), частичный файл удаляется,
///   итог — [`Announced::NotWritten`]: вызывающий отзывает токен ([`revoke_initial_token`]), а в
///   stderr — как получить новый: вход в WebUI паролем администратора (он на первом старте
///   обязателен) **[решение контроллера, fix round 1]**.
fn announce_initial_token(
    token: &str,
    data_root: &std::path::Path,
    tty: bool,
    out: &mut impl std::io::Write,
) -> std::io::Result<Announced> {
    if tty {
        writeln!(
            out,
            "==== ZeroAIMemory: первый токен администратора (показывается один раз) ===="
        )?;
        writeln!(out, "ZeroAIMemoryToken={token}")?;
        writeln!(
            out,
            "=========================================================================="
        )?;
        return Ok(Announced::Frame);
    }
    let path = data_root.join(INITIAL_TOKEN_FILE);
    if let Err(e) = write_token_file(&path, token) {
        writeln!(
            out,
            "предупреждение: не удалось записать {}: {e}; первый токен не показан. Войдите в WebUI паролем администратора и выпустите токен (Аккаунт/Пользователи)",
            path.display()
        )?;
        return Ok(Announced::NotWritten);
    }
    writeln!(
        out,
        "==== ZeroAIMemory: первый токен администратора записан в файл (показывается один раз) ===="
    )?;
    writeln!(out, "{INITIAL_TOKEN_FILE_LINE}{}", path.display())?;
    writeln!(
        out,
        "Прочитайте токен, сохраните его и удалите файл: в журнал службы токен не пишется."
    )?;
    writeln!(
        out,
        "=========================================================================================="
    )?;
    Ok(Announced::File)
}

/// Как сообщён первый токен.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Announced {
    /// Рамка в stderr-терминале.
    Frame,
    /// Записан в файл, в stderr — путь.
    File,
    /// stderr не терминал, файл не записан: токен не показан, его надо отозвать.
    NotWritten,
}

/// Пишет файл токена заново (остаток прошлого старта удаляется) сразу с правами только
/// владельца, затем `sync_all`. Сбой после создания — файл удаляется: частичного токена на
/// диске не остаётся.
fn write_token_file(path: &std::path::Path, token: &str) -> std::io::Result<()> {
    use std::io::Write;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut f = vault::keys::create_private(path)?;
    let written = f
        .write_all(format!("{token}\n").as_bytes())
        .and_then(|()| f.sync_all());
    if written.is_err() {
        drop(f);
        let _ = std::fs::remove_file(path);
    }
    written
}

/// Отзывает первый токен, который не удалось сообщить (файл не записан): действующий токен
/// без владельца не остаётся. Аудит `initial_token_revoked` — с маской токена. Сбой — WARN
/// без токена (сервер продолжает работу: токен никому не известен). `true` — токен больше не
/// действует (отозван сейчас или уже не найден); по результату вызывающий пишет в stderr
/// ([`revoke_notice`]).
async fn revoke_initial_token(system: &zam_store::SystemStore, token: &str) -> bool {
    let found = match system.verify_token(token, None).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "первый токен не отозван: проверка не удалась");
            return false;
        }
    };
    let Some(auth) = found else {
        return true;
    };
    if let Err(e) = system.revoke_token(auth.token_id).await {
        tracing::warn!(error = %e, "первый токен не отозван");
        return false;
    }
    let _ = system
        .audit(
            "system",
            Some(auth.user_id),
            "initial_token_revoked",
            None,
            None,
            serde_json::json!({
                "token": zam_core::mask_token(token),
                "reason": "файл первого токена не записан",
            }),
        )
        .await;
    tracing::warn!("первый токен администратора отозван: файл не записан; выпустите токен в WebUI");
    true
}

/// Строка stderr после [`revoke_initial_token`] — по её результату (финальное ревью M4, M-8:
/// раньше «отозван» печаталось до попытки отзыва).
fn revoke_notice(revoked: bool) -> &'static str {
    if revoked {
        "первый токен администратора отозван"
    } else {
        "предупреждение: первый токен администратора отозвать не удалось (см. журнал) — отзовите          его в WebUI: «Пользователи» → администратор → токены"
    }
}

/// Файл первого токена остался с прошлого старта — WARN при каждом старте (без содержимого),
/// пока его не удалят. `true` — файл есть.
fn warn_if_token_file_left(data_root: &std::path::Path) -> bool {
    let path = data_root.join(INITIAL_TOKEN_FILE);
    let left = path.is_file();
    if left {
        tracing::warn!(
            path = %path.display(),
            "файл первого токена администратора всё ещё на диске: удалите файл после прочтения"
        );
    }
    left
}

/// Как запущен сервер.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RunMode {
    /// Консоль, systemd (`serve` с `INVOCATION_ID` — проверка таймаута юнита), NSSM.
    #[default]
    Console,
    /// Служба Windows (SCM) с этим именем: журнал только в файл, проверка таймаута службы.
    WindowsService { name: String },
}

/// Параметры запуска [`run_with`].
#[derive(Default)]
pub struct RunOptions {
    pub mode: RunMode,
    /// Общий сигнал остановки: к нему подключаются сигналы ОС, служба Windows — SCM.
    pub signal: ShutdownSignal,
    /// Вызывается, когда сервер принимает запросы (служба: `Running`).
    pub on_ready: Option<Box<dyn FnOnce() + Send>>,
}

impl std::fmt::Debug for RunOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunOptions")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

/// Запускает сервер до Ctrl+C / SIGTERM.
pub async fn run(cfg: ServerConfig, env_password: Option<String>) -> anyhow::Result<()> {
    run_with(cfg, env_password, RunOptions::default()).await
}

/// Запускает сервер до общего сигнала остановки (§4.3): Ctrl+C, SIGTERM, Ctrl+Break и (служба
/// Windows) команды SCM ведут в один путь — штатное завершение HTTP и [`AppState::shutdown`].
pub async fn run_with(
    cfg: ServerConfig,
    env_password: Option<String>,
    opts: RunOptions,
) -> anyhow::Result<()> {
    let RunOptions {
        mode,
        signal,
        on_ready,
    } = opts;
    let console = mode == RunMode::Console;
    let bind = cfg.server.bind;
    let log_cfg = cfg.log.clone();
    // Порядок: пароль, лок каталога, логи, базы; bind до выпуска токена, печать после.
    let prepared = app::prepare(cfg, env_password.as_deref())?;
    let data_root = prepared.data_dir.root().to_path_buf();
    let _guard = logging::init(&log_cfg, &prepared.data_dir.logs(), console)?;
    // сигналы ОС — сразу: остановка во время долгого старта ждёт его конца и выходит штатно
    signal.watch(app::os_stop_signals());
    // bind до выпуска токена: при занятом порту в БД ничего не появляется.
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let (app, state, report) = app::finish(prepared).await?;
    // до объявления: на первом старте файла ещё нет, WARN — только за оставленный
    warn_if_token_file_left(&data_root);
    if let Some(t) = &report.initial_token {
        use std::io::IsTerminal;
        let err = std::io::stderr();
        // служба Windows: консоли нет — только файл
        let tty = console && err.is_terminal();
        let announced = announce_initial_token(t, &data_root, tty, &mut err.lock());
        if !console && matches!(announced, Ok(Announced::File)) {
            tracing::warn!(
                path = %data_root.join(INITIAL_TOKEN_FILE).display(),
                "первый токен администратора записан в файл: прочитайте и удалите файл"
            );
        }
        // не сообщён (файл не записан или stderr сломан) — токен никому не известен
        if !matches!(announced, Ok(Announced::Frame | Announced::File)) {
            let revoked = revoke_initial_token(&state.system, t).await;
            use std::io::Write;
            let _ = writeln!(err.lock(), "{}", revoke_notice(revoked));
        }
    }
    if !crate::config::has_default_params(&state.admin_hash) {
        tracing::warn!(
            "хэш пароля администратора с нестандартными параметрами argon2: время ответа входа \
             выдаёт логин администратора; получите хэш командой `zam-server hash-password`"
        );
    }
    service::check_stop_timeout(&mode, state.cfg.qdrant.shutdown_flush_wait_ms).await;
    tracing::info!(addr = %listener.local_addr()?, version = state.version, "zam-server запущен");
    // действующие веса поиска и пороги чанкинга (секретов нет): по ним eval записывает
    // итоговую конфигурацию прогона
    tracing::info!(
        search = %serde_json::to_string(&state.cfg.search).unwrap_or_default(),
        index = %serde_json::to_string(&state.cfg.index).unwrap_or_default(),
        "параметры поиска и индексации"
    );
    if let Some(f) = on_ready {
        f();
    }
    let stop = signal.clone();
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let source = stop.wait().await;
        tracing::info!(%source, "остановка zam-server");
    })
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
        let r = announce_initial_token(TOKEN, &d, false, &mut out).unwrap();
        assert_eq!(r, Announced::File);
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
        let r = announce_initial_token(TOKEN, &d, true, &mut out).unwrap();
        assert_eq!(r, Announced::Frame);
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.lines()
                .any(|l| l == format!("ZeroAIMemoryToken={TOKEN}")),
            "{out}"
        );
        assert!(!d.join(INITIAL_TOKEN_FILE).exists());
    }

    /// Файл не записан (каталога нет), stderr не терминал: токена в выводе нет (иначе — снова
    /// journald), он отозван в базе с аудитом, в выводе — как выпустить новый (fix round 1).
    #[tokio::test]
    async fn file_failure_hides_and_revokes_token() {
        let data = test_util::dir_in_target("initial-token-fail");
        let cfg = ServerConfig {
            data_dir: data.join("data"),
            ..ServerConfig::default()
        };
        let (_app, st, rep) = build(cfg, Some("pw".into())).await.unwrap();
        let token = rep.initial_token.expect("первый старт");
        assert!(
            st.system
                .verify_token(&token, None)
                .await
                .unwrap()
                .is_some()
        );
        let missing = data.join("нет-каталога");
        let mut out = Vec::new();
        let r = announce_initial_token(&token, &missing, false, &mut out).unwrap();
        assert_eq!(r, Announced::NotWritten);
        let out = String::from_utf8(out).unwrap();
        assert!(!out.contains(&token), "{out}");
        assert!(!out.contains("ZeroAIMemoryToken="), "{out}");
        assert!(
            out.contains("не удалось записать") && out.contains("WebUI паролем администратора"),
            "{out}"
        );
        assert!(!missing.join(INITIAL_TOKEN_FILE).exists());
        assert!(revoke_initial_token(&st.system, &token).await);
        assert!(revoke_notice(true).contains("отозван"));
        assert!(revoke_notice(false).contains("не удалось"));
        assert!(
            st.system
                .verify_token(&token, None)
                .await
                .unwrap()
                .is_none()
        );
        let audit = st.system.list_audit(20, None).await.unwrap();
        let row = audit
            .iter()
            .find(|r| r.action == "initial_token_revoked")
            .expect("аудит отзыва");
        assert!(!row.details.to_string().contains(&token), "{}", row.details);
    }

    /// Оставленный файл первого токена — WARN при старте, пока его не удалят (Minor 1).
    #[test]
    fn left_token_file_warns_without_content() {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::layer::SubscriberExt;
        #[derive(Clone, Default)]
        struct Buf(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let d = test_util::dir_in_target("initial-token-left");
        assert!(!warn_if_token_file_left(&d));
        std::fs::write(d.join(INITIAL_TOKEN_FILE), format!("{TOKEN}\n")).unwrap();
        let buf = Buf::default();
        let w = buf.clone();
        let sub = tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer().with_writer(move || w.clone()));
        let warned = tracing::subscriber::with_default(sub, || warn_if_token_file_left(&d));
        assert!(warned);
        let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(
            log.contains("WARN") && log.contains("удалите файл после прочтения"),
            "{log}"
        );
        assert!(!log.contains(TOKEN), "{log}");
    }
}
