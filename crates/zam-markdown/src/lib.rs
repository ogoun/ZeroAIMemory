//! Разбор Markdown ZeroAIMemory на comrak: байтовые позиции, заголовки,
//! секции, ссылки и frontmatter.
//!
//! Единственное место в проекте, которое разбирает Markdown (ТЗ §5.3).
//! Все диапазоны — байтовые оффсеты в исходной строке; исходник никогда
//! не сериализуется обратно из AST: PATCH — байтовый splice исходника.

mod doc;
mod frontmatter;
pub mod options;
mod patch;
mod pos;

pub use doc::{
    Doc, Frontmatter, Heading, Link, LinkKind, OutlineItem, Section, first_paragraph, outline,
    parse, section_by_path, slug,
};
pub use options::{HEADER_ID_PREFIX, comrak_options};
pub use patch::{Applied, PatchError, PatchOp, apply_patch, section_bytes};
pub use pos::LineIndex;
