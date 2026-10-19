//! Гибридный поиск ZeroAIMemory (§9.2 ТЗ) — чистые функции без ввода-вывода.
//!
//! - [`hybrid`] — слияние каналов weighted RRF: Qdrant (`vector`, внутри — dense, sparse и
//!   сводки, уже слитые RRF в Qdrant), FTS5 `bm25` (`fts`) и FTS5 `trigram` (`trigram`);
//!   затухание по свежести и не больше `max_per_node` чанков с узла.
//! - [`adjust`] — поправки: затухание ([`recency_factor`]), замена сводки заметки или
//!   секции лучшим листом ([`note_to_leaf`], §9.2 п. 6).
//! - [`pack`] — упаковка в `budget_tokens` по числу токенов элементов.
//!
//! Сервер готовит входы (каналы — списки `chunk_id` по убыванию релевантности, метаданные
//! чанков из SQLite) и превращает результат в ответ. Векторный хит без метаданных
//! (узел удалён, в корзине, исключён или вне области в SQLite) в выдачу не попадает:
//! источник истины — SQLite, а точки в Qdrant могут остаться после неудачного удаления.

pub mod adjust;
pub mod hybrid;
pub mod pack;

pub use adjust::{RECENCY_FLOOR, note_to_leaf, recency_factor};
pub use hybrid::{Channel, ChunkMeta, Level, Ranked, SearchInputs, Weights, fuse};
pub use pack::{Cost, Fit, pack, truncate_to_tokens};
