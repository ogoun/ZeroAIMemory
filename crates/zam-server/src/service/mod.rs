//! Режим службы (§13.3, §16.2): служба Windows (SCM) и юнит systemd.
//!
//! Тот же бинарник работает и службой, и обычным процессом. Под SCM точка входа —
//! `zam-server service run` ([`windows`]), под systemd — обычный `serve` из юнита
//! ([`systemd`]). Все пути остановки ведут в общий сигнал сервера ([`crate::ShutdownSignal`]).
//! Таймаут остановки службы — одна величина [`stop_timeout`] для юнита, SCM и проверки при
//! старте (R4, §4.3).
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::RunMode;
use crate::config::{CliOverrides, ServerConfig};

pub mod systemd;
#[cfg(windows)]
pub mod windows;

/// Имя службы по умолчанию.
pub const DEFAULT_NAME: &str = "zam-server";
/// Пользователь службы systemd по умолчанию (как в шаблоне `deploy/systemd`).
pub const DEFAULT_USER: &str = "zam";

/// Код выхода: команда не выполнена.
pub const EXIT_FAILED: u8 = 1;
/// Код выхода: неверный вызов (не та платформа, `run` из консоли, нет конфига).
pub const EXIT_USAGE: u8 = 2;
/// Код выхода: нет прав администратора (root).
pub const EXIT_DENIED: u8 = 3;

/// Ошибка команды `service` с кодом выхода процесса.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ServiceError {
    pub code: u8,
    pub message: String,
}

impl ServiceError {
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_FAILED,
            message: message.into(),
        }
    }
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_USAGE,
            message: message.into(),
        }
    }
    pub fn denied(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_DENIED,
            message: message.into(),
        }
    }
}

/// Таймаут остановки службы `T = max(90, ceil(shutdown_flush_wait_ms / 1000) + 80)` с
/// **[решение контроллера, R4]**: 80 с запаса — недоделанная пачка инференса (до 60 с),
/// перечанковка (до 10 с), SIGTERM Qdrant (до 10 с) — плюс ожидание подтверждения WAL Qdrant.
/// При умолчании 14 000 мс — 94 с, при максимуме 60 000 — 140 с. Одна функция для
/// `TimeoutStopSec` юнита, `Preshutdown` SCM и проверки при старте.
pub fn stop_timeout(shutdown_flush_wait_ms: u64) -> Duration {
    Duration::from_secs((shutdown_flush_wait_ms.div_ceil(1000) + 80).max(90))
}

/// Текст WARN, если таймаут остановки службы `actual` меньше [`stop_timeout`].
pub fn stop_timeout_warning(actual: Duration, shutdown_flush_wait_ms: u64) -> Option<String> {
    let need = stop_timeout(shutdown_flush_wait_ms);
    (actual < need).then(|| {
        format!(
            "таймаут остановки службы {} с меньше ожидания Qdrant {} с с запасом на остановку \
             (нужно не меньше {} с): выполните `zam-server service install` повторно",
            actual.as_secs(),
            shutdown_flush_wait_ms.div_ceil(1000),
            need.as_secs()
        )
    })
}

/// Предел проверки таймаута службы при старте.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Проверка таймаута остановки при старте — только под службой (§13.3): под SCM —
/// `Preshutdown` собственной службы, под systemd (`INVOCATION_ID`) — `TimeoutStopUSec` юнита.
/// При ручном запуске проверки нет. Не удалось узнать таймаут — DEBUG, без WARN.
pub async fn check_stop_timeout(mode: &RunMode, shutdown_flush_wait_ms: u64) {
    let mode = mode.clone();
    // `systemctl show` или SCM не задерживают старт дольше [`PROBE_TIMEOUT`] (M-2)
    let task = tokio::task::spawn_blocking(move || probe_stop_timeout(&mode));
    let probed = match tokio::time::timeout(PROBE_TIMEOUT, task).await {
        Ok(r) => r.ok().flatten(),
        Err(_) => Some(Err(format!("нет ответа за {} с", PROBE_TIMEOUT.as_secs()))),
    };
    match probed {
        Some(Ok(actual)) => {
            if let Some(msg) = stop_timeout_warning(actual, shutdown_flush_wait_ms) {
                tracing::warn!("{msg}");
            } else {
                tracing::debug!(
                    secs = actual.as_secs(),
                    "таймаут остановки службы достаточен"
                );
            }
        }
        Some(Err(e)) => tracing::debug!(error = %e, "таймаут остановки службы не определён"),
        None => {}
    }
}

