//! Вид документа: сначала сигнатура содержимого, расширение — только для видов без сигнатуры.

use std::io::{Cursor, Read, Seek, SeekFrom};

use zip::ZipArchive;

use crate::ooxml::ZipCheck;
use crate::{DocKind, Limits};

/// Сигнатура составного документа OLE (CFB): XLS, а также DOC/PPT (не поддерживаются).
const OLE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
/// `%PDF-` ищется в первом килобайте (допускается мусор перед заголовком, как у читалок).
const PDF_WINDOW: usize = 1024;
/// Сколько читать из `[Content_Types].xml` и `mimetype` при определении вида.
const SNIFF_READ: u64 = 256 * 1024;
/// Начало файла для [`sniff_file`], если это не zip: окно проверки текста (64 КиБ) с запасом —
/// обрезанный посреди символа хвост окна тогда не считается ошибкой UTF-8.
const FILE_HEAD: u64 = 64 * 1024 + 16;
/// Хвост файла с концом центрального каталога zip (EOCD с комментарием до 64 КиБ).
const EOCD_TAIL: u64 = crate::ooxml::EOCD_WINDOW;

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

/// Вид изображения для миниатюры (превью, задача 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Gif,
    Webp,
}

/// Вид изображения только по сигнатуре начала файла (расширение и MIME не учитываются; SVG —
/// не изображение: это документ со скриптами).
pub fn sniff_image(head: &[u8]) -> Option<ImageKind> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageKind::Png)
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageKind::Jpeg)
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some(ImageKind::Gif)
    } else if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        Some(ImageKind::Webp)
    } else {
        None
    }
}

/// Похоже ли начало файла на текст: UTF-8 (с BOM или без) без нулевых байтов в первых 64 КиБ;
/// обрезанный посреди символа хвост окна допускается.
pub fn is_text(head: &[u8]) -> bool {
    looks_text(head)
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

/// [`sniff`] по файлу без чтения его в память: не zip — по началу файла ([`FILE_HEAD`]); zip —
/// по строго проверенному концу центрального каталога (`ooxml::zip_directory`: число записей —
/// `limits.max_entries`, размер каталога — `limits.max_central_dir`, до разбора каталога), самому
/// каталогу и не больше 256 КиБ `[Content_Types].xml` или `mimetype`. Ошибка ввода-вывода —
/// `Err`; неразборчивый или несогласованный zip — `Ok(None)`.
pub fn sniff_file<R: Read + Seek>(
    mut r: R,
    name: &str,
    limits: &Limits,
) -> std::io::Result<Option<DocKind>> {
    r.seek(SeekFrom::Start(0))?;
    let mut head = Vec::new();
    (&mut r).take(FILE_HEAD).read_to_end(&mut head)?;
    if !head.starts_with(b"PK\x03\x04") {
        return Ok(sniff(&head, name));
    }
    drop(head);
    let len = r.seek(SeekFrom::End(0))?;
    let start = len.saturating_sub(EOCD_TAIL);
    r.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::new();
    (&mut r).take(EOCD_TAIL).read_to_end(&mut tail)?;
    let mut read_at = |off: u64, n: usize| {
        let mut b = vec![0u8; n];
        r.seek(SeekFrom::Start(off)).ok()?;
        r.read_exact(&mut b).ok()?;
        Some(b)
    };
    let dir = crate::ooxml::zip_directory(&tail, start, len, limits, &mut read_at);
    // сверх `max_entries`/`max_central_dir` или несогласованный — не документ (`unsupported`)
    if !matches!(dir, Ok(ZipCheck::Fits(_))) {
        return Ok(None);
    }
    r.seek(SeekFrom::Start(0))?;
    Ok(crate::ooxml::open_archive(r)
        .ok()
        .and_then(|mut zip| sniff_archive(&mut zip)))
}

fn sniff_zip(bytes: &[u8]) -> Option<DocKind> {
    // та же строгая проверка конца каталога и его размера, что и при извлечении
    let dir = crate::ooxml::zip_directory_of(bytes, &Limits::default());
    if !matches!(dir, Ok(ZipCheck::Fits(_))) {
        return None;
    }
    let mut zip = crate::ooxml::open_archive(Cursor::new(bytes)).ok()?;
    sniff_archive(&mut zip)
}

/// Начало записи `name` архива (до [`SNIFF_READ`] байт) строкой.
fn read_part<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str) -> Option<String> {
    let f = zip.by_name(name).ok()?;
    let mut s = Vec::new();
    f.take(SNIFF_READ).read_to_end(&mut s).ok()?;
    Some(String::from_utf8_lossy(&s).into_owned())
}

/// Вид по содержимому открытого архива: `[Content_Types].xml` (OOXML) или `mimetype` (ODS).
fn sniff_archive<R: Read + Seek>(zip: &mut ZipArchive<R>) -> Option<DocKind> {
    let read = read_part::<R>;
    if let Some(ct) = read(zip, "[Content_Types].xml") {
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
    let mime = read(zip, "mimetype")?;
    let mime = mime.trim();
    (mime == "application/vnd.oasis.opendocument.spreadsheet"
        || mime == "application/vnd.oasis.opendocument.spreadsheet-template")
        .then_some(DocKind::Ods)
}
