//! Процесс Qdrant: запуск с привязкой к родителю, остановка, pid-файл, лок, сироты,
//! пересылка stdout/stderr в `tracing`.
//!
//! Привязка к родителю (§4.3, исследование 05 §7):
//! - Windows — `process-wrap`: Job Object с `KILL_ON_JOB_CLOSE` (процесс создаётся
//!   с `CREATE_SUSPENDED`, назначается в job, затем возобновляется) и `CREATE_NO_WINDOW`;
//!   при смерти `zam-server` ядро закрывает handle job и завершает Qdrant;
//! - Linux — `pre_exec`: `prctl(PR_SET_PDEATHSIG, SIGKILL)` и сверка `getppid()` с pid
//!   родителя (гонка «родитель умер до prctl»); плюс своя группа процессов. Сигнал
//!   приходит при смерти **потока**, сделавшего fork, поэтому `spawn` вызывается только
//!   из долгоживущего потока «qdrant-supervisor» (см. `mod.rs`).

use std::fs::{File, OpenOptions};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fs4::{FileExt, TryLockError};
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use tokio::io::{AsyncBufReadExt as _, AsyncRead, BufReader};

use super::config::ManagedConfig;
use crate::IndexError;

/// Лок каталога `<data_dir>/qdrant/`: один супервизор на каталог.
pub(crate) const LOCK_FILE: &str = ".zam-qdrant.lock";
/// pid запущенного Qdrant: по нему находится сирота после аварийного завершения сервера.
pub(crate) const PID_FILE: &str = "qdrant.pid";
/// Признак успешной инициализации Qdrant (`QDRANT_INIT_FILE_PATH`).
pub(crate) const INIT_FILE: &str = ".qdrant-initialized";

/// Тексты panic Qdrant при несовместимом хранилище (исследование 05 §6.1).
const STORAGE_PANICS: [&str; 2] = ["Collection version is greater", "Cannot upgrade version"];

/// Берёт эксклюзивный лок каталога; занят — [`IndexError::Locked`].
pub(crate) fn acquire_lock(qdir: &Path) -> Result<File, IndexError> {
    let path = qdir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| IndexError::Process(format!("лок {}: {e}", path.display())))?;
    match FileExt::try_lock(&file) {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(IndexError::Locked(qdir.display().to_string())),
        Err(TryLockError::Error(e)) => {
            Err(IndexError::Process(format!("лок {}: {e}", path.display())))
        }
    }
}

/// Лок каталога удерживается другим процессом (или этим же через другой handle).
pub(crate) fn lock_held(qdir: &Path) -> bool {
    let path = qdir.join(LOCK_FILE);
    let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
        return false;
    };
    matches!(FileExt::try_lock(&file), Err(TryLockError::WouldBlock))
}

pub(crate) fn read_pid_file(qdir: &Path) -> Option<u32> {
    std::fs::read_to_string(qdir.join(PID_FILE))
        .ok()?
        .trim()
        .parse()
        .ok()
}

pub(crate) fn write_pid_file(qdir: &Path, pid: u32) -> io::Result<()> {
    std::fs::write(qdir.join(PID_FILE), pid.to_string())
}

pub(crate) fn remove_pid_file(qdir: &Path) {
    let _ = std::fs::remove_file(qdir.join(PID_FILE));
}

/// Порт 127.0.0.1 свободен (bind проходит).
pub(crate) fn port_free(port: u16) -> bool {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).is_ok()
}

/// Завершает осиротевший Qdrant из pid-файла: процесс жив и это именно `qdrant`
/// (защита от переиспользования pid). Возвращает pid завершённого процесса.
pub(crate) fn cleanup_orphan(qdir: &Path, grace: Duration) -> Option<u32> {
    let pid = read_pid_file(qdir)?;
    let killed = if pid != std::process::id() && pid_alive(pid) && is_qdrant_process(pid) {
        tracing::warn!(
            component = "qdrant",
            pid,
            "найден осиротевший процесс Qdrant, завершаю"
        );
        terminate_pid(pid, grace);
        Some(pid)
    } else {
        None
    };
    remove_pid_file(qdir);
    killed
}

/// Ждёт исчезновения процесса не дольше `limit`.
fn wait_exit(pid: u32, limit: Duration) -> bool {
    let t = Instant::now();
    while t.elapsed() < limit {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !pid_alive(pid)
}

// ---------------------------------------------------------------- Linux / Unix

/// Процесс с таким pid существует и не зомби.
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // Поле state идёт после имени в скобках: «pid (comm) S ...».
        Ok(s) => s
            .rsplit_once(')')
            .is_none_or(|(_, rest)| !rest.trim_start().starts_with('Z')),
        Err(_) => false,
    }
}

/// Имя процесса — `qdrant`.
#[cfg(unix)]
pub fn is_qdrant_process(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == "qdrant")
}