/// `None` — не служба (ручной запуск).
fn probe_stop_timeout(mode: &RunMode) -> Option<Result<Duration, String>> {
    match mode {
        #[cfg(windows)]
        RunMode::WindowsService { name } => Some(windows::preshutdown_timeout(name)),
        #[cfg(not(windows))]
        RunMode::WindowsService { .. } => None,
        RunMode::Console => {
            std::env::var_os("INVOCATION_ID")?;
            systemd::probe_own_timeout()
        }
    }
}

/// Имя службы: латиница, цифры, `_ . - @`, до 80 символов (подставляется в имя юнита и
/// параметры SCM — без пробелов и разделителей пути).
pub fn validate_name(name: &str) -> Result<(), ServiceError> {
    let ok = !name.is_empty()
        && !name.starts_with('-')
        && name.len() <= 80
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-@".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(ServiceError::usage(format!(
            "недопустимое имя службы «{name}»: латиница, цифры и _ . - @, до 80 символов"
        )))
    }
}

/// Каталог исполняемого файла (рабочий каталог службы).
pub fn exe_path() -> Result<PathBuf, ServiceError> {
    std::env::current_exe()
        .map_err(|e| ServiceError::failed(format!("путь к исполняемому файлу: {e}")))
}

/// Конфиг службы: явный путь, иначе `<каталог exe>\config.toml` (Windows) или
/// `/etc/zeroaimemory/config.toml` (Linux); абсолютный и существующий.
pub fn resolve_config(path: Option<&Path>, exe_dir: &Path) -> Result<PathBuf, ServiceError> {
    let p = match path {
        Some(p) => p.to_path_buf(),
        None if cfg!(windows) => exe_dir.join("config.toml"),
        None => PathBuf::from("/etc/zeroaimemory/config.toml"),
    };
    if !p.is_file() {
        return Err(ServiceError::usage(format!(
            "файл конфигурации не найден: {}; укажите --config",
            p.display()
        )));
    }
    std::path::absolute(&p).map_err(|e| ServiceError::failed(format!("путь {}: {e}", p.display())))
}

/// Конфигурация службы (файл → `ZAM_*` → флаги), как у `serve`.
pub fn load_config(
    config: Option<&Path>,
    cli: &CliOverrides,
) -> Result<ServerConfig, ServiceError> {
    // `vars()` паникует на не-Unicode значении: такие переменные пропускаются
    let env: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect();
    let cfg = crate::config::load(config, &env, cli)
        .map_err(|e| ServiceError::usage(format!("конфигурация: {e}")))?;
    zam_store::check_data_dir_len(&cfg.data_dir).map_err(|e| ServiceError::usage(e.to_string()))?;
    Ok(cfg)
}

/// Относительный путь конфига — от рабочего каталога службы (каталог exe).
pub fn absolute_in(base: &Path, p: &Path) -> PathBuf {
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    };
    // без обращения к диску: каталога данных может ещё не быть
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Аргументы `service install`.
#[derive(Debug, Clone)]
pub struct InstallArgs {
    pub name: String,
    pub config: Option<PathBuf>,
    /// Пользователь службы (только Linux).
    pub user: Option<String>,
    /// Вывести юнит systemd / параметры SCM без установки.
    pub print: bool,
}

