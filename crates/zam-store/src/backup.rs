//! Копия баз без остановки сервера (M6 §12б.2): `VACUUM INTO` на соединении, которое не
//! мешает писателю, признак копии `.zam-backup` для потока контрольных точек [`crate::Db`],
//! проверка копии и чтение служебных значений закрытого файла базы.
//!
//! **Режимы открытия источника** ([`OpenMode`]):
//! - [`OpenMode::QueryOnly`] — `READ_WRITE` без записи данных: соединение может создать `-shm`
//!   закрытой базы (у `SQLITE_OPEN_READONLY` в каталоге без прав записи — `CANTOPEN`), а
//!   писателю параллельного сервера не мешает — `VACUUM INTO` берёт на источнике только
//!   транзакцию чтения. `PRAGMA query_only = 1` спецификации не ставится: SQLite с ним
//!   отказывает и в записи файла `INTO` («attempt to write a readonly database»; проверено
//!   тестом `vacuum_into_query_only_and_immutable`) **[решение исполнителя 18a]**. Соединение
//!   выполняет только `VACUUM INTO` и закрывается;
//! - [`OpenMode::Immutable`] — `immutable=1`: SQLite не смотрит ни на `-wal`, ни на блокировки.
//!   Только под **своим** `.zam.lock` (сервер не запущен и не запустится до конца копии): иначе
//!   сервер мог бы лениво открыть ту же базу, и копия прочла бы несогласованный файл.
//!
//! **Признак копии** — файл `<data_dir>/.zam-backup` (`{pid, started_at}` — для человека).
//! `backup` держит на нём исключительный лок ОС (fs4: `flock`/`LockFileEx`) до конца копии
//! ([`hold_backup_flag`]). Поток контрольных точек базы раз в свой интервал проверяет признак
//! ([`backup_flag_active`]); пока он стоит, оба места `wal_checkpoint(RESTART)` делают только
//! `PASSIVE` (см. `db.rs`). Живость — по локу, а не по `pid`: лок снимает ОС при любой смерти
//! процесса, а `pid` мог бы достаться другому процессу. Брошенный признак — файл без
//! удерживающего процесса (`backup` убит) или старше [`backup_flag_max_age`] (по времени
//! изменения файла) — игнорируется с WARN «брошенный признак копии».

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use fs4::FileExt;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::StoreError;

/// Имя файла признака копии в каталоге данных.
pub const BACKUP_FLAG_FILE: &str = ".zam-backup";

/// Срок годности признака копии по умолчанию (`backup.flag_max_hours` = 6).
pub const BACKUP_FLAG_MAX_AGE_DEFAULT: Duration = Duration::from_secs(6 * 3600);

/// Признак моложе этого считается действующим и без лока: `backup` создал файл и ещё не
/// взял лок (или проверка на миг взяла лок раньше него).
const FLAG_GRACE: Duration = Duration::from_secs(10);

/// Срок годности признака процесса, секунды ([`set_backup_flag_max_age`]).
static FLAG_MAX_AGE_SECS: AtomicU64 = AtomicU64::new(BACKUP_FLAG_MAX_AGE_DEFAULT.as_secs());

/// Задаёт срок годности признака копии для потоков контрольных точек процесса
/// (`backup.flag_max_hours` конфигурации сервера; до первого открытия баз).
pub fn set_backup_flag_max_age(d: Duration) {
    FLAG_MAX_AGE_SECS.store(d.as_secs().max(1), Ordering::Relaxed);
}

/// Срок годности признака копии процесса.
pub fn backup_flag_max_age() -> Duration {
    Duration::from_secs(FLAG_MAX_AGE_SECS.load(Ordering::Relaxed))
}

/// Как открывать источник копии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    /// `READ_WRITE` без записи данных (только `VACUUM INTO`): может создать `-shm`, писателю не
    /// мешает.
    QueryOnly,
    /// `immutable=1`: только под своим `.zam.lock`.
    Immutable,
}

