//! Общий разбор Office Open XML: записи zip с лимитами, потоковый XML, отношения частей.
//!
//! Защита (исследование M5 §3): записи читаются только в память через `take(лимит + 1)` с
//! подсчётом фактических байт (заявленным размерам не верим); суммарный объём — `max_unpacked`
//! (ловит и перекрывающиеся записи), степень сжатия записи больше 1 МиБ — `max_ratio`, число
//! записей — `max_entries` до разбора центрального каталога. XML: `<!DOCTYPE>` — ошибка, своих
//! сущностей нет (только пять предопределённых и числовые), глубина — `max_xml_depth`.

use std::borrow::Cow;
use std::io::{Cursor, Read};

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use zip::{CompressionMethod, ZipArchive};

use crate::DocError;
use crate::ctx::Ctx;

/// Степень сжатия проверяется у записей больше этого объёма: мелкий XML сжимается сильно.
const RATIO_FROM: u64 = 1024 * 1024;
/// Шаг чтения записи.
const READ_STEP: usize = 64 * 1024;

/// Пакет OOXML (zip) с учётом распакованного объёма.
pub(crate) struct Package<'a> {
    zip: ZipArchive<Cursor<&'a [u8]>>,
    unpacked: u64,
}

/// Прочитанная часть пакета.
pub(crate) struct Part {
    pub name: String,
    pub text: String,
    /// Часть обрезана лимитом: ошибки XML в конце не считаются повреждением.
    pub truncated: bool,
}

impl<'a> Package<'a> {
    /// Открывает zip. `Ok(None)` — записей больше `max_entries` (предупреждение уже записано).
    pub fn open(bytes: &'a [u8], ctx: &mut Ctx) -> Result<Option<Self>, DocError> {
        let max = u64::from(ctx.limits.max_entries);
        if declared_entries(bytes).is_some_and(|n| n > max) {
            ctx.stop("max_entries");
            return Ok(None);
        }
        let zip = ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| DocError::Malformed(format!("zip: {e}")))?;
        if zip.len() as u64 > max {
            ctx.stop("max_entries");
            return Ok(None);
        }
        Ok(Some(Package { zip, unpacked: 0 }))
    }

    /// Имя записи с учётом регистра (OPC сравнивает имена частей без учёта регистра).
    fn entry_name(&self, name: &str) -> Option<String> {
        if self.zip.index_for_name(name).is_some() {
            return Some(name.to_string());
        }
        self.zip
            .file_names()
            .filter_map(Result::ok)
            .find(|n| n.eq_ignore_ascii_case(name))
            .map(Cow::into_owned)
    }

    /// Есть ли часть.
    pub fn has(&self, name: &str) -> bool {
        self.entry_name(name).is_some()
    }

    /// Читает часть в память в пределах лимитов. `Ok(None)` — нет части, она пропущена
    /// (шифрование, неизвестный метод сжатия) или объём исчерпан.
    pub fn read(&mut self, name: &str, ctx: &mut Ctx) -> Result<Option<Part>, DocError> {
        if ctx.stopped() {
            return Ok(None);
        }
        let Some(real) = self.entry_name(name) else {
            return Ok(None);
        };
        let left = ctx.limits.max_unpacked.saturating_sub(self.unpacked);
        if left == 0 {
            ctx.stop("max_unpacked");
            return Ok(None);
        }
        let mut file = match self.zip.by_name(&real) {
            Ok(f) => f,
            Err(
                zip::result::ZipError::UnsupportedArchive(_)
                | zip::result::ZipError::CompressionMethodNotSupported(_)
                | zip::result::ZipError::InvalidPassword,
            ) => {
                ctx.warn(format!("skipped: {real}"));
                return Ok(None);
            }
            Err(e) => return Err(DocError::Malformed(format!("zip {real}: {e}"))),
        };
        if file.encrypted()
            || !matches!(
                file.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            )
        {
            ctx.warn(format!("skipped: {real}"));
            return Ok(None);
        }
        let compressed = file.compressed_size().max(1);
        let ratio_cap = compressed.saturating_mul(u64::from(ctx.limits.max_ratio));
        let mut reader = (&mut file).take(left.saturating_add(1));
        let mut buf = Vec::new();
        let mut chunk = vec![0u8; READ_STEP];
        let mut truncated = false;
        loop {
            if !ctx.tick() {
                truncated = true;
                break;
            }
            let n = match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => return Err(DocError::Malformed(format!("zip {real}: {e}"))),
            };
            buf.extend_from_slice(&chunk[..n]);
            let total = buf.len() as u64;
            if total > left {
                buf.truncate(left as usize);
                ctx.stop("max_unpacked");
                truncated = true;
                break;
            }
            if total > RATIO_FROM && total > ratio_cap {
                ctx.truncated("max_ratio");
                truncated = true;
                break;
            }
        }
        self.unpacked += buf.len() as u64;
        Ok(Some(Part {
            name: real,
            text: decode(buf),
            truncated,
        }))
    }

    /// Отношения части `part` (`<dir>/_rels/<file>.rels`); цели — полные имена частей.
    pub fn rels(&mut self, part: &str, ctx: &mut Ctx) -> Result<Vec<Rel>, DocError> {
        let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
        let rels_name = if dir.is_empty() {
            format!("_rels/{file}.rels")
        } else {
            format!("{dir}/_rels/{file}.rels")
        };
        let Some(p) = self.read(&rels_name, ctx)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut xml = Xml::new(&p, ctx);
        while let Some(ev) = xml.next(ctx)? {
            if let Ev::Start(e) | Ev::Empty(e) = ev
                && local(&e) == "Relationship"
            {
                if attr(&e, "TargetMode").is_some_and(|m| m.eq_ignore_ascii_case("External")) {
                    continue;
                }
                let (Some(id), Some(ty), Some(target)) =
                    (attr(&e, "Id"), attr(&e, "Type"), attr(&e, "Target"))
                else {
                    continue;
                };
                out.push(Rel {
                    id,
                    kind: ty.rsplit('/').next().unwrap_or_default().to_string(),
                    target: resolve(dir, &target),
                });
            }
        }
        Ok(out)
    }
}

