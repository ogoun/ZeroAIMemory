//! Каталог данных: структура подкаталогов и эксклюзивный лок экземпляра.
//!
//! **Метка лока (M6 §12б.3).** `.zam.lock` держит сервер (`serve`, `reindex`, `migrate`) или
//! `zam-server restore`; содержимое — pid, а у `restore` — `restore <pid>`. На Windows занятый
//! `LockFileEx` диапазон другим дескрипторам не читается, поэтому `restore` дублирует метку в
//! файле `.zam-restore` рядом; обычное открытие каталога его снимает. [`lock_mark`] отвечает,
//! кто держит лок, — по нему `backup` отказывает во время восстановления.
//!
//! **Незавершённое восстановление.** Перед первым изменением каталога данных `restore` пишет
//! [`RESTORE_INCOMPLETE_FILE`] и снимает его только в самом конце. Файл переживает сбой
//! (питание, `kill`): пока он есть, [`DataDir::open`] отказывает
//! ([`StoreError::RestoreIncomplete`]) — сервер не примет полувосстановленный каталог (иначе
//! создал бы пустую `system.db` и нового администратора), а `backup` не снимет его копию.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs4::{FileExt, TryLockError};
use zam_core::Id;

use crate::StoreError;

const SUBDIRS: [&str; 7] = [
    "users", "blobs", "previews", "models", "qdrant", "logs", "backups",
];

/// Файл-дубль метки `restore` (см. документацию модуля).
pub const RESTORE_MARK_FILE: &str = ".zam-restore";

/// Признак незавершённого восстановления (см. документацию модуля).
pub const RESTORE_INCOMPLETE_FILE: &str = ".zam-restore-incomplete";

/// Восстановление в каталог `root` начато и не завершено.
pub fn restore_incomplete(root: &Path) -> bool {
    root.join(RESTORE_INCOMPLETE_FILE).exists()
}

/// Кто держит `.zam.lock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMark {
    /// Сервер или другая команда под локом.
    Serve,
    /// Идёт `zam-server restore`.
    Restore,
}

/// Кто держит лок каталога данных `root`; `None` — лок свободен (или файла лока нет).
/// Проверка берёт лок на мгновение и сразу отпускает.
pub fn lock_mark(root: &Path) -> Option<LockMark> {
    let path = root.join(".zam.lock");
    let file = OpenOptions::new().read(true).open(&path).ok()?;
    match FileExt::try_lock(&file) {
        Ok(()) => {
            let _ = FileExt::unlock(&file);
            None
        }
        Err(_) => {
            // Unix: содержимое читается и под flock; Windows — нет (берётся дубль)
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            if text.trim_start().starts_with("restore") || root.join(RESTORE_MARK_FILE).exists() {
                Some(LockMark::Restore)
            } else {
                Some(LockMark::Serve)
            }
        }
    }
}

/// Лок каталога данных без создания подкаталогов (`backup` на остановленном сервере, в
/// каталоге, возможно, только для чтения): `Ok(None)` — лок занят или файл лока не открыть.
#[derive(Debug)]
pub struct DirLock {
    _file: File,
}

/// Берёт `.zam.lock` каталога `root`, если он свободен; содержимое не меняется.
pub fn try_lock_dir(root: &Path) -> Option<DirLock> {
    let path = root.join(".zam.lock");
    let file = OpenOptions::new()
        .read(true)
        .open(&path)
        .or_else(|_| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
        })
        .ok()?;
    FileExt::try_lock(&file).ok()?;
    Some(DirLock { _file: file })
}

/// Каталог данных; пока значение живо, лок `.zam.lock` удерживается.
#[derive(Debug)]
pub struct DataDir {
    root: PathBuf,
    _lock: File,
    /// Снять `.zam-restore` при уничтожении (открыт [`DataDir::open_for_restore`]).
    restore_mark: bool,
}

impl Drop for DataDir {
    fn drop(&mut self) {
        if self.restore_mark {
            let _ = std::fs::remove_file(self.root.join(RESTORE_MARK_FILE));
        }
    }
}

/// Предел длины абсолютного пути каталога данных на Windows, символов UTF-16 (так считает
/// MAX_PATH = 260 с завершающим нулём). Rust и Qdrant (пути хранилища `\\?\`) работают с
/// длинными путями, но SQLite открывает файлы обычными путями Win32, а рабочий каталог
/// процесса Qdrant ограничен MAX_PATH всегда. Самый глубокий такой файл — журнал копии базы
/// перед миграцией `users\<uuid>.db.bak-v<N>-journal`, ещё 62 символа; модели по умолчанию
/// (`models\<id>\tokenizer.json`) — до 47. Запас — на номера миграций.
pub const MAX_DATA_DIR_CHARS_WINDOWS: usize = 190;

