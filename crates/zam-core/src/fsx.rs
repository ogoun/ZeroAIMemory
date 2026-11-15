//! Файловые операции, общие для сервера и клиента.
use std::path::Path;
use std::time::Duration;

/// Суммарное ожидание повторов [`rename_retry`].
pub const RENAME_RETRY_TOTAL: Duration = Duration::from_secs(10);

/// `rename` с повтором на Windows: антивирус и индексатор ненадолго держат только что
/// записанные файлы (`ERROR_ACCESS_DENIED` 5, `ERROR_SHARING_VIOLATION` 32). Паузы растут
/// вдвое от 50 мс, пока сумма не превысит [`RENAME_RETRY_TOTAL`]; перед каждой паузой —
/// `on_retry(попытка, код ошибки, пауза)` (журнал вызывающего). На других ОС — одна попытка.
pub fn rename_retry(
    from: &Path,
    to: &Path,
    mut on_retry: impl FnMut(u32, i32, Duration),
) -> std::io::Result<()> {
    let (mut attempt, mut pause, mut waited) = (1u32, Duration::from_millis(50), Duration::ZERO);
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e)
                if cfg!(windows)
                    && matches!(e.raw_os_error(), Some(5 | 32))
                    && waited + pause <= RENAME_RETRY_TOTAL =>
            {
                on_retry(attempt, e.raw_os_error().unwrap_or_default(), pause);
                std::thread::sleep(pause);
                waited += pause;
                pause *= 2;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}
