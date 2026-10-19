//! Каталог данных: структура подкаталогов и эксклюзивный лок экземпляра.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs4::{FileExt, TryLockError};
use zam_core::Id;

use crate::StoreError;

const SUBDIRS: [&str; 7] = [
    "users", "blobs", "previews", "models", "qdrant", "logs", "backups",
];

/// Каталог данных; пока значение живо, лок `.zam.lock` удерживается.
#[derive(Debug)]
pub struct DataDir {
    root: PathBuf,
    _lock: File,
}

/// Предел длины абсолютного пути каталога данных на Windows, символов UTF-16 (так считает
/// MAX_PATH = 260 с завершающим нулём). Rust и Qdrant (пути хранилища `\\?\`) работают с
/// длинными путями, но SQLite открывает файлы обычными путями Win32, а рабочий каталог
/// процесса Qdrant ограничен MAX_PATH всегда. Самый глубокий такой файл — журнал копии базы
/// перед миграцией `users\<uuid>.db.bak-v<N>-journal`, ещё 62 символа; модели по умолчанию
/// (`models\<id>\tokenizer.json`) — до 47. Запас — на номера миграций.
pub const MAX_DATA_DIR_CHARS_WINDOWS: usize = 190;

/// Windows: каталог данных не длиннее [`MAX_DATA_DIR_CHARS_WINDOWS`]; на Linux предела нет.
/// Проверка до создания каталогов: иначе ошибка всплыла бы позже, на открытии базы, как
/// «unable to open database file» без указания причины.
fn check_len(root: &Path) -> Result<(), StoreError> {
    if !cfg!(windows) {
        return Ok(());
    }
    let abs = std::path::absolute(root)?;
    let len = abs.as_os_str().to_string_lossy().encode_utf16().count();
    if len > MAX_DATA_DIR_CHARS_WINDOWS {
        return Err(StoreError::DataDirTooLong {
            path: abs,
            len,
            max: MAX_DATA_DIR_CHARS_WINDOWS,
        });
    }
    Ok(())
}

impl DataDir {
    /// Создаёт структуру каталогов и берёт эксклюзивный лок экземпляра.
    pub fn open(root: &Path) -> Result<DataDir, StoreError> {
        check_len(root)?;
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
        let _ = write!(file, "{}", std::process::id());
        let _ = file.flush();
        Ok(DataDir {
            root: root.to_path_buf(),
            _lock: file,
        })
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

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn qdrant(&self) -> PathBuf {
        self.root.join("qdrant")
    }
}