/// Windows: каталог данных не длиннее [`MAX_DATA_DIR_CHARS_WINDOWS`]; на Linux предела нет.
/// Вызывается до любой работы с каталогом: в [`DataDir::open`] и в CLI до долгих загрузок
/// (`models fetch`, `cuda fetch`, `qdrant …`) — иначе ошибка всплыла бы позже, на открытии
/// базы, как «unable to open database file» без указания причины. Префикс `\\?\` не
/// считается: SQLite и рабочий каталог Qdrant получают путь и без него.
pub fn check_data_dir_len(root: &Path) -> Result<(), StoreError> {
    if !cfg!(windows) {
        return Ok(());
    }
    let abs = std::path::absolute(root)?;
    let len = win32_len(&abs.as_os_str().to_string_lossy());
    if len > MAX_DATA_DIR_CHARS_WINDOWS {
        return Err(StoreError::DataDirTooLong {
            path: abs,
            len,
            max: MAX_DATA_DIR_CHARS_WINDOWS,
        });
    }
    Ok(())
}

/// Длина пути Win32 в символах UTF-16 без префикса `\\?\` (`\\?\UNC\сервер` считается
/// как `\\сервер`).
fn win32_len(p: &str) -> usize {
    if let Some(rest) = p.strip_prefix(r"\\?\UNC\") {
        2 + rest.encode_utf16().count()
    } else {
        p.strip_prefix(r"\\?\").unwrap_or(p).encode_utf16().count()
    }
}

impl DataDir {
    /// Создаёт структуру каталогов и берёт эксклюзивный лок экземпляра.
    pub fn open(root: &Path) -> Result<DataDir, StoreError> {
        let dd = Self::open_marked(root, None)?;
        // дубль метки прерванного restore больше не нужен: лок теперь у этого процесса
        let _ = std::fs::remove_file(root.join(RESTORE_MARK_FILE));
        if restore_incomplete(root) {
            return Err(StoreError::RestoreIncomplete(root.to_path_buf()));
        }
        Ok(dd)
    }

    /// То же, что [`DataDir::open`], с меткой `restore` в `.zam.lock` и в `.zam-restore`
    /// ([`lock_mark`] — [`LockMark::Restore`]). Дубль снимается при уничтожении значения.
    pub fn open_for_restore(root: &Path) -> Result<DataDir, StoreError> {
        Self::open_marked(root, Some("restore"))
    }

    fn open_marked(root: &Path, mark: Option<&str>) -> Result<DataDir, StoreError> {
        check_data_dir_len(root)?;
        std::fs::create_dir_all(root)?;
        for s in SUBDIRS {
            std::fs::create_dir_all(root.join(s))?;
        }
        let lock_path = root.join(".zam.lock");
        // Без truncate: содержимое трогаем только после получения лока.
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        match FileExt::try_lock(&file) {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(StoreError::DataDirLocked(root.to_path_buf()));
            }
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
        // pid — только для диагностики; ошибка записи не критична.
        let _ = file.set_len(0);
        let _ = match mark {
            Some(m) => write!(file, "{m} {}", std::process::id()),
            None => write!(file, "{}", std::process::id()),
        };
        let _ = file.flush();
        let dd = DataDir {
            root: root.to_path_buf(),
            _lock: file,
            restore_mark: mark.is_some(),
        };
        if mark.is_some() {
            std::fs::write(
                root.join(RESTORE_MARK_FILE),
                format!(
                    "restore {}
",
                    std::process::id()
                ),
            )?;
        }
        Ok(dd)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn users_dir(&self) -> PathBuf {
        self.root.join("users")
    }

    pub fn system_db(&self) -> PathBuf {
        self.root.join("system.db")
    }

    pub fn user_db(&self, id: &Id) -> PathBuf {
        self.users_dir().join(format!("{id}.db"))
    }

    /// Блобы вложений: `<data_dir>/blobs/<user_id>/<aa>/<sha256>`.
    pub fn blobs_dir(&self) -> PathBuf {
        self.root.join("blobs")
    }

    /// Производные вложений (кэш превью, можно удалить): `<data_dir>/previews/<user_id>/<sha256>/…`.
    pub fn previews_dir(&self) -> PathBuf {
        self.root.join("previews")
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn qdrant(&self) -> PathBuf {
        self.root.join("qdrant")
    }
}

#[cfg(test)]
mod tests {
    use super::win32_len;

    #[test]
    fn verbatim_prefix_is_not_counted() {
        assert_eq!(win32_len(r"C:\d"), 4);
        assert_eq!(win32_len(r"\\?\C:\d"), 4);
        assert_eq!(win32_len(r"\\?\UNC\srv\share"), win32_len(r"\\srv\share"));
        assert_eq!(win32_len(r"C:\ж"), 4);
    }
}
