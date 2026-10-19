//! Зависимости поставки ZeroAIMemory по манифестам `deploy/*.toml`.
//!
//! Общая часть `cargo xtask fetch-deps` и команд сервера (`zam-server qdrant fetch`;
//! далее `models fetch` и `cuda fetch`): разбор манифеста с закреплёнными URL и SHA-256,
//! загрузка (`ureq`, прокси из окружения, кроме локальной петли), проверка хэша,
//! извлечение перечисленных файлов из zip/tar.gz без симлинков и путей наружу,
//! атомарная замена каталога. Временные файлы — только рядом с назначением
//! (`*.part`, `<каталог>.tmp`), системный temp не используется.

mod download;
mod manifest;

pub use download::{
    dest_names, download, extract_atomic, extract_selected, fetch_file, install_archive,
    sha256_file, verify_sha256,
};
pub use manifest::{DepManifest, PLATFORMS, PlatformEntry, host_platform};

/// Ошибки загрузки и манифестов. Тексты на русском, без содержимого файлов.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// Ошибка ввода-вывода с указанием действия и пути.
    #[error("{context}: {err}")]
    Io {
        context: String,
        err: std::io::Error,
    },
    /// Сетевая ошибка или HTTP-статус не 2xx.
    #[error("загрузка {url}: {message}")]
    Http { url: String, message: String },
    /// Хэш скачанного или существующего файла не совпал с манифестом.
    #[error("несовпадение SHA-256 для {what}: ожидалось {expected}, получено {actual}")]
    Sha256Mismatch {
        what: String,
        expected: String,
        actual: String,
    },
    /// Неверный манифест или архив (нет файла, не обычный файл, недопустимое имя).
    #[error("{0}")]
    Invalid(String),
}

impl FetchError {
    pub(crate) fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> FetchError {
        let context = context.into();
        move |err| FetchError::Io { context, err }
    }
}
