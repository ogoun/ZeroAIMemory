//! Карта кода ZeroAIMemory (спецификация M5.6, `docs/specs/2026-11-03-m5.6-code-map.md`).
//!
//! Без фич — чистые функции, общие для клиента и сервера: типы протокола ([`proto`]),
//! нормализация имён и разбор ссылки `путь#qual` ([`names`]), ранжирование PageRank
//! ([`rank`](mod@rank)) и упаковка карты в бюджет токенов ([`pack`]). Фича `parse` — экстрактор
//! символов на tree-sitter ([`extract()`]); её включает только клиент `zam`, сервер C-код
//! грамматик не тянет.

pub mod names;
pub mod pack;
pub mod proto;
pub mod rank;

#[cfg(feature = "parse")]
pub mod extract;

pub use names::{fts_terms, split_ident, split_ref};
pub use pack::{MapFile, MapSym, Packed, map_line, render_map};
pub use proto::{
    FileUpload, Lang, MISSING_HASH, ManifestItem, NAME_MAX_CHARS, ParseState, QUAL_MAX_CHARS,
    RefAgg, RefKind, SIG_MAX_CHARS, SkipReason, Symbol, SymbolHash, SymbolKind, UPLOAD_FILES_MAX,
    body_hash, lang_of, sig_line,
};
pub use rank::{
    Focus, Graph, RankFile, Ranked, central, edges_among, focus_idents, focus_restart, ident_mult,
    pagerank, rank, rank_on_graph,
};

#[cfg(feature = "parse")]
pub use extract::{Extracted, Limits, extract, sniff, symbol_hashes};
