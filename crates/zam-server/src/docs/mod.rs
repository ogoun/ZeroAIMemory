//! Документы (M5): разбор в рабочем процессе `zam-server doc-worker` с лимитами памяти и
//! времени (спецификация M5 §2) и конвейер индексации вложений (§3).
//!
//! - [`protocol`] — кадры stdin/stdout (длина `u32` BE + JSON) и типы запросов и ответов;
//! - [`worker`] — сам рабочий процесс (подкоманда, без tokio, журнал JSON в stderr);
//! - [`pool`] — пул процессов в сервере: [`DocPool`], сбои [`ParseFailure`];
//! - [`indexer`] — конвейер индексации вложений [`DocIndexer`]: извлечение, кэш, чанки;
//! - [`preview`] — превью вложений [`Previews`]: свой пул процессов и кэш `previews/`.
//!
//! Все виды документов (не только PDF) разбираются в рабочем процессе — обоснование в
//! [`indexer`]; `zam_docs::extract` в процессе сервера не вызывается.

pub mod indexer;
pub mod pool;
pub mod preview;
pub mod protocol;
pub mod worker;

pub use indexer::{DocIndexer, DocsStatus};

/// Отпечаток лимитов разбора (8 hex blake3 их `Debug`): ключ годности результата, обрезанного
/// лимитом, — кэша извлечения `blob_extract.limits_fp` и файлов превью.
pub fn limits_fingerprint(limits: &zam_docs::Limits) -> String {
    blake3::hash(format!("{limits:?}").as_bytes()).to_hex()[..8].to_string()
}
pub use pool::{DocPool, ParseFailure};
pub use preview::{PreviewKey, Previews};