/// SIGTERM, ожидание `grace`, затем SIGKILL.
#[cfg(unix)]
fn terminate_pid(pid: u32, grace: Duration) {
    let Ok(p) = libc::pid_t::try_from(pid) else {
        return;
    };
    // SAFETY: kill(2) с проверенным положительным pid.
    unsafe { libc::kill(p, libc::SIGTERM) };
    if !wait_exit(pid, grace) {
        // SAFETY: то же.
        unsafe { libc::kill(p, libc::SIGKILL) };
        wait_exit(pid, Duration::from_secs(5));
    }
}

// ---------------------------------------------------------------- Windows

#[cfg(windows)]
mod win {
    use windows::Win32::Foundation::{CloseHandle, HANDLE, STILL_ACTIVE};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, QueryFullProcessImageNameW,
        TerminateProcess,
    };
    use windows::core::PWSTR;

    /// Handle процесса, закрывается в Drop.
    struct Proc(HANDLE);

    impl Drop for Proc {
        fn drop(&mut self) {
            // SAFETY: handle получен из OpenProcess и закрывается один раз.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> Option<Proc> {
        // SAFETY: OpenProcess не требует инвариантов кроме корректных аргументов.
        unsafe { OpenProcess(access, false, pid) }.ok().map(Proc)
    }

    pub(super) fn alive(pid: u32) -> bool {
        let Some(p) = open(pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
            return false;
        };
        let mut code = 0u32;
        // SAFETY: валидный handle и указатель на локальную переменную.
        unsafe { GetExitCodeProcess(p.0, &mut code) }.is_ok() && code == STILL_ACTIVE.0 as u32
    }

    pub(super) fn image_name(pid: u32) -> Option<String> {
        let p = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        // SAFETY: буфер на `len` u16, Windows пишет не больше и возвращает длину.
        unsafe {
            QueryFullProcessImageNameW(p.0, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
        }
        .ok()?;
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        full.rsplit(['\\', '/']).next().map(str::to_string)
    }

    pub(super) fn terminate(pid: u32) {
        if let Some(p) = open(pid, PROCESS_TERMINATE) {
            // SAFETY: валидный handle с правом PROCESS_TERMINATE.
            let _ = unsafe { TerminateProcess(p.0, 1) };
        }
    }
}

/// Процесс с таким pid существует и не завершён.
#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    win::alive(pid)
}

/// Имя образа процесса — `qdrant.exe`.
#[cfg(windows)]
pub fn is_qdrant_process(pid: u32) -> bool {
    win::image_name(pid).is_some_and(|n| n.eq_ignore_ascii_case("qdrant.exe"))
}

/// Windows: Qdrant штатно останавливается только по Ctrl+C, которого у фонового процесса
/// нет, — `TerminateProcess`; целостность обеспечивают WAL и пересборка из SQLite.
#[cfg(windows)]
fn terminate_pid(pid: u32, _grace: Duration) {
    win::terminate(pid);
    wait_exit(pid, Duration::from_secs(5));
}

// ---------------------------------------------------------------- запуск и остановка

/// Запускает Qdrant в `qdir`. Вызывать только из потока «qdrant-supervisor» (PDEATHSIG).
pub(crate) fn spawn(
    cfg: &ManagedConfig,
    qdir: &Path,
    api_key: &str,
) -> io::Result<Box<dyn ChildWrapper>> {
    let mut cmd = CommandWrap::with_new(&cfg.binary, |c| {
        // Унаследованные QDRANT__* и RUN_MODE перекрыли бы наш конфиг (в том числе host).
        for (k, _) in std::env::vars_os() {
            let k = k.to_string_lossy();
            if k.starts_with("QDRANT") || k == "RUN_MODE" {
                c.env_remove(&*k);
            }
        }
        c.current_dir(qdir)
            .args(["--config-path", "config/zam.yaml", "--disable-telemetry"])
            .env("QDRANT__SERVICE__API_KEY", api_key)
            .env("QDRANT_INIT_FILE_PATH", qdir.join(INIT_FILE))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            let parent = std::process::id() as libc::pid_t;
            // SAFETY: замыкание исполняется в дочернем процессе между fork и exec и
            // вызывает только async-signal-safe функции (prctl, getppid, _exit).
            unsafe {
                c.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    // Родитель умер до prctl — сигнал уже не придёт.
                    if libc::getppid() != parent {
                        libc::_exit(1);
                    }
                    Ok(())
                });
            }
        }
    });
    #[cfg(windows)]
    {
        use process_wrap::tokio::{CreationFlags, JobObject};
        use windows::Win32::System::Threading::CREATE_NO_WINDOW;
        cmd.wrap(JobObject)
            .wrap(KillOnDrop)
            .wrap(CreationFlags(CREATE_NO_WINDOW));
    }
    #[cfg(unix)]
    {
        cmd.wrap(process_wrap::tokio::ProcessGroup::leader())
            .wrap(KillOnDrop);
    }
    cmd.spawn()
}

/// Штатная остановка: Linux — SIGTERM, ожидание `grace`, затем SIGKILL;
/// Windows — `TerminateProcess` (через job).
pub(crate) async fn stop_child(child: &mut Box<dyn ChildWrapper>, grace: Duration) {
    #[cfg(unix)]
    {
        if child.signal(libc::SIGTERM).is_ok()
            && tokio::time::timeout(grace, child.wait()).await.is_ok()
        {
            return;
        }
    }
    #[cfg(windows)]
    let _ = grace;
    let _ = child.start_kill();
    let _ = child.wait().await;
}