/// Отношение OPC: `kind` — последний сегмент `Type` (`officeDocument`, `slide`, `notesSlide`…).
#[derive(Debug, Clone)]
pub(crate) struct Rel {
    pub id: String,
    pub kind: String,
    pub target: String,
}

/// Цель отношения относительно каталога части: `../`, `./`, абсолютные `/word/…`.
fn resolve(dir: &str, target: &str) -> String {
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        dir.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Число записей из конца центрального каталога (EOCD, zip64) — до разбора архива.
pub(crate) fn declared_entries(b: &[u8]) -> Option<u64> {
    const EOCD: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
    const LOC64: [u8; 4] = [0x50, 0x4b, 0x06, 0x07];
    const EOCD64: [u8; 4] = [0x50, 0x4b, 0x06, 0x06];
    let from = b.len().saturating_sub(22 + 65_535);
    let pos = (from..b.len().saturating_sub(21))
        .rev()
        .find(|&i| b[i..i + 4] == EOCD)?;
    let total = u64::from(u16::from_le_bytes([b[pos + 10], b[pos + 11]]));
    if total != 0xFFFF {
        return Some(total);
    }
    let loc = pos.checked_sub(20)?;
    if b[loc..loc + 4] != LOC64 {
        return Some(total);
    }
    let off = u64::from_le_bytes(b[loc + 8..loc + 16].try_into().ok()?);
    let off = usize::try_from(off).ok()?;
    let rec = b.get(off..off.checked_add(40)?)?;
    if rec[..4] != EOCD64 {
        return Some(total);
    }
    Some(u64::from_le_bytes(rec[32..40].try_into().ok()?))
}

/// Байты части → строка: BOM UTF-8/UTF-16 снимаются, неверные последовательности заменяются.
fn decode(buf: Vec<u8>) -> String {
    let utf16 = |b: &[u8], le: bool| {
        let units = b.as_chunks::<2>().0.iter().map(|&c| {
            if le {
                u16::from_le_bytes(c)
            } else {
                u16::from_be_bytes(c)
            }
        });
        char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    };
    match buf.as_slice() {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [0xFF, 0xFE, rest @ ..] => utf16(rest, true),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, false),
        _ => String::from_utf8(buf)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()),
    }
}

