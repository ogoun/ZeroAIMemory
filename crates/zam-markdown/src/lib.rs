//! Разбор Markdown ZeroAIMemory на comrak: байтовые позиции, заголовки,
//! секции, ссылки, frontmatter, санитизированный HTML, а также чанкер §8.2,
//! сегментация на предложения и уровни L1 (§8.3).
//!
//! Единственное место в проекте, которое разбирает Markdown (ТЗ §5.3).
//! Все диапазоны — байтовые оффсеты в исходной строке; исходник никогда
//! не сериализуется обратно из AST: PATCH — байтовый splice исходника.

mod boundary;
mod chunk;
mod doc;
mod frontmatter;
mod highlight;
mod levels;
pub mod options;
mod patch;
mod pos;
mod render;
mod sanitize;
mod sentences;

pub use chunk::{
    ApproxCounter, ChunkDraft, ChunkLevel, ChunkParams, ESTIMATE_C, ESTIMATE_M, EstimateCounter,
    TokenCounter, chunk_note, split_plain,
};
pub use doc::{
    Doc, Frontmatter, Heading, Link, LinkKind, OutlineItem, Section, SectionLookup,
    first_paragraph, outline, parse, resolve_section, section_by_path, slug,
};
pub use frontmatter::{VERIFIED_AT_INVALID, unrecognized_certainty_keys};
pub use highlight::{
    HIGHLIGHT_BUDGET_BYTES, HIGHLIGHT_HTML_BUDGET_BYTES, HIGHLIGHT_LINE_MAX, SourceHtml,
    escape_html, highlight_css, highlight_html_max, highlight_source, is_highlight_html,
    source_syntax_name,
};
pub use levels::{L1_MAX_TOKENS, NOTE_L1_MAX_CHARS, note_l1, section_l1};
pub use options::{HEADER_ID_PREFIX, comrak_options};
pub use patch::{Applied, PatchError, PatchOp, apply_patch, section_bytes};
pub use pos::LineIndex;
pub use render::{
    LinkResolution, NoResolve, RENDER_VERSION, Rendered, Resolver, attachment_ref, render_html,
    render_report_html,
};
pub use sanitize::{sanitize_html, sanitize_report_html};
pub use sentences::split_sentences;
pub use zam_core::text::is_stop_word;
/// Сканер секретов §7.6 — крейт `zam-secrets` (задача 15b M6); прежние имена — реэкспортом.
pub use zam_secrets as secrets;
pub use zam_secrets::{
    SECRET_MARK, ScanOutcome, SecretHit, SecretKind, mask_secrets, mask_text, scan_secrets,
    scan_secrets_ext,
};
