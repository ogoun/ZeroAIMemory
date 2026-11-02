//! Общие функции запуска дочерних процессов сервера: Qdrant ([`crate::supervisor`]) и рабочего
//! процесса разбора документов `zam-server doc-worker` (M5 §2).
//!
//! - окружение: секреты сервера ([`SECRET_ENV`]) дочерним процессам не передаются;
//! - Linux: [`pre_exec_hook`] — `PR_SET_PDEATHSIG` (`SIGKILL`) со сверкой `getppid()` и, по
//!   желанию, `RLIMIT_AS`. Сигнал приходит при смерти **потока**, сделавшего `fork`, поэтому
//!   запускать дочерний процесс нужно из долгоживущего потока;
//! - Windows: [`KillJob`] — свой Job Object с `KILL_ON_JOB_CLOSE` и, по желанию, лимитом памяти
//!   процесса (`JOB_OBJECT_LIMIT_PROCESS_MEMORY`); `process-wrap` лимита памяти не умеет;
//! - журнал: [`parse_json_line`] — строка JSON-журнала `tracing` дочернего процесса.

/// Секреты zam-server, которые дочерним процессам не передаются.
pub const SECRET_ENV: [&str; 3] = ["ZAM_MASTER_KEY", "ZAM_ADMIN_PASSWORD", "ZAM_TOKEN"];

/// Переменная — секрет сервера ([`SECRET_ENV`]); регистр имени на Windows не важен.
pub fn is_secret_env(name: &str) -> bool {
    if cfg!(windows) {
        let upper = name.to_ascii_uppercase();
        SECRET_ENV.contains(&upper.as_str())
    } else {
        SECRET_ENV.contains(&name)
    }
}

/// Строка JSON-журнала дочернего процесса.
#[derive(Debug, PartialEq)]
pub struct LogLine {
    pub level: tracing::Level,
    pub target: String,
    pub message: String,
}