/// `service install`: юнит systemd или служба SCM с таймаутом остановки по конфигу.
pub fn install(a: InstallArgs) -> Result<(), ServiceError> {
    validate_name(&a.name)?;
    if cfg!(windows) && a.user.is_some() {
        return Err(ServiceError::usage(
            "--user — только для Linux: служба Windows работает под LocalSystem",
        ));
    }
    let exe = exe_path()?;
    let exe_dir = exe.parent().unwrap_or(Path::new(".")).to_path_buf();
    let config = resolve_config(a.config.as_deref(), &exe_dir)?;
    let cfg = load_config(Some(&config), &CliOverrides::default())?;
    warn_env_not_inherited(std::env::vars_os().filter_map(|(k, _)| k.into_string().ok()));
    let timeout = stop_timeout(cfg.qdrant.shutdown_flush_wait_ms);
    // относительные пути конфига — от рабочего каталога службы (каталог exe)
    let data_dir = absolute_in(&exe_dir, &cfg.data_dir);
    #[cfg(windows)]
    {
        let plan = windows::InstallPlan {
            name: a.name,
            exe,
            config,
            timeout,
            logs_dir: data_dir.join("logs"),
        };
        if a.print {
            print!("{}", plan.describe());
            return Ok(());
        }
        windows::install(&plan)
    }
    #[cfg(not(windows))]
    {
        let mut rw_paths = vec![data_dir.clone()];
        if let Some(k) = &cfg.vault.master_key_file {
            let dir = absolute_in(&exe_dir, k.parent().unwrap_or(Path::new(".")));
            if !dir.starts_with(&data_dir) {
                rw_paths.push(dir);
            }
        }
        let unit = systemd::render_unit(&systemd::UnitParams {
            exe,
            config,
            user: a.user.clone(),
            rw_paths,
            timeout,
        })?;
        if a.print {
            print!("{unit}");
            return Ok(());
        }
        #[cfg(unix)]
        {
            systemd::install(&a.name, &unit, a.user.as_deref().unwrap_or(DEFAULT_USER))
        }
        #[cfg(not(unix))]
        {
            Err(ServiceError::usage("режим службы — только Windows и Linux"))
        }
    }
}

/// Переменные `ZAM_*` окружения установки (кроме клиентских `ZAM_URL`/`ZAM_TOKEN`).
pub fn zam_env_names(keys: impl Iterator<Item = String>) -> Vec<String> {
    let mut v: Vec<String> = keys
        .filter(|k| k.to_ascii_uppercase().starts_with("ZAM_"))
        .filter(|k| !matches!(k.to_ascii_uppercase().as_str(), "ZAM_URL" | "ZAM_TOKEN"))
        .collect();
    v.sort();
    v
}

/// M-7: служба не наследует окружение консоли установки — `ZAM_*` (слой конфигурации,
/// `ZAM_ADMIN_PASSWORD`, `ZAM_MASTER_KEY`) ей не видны; таймаут посчитан с ними.
fn warn_env_not_inherited(keys: impl Iterator<Item = String>) {
    let names = zam_env_names(keys);
    if !names.is_empty() {
        eprintln!(
            "предупреждение: в окружении заданы {}: служба их не увидит — перенесите значения в \
             конфиг (секреты — [admin] password_hash, [vault] master_key_file)",
            names.join(", ")
        );
    }
}

/// Команда управления установленной службой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Uninstall,
    Start,
    Stop,
    Status,
}

