//! Извлечение структуры документов (спецификация M5 §1): без сети, без записи на диск, с лимитами.
//!
//! Вход — байты файла; выход — [`Extracted`]: единицы текста с [`Locator`] (страница, слайд,
//! диапазон листа, абзац) и путём заголовков. Превышение лимитов, кроме размера файла, даёт
//! частичный результат и предупреждение `truncated: <лимит>`, а не ошибку.
//!
//! Модули: [`sniff`] — вид по сигнатуре, затем по расширению; `ooxml` — общий разбор zip и XML
//! с лимитами; `docx`, `pptx` — форматы Office Open XML; `text` — простой текст; `sheet` —
//! таблицы (`xlsx`, `ods` на `ooxml`, `xls` на `cfb`, `csv`) и превью листа [`sheet_page`].

mod cfb;
mod csv;
mod ctx;
mod docx;
mod locator;
mod ods;
mod ooxml;
mod pptx;
mod sheet;
mod sniff;
mod text;
mod xls;
mod xlsx;

use std::time::Instant;

pub use ctx::HEADING_PATH_MAX_CHARS;
pub use locator::Locator;
pub use sheet::{SheetPage, sheet_page};
pub use sniff::sniff;

/// Версия извлечения: смена правил разбора — перенарезка вложений (кэш по `blob_sha256`).
pub const EXTRACTOR_VERSION: u32 = 1;

/// Вид документа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocKind {
    Pdf,
    Docx,
    Pptx,
    Xlsx,
    Xls,
    Ods,
    Csv,
    Text,
}

impl DocKind {
    /// Имя вида для журналов и API.
    pub fn as_str(self) -> &'static str {
        match self {
            DocKind::Pdf => "pdf",
            DocKind::Docx => "docx",
            DocKind::Pptx => "pptx",
            DocKind::Xlsx => "xlsx",
            DocKind::Xls => "xls",
            DocKind::Ods => "ods",
            DocKind::Csv => "csv",
            DocKind::Text => "text",
        }
    }
}

/// Лимиты разбора (`[docs]` конфигурации, спецификация §1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Размер файла, байт; превышение — отказ [`DocError::TooLarge`].
    pub max_bytes: u64,
    /// Суммарно распакованных байт zip (фактически прочитанных, не заявленных).
    pub max_unpacked: u64,
    /// Степень сжатия записи zip (распакованные / сжатые) для записей больше 1 МиБ.
    pub max_ratio: u32,
    /// Записей в архиве zip.
    pub max_entries: u32,
    /// Глубина вложенности XML.
    pub max_xml_depth: u32,
    /// Непустых ячеек листа; заодно — число общих строк книги и ячеек страницы превью.
    pub max_cells: u64,
    /// Объём выдачи, символов: текст единиц вместе с путями заголовков и 64 служебными
    /// символами на единицу.
    pub max_chars: u64,
    /// Страниц PDF.
    pub max_pages: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_bytes: 100 * 1024 * 1024,
            max_unpacked: 500 * 1024 * 1024,
            max_ratio: 100,
            max_entries: 10_000,
            max_xml_depth: 256,
            max_cells: 2_000_000,
            max_chars: 20_000_000,
            max_pages: 5000,
        }
    }
}

/// Единица текста документа: адрес, путь заголовков и текст.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub locator: Locator,
    pub heading_path: Vec<String>,
    pub text: String,
}

/// Результат извлечения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub kind: DocKind,
    pub units: Vec<Unit>,
    /// Предупреждения: `truncated: <лимит>` при частичном результате, `skipped: <часть>`.
    pub warnings: Vec<String>,
    /// Есть страницы без текстового слоя с изображением (OCR — вне M5).
    pub ocr_needed: bool,
}

/// Ошибка извлечения.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocError {
    /// Файл больше `max_bytes`.
    #[error("файл больше лимита max_bytes")]
    TooLarge,
    /// Вид не поддерживается этим путём разбора (PDF — только в рабочем процессе).
    #[error("вид документа не поддерживается")]
    Unsupported,
    /// Документ повреждён или запрещённого строения (например, `<!DOCTYPE>` в OOXML).
    #[error("документ повреждён: {0}")]
    Malformed(String),
    /// Срок разбора истёк, а частичного результата нет (рабочий процесс PDF).
    #[error("истёк срок разбора")]
    Timeout,
    /// Внутренняя ошибка разборщика.
    #[error("внутренняя ошибка разбора: {0}")]
    Internal(String),
}

/// Извлекает структуру документа вида `kind` из `bytes` в пределах `limits` и срока `deadline`.
///
/// PDF здесь не разбирается ([`DocError::Unsupported`]): только в рабочем процессе (§2).
/// Истечение `deadline` даёт частичный результат с `truncated: timeout_ms`.
pub fn extract(
    kind: DocKind,
    bytes: &[u8],
    limits: &Limits,
    deadline: Instant,
) -> Result<Extracted, DocError> {
    if bytes.len() as u64 > limits.max_bytes {
        return Err(DocError::TooLarge);
    }
    let mut ctx = ctx::Ctx::new(limits, deadline);
    let units = match kind {
        DocKind::Docx => docx::extract(bytes, &mut ctx)?,
        DocKind::Pptx => pptx::extract(bytes, &mut ctx)?,
        DocKind::Text => text::extract(bytes, &mut ctx),
        DocKind::Xlsx | DocKind::Xls | DocKind::Ods | DocKind::Csv => {
            sheet::extract(kind, bytes, &mut ctx)?
        }
        // PDF — только рабочий процесс (задача 3)
        DocKind::Pdf => return Err(DocError::Unsupported),
    };
    Ok(Extracted {
        kind,
        units,
        warnings: ctx.into_warnings(),
        ocr_needed: false,
    })
}