/// Событие XML для разборщиков форматов.
pub(crate) enum Ev<'x> {
    Start(BytesStart<'x>),
    Empty(BytesStart<'x>),
    End(String),
    Text(Cow<'x, str>),
}

/// Потоковый XML-читатель с лимитом глубины и сроком; без DTD и своих сущностей.
pub(crate) struct Xml<'x> {
    reader: Reader<&'x [u8]>,
    depth: u32,
    max_depth: u32,
    part: &'x str,
    lenient: bool,
    done: bool,
}

impl<'x> Xml<'x> {
    pub fn new(part: &'x Part, ctx: &Ctx) -> Self {
        Xml {
            reader: Reader::from_str(&part.text),
            depth: 0,
            max_depth: ctx.limits.max_xml_depth,
            part: &part.name,
            lenient: part.truncated,
            done: false,
        }
    }

    /// Следующее значимое событие; `None` — конец части или остановка по лимиту.
    pub fn next(&mut self, ctx: &mut Ctx) -> Result<Option<Ev<'x>>, DocError> {
        loop {
            if self.done {
                return Ok(None);
            }
            if !ctx.tick() {
                self.done = true;
                return Ok(None);
            }
            let ev = match self.reader.read_event() {
                Ok(ev) => ev,
                Err(e) => {
                    self.done = true;
                    if self.lenient {
                        return Ok(None);
                    }
                    return Err(DocError::Malformed(format!("{}: {e}", self.part)));
                }
            };
            match ev {
                Event::DocType(_) => {
                    self.done = true;
                    return Err(DocError::Malformed(format!(
                        "{}: <!DOCTYPE> запрещён",
                        self.part
                    )));
                }
                Event::Start(e) => {
                    self.depth += 1;
                    if self.depth > self.max_depth {
                        self.done = true;
                        ctx.truncated("max_xml_depth");
                        return Ok(None);
                    }
                    return Ok(Some(Ev::Start(e)));
                }
                Event::Empty(e) => return Ok(Some(Ev::Empty(e))),
                Event::End(e) => {
                    self.depth = self.depth.saturating_sub(1);
                    return Ok(Some(Ev::End(e.local_name().as_ref().to_string())));
                }
                Event::Text(t) => return Ok(Some(Ev::Text(t.xml10_content()))),
                Event::CData(t) => return Ok(Some(Ev::Text(t.xml10_content()))),
                Event::GeneralRef(r) => {
                    let ch = match r.resolve_char_ref() {
                        Ok(Some(c)) => Some(c.to_string()),
                        Ok(None) => {
                            quick_xml::escape::resolve_predefined_entity(&r).map(str::to_string)
                        }
                        Err(_) => None,
                    };
                    match ch {
                        Some(s) => return Ok(Some(Ev::Text(Cow::Owned(s)))),
                        None if self.lenient => {
                            self.done = true;
                            return Ok(None);
                        }
                        None => {
                            self.done = true;
                            return Err(DocError::Malformed(format!(
                                "{}: неизвестная сущность &{};",
                                self.part, &*r
                            )));
                        }
                    }
                }
                Event::Eof => {
                    self.done = true;
                    return Ok(None);
                }
                Event::Decl(_) | Event::PI(_) | Event::Comment(_) => {}
            }
        }
    }

    /// Пропускает поддерево только что открытого элемента (`Start`).
    pub fn skip(&mut self, ctx: &mut Ctx) -> Result<(), DocError> {
        let mut level = 1u32;
        while let Some(ev) = self.next(ctx)? {
            match ev {
                Ev::Start(_) => level += 1,
                Ev::End(_) => {
                    level -= 1;
                    if level == 0 {
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Локальное имя элемента без префикса пространства имён.
pub(crate) fn local<'e>(e: &'e BytesStart<'_>) -> &'e str {
    let name = e.name().0;
    name.rsplit_once(':').map_or(name, |(_, l)| l)
}

/// Значение атрибута по локальному имени без префикса (`Id`, `val`).
pub(crate) fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    find_attr(e, name, false)
}

/// Значение атрибута с префиксом (`r:id`) по локальному имени.
pub(crate) fn attr_ns(e: &BytesStart<'_>, name: &str) -> Option<String> {
    find_attr(e, name, true)
}

fn find_attr(e: &BytesStart<'_>, name: &str, prefixed: bool) -> Option<String> {
    for a in e.attributes().flatten() {
        let key = a.key.0;
        let (has_prefix, l) = match key.rsplit_once(':') {
            Some((_, l)) => (true, l),
            None => (false, key),
        };
        if l == name && has_prefix == prefixed {
            return a
                .normalized_value(XmlVersion::Implicit1_0)
                .ok()
                .map(Cow::into_owned);
        }
    }
    None
}

/// Значение «включено» у булевых свойств OOXML (`<w:vanish/>`, `w:val="0|false|off"` — нет).
pub(crate) fn on(e: &BytesStart<'_>) -> bool {
    !matches!(attr_ns(e, "val").as_deref(), Some("0" | "false" | "off"))
}

/// Таблица: строки ячеек → строки `| a | b |` с разделителем шапки после первой строки.
#[derive(Default)]
pub(crate) struct Table {
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
}

impl Table {
    /// Текст в текущую ячейку (абзацы ячейки — через пробел).
    pub fn text(&mut self, s: &str) {
        let s = s.trim();
        if s.is_empty() {
            return;
        }
        if !self.cell.is_empty() {
            self.cell.push(' ');
        }
        self.cell.push_str(s);
    }

    pub fn end_cell(&mut self) {
        let c = std::mem::take(&mut self.cell)
            .replace('\n', " ")
            .replace('|', "\\|");
        self.row.push(c);
    }

    pub fn end_row(&mut self) {
        let row = std::mem::take(&mut self.row);
        if row.iter().any(|c| !c.is_empty()) {
            self.rows.push(row);
        }
    }

    pub fn render(self) -> String {
        let mut out = String::new();
        for (i, row) in self.rows.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push('|');
            for c in row {
                out.push(' ');
                out.push_str(c);
                out.push_str(" |");
            }
            if i == 0 {
                out.push('\n');
                out.push('|');
                for _ in row {
                    out.push_str(" --- |");
                }
            }
        }
        out
    }
}
