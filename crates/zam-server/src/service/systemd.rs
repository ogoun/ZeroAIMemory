//! Юнит systemd (§13.3, §16.2): `zam-server service install` пишет
//! `/etc/systemd/system/<имя>.service` из шаблона `deploy/systemd/zam-server.service` с
//! таймаутом остановки по конфигу, `start|stop|status` — обёртки `systemctl`.
//!
//! Служба под systemd запускается обычным `serve` (`ExecStart`); остановка — `SIGTERM`
//! (`KillSignal=SIGTERM`), сервер понимает и SIGINT: оба ведут в общий сигнал остановки.
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::ServiceError;

/// Шаблон юнита из комплекта (тот же файл, что `deploy/systemd/zam-server.service`).
pub const TEMPLATE: &str = include_str!("../../../../deploy/systemd/zam-server.service");

/// Параметры юнита.
#[derive(Debug, Clone)]
pub struct UnitParams {
    /// `zam-server` (абсолютный путь); его каталог — `WorkingDirectory`.
    pub exe: PathBuf,
    /// Конфиг (абсолютный путь) для `ExecStart … serve --config`.
    pub config: PathBuf,
    /// `User=`; `None` — пользователь шаблона `zam` (и `Group=zam`).
    pub user: Option<String>,
    /// `ReadWritePaths=`: каталог данных и, если вне его, каталог мастер-ключа Vault.
    pub rw_paths: Vec<PathBuf>,
    /// `TimeoutStopSec=` — [`super::stop_timeout`].
    pub timeout: Duration,
}

/// Путь юнита по имени службы.
pub fn unit_path(name: &str) -> PathBuf {
    PathBuf::from(format!("/etc/systemd/system/{name}.service"))
}

/// Строковое представление пути юнита: перевод строки в пути недопустим (ломает юнит).
fn path_str(p: &Path) -> Result<String, ServiceError> {
    let s = p.display().to_string();
    if s.contains(['\n', '\r']) {
        return Err(ServiceError::usage(format!(
            "перевод строки в пути недопустим: {s:?}"
        )));
    }
    Ok(s)
}

/// Слово командной строки systemd: в кавычках при пробелах и кавычках, `%` — `%%`
/// (спецификаторы systemd).
fn quote(s: &str) -> String {
    let s = s.replace('%', "%%");
    if s.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '\\') {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s
    }
}

/// Юнит из шаблона: подставляются `User`/`Group`, `WorkingDirectory`, `ExecStart`,
/// `TimeoutStopSec`, `ReadWritePaths`; данные в `/home`, `/root` или `/run/user` —
/// `ProtectHome=read-only` (иначе каталог данных скрыт от службы).
pub fn render_unit(p: &UnitParams) -> Result<String, ServiceError> {
    if let Some(u) = &p.user {
        let ok = !u.is_empty()
            && u.len() <= 64
            && u.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b));
        if !ok {
            return Err(ServiceError::usage(format!(
                "недопустимое имя пользователя «{u}»"
            )));
        }
    }
    let exe = path_str(&p.exe)?;
    let workdir = path_str(p.exe.parent().unwrap_or(Path::new("/")))?;
    let config = path_str(&p.config)?;
    let rw: Vec<String> = p
        .rw_paths
        .iter()
        .map(|x| path_str(x))
        .collect::<Result<_, _>>()?;
    let home = rw.iter().any(|x| {
        ["/home", "/root", "/run/user"]
            .iter()
            .any(|h| x == h || x.starts_with(&format!("{h}/")))
    });
    let mut out = String::from(
        "# Создан `zam-server service install` из шаблона deploy/systemd/zam-server.service;\n\
         # после смены qdrant.shutdown_flush_wait_ms выполните install повторно.\n",
    );
    // шапка шаблона (как им пользоваться) в готовый юнит не попадает
    for line in TEMPLATE.lines().skip_while(|l| l.starts_with('#')) {
        let key = line.split('=').next().unwrap_or("");
        let replaced = match key {
            "User" => Some(format!(
                "User={}",
                p.user.as_deref().unwrap_or(super::DEFAULT_USER)
            )),
            // свой пользователь — его основная группа (Group= не задаётся)
            "Group" if p.user.as_deref().is_some_and(|u| u != super::DEFAULT_USER) => {
                continue;
            }
            "WorkingDirectory" => Some(format!("WorkingDirectory={}", quote(&workdir))),
            "ExecStart" => Some(format!(
                "ExecStart={} serve --config {}",
                quote(&exe),
                quote(&config)
            )),
            "TimeoutStopSec" => Some(format!("TimeoutStopSec={}", p.timeout.as_secs())),
            "ReadWritePaths" => Some(format!(
                "ReadWritePaths={}",
                rw.iter().map(|x| quote(x)).collect::<Vec<_>>().join(" ")
            )),
            "ProtectHome" if home => Some("ProtectHome=read-only".to_string()),
            _ => None,
        };
        out.push_str(replaced.as_deref().unwrap_or(line));
        out.push('\n');
    }
    Ok(out)
}