// ---------------------------------------------------------------- журнал

/// Строка JSON-журнала Qdrant.
#[derive(Debug, PartialEq)]
pub(crate) struct LogLine {
    pub level: tracing::Level,
    pub target: String,
    pub message: String,
}

/// Разбор строки `logger.format: json`:
/// `{"timestamp":…,"level":"INFO","fields":{"message":…},"target":…}`.
pub(crate) fn parse_json_line(line: &str) -> Option<LogLine> {
    if !line.starts_with('{') {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let level = match v.get("level")?.as_str()? {
        "ERROR" => tracing::Level::ERROR,
        "WARN" => tracing::Level::WARN,
        "INFO" => tracing::Level::INFO,
        "DEBUG" => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    };
    let message = v
        .get("fields")
        .and_then(|f| f.get("message"))
        .or_else(|| v.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or_default()
        .to_string();
    let target = v
        .get("target")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();
    Some(LogLine {
        level,
        target,
        message,
    })
}

/// Строка с текстом panic несовместимого хранилища.
pub(crate) fn is_storage_panic(line: &str) -> bool {
    STORAGE_PANICS.iter().any(|p| line.contains(p))
}

/// Обработка одной строки вывода: детектор panic, маскирование ключа, событие tracing.
fn handle_line(line: &str, stream: &'static str, key: &str, panic: &AtomicBool) {
    if line.trim().is_empty() {
        return;
    }
    // Qdrant ключ не печатает, но строка из окружения могла бы попасть в текст ошибки.
    let owned;
    let line = if !key.is_empty() && line.contains(key) {
        owned = line.replace(key, "***");
        owned.as_str()
    } else {
        line
    };
    if is_storage_panic(line) {
        panic.store(true, Ordering::SeqCst);
    }
    match parse_json_line(line) {
        Some(l) => {
            let (t, m) = (l.target.as_str(), l.message.as_str());
            match l.level {
                tracing::Level::ERROR => {
                    tracing::error!(component = "qdrant", qdrant_target = t, "{m}")
                }
                tracing::Level::WARN => {
                    tracing::warn!(component = "qdrant", qdrant_target = t, "{m}")
                }
                tracing::Level::INFO => {
                    tracing::info!(component = "qdrant", qdrant_target = t, "{m}")
                }
                tracing::Level::DEBUG => {
                    tracing::debug!(component = "qdrant", qdrant_target = t, "{m}")
                }
                _ => tracing::trace!(component = "qdrant", qdrant_target = t, "{m}"),
            }
        }
        // stderr без JSON — panic и сообщения до инициализации журнала; stdout — баннер.
        None if stream == "stderr" => tracing::warn!(component = "qdrant", stream, "{line}"),
        None => tracing::debug!(component = "qdrant", stream, "{line}"),
    }
}

/// Пересылает поток построчно до EOF (процесс завершился).
pub(crate) async fn forward<R: AsyncRead + Unpin>(
    r: R,
    stream: &'static str,
    key: Arc<str>,
    panic: Arc<AtomicBool>,
) {
    let mut reader = BufReader::new(r);
    let mut buf = Vec::with_capacity(512);
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf);
                handle_line(line.trim_end(), stream, &key, &panic);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_qdrant_json_line() {
        let l = parse_json_line(
            r#"{"timestamp":"2026-10-19T03:38:37Z","level":"WARN","fields":{"message":"Config file not found: config/config","log.target":"qdrant::settings"},"target":"qdrant::settings"}"#,
        )
        .unwrap();
        assert_eq!(
            l,
            LogLine {
                level: tracing::Level::WARN,
                target: "qdrant::settings".into(),
                message: "Config file not found: config/config".into(),
            }
        );
        assert!(parse_json_line("Version: 1.19.2, build: 016542aa").is_none());
        assert!(parse_json_line("{broken").is_none());
    }

    #[test]
    fn storage_panic_detector() {
        let flag = AtomicBool::new(false);
        handle_line(
            "Access web UI at http://localhost:6333",
            "stdout",
            "k",
            &flag,
        );
        assert!(!flag.load(Ordering::SeqCst));
        handle_line(
            r#"{"level":"ERROR","fields":{"message":"Panic occurred: Collection version is greater than application version"},"target":"qdrant::startup"}"#,
            "stdout",
            "k",
            &flag,
        );
        assert!(flag.load(Ordering::SeqCst));
        assert!(is_storage_panic(
            "Cannot upgrade version 0.3.0 to 0.4.2. Try to use older version of Qdrant first."
        ));
    }

    #[test]
    fn own_process_is_alive_and_not_qdrant() {
        let me = std::process::id();
        assert!(pid_alive(me));
        assert!(!is_qdrant_process(me));
        assert!(!pid_alive(u32::MAX - 1));
    }
}
