//! Вид документа: сначала сигнатура содержимого, расширение — только для видов без сигнатуры.

use std::io::{Cursor, Read};

use zip::ZipArchive;

use crate::{DocKind, Limits};

/// Сигнатура составного документа OLE (CFB): XLS, а также DOC/PPT (не поддерживаются).
const OLE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
/// `%PDF-` ищется в первом килобайте (допускается мусор перед заголовком, как у читалок).
const PDF_WINDOW: usize = 1024;
/// Сколько читать из `[Content_Types].xml` и `mimetype` при определении вида.
const SNIFF_READ: u64 = 256 * 1024;

/// Вид документа по сигнатуре (zip с `[Content_Types].xml` или `mimetype` ODS, `%PDF-`, OLE),
/// затем по расширению имени (CSV, текст). `None` — вид не поддерживается.
///
/// Расширение не перекрывает сигнатуру: PDF с именем `.docx` — `Pdf`, а `.docx` без
/// сигнатуры zip — `None`.
pub fn sniff(bytes: &[u8], name: &str) -> Option<DocKind> {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    if bytes.starts_with(b"PK\x03\x04") {
        return sniff_zip(bytes);
    }
    if bytes.starts_with(&OLE) {
        // DOC, PPT, MSG тоже OLE; без расширения или с любым другим — пробуем как XLS
        return match ext.as_str() {
            "doc" | "dot" | "ppt" | "pps" | "pot" | "msg" => None,
            _ => Some(DocKind::Xls),
        };
    }
    let text_kind = match ext.as_str() {
        "csv" | "tsv" => Some(DocKind::Csv),
        "txt" | "text" | "md" | "markdown" | "log" | "json" | "jsonl" | "yaml" | "yml" | "toml"
        | "ini" | "cfg" | "conf" | "xml" | "html" | "htm" | "rst" | "adoc" => Some(DocKind::Text),
        _ => None,
    };
    let is_text = looks_text(bytes);
    // текстовый файл с упоминанием `%PDF-` (заметка о PDF) — текст; PDF — только с начала
    let pdf = if text_kind.is_some() && is_text {
        bytes.trim_ascii_start().starts_with(b"%PDF-")
    } else {
        let head = &bytes[..bytes.len().min(PDF_WINDOW)];
        head.windows(5).any(|w| w == b"%PDF-")
    };
    if pdf {
        return Some(DocKind::Pdf);
    }
    if is_text { text_kind } else { None }
}

/// Текст: UTF-8 (с BOM или без) без нулевых байтов в начале файла.
fn looks_text(bytes: &[u8]) -> bool {
    let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let head = &b[..b.len().min(64 * 1024)];
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        // обрезанный на середине символа хвост окна — не повод отказать
        Err(e) => e.error_len().is_none() && head.len() < b.len(),
    }
}

fn sniff_zip(bytes: &[u8]) -> Option<DocKind> {
    // та же проверка числа записей до разбора центрального каталога, что и при извлечении
    let max = u64::from(Limits::default().max_entries);
    if crate::ooxml::declared_entries(bytes).is_some_and(|n| n > max) {
        return None;
    }
    let mut zip = ZipArchive::new(Cursor::new(bytes)).ok()?;
    let read = |zip: &mut ZipArchive<Cursor<&[u8]>>, name: &str| -> Option<String> {
        let f = zip.by_name(name).ok()?;
        let mut s = Vec::new();
        f.take(SNIFF_READ).read_to_end(&mut s).ok()?;
        Some(String::from_utf8_lossy(&s).into_owned())
    };
    if let Some(ct) = read(&mut zip, "[Content_Types].xml") {
        let has = |n: &str| zip.index_for_name(n).is_some();
        if ct.contains("wordprocessingml.document.main")
            || ct.contains("wordprocessingml.template.main")
            || ct.contains("ms-word.document.macroEnabled.main")
            || ct.contains("ms-word.template.macroEnabledTemplate.main")
            || has("word/document.xml")
        {
            return Some(DocKind::Docx);
        }
        if ct.contains("presentationml.presentation.main")
            || ct.contains("presentationml.slideshow.main")
            || ct.contains("presentationml.template.main")
            || ct.contains("ms-powerpoint.presentation.macroEnabled.main")
            || ct.contains("ms-powerpoint.slideshow.macroEnabled.main")
            || has("ppt/presentation.xml")
        {
            return Some(DocKind::Pptx);
        }
        if ct.contains("spreadsheetml.sheet.main")
            || ct.contains("spreadsheetml.template.main")
            || ct.contains("ms-excel.sheet.macroEnabled.main")
            || has("xl/workbook.xml")
        {
            return Some(DocKind::Xlsx);
        }
        return None;
    }
    let mime = read(&mut zip, "mimetype")?;
    let mime = mime.trim();
    (mime == "application/vnd.oasis.opendocument.spreadsheet"
        || mime == "application/vnd.oasis.opendocument.spreadsheet-template")
        .then_some(DocKind::Ods)
}
