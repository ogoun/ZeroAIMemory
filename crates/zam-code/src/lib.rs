//! Карта кода ZeroAIMemory (спецификация M5.6, `docs/specs/2026-11-03-m5.6-code-map.md`).
//!
//! Без фич — чистые функции, общие для клиента и сервера: типы протокола ([`proto`]),
//! нормализация имён и разбор ссылки `путь#qual` ([`names`]). Фича `parse` — экстрактор символов
//! на tree-sitter ([`extract()`]); её включает только клиент `zam`, сервер C-код грамматик не тянет.

pub mod names;
pub mod proto;

#[cfg(feature = "parse")]
pub mod extract;

pub use names::{fts_terms, split_ident, split_ref};
pub use proto::{
    FileUpload, Lang, MISSING_HASH, ManifestItem, NAME_MAX_CHARS, ParseState, QUAL_MAX_CHARS,
    RefAgg, RefKind, SIG_MAX_CHARS, SkipReason, Symbol, SymbolHash, SymbolKind, UPLOAD_FILES_MAX,
    body_hash, lang_of, sig_line,
};

#[cfg(feature = "parse")]
pub use extract::{Extracted, Limits, extract, sniff, symbol_hashes};