/// Содержимое признака копии `.zam-backup` (справочно; живость — по локу).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupFlag {
    pub pid: u32,
    /// UTC, формат `crate::time::fmt_ts`.
    pub started_at: String,
}

/// Путь признака копии каталога данных.
pub fn backup_flag_path(data_dir: &Path) -> PathBuf {
    data_dir.join(BACKUP_FLAG_FILE)
}

/// Удерживаемый признак копии: файл и лок снимаются при уничтожении (при любом исходе копии;
/// при смерти процесса лок снимает ОС).
#[derive(Debug)]
pub struct BackupFlagHold {
    path: PathBuf,
    file: Option<File>,
}

impl Drop for BackupFlagHold {
    fn drop(&mut self) {
        // файл удаляется под локом: проверка между удалением и снятием лока видит «нет файла»
        let _ = std::fs::remove_file(&self.path);
        if let Some(f) = self.file.take() {
            let _ = FileExt::unlock(&f);
        }
    }
}

/// Ставит признак копии каталога данных `data_dir` и держит его лок до уничтожения результата.
pub fn hold_backup_flag(data_dir: &Path) -> std::io::Result<BackupFlagHold> {
    let path = backup_flag_path(data_dir);
    let mut file = File::create(&path)?;
    // блокирующий лок: проверка признака держит разделяемый лок лишь мгновение
    FileExt::lock(&file)?;
    let body = serde_json::to_string(&BackupFlag {
        pid: std::process::id(),
        started_at: crate::time::fmt_ts(crate::time::now()),
    })
    .map_err(std::io::Error::other)?;
    file.write_all(body.as_bytes())?;
    file.flush()?;
    Ok(BackupFlagHold {
        path,
        file: Some(file),
    })
}

/// Состояние признака копии.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FlagState {
    Absent,
    Active,
    /// Брошенный признак: причина для WARN.
    Stale(String),
}

/// Проверяет признак копии `flag` (полный путь файла).
pub(crate) fn flag_state(flag: &Path, max_age: Duration) -> FlagState {
    let meta = match std::fs::metadata(flag) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return FlagState::Absent,
        // не прочитать — считаем копию идущей: писатель лишь откладывает RESTART
        Err(_) => return FlagState::Active,
    };
    let age = meta
        .modified()
        .ok()
        .and_then(|t| t.elapsed().ok())
        .unwrap_or_default();
    if age > max_age {
        return FlagState::Stale(format!("признак старше {} ч", max_age.as_secs() / 3600));
    }
    let Ok(f) = File::open(flag) else {
        return FlagState::Active;
    };
    match FileExt::try_lock_shared(&f) {
        Ok(()) => {
            let _ = FileExt::unlock(&f);
            if age < FLAG_GRACE {
                FlagState::Active
            } else {
                FlagState::Stale("признак не удерживается процессом копии (копия прервана)".into())
            }
        }
        Err(_) => FlagState::Active,
    }
}

/// Последний брошенный признак, о котором уже предупредили (один WARN на признак, а не на
/// каждую базу и тик).
static WARNED: Mutex<Option<String>> = Mutex::new(None);

pub(crate) fn warn_stale(flag: &Path, why: &str) {
    let key = format!("{}|{why}", flag.display());
    let mut w = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    if w.as_deref() != Some(key.as_str()) {
        tracing::warn!(path = %flag.display(), reason = why, "брошенный признак копии: игнорируется");
        *w = Some(key);
    }
}

/// Идёт ли копия каталога данных: признак есть, его лок держит процесс копии и признак не
/// старше `max_age`. Брошенный признак — `false` и WARN «брошенный признак копии».
pub fn backup_flag_active(data_dir: &Path, max_age: Duration) -> bool {
    let flag = backup_flag_path(data_dir);
    match flag_state(&flag, max_age) {
        FlagState::Absent => false,
        FlagState::Active => true,
        FlagState::Stale(why) => {
            warn_stale(&flag, &why);
            false
        }
    }
}

