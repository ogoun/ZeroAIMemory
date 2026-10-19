//! ML ZeroAIMemory (§8.4, исследование 05 §1–4):
//! - ONNX Runtime только через `load-dynamic` из файла рядом с бинарником ([`init_runtime`]);
//! - выбор устройства CPU / CUDA с откатом на CPU и причиной ([`DeviceReport`]);
//! - токенизатор Hugging Face, счётчик токенов без спецтокенов ([`BgeTokenCounter`]);
//! - эмбеддер ([`Embedder`]): bge-m3 (dense CLS + sparse-голова) и MiniLM (mean pooling).
//!
//! В логи не попадает текст заметок и чанков: только id моделей, устройства и время.

mod device;
mod embed;
mod models;
mod runtime;
mod sparse;
mod spec;
mod tokenize;

use std::path::PathBuf;
use std::time::Duration;

pub use device::{Device, DevicePref, DeviceReport};
pub use embed::{Embedder, Embedding};
pub use runtime::init_runtime;
pub use sparse::SparseHead;
pub use spec::{ModelSpec, Pooling};
pub use tokenize::BgeTokenCounter;

/// Ошибки zam-ml. `NoRuntime` и `NoModel` означают режим FTS-only (сервер стартует и сообщает причину).
#[derive(Debug, thiserror::Error)]
pub enum MlError {
    /// Библиотека ONNX Runtime не найдена, не загрузилась или не той версии.
    #[error("ONNX Runtime недоступен: {0}")]
    NoRuntime(String),
    /// Нет каталога модели или её файлов (`zam-server models fetch`).
    #[error("модель не загружена: {0}")]
    NoModel(String),
    /// Ошибка ONNX Runtime (создание сессии, инференс).
    #[error("ONNX Runtime: {0}")]
    Ort(String),
    /// Ошибка токенизатора.
    #[error("токенизатор: {0}")]
    Tokenizer(String),
    #[error("ввод-вывод: {0}")]
    Io(#[from] std::io::Error),
    /// Некорректный описатель модели (`model.toml`) или файл весов.
    #[error("описатель модели: {0}")]
    Spec(String),
}

impl From<ort::Error> for MlError {
    fn from(e: ort::Error) -> Self {
        MlError::Ort(e.to_string())
    }
}

/// Настройки ML (`[ml]` конфигурации сервера).
#[derive(Debug, Clone)]
pub struct MlConfig {
    /// `ml.device`: auto | cpu | cuda.
    pub device: DevicePref,
    /// Каталог моделей: `<models_dir>/<id>/{model.toml, model.onnx, tokenizer.json, …}`.
    pub models_dir: PathBuf,
    /// Путь к `onnxruntime.{dll,so}`; по умолчанию рядом с исполняемым файлом
    /// ([`MlConfig::default_ort_dylib`]). Библиотеки CUDA ищутся в `<каталог ORT>/cuda/`.
    pub ort_dylib: PathBuf,
    /// Потоки сессии индексации; 0 — авто (все физические ядра, решение ORT).
    pub index_threads: usize,
    /// Потоки сессии запросов (по умолчанию 2).
    pub query_threads: usize,
    /// Выгрузка моделей после простоя (`None` — выключено, по умолчанию). Применяет владелец
    /// [`Embedder`] (сервер), сам эмбеддер её не делает.
    pub unload_after: Option<Duration>,
}

impl MlConfig {
    /// Настройки по умолчанию: `auto`, ORT рядом с бинарником, потоки индексации — авто,
    /// 2 потока запросов, без выгрузки по простою.
    pub fn new(models_dir: PathBuf) -> Self {
        MlConfig {
            device: DevicePref::Auto,
            models_dir,
            ort_dylib: Self::default_ort_dylib(),
            index_threads: 0,
            query_threads: 2,
            unload_after: None,
        }
    }

    /// `<exe_dir>/onnxruntime.dll` (Windows) или `<exe_dir>/libonnxruntime.so` (Linux).
    pub fn default_ort_dylib() -> PathBuf {
        let name = format!(
            "{}onnxruntime{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        );
        match std::env::current_exe() {
            Ok(exe) => exe.parent().map_or(PathBuf::from(&name), |d| d.join(&name)),
            Err(_) => PathBuf::from(name),
        }
    }
}
