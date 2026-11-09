//! Перенос пространства (M6 §12б.5): формат архива ([`archive`]) и экспорт
//! `GET /spaces/{space}/export` ([`export`]). Импорт — задача 18b2.
pub mod archive;
pub mod export;

pub use archive::{
    EXPORT_FORMAT, ExportFilter, ExportManifest, Fingerprint, attachment_file, encode_segment,
    file_name_for, front_matter,
};