/// Значение `TimeoutStopUSec` (`systemctl show`): `1min 34s`, `90s`, `500ms`, `infinity`.
pub fn parse_timespan(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s == "infinity" {
        return Some(Duration::MAX);
    }
    let mut total = Duration::ZERO;
    let mut any = false;
    for tok in s.split_whitespace() {
        let split = tok.find(|c: char| !c.is_ascii_digit() && c != '.')?;
        let (num, unit) = tok.split_at(split);
        let n: f64 = num.parse().ok()?;
        let mult = match unit {
            "us" | "usec" | "µs" => 1e-6,
            "ms" | "msec" => 1e-3,
            "s" | "sec" => 1.0,
            "m" | "min" => 60.0,
            "h" | "hr" => 3600.0,
            "d" => 86_400.0,
            "w" => 604_800.0,
            _ => return None,
        };
        total += Duration::from_secs_f64(n * mult);
        any = true;
    }
    any.then_some(total)
}

/// Имя юнита своего процесса из `/proc/self/cgroup` (`0::/system.slice/<имя>.service`).
pub fn unit_from_cgroup(cgroup: &str) -> Option<String> {
    cgroup.lines().find_map(|l| {
        let path = l.rsplit(':').next()?;
        let last = path.rsplit('/').next()?;
        last.ends_with(".service").then(|| last.to_string())
    })
}

/// `TimeoutStopUSec` собственного юнита; `None` — не юнит `.service`.
#[cfg(unix)]
pub(super) fn probe_own_timeout() -> Option<Result<Duration, String>> {
    let cg = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let unit = unit_from_cgroup(&cg)?;
    Some((|| {
        let out = std::process::Command::new("systemctl")
            .args(["show", "-p", "TimeoutStopUSec", "--value", &unit])
            .output()
            .map_err(|e| format!("systemctl: {e}"))?;
        let v = String::from_utf8_lossy(&out.stdout);
        parse_timespan(&v).ok_or_else(|| format!("TimeoutStopUSec {unit}: «{}»", v.trim()))
    })())
}

#[cfg(not(unix))]
pub(super) fn probe_own_timeout() -> Option<Result<Duration, String>> {
    None
}

#[cfg(unix)]
pub use ops::*;

/// Команды `service` на Linux.
#[cfg(unix)]
mod ops {
    use std::process::Command;

    use super::super::{ServiceError, validate_name};
    use super::*;

    /// Эффективный uid из `/proc/self/status`.
    fn euid() -> Option<u32> {
        let s = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = s.lines().find(|l| l.starts_with("Uid:"))?;
        line.split_whitespace().nth(2)?.parse().ok()
    }

    fn require_root(what: &str) -> Result<(), ServiceError> {
        if euid() == Some(0) {
            Ok(())
        } else {
            Err(ServiceError::denied(format!(
                "{what} требует прав root: запустите через sudo"
            )))
        }
    }

    /// `systemctl <args>`; код выхода — как у systemctl.
    pub fn systemctl(args: &[&str]) -> Result<(), ServiceError> {
        let st = Command::new("systemctl")
            .args(args)
            .status()
            .map_err(|e| ServiceError::failed(format!("systemctl: {e}")))?;
        if st.success() {
            Ok(())
        } else {
            let code = st.code().unwrap_or(1);
            Err(ServiceError {
                code: u8::try_from(code).unwrap_or(super::super::EXIT_FAILED),
                message: format!("systemctl {} завершился с кодом {code}", args.join(" ")),
            })
        }
    }