pub fn control(what: Control, name: &str) -> Result<(), ServiceError> {
    validate_name(name)?;
    #[cfg(windows)]
    {
        match what {
            Control::Uninstall => windows::uninstall(name),
            Control::Start => windows::start(name),
            Control::Stop => windows::stop(name),
            Control::Status => windows::status(name),
        }
    }
    #[cfg(unix)]
    {
        match what {
            Control::Uninstall => systemd::uninstall(name),
            Control::Start => systemd::start(name),
            Control::Stop => systemd::stop(name),
            Control::Status => systemd::status(name),
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = what;
        Err(ServiceError::usage("режим службы — только Windows и Linux"))
    }
}

/// `service run` — точка входа SCM; на Linux служба запускается systemd командой `serve`.
pub fn run(config: Option<PathBuf>, cli: CliOverrides) -> Result<(), ServiceError> {
    #[cfg(windows)]
    {
        windows::run(windows::RunArgs { config, cli })
    }
    #[cfg(not(windows))]
    {
        let _ = (config, cli);
        Err(ServiceError::usage(
            "`zam-server service run` — точка входа службы Windows; на Linux служба \
             запускается systemd командой `zam-server serve` (zam-server service install)",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R4: `max(90, ceil(ms/1000) + 80)`; при умолчании 14 000 — 94 с, при 60 000 — 140 с.
    #[test]
    fn stop_timeout_formula() {
        assert_eq!(stop_timeout(14_000), Duration::from_secs(94));
        assert_eq!(stop_timeout(60_000), Duration::from_secs(140));
        assert_eq!(stop_timeout(0), Duration::from_secs(90));
        assert_eq!(stop_timeout(10_000), Duration::from_secs(90));
        assert_eq!(stop_timeout(10_001), Duration::from_secs(91));
        let def = crate::config::ServerConfig::default();
        assert_eq!(
            stop_timeout(def.qdrant.shutdown_flush_wait_ms),
            Duration::from_secs(94)
        );
    }

    /// Проверка при старте: WARN с командой исправления, если таймаут службы короче нужного.
    #[test]
    fn timeout_check_warns_when_short() {
        let w = stop_timeout_warning(Duration::from_secs(90), 14_000).expect("90 < 94");
        assert!(
            w.contains("таймаут остановки службы 90 с меньше ожидания Qdrant 14 с")
                && w.contains("94 с")
                && w.contains("zam-server service install"),
            "{w}"
        );
        assert!(stop_timeout_warning(Duration::from_secs(94), 14_000).is_none());
        assert!(stop_timeout_warning(Duration::from_secs(140), 60_000).is_none());
        assert!(stop_timeout_warning(Duration::from_secs(139), 60_000).is_some());
        // NSSM по умолчанию — 1,5 с
        assert!(stop_timeout_warning(Duration::from_millis(1500), 0).is_some());
        assert!(stop_timeout_warning(Duration::MAX, 60_000).is_none());
    }

    #[test]
    fn names_validated() {
        for ok in ["zam-server", "zam_server2", "zam@x", "a.b"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "zam server",
            "zam/x",
            "zam\nx",
            "з",
            "-zam",
            &"a".repeat(81),
        ] {
            assert_eq!(validate_name(bad).unwrap_err().code, EXIT_USAGE, "{bad}");
        }
    }

    /// M-7: `ZAM_*` окружения установки перечисляются (клиентские `ZAM_URL`/`ZAM_TOKEN` — нет).
    #[test]
    fn zam_env_listed_for_warning() {
        let keys = [
            "PATH",
            "ZAM_DATA_DIR",
            "ZAM_URL",
            "zam_admin_password",
            "ZAM_TOKEN",
        ]
        .into_iter()
        .map(String::from);
        assert_eq!(
            zam_env_names(keys),
            vec!["ZAM_DATA_DIR".to_string(), "zam_admin_password".to_string()]
        );
    }

    #[test]
    fn absolute_in_normalizes() {
        let base = Path::new("/opt/zeroaimemory");
        assert_eq!(
            absolute_in(base, Path::new("./data")),
            PathBuf::from("/opt/zeroaimemory/data")
        );
        assert_eq!(
            absolute_in(base, Path::new("../x/data")),
            PathBuf::from("/opt/x/data")
        );
    }
}