/// Ожидание блокировки соединением копии: писатель держит блокировку записи коротко, а
/// `VACUUM INTO` берёт только транзакцию чтения.
const COPY_BUSY: Duration = Duration::from_secs(30);

/// Соединение-источник копии в режиме `mode`.
fn open_source(src: &Path, mode: OpenMode) -> Result<Connection, StoreError> {
    let c = match mode {
        OpenMode::QueryOnly => Connection::open_with_flags(
            src,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?,
        OpenMode::Immutable => Connection::open_with_flags(
            immutable_uri(src),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?,
    };
    c.busy_timeout(COPY_BUSY)?;
    Ok(c)
}

/// `file:<путь>?immutable=1` с процент-кодированием (`%`, `?`, `#`, пробелы, не-ASCII).
fn immutable_uri(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file:");
    // абсолютный путь Windows `C:/…` — как `file:/C:/…`
    if s.as_bytes().get(1) == Some(&b':') {
        out.push('/');
    }
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out.push_str("?immutable=1");
    out
}

/// Согласованный снимок базы `src` в новый файл `dst` (`VACUUM INTO`, одна транзакция
/// чтения). `dst` не должен существовать. Соединение `QueryOnly` писателю не мешает: на
/// источнике — только транзакция чтения.
pub fn vacuum_into(src: &Path, dst: &Path, mode: OpenMode) -> Result<(), StoreError> {
    let c = open_source(src, mode)?;
    // путь — параметром, а не в тексте SQL (кавычки в путях)
    c.execute("VACUUM INTO ?1", [dst.to_string_lossy().as_ref()])?;
    Ok(())
}

/// Соединение только для чтения на закрытом файле базы (копия, файл из описи).
fn open_closed(path: &Path) -> Result<Connection, StoreError> {
    Ok(Connection::open_with_flags(
        immutable_uri(path),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?)
}

/// Проверка закрытого файла базы (копии): `(user_version, integrity_check и
/// foreign_key_check без замечаний)`.
pub fn integrity_ok(path: &Path) -> Result<(u32, bool), StoreError> {
    let c = open_closed(path)?;
    let version: u32 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let integrity: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    let fk_bad = c
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .next()?
        .is_some();
    Ok((version, integrity == "ok" && !fk_bad))
}

/// Значение `meta.<key>` закрытого файла базы пользователя (`meta.chunker*`); таблицы `meta`
/// нет — `None`.
pub fn meta_value(path: &Path, key: &str) -> Result<Option<String>, StoreError> {
    let c = open_closed(path)?;
    let has_meta: bool = c.query_row(
        "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
        [],
        |r| r.get(0),
    )?;
    if !has_meta {
        return Ok(None);
    }
    Ok(
        c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?,
    )
}

/// Строки одного столбца запроса `sql` закрытого файла базы (список пользователей копии
/// `system.db`, блобы и `model_id` копии базы пользователя). Таблицы нет — пусто.
pub fn query_strings(path: &Path, table: &str, sql: &str) -> Result<Vec<String>, StoreError> {
    let c = open_closed(path)?;
    let has: bool = c.query_row(
        "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get(0),
    )?;
    if !has {
        return Ok(Vec::new());
    }
    let mut st = c.prepare(sql)?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Блобы копии базы пользователя: `(sha256, size)`.
pub fn blob_list(path: &Path) -> Result<Vec<(String, u64)>, StoreError> {
    let c = open_closed(path)?;
    let has: bool = c.query_row(
        "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'blobs'",
        [],
        |r| r.get(0),
    )?;
    if !has {
        return Ok(Vec::new());
    }
    let mut st = c.prepare("SELECT sha256, size FROM blobs ORDER BY sha256")?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Отпечатки нарезки закрытой базы пользователя (`meta.chunker`, `meta.chunker_entries`,
/// `meta.chunker_attachments`) — для описи копии.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunkers {
    pub chunker: Option<String>,
    pub chunker_entries: Option<String>,
    pub chunker_attachments: Option<String>,
}

/// Отпечатки нарезки закрытого файла базы пользователя.
pub fn chunkers_of(path: &Path) -> Result<Chunkers, StoreError> {
    Ok(Chunkers {
        chunker: meta_value(path, "chunker")?,
        chunker_entries: meta_value(path, "chunker_entries")?,
        chunker_attachments: meta_value(path, crate::CHUNKER_ATTACHMENTS_KEY)?,
    })
}

/// Части закрытой базы пользователя, которые текущий процесс перенарежет при открытии
/// (`notes`, `entries`, `attachments`): отпечаток расходится с нарезкой процесса
/// (`set_chunk_params`, счётчик токенов — как у сервера) и в части есть живые цели. Второе —
/// число живых чанков разошедшихся частей (столько векторов посчитается заново).
pub fn stale_chunkers(path: &Path) -> Result<(Vec<&'static str>, u64), StoreError> {
    let ch = chunkers_of(path)?;
    let exact = crate::nodes::token_counter_exact();
    let c = open_closed(path)?;
    let count = |sql: &str| -> u64 {
        c.query_row(sql, [], |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as u64)
            .unwrap_or(0)
    };
    let parts: [(&'static str, Option<String>, String, &str, &str); 3] = [
        (
            "notes",
            ch.chunker,
            crate::nodes::chunker_fingerprint(),
            "SELECT count(*) FROM nodes WHERE kind = 'note' AND deleted_at IS NULL",
            "SELECT count(*) FROM chunks
             WHERE entry_id IS NULL AND attachment_id IS NULL AND level <> 'folder'",
        ),
        (
            "entries",
            ch.chunker_entries,
            crate::nodes::entry_chunker_fingerprint(),
            "SELECT count(*) FROM entries WHERE deleted_at IS NULL",
            "SELECT count(*) FROM chunks WHERE entry_id IS NOT NULL",
        ),
        (
            "attachments",
            ch.chunker_attachments,
            crate::attachments::attachment_chunker_fingerprint(),
            "SELECT count(*) FROM attachment_docs WHERE deleted_at IS NULL",
            "SELECT count(*) FROM chunks WHERE attachment_id IS NOT NULL",
        ),
    ];
    let mut stale = Vec::new();
    let mut chunks = 0;
    for (name, stored, current, live, chunk_sql) in parts {
        if crate::nodes::same_chunker(stored.as_deref(), &current, exact) || count(live) == 0 {
            continue;
        }
        stale.push(name);
        chunks += count(chunk_sql);
    }
    Ok((stale, chunks))
}

/// Число чанков закрытой базы пользователя всех видов (заметки, записи, вложения) — столько
/// векторов посчитается заново при другой модели.
pub fn chunk_count(path: &Path) -> Result<u64, StoreError> {
    let c = open_closed(path)?;
    Ok(c.query_row(
        "SELECT count(*) FROM chunks WHERE level <> 'folder'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|v| v.max(0) as u64)
    .unwrap_or(0))
}

/// Версия схемы (`user_version`) из заголовка файла SQLite: 4 байта big-endian по смещению
/// 60. Не SQLite — `Corrupt`.
pub fn header_user_version(head: &[u8]) -> Result<u32, StoreError> {
    if head.len() < 100 || !head.starts_with(b"SQLite format 3\0") {
        return Err(StoreError::Corrupt("не файл базы SQLite".into()));
    }
    Ok(u32::from_be_bytes([head[60], head[61], head[62], head[63]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immutable_uri_escapes() {
        assert_eq!(
            immutable_uri(Path::new("/srv/z am/a#b.db")),
            "file:/srv/z%20am/a%23b.db?immutable=1"
        );
        assert_eq!(
            immutable_uri(Path::new(r"C:\d\u.db")),
            "file:/C:/d/u.db?immutable=1"
        );
    }
}
