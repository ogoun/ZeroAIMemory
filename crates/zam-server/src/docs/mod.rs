//! Документы (M5): разбор PDF в рабочем процессе `zam-server doc-worker` с лимитами памяти и
//! времени (спецификация M5 §2).
//!
//! - [`protocol`] — кадры stdin/stdout (длина `u32` BE + JSON) и типы запросов и ответов;
//! - [`worker`] — сам рабочий процесс (подкоманда, без tokio, журнал JSON в stderr);
//! - [`pool`] — пул процессов в сервере: [`DocPool`], сбои [`ParseFailure`].
//!
//! OOXML и таблицы разбираются в процессе сервера (`zam_docs::extract`, чистый Rust).

pub mod pool;
pub mod protocol;
pub mod worker;

pub use pool::{DocPool, ParseFailure};
