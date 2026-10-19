//! Разбор Markdown ZeroAIMemory на comrak: байтовые позиции, заголовки,
//! секции, ссылки, frontmatter, санитизированный HTML, а также чанкер §8.2,
//! сегментация на предложения и уровни L1 (§8.3).
//!
//! Единственное место в проекте, которое разбирает Markdown (ТЗ §5.3).
//! Все диапазоны — байтовые оффсеты в исходной строке; исходник никогда
//! не сериализуется обратно из AST: PATCH — байтовый splice исходника.

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

pub use chunk::{ApproxCounter, ChunkDraft, ChunkLevel, ChunkParams, TokenCounter, chunk_note};
pub use doc::{
    Doc, Frontmatter, Heading, Link, LinkKind, OutlineItem, Section, first_paragraph, outline,
    parse, section_by_path, slug,
};
pub use highlight::highlight_css;
pub use levels::{L1_MAX_TOKENS, NOTE_L1_MAX_CHARS, note_l1, section_l1};
pub use options::{HEADER_ID_PREFIX, comrak_options};
pub use patch::{Applied, PatchError, PatchOp, apply_patch, section_bytes};
pub use pos::LineIndex;
pub use render::{
    LinkResolution, NoResolve, RENDER_VERSION, Rendered, Resolver, attachment_ref, render_html,
};
pub use sanitize::sanitize_html;
pub use sentences::split_sentences;