/// Разбор строки JSON-журнала `tracing-subscriber` (у Qdrant — `logger.format: json`):
/// `{"timestamp":…,"level":"INFO","fields":{"message":…},"target":…}`.
pub fn parse_json_line(line: &str) -> Option<LogLine> {
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

/// Замыкание для `pre_exec` (Linux): `PR_SET_PDEATHSIG` = `SIGKILL`, выход, если родитель
/// `parent` умер до `prctl` (сигнал уже не придёт), и лимит адресного пространства
/// `address_space` байт (`RLIMIT_AS`), если задан, и `PR_SET_NO_NEW_PRIVS`, если `no_new_privs`
/// (процесс и его потомки не получат привилегий через setuid/setgid/capabilities файлов при
/// `exec`; рабочий процесс разбора — финальное ревью M5, I-3).
///
/// Исполняется в дочернем процессе между `fork` и `exec` и вызывает только async-signal-safe
/// функции (`prctl`, `getppid`, `setrlimit`, `_exit`). Сигнал привязан к потоку, сделавшему
/// `fork`: запускать из долгоживущего потока.
#[cfg(unix)]
pub fn pre_exec_hook(
    parent: libc::pid_t,
    address_space: Option<u64>,
    no_new_privs: bool,
) -> impl FnMut() -> std::io::Result<()> + Send + Sync + 'static {
    move || {
        // SAFETY: вызовы libc с корректными аргументами; структура лимита — на стеке.
        unsafe {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if no_new_privs && libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Родитель умер до prctl — сигнал уже не придёт.
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            if let Some(bytes) = address_space {
                let lim = libc::rlimit {
                    rlim_cur: bytes as libc::rlim_t,
                    rlim_max: bytes as libc::rlim_t,
                };
                if libc::setrlimit(libc::RLIMIT_AS, &lim) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
        }
        Ok(())
    }
}

/// Пиковый объём виртуальной памяти процесса (`VmPeak` из `/proc/<pid>/status`), байт.
///
/// У завершившегося процесса (зомби) поля нет — `None`.
#[cfg(unix)]
pub fn vm_peak_bytes(pid: u32) -> Option<u64> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = s.lines().find(|l| l.starts_with("VmPeak:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

#[cfg(windows)]
pub use win::KillJob;

#[cfg(windows)]
mod win {
    use std::os::windows::io::AsRawHandle as _;
    use std::process::Child;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    };
    use windows::core::PCWSTR;

    /// Job Object с `KILL_ON_JOB_CLOSE` и необязательным лимитом памяти процесса.
    ///
    /// Handle держит только этот процесс: при его завершении любым способом ядро закрывает
    /// handle и завершает процессы job. Drop закрывает handle — процессы job завершаются.
    pub struct KillJob(HANDLE);

    // SAFETY: handle job — объект ядра; вызовы с ним потокобезопасны.
    unsafe impl Send for KillJob {}
    // SAFETY: то же; методы не меняют состояние структуры.
    unsafe impl Sync for KillJob {}

    impl KillJob {
        /// Новый job; `process_memory` — лимит выделенной (commit) памяти процесса, байт;
        /// `active_processes` — сколько процессов может быть в job одновременно
        /// (`JOB_OBJECT_LIMIT_ACTIVE_PROCESS`): `Some(1)` — процесс job не запустит дочерних
        /// (`CreateProcess` откажет) и при захвате.
        pub fn new(
            process_memory: Option<u64>,
            active_processes: Option<u32>,
        ) -> Result<Self, String> {
            // SAFETY: вызовы WinAPI с корректными аргументами; структура лимитов живёт до
            // конца вызова, размер передан точно.
            unsafe {
                let job = CreateJobObjectW(None, PCWSTR::null()).map_err(|e| e.to_string())?;
                let job = KillJob(job);
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if let Some(bytes) = process_memory {
                    info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
                    info.ProcessMemoryLimit = usize::try_from(bytes).unwrap_or(usize::MAX);
                }
                if let Some(n) = active_processes {
                    info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
                    info.BasicLimitInformation.ActiveProcessLimit = n;
                }
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&info).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
                .map_err(|e| e.to_string())?;
                Ok(job)
            }
        }

        /// Назначает процесс `child` в job.
        pub fn assign(&self, child: &Child) -> Result<(), String> {
            // SAFETY: handle процесса принадлежит живому `Child`, handle job — открыт.
            unsafe { AssignProcessToJobObject(self.0, HANDLE(child.as_raw_handle())) }
                .map_err(|e| e.to_string())
        }

        /// Пик выделенной памяти одного процесса job (`PeakProcessMemoryUsed`), байт.
        pub fn peak_process_memory(&self) -> Option<u64> {
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            // SAFETY: буфер нужного размера на стеке, handle job открыт.
            unsafe {
                QueryInformationJobObject(
                    Some(self.0),
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_mut(&mut info).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    None,
                )
            }
            .ok()?;
            Some(info.PeakProcessMemoryUsed as u64)
        }

        /// Завершает все процессы job с кодом `code`.
        pub fn terminate(&self, code: u32) {
            // SAFETY: handle job открыт.
            let _ = unsafe { TerminateJobObject(self.0, code) };
        }
    }

    impl Drop for KillJob {
        fn drop(&mut self) {
            // SAFETY: handle получен из CreateJobObjectW и закрывается один раз.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_env_names() {
        for k in SECRET_ENV {
            assert!(is_secret_env(k), "{k}");
        }
        for k in ["PATH", "ZAM_URL", "QDRANT__SERVICE__HOST"] {
            assert!(!is_secret_env(k), "{k}");
        }
        assert_eq!(is_secret_env("zam_token"), cfg!(windows));
    }

    #[cfg(windows)]
    #[test]
    fn kill_job_limits_memory_and_kills_on_close() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let job = KillJob::new(Some(64 * 1024 * 1024), None).unwrap();
        // `pause` — встроенная команда: внуков, запущенных до назначения в job, нет
        let mut child = Command::new("cmd")
            .args(["/C", "pause"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        job.assign(&child).unwrap();
        assert!(job.peak_process_memory().is_some());
        drop(job);
        let t = Instant::now();
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(t.elapsed() < Duration::from_secs(10), "процесс пережил job");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Предел одного процесса: процесс job не запускает дочерних, а сам работает.
    #[cfg(windows)]
    #[test]
    fn kill_job_single_process_blocks_children() {
        use std::io::{Read as _, Write as _};
        use std::process::{Command, Stdio};

        let run = |limit: Option<u32>| {
            let job = KillJob::new(None, limit).unwrap();
            let mut child = Command::new("cmd")
                .args(["/Q", "/K"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            job.assign(&child).unwrap();
            // команды — после назначения в job: внешний `cmd /c exit 7` — дочерний процесс
            let mut stdin = child.stdin.take().unwrap();
            stdin
                .write_all(b"cmd /c exit 7\r\necho rc=%errorlevel%\r\nexit\r\n")
                .unwrap();
            drop(stdin);
            let mut out = String::new();
            child
                .stdout
                .take()
                .unwrap()
                .read_to_string(&mut out)
                .unwrap();
            child.wait().unwrap();
            out
        };
        assert!(
            run(None).contains("rc=7"),
            "без предела дочерний процесс запускается"
        );
        let limited = run(Some(1));
        assert!(limited.contains("rc="), "{limited}");
        assert!(!limited.contains("rc=7"), "{limited}");
    }

    /// `no_new_privs` выставляется в дочернем процессе (`/proc/self/status`).
    #[cfg(target_os = "linux")]
    #[test]
    fn pre_exec_sets_no_new_privs() {
        use std::os::unix::process::CommandExt as _;

        let status = |nnp: bool| {
            let mut cmd = std::process::Command::new("cat");
            cmd.arg("/proc/self/status");
            // SAFETY: замыкание вызывает только async-signal-safe функции
            unsafe {
                cmd.pre_exec(pre_exec_hook(std::process::id() as _, None, nnp));
            }
            let out = String::from_utf8(cmd.output().unwrap().stdout).unwrap();
            out.lines()
                .find(|l| l.starts_with("NoNewPrivs:"))
                .and_then(|l| l.split_whitespace().nth(1).map(str::to_string))
        };
        assert_eq!(status(true).as_deref(), Some("1"));
    }
}
