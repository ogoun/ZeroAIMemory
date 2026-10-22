//! Гибридный поиск ZeroAIMemory (§9.2 ТЗ) — чистые функции без ввода-вывода.
//!
//! - [`hybrid`] — слияние каналов weighted RRF: Qdrant (`vector`, внутри — dense, sparse и
//!   сводки, уже слитые RRF в Qdrant), FTS5 `bm25` (`fts`) и FTS5 `trigram` (`trigram`);
//!   затухание по свежести.
//! - [`adjust`] — поправки: затухание ([`recency_factor`]), бусты вида инсайта, надёжности и
//!   текущей области ([`apply_boosts`]), замена сводки заметки или
//!   секции лучшим листом ([`note_to_leaf`], §9.2 п. 6), затем не больше `max_per_node`
//!   элементов с узла ([`cap_per_node`]).
//! - [`pack`] — упаковка в `budget_tokens` по числу токенов элементов.
//!
//! Сервер готовит входы (каналы — списки `chunk_id` по убыванию релевантности, метаданные
//! чанков из SQLite) и превращает результат в ответ. Векторный хит без метаданных
//! (узел удалён, в корзине, исключён или вне области в SQLite) в выдачу не попадает:
//! источник истины — SQLite, а точки в Qdrant могут остаться после неудачного удаления.

pub mod adjust;
pub mod hybrid;
pub mod pack;

pub use adjust::{
    Boosts, LeafRef, RECENCY_FLOOR, apply_boosts, cap_per_node, drop_unresolved_tail, in_section,
    note_to_leaf, recency_factor, summaries_without_leaf,
};
pub use hybrid::{Channel, ChunkMeta, Level, Ranked, SearchInputs, Weights, fuse};
pub use pack::{Cost, Fit, pack, truncate_to_tokens};
