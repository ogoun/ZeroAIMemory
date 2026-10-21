//! Зависимости поставки ZeroAIMemory по манифестам `deploy/*.toml`.
//!
//! Общая часть `cargo xtask fetch-deps|fetch-models` и команд сервера (`zam-server qdrant
//! fetch`, `models fetch|status`, `cuda fetch`): разбор манифестов с закреплёнными URL,
//! размерами и SHA-256 (`deploy/{qdrant,onnxruntime*,pdfium}.toml`, `models.toml`,
//! `cuda.toml`), загрузка (`ureq`, прокси из окружения, кроме локальной петли), проверка
//! размера и хэша, извлечение перечисленных файлов из zip/tar.gz/tar.xz без путей наружу
//! (в tar — ссылка на файл того же каталога, как SONAME в архивах NVIDIA), атомарная
//! замена каталога. Временные файлы — только рядом с назначением (`*.part`,
//! `<каталог>.tmp`; `*.part` прерванных загрузок удаляются при следующей), системный temp
//! не используется.

mod cuda;
mod download;
mod manifest;
mod models;

pub use cuda::{CudaLib, CudaManifest, install_cuda};
pub use download::{
    dest_names, download, extract_atomic, extract_selected, fetch_file, install_archive,
    is_installed, sha256_file, stamp_path, verify_sha256,
};
pub use manifest::{DepManifest, PLATFORMS, PlatformEntry, host_platform};
pub use models::{
    FetchedModel, MODEL_LICENSE_FILE, MODEL_SPEC_FILE, ManifestSpec, ModelDevice, ModelEntry,
    ModelFile, ModelRole, ModelState, ModelsManifest, Pooling, check_license_ack, fetch_model,
    fetch_model_with, model_state, spec_stale,
};

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
    /// Размер скачанного файла не совпал с манифестом.
    #[error("несовпадение размера {what}: ожидалось {expected} байт, получено {actual}")]
    SizeMismatch {
        what: String,
        expected: u64,
        actual: u64,
    },
    /// Модель с некоммерческой лицензией без явного согласия (`--accept-nc-license`).
    #[error(
        "модель '{id}' распространяется по лицензии {license} (только некоммерческое \
         использование) и не входит в комплект zam-models; для загрузки подтвердите согласие \
         с лицензией флагом --accept-nc-license"
    )]
    LicenseNotAccepted { id: String, license: String },
    /// Неверный манифест или архив (нет файла, не обычный файл, недопустимое имя).
    #[error("{0}")]
    Invalid(String),
}

impl FetchError {
    /// Та же ошибка с контекстом (компонент, модель и файл) — вариант сохраняется.
    pub(crate) fn context(self, ctx: &str) -> FetchError {
        match self {
            FetchError::Io { context, err } => FetchError::Io {
                context: format!("{ctx}: {context}"),
                err,
            },
            FetchError::Sha256Mismatch {
                what,
                expected,
                actual,
            } => FetchError::Sha256Mismatch {
                what: format!("{ctx}: {what}"),
                expected,
                actual,
            },
            FetchError::SizeMismatch {
                what,
                expected,
                actual,
            } => FetchError::SizeMismatch {
                what: format!("{ctx}: {what}"),
                expected,
                actual,
            },
            FetchError::Invalid(m) => FetchError::Invalid(format!("{ctx}: {m}")),
            // в URL уже видно, что качалось; согласие — о модели целиком
            e @ (FetchError::Http { .. } | FetchError::LicenseNotAccepted { .. }) => e,
        }
    }

    pub(crate) fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> FetchError {
        let context = context.into();
        move |err| FetchError::Io { context, err }
    }
}