    /// Записать юнит, `daemon-reload`, `enable`. Пользователь должен существовать.
    pub fn install(name: &str, unit: &str, user: &str) -> Result<(), ServiceError> {
        validate_name(name)?;
        require_root("установка службы")?;
        let known = Command::new("id")
            .args(["-u", user])
            .output()
            .is_ok_and(|o| o.status.success());
        if !known {
            return Err(ServiceError::usage(format!(
                "пользователь «{user}» не найден: создайте его (useradd --system …) или укажите --user"
            )));
        }
        let path = unit_path(name);
        let tmp = path.with_extension("service.tmp");
        std::fs::write(&tmp, unit)
            .and_then(|()| std::fs::rename(&tmp, &path))
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    ServiceError::denied(format!("{}: {e}; запустите через sudo", path.display()))
                } else {
                    ServiceError::failed(format!("{}: {e}", path.display()))
                }
            })?;
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", &format!("{name}.service")])?;
        println!("юнит записан: {}", path.display());
        println!(
            "служба включена; запуск: sudo systemctl start {name} (или zam-server service start)"
        );
        Ok(())
    }

    /// `disable --now`, удалить юнит, `daemon-reload`.
    pub fn uninstall(name: &str) -> Result<(), ServiceError> {
        validate_name(name)?;
        require_root("удаление службы")?;
        let path = unit_path(name);
        if !path.is_file() {
            return Err(ServiceError::failed(format!(
                "служба не установлена: нет {}",
                path.display()
            )));
        }
        // остановка ждёт штатного выхода сервера (до TimeoutStopSec)
        systemctl(&["disable", "--now", &format!("{name}.service")])?;
        std::fs::remove_file(&path)
            .map_err(|e| ServiceError::failed(format!("{}: {e}", path.display())))?;
        systemctl(&["daemon-reload"])?;
        println!("служба удалена: {}", path.display());
        Ok(())
    }

    pub fn start(name: &str) -> Result<(), ServiceError> {
        validate_name(name)?;
        systemctl(&["start", &format!("{name}.service")])
    }

    pub fn stop(name: &str) -> Result<(), ServiceError> {
        validate_name(name)?;
        systemctl(&["stop", &format!("{name}.service")])
    }

    pub fn status(name: &str) -> Result<(), ServiceError> {
        validate_name(name)?;
        systemctl(&["status", "--no-pager", &format!("{name}.service")])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(user: Option<&str>, rw: &str) -> UnitParams {
        UnitParams {
            exe: PathBuf::from("/opt/zeroaimemory/zam-server"),
            config: PathBuf::from("/etc/zeroaimemory/config.toml"),
            user: user.map(String::from),
            rw_paths: vec![PathBuf::from(rw)],
            timeout: Duration::from_secs(94),
        }
    }

    /// Шаблон комплекта согласован с формулой R4 и остановкой по SIGTERM.
    #[test]
    fn template_matches_formula() {
        let def = crate::config::ServerConfig::default();
        let t = super::super::stop_timeout(def.qdrant.shutdown_flush_wait_ms).as_secs();
        assert!(
            TEMPLATE.lines().any(|l| l == format!("TimeoutStopSec={t}")),
            "{TEMPLATE}"
        );
        assert!(TEMPLATE.lines().any(|l| l == "KillSignal=SIGTERM"));
        assert!(TEMPLATE.lines().any(|l| l == "Restart=on-failure"));
    }

    #[test]
    fn render_substitutes_fields() {
        let u = render_unit(&params(None, "/var/lib/zeroaimemory")).unwrap();
        for want in [
            "User=zam",
            "Group=zam",
            "WorkingDirectory=/opt/zeroaimemory",
            "ExecStart=/opt/zeroaimemory/zam-server serve --config /etc/zeroaimemory/config.toml",
            "TimeoutStopSec=94",
            "KillSignal=SIGTERM",
            "ReadWritePaths=/var/lib/zeroaimemory",
            "ProtectHome=true",
        ] {
            assert!(u.lines().any(|l| l == want), "{want}\n{u}");
        }
        // свой пользователь — без Group (основная группа), данные в /home — read-only
        let u = render_unit(&params(Some("alice"), "/home/alice/zam")).unwrap();
        assert!(u.lines().any(|l| l == "User=alice"), "{u}");
        assert!(!u.lines().any(|l| l.starts_with("Group=")), "{u}");
        assert!(u.lines().any(|l| l == "ProtectHome=read-only"), "{u}");
        assert!(render_unit(&params(Some("a b"), "/x")).is_err());
        assert!(render_unit(&params(Some("a\nExecStartPre=/bin/x"), "/x")).is_err());
    }

    #[test]
    fn render_quotes_paths() {
        let mut p = params(None, "/srv/zam data");
        p.config = PathBuf::from("/etc/zam 100%/config.toml");
        let u = render_unit(&p).unwrap();
        assert!(
            u.lines().any(|l| l
                == "ExecStart=/opt/zeroaimemory/zam-server serve --config \"/etc/zam 100%%/config.toml\""),
            "{u}"
        );
        assert!(
            u.lines().any(|l| l == "ReadWritePaths=\"/srv/zam data\""),
            "{u}"
        );
        p.config = PathBuf::from("/etc/x\n/config.toml");
        assert!(render_unit(&p).is_err());
    }

    #[test]
    fn timespan_parsed() {
        assert_eq!(parse_timespan("1min 34s"), Some(Duration::from_secs(94)));
        assert_eq!(parse_timespan("90s\n"), Some(Duration::from_secs(90)));
        assert_eq!(parse_timespan("2min 20s"), Some(Duration::from_secs(140)));
        assert_eq!(parse_timespan("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_timespan("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_timespan("infinity"), Some(Duration::MAX));
        assert_eq!(parse_timespan(""), None);
        assert_eq!(parse_timespan("abc"), None);
    }

    #[test]
    fn unit_name_from_cgroup() {
        assert_eq!(
            unit_from_cgroup("0::/system.slice/zam-server.service\n").as_deref(),
            Some("zam-server.service")
        );
        assert_eq!(
            unit_from_cgroup(
                "12:pids:/system.slice/zam2.service\n1:name=systemd:/system.slice/zam2.service"
            )
            .as_deref(),
            Some("zam2.service")
        );
        assert_eq!(
            unit_from_cgroup("0::/user.slice/user-1000.slice/session-3.scope"),
            None
        );
    }
}
