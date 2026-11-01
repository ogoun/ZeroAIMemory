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
pub use pool::{DocPool, ParseFailure};
pub use preview::{PreviewKey, Previews};
