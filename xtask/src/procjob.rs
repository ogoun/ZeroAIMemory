//! Зачистка дочерних процессов xtask при его аварийном завершении (Ctrl+C, закрытое окно).
//!
//! Windows: процессы, которые запускает xtask (`zam-server`, через его Job Object — и
//! Qdrant), назначаются в Job Object с `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Его handle
//! держит только процесс xtask: когда xtask завершается любым способом, ядро закрывает
//! handle и завершает весь job. Без этого сервер в своей группе процессов (Ctrl+Break для
//! `bench --scenario first-start`) не получал бы Ctrl+C терминала и оставался бы жить с
//! занятым каталогом данных и портами. Вложенные job (у `zam-server` свой для Qdrant)
//! поддерживаются Windows 8+.
//!
//! Unix: Ctrl+C терминала получает вся группа процессов переднего плана, сервер
//! останавливается сам — назначать нечего.

use std::process::Child;

/// Назначает `child` в общий Job Object xtask (Windows); на Unix — ничего. Ошибка
/// назначения не фатальна: процесс работает, но может пережить аварийный выход xtask.
pub fn adopt(child: &Child) {
    #[cfg(windows)]
    if let Err(e) = imp::adopt(child) {
        eprintln!(
            "ВНИМАНИЕ: процесс {} не назначен в Job Object xtask: {e}",
            child.id()
        );
    }
    #[cfg(not(windows))]
    let _ = child;
}

#[cfg(windows)]
pub(crate) mod imp {
    use std::os::windows::io::AsRawHandle as _;
    use std::process::Child;
    use std::sync::OnceLock;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows::core::PCWSTR;

    /// Handle Job Object (как число: `HANDLE` не `Sync`). Не закрывается до выхода процесса.
    static JOB: OnceLock<Result<usize, String>> = OnceLock::new();

    /// Новый Job Object с `KILL_ON_JOB_CLOSE`.
    pub(crate) fn kill_on_close_job() -> Result<HANDLE, String> {
        // SAFETY: вызовы WinAPI с корректными аргументами; структура лимитов живёт до конца
        // вызова, размер передан точно.
        unsafe {
            let job = CreateJobObjectW(None, PCWSTR::null()).map_err(|e| e.to_string())?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if let Err(e) = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) {
                let _ = CloseHandle(job);
                return Err(e.to_string());
            }
            Ok(job)
        }
    }

    /// Назначает процесс `child` в job `job`.
    pub(crate) fn assign(job: HANDLE, child: &Child) -> Result<(), String> {
        // SAFETY: handle процесса принадлежит живому `Child`, handle job — открыт.
        unsafe { AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())) }
            .map_err(|e| e.to_string())
    }

    pub(super) fn adopt(child: &Child) -> Result<(), String> {
        let job = JOB
            .get_or_init(|| kill_on_close_job().map(|h| h.0 as usize))
            .clone()?;
        assign(HANDLE(job as *mut std::ffi::c_void), child)
    }

    #[cfg(test)]
    mod tests {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        use windows::Win32::Foundation::CloseHandle;

        use super::*;

        /// Закрытие handle job (как при выходе xtask) завершает назначенный процесс.
        #[test]
        fn job_kills_children_on_close() {
            let job = kill_on_close_job().unwrap();
            let mut child = Command::new("cmd")
                .args(["/c", "ping", "-n", "60", "127.0.0.1"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            assign(job, &child).unwrap();
            assert!(
                child.try_wait().unwrap().is_none(),
                "процесс должен работать"
            );
            // SAFETY: handle job открыт этим тестом и больше не используется.
            unsafe { CloseHandle(job) }.unwrap();
            let t = Instant::now();
            while child.try_wait().unwrap().is_none() {
                assert!(
                    t.elapsed() < Duration::from_secs(10),
                    "процесс пережил закрытие job"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}
