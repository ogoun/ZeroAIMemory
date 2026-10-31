//! Общий разбор Office Open XML: записи zip с лимитами, потоковый XML, отношения частей.
//!
//! Защита (исследование M5 §3): записи читаются только в память через `take(лимит + 1)` с
//! подсчётом фактических байт (заявленным размерам не верим); суммарный объём — `max_unpacked`;
//! степень сжатия записи больше 1 МиБ — `max_ratio` по фактически прочитанным сжатым байтам
//! (счётчик под zip), а не по заголовку; перекрывающиеся записи (бомба Фифилда) и повторы имён
//! (в том числе без учёта регистра) — отказ; число записей — `max_entries` до разбора
//! центрального каталога. XML: `<!DOCTYPE>` — ошибка, своих сущностей нет (только пять
//! предопределённых и числовые), глубина — `max_xml_depth`.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::HashMap;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::rc::Rc;

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use zip::{CompressionMethod, ZipArchive};

use crate::DocError;
use crate::ctx::Ctx;

/// Степень сжатия проверяется у записей больше этого объёма: мелкий XML сжимается сильно.
const RATIO_FROM: u64 = 1024 * 1024;
/// Шаг чтения записи.
const READ_STEP: usize = 64 * 1024;
/// Начальная ёмкость буфера записи: заявленному размеру не верим, дальше — рост до `left`.
const INITIAL_CAP: usize = 4 * 1024 * 1024;

/// Читатель архива со счётчиком фактически прочитанных (сжатых) байт.
struct Counting<'a> {
    inner: Cursor<&'a [u8]>,
    n: Rc<Cell<u64>>,
}

impl Read for Counting<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let k = self.inner.read(buf)?;
        self.n.set(self.n.get() + k as u64);
        Ok(k)
    }
}

impl Seek for Counting<'_> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// Пакет OOXML (zip) с учётом распакованного объёма.
pub(crate) struct Package<'a> {
    zip: ZipArchive<Counting<'a>>,
    /// Имя части с ASCII в нижнем регистре → имя записи (OPC: без учёта регистра ASCII).
    names: HashMap<String, String>,
    read_bytes: Rc<Cell<u64>>,
    len: u64,
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
        let declared = declared_entries(bytes);
        if declared.is_some_and(|n| n > max) {
            ctx.stop("max_entries");
            return Ok(None);
        }
        let read_bytes = Rc::new(Cell::new(0));
        let reader = Counting {
            inner: Cursor::new(bytes),
            n: read_bytes.clone(),
        };
        let mut zip =
            ZipArchive::new(reader).map_err(|e| DocError::Malformed(format!("zip: {e}")))?;
        if zip.len() as u64 > max {
            ctx.stop("max_entries");
            return Ok(None);
        }
        // `zip` складывает записи с одинаковым именем в одну: их число меньше заявленного
        if declared.is_some_and(|n| n != 0xFFFF && n > zip.len() as u64) {
            return Err(DocError::Malformed("zip: повтор имён записей".into()));
        }
        let mut names = HashMap::with_capacity(zip.len());
        for n in zip.file_names() {
            let n = n.map_err(|e| DocError::Malformed(format!("zip: {e}")))?;
            if names
                .insert(n.to_ascii_lowercase(), n.to_string())
                .is_some()
            {
                return Err(DocError::Malformed("zip: повтор имён записей".into()));
            }
        }
        match zip.has_overlapping_entries() {
            Ok(false) => {}
            Ok(true) => return Err(DocError::Malformed("zip: overlapping entries".into())),
            Err(e) => return Err(DocError::Malformed(format!("zip: {e}"))),
        }
        Ok(Some(Package {
            zip,
            names,
            read_bytes,
            len: bytes.len() as u64,
            unpacked: 0,
        }))
    }

    /// Имя записи части без учёта регистра ASCII.
    fn entry_name(&self, name: &str) -> Option<&str> {
        self.names
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
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
        let Some(real) = self.entry_name(name).map(str::to_string) else {
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
        // сжатых байт не больше, чем осталось в файле после начала данных
        let tail = self
            .len
            .saturating_sub(file.data_start().unwrap_or(file.header_start()));
        let effective = file.compressed_size().min(tail).max(1);
        let ratio = u64::from(ctx.limits.max_ratio);
        let n0 = self.read_bytes.get();
        let declared = file.size();
        let cap = left.min(declared).min(usize::MAX as u64) as usize;
        let mut buf: Vec<u8> = Vec::with_capacity(cap.min(INITIAL_CAP));
        let mut reader = (&mut file).take(left.saturating_add(1));
        let mut chunk = vec![0u8; READ_STEP];
        let mut sink = Sink::default();
        let mut raw = 0u64;
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
            raw += n as u64;
            // выход UTF-8 (UTF-16 раскодируется по ходу) и вход ограничены одним `left`
            sink.feed(&chunk[..n], &mut buf, left);
            if raw > left || buf.len() as u64 > left {
                buf.truncate(left as usize);
                // эта часть разбирается частично, следующие уже не читаются (`left == 0`)
                ctx.truncated("max_unpacked");
                truncated = true;
                break;
            }
            let packed = self.read_bytes.get().saturating_sub(n0).max(1);
            if raw > RATIO_FROM
                && (raw > packed.saturating_mul(ratio) || raw > effective.saturating_mul(ratio))
            {
                ctx.truncated("max_ratio");
                truncated = true;
                break;
            }
        }
        drop(file);
        sink.finish(&mut buf, left);
        if sink.bad_utf16 && !truncated {
            return Err(DocError::Malformed(format!("invalid utf-16 in {real}")));
        }
        self.unpacked += raw.max(buf.len() as u64);
        let text = decode(buf, &real, truncated)?;
        Ok(Some(Part {
            name: real,
            text,
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

/// Приёмник байтов записи: UTF-8 копится как есть, UTF-16 (по BOM) раскодируется в UTF-8 по
/// ходу чтения — в памяти одна копия текста, и её объём учитывается в `max_unpacked`.
#[derive(Default)]
struct Sink {
    /// Первые байты до решения о кодировке (BOM — 2 байта).
    head: Vec<u8>,
    /// `Some(le)` — UTF-16; `None` после решения — UTF-8.
    utf16: Option<bool>,
    decided: bool,
    carry: Option<u8>,
    high: Option<u16>,
    /// Непарный суррогат UTF-16.
    bad_utf16: bool,
}

impl Sink {
    fn feed(&mut self, mut data: &[u8], out: &mut Vec<u8>, left: u64) {
        if !self.decided {
            let need = 2usize.saturating_sub(self.head.len()).min(data.len());
            self.head.extend_from_slice(&data[..need]);
            data = &data[need..];
            if self.head.len() < 2 {
                return;
            }
            self.decided = true;
            let head = std::mem::take(&mut self.head);
            match head.as_slice() {
                [0xFF, 0xFE] => self.utf16 = Some(true),
                [0xFE, 0xFF] => self.utf16 = Some(false),
                _ => {
                    grow(out, head.len(), left);
                    out.extend_from_slice(&head);
                }
            }
        }
        let Some(le) = self.utf16 else {
            grow(out, data.len(), left);
            out.extend_from_slice(data);
            return;
        };
        // UTF-16 → UTF-8: до 3 байт на кодовую единицу
        grow(out, data.len() / 2 * 3 + 4, left);
        let mut i = 0;
        if let Some(c) = self.carry.take() {
            match data.first() {
                Some(&d) => {
                    self.unit([c, d], le, out);
                    i = 1;
                }
                None => {
                    self.carry = Some(c);
                    return;
                }
            }
        }
        let rest = &data[i..];
        let (pairs, tail) = rest.as_chunks::<2>();
        for &p in pairs {
            self.unit(p, le, out);
        }
        self.carry = tail.first().copied();
    }

    fn unit(&mut self, b: [u8; 2], le: bool, out: &mut Vec<u8>) {
        let u = if le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        };
        let c = match (self.high.take(), u) {
            (None, 0xD800..=0xDBFF) => {
                self.high = Some(u);
                return;
            }
            (Some(h), 0xDC00..=0xDFFF) => {
                let v = 0x10000 + ((u32::from(h) - 0xD800) << 10) + (u32::from(u) - 0xDC00);
                char::from_u32(v)
            }
            (Some(_), _) | (None, 0xDC00..=0xDFFF) => None,
            (None, u) => char::from_u32(u32::from(u)),
        };
        match c {
            Some(c) => {
                let mut tmp = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
            }
            None => self.bad_utf16 = true,
        }
    }

    /// Конец записи: недочитанный BOM-кандидат — это UTF-8; хвост UTF-16 без пары — ошибка.
    fn finish(&mut self, out: &mut Vec<u8>, left: u64) {
        if !self.decided {
            let head = std::mem::take(&mut self.head);
            grow(out, head.len(), left);
            out.extend_from_slice(&head);
            self.decided = true;
        }
        if self.utf16.is_some() && (self.carry.is_some() || self.high.is_some()) {
            self.bad_utf16 = true;
        }
    }
}

/// Байты части (UTF-8) → строка: BOM снимается, концы строк `\r\n` и `\r` → `\n` на месте
/// (как в разборщике XML 1.0, но без копии больших текстовых узлов); неверный UTF-8 — ошибка
/// `Malformed`, кроме неполного символа в конце обрезанной лимитом части.
fn decode(mut buf: Vec<u8>, part: &str, truncated: bool) -> Result<String, DocError> {
    if buf.starts_with(&[0xEF, 0xBB, 0xBF]) {
        buf.drain(..3);
    }
    normalize_eol(&mut buf);
    match String::from_utf8(buf) {
        Ok(s) => Ok(s),
        Err(e) if truncated && e.utf8_error().error_len().is_none() => {
            let valid = e.utf8_error().valid_up_to();
            let mut b = e.into_bytes();
            b.truncate(valid);
            String::from_utf8(b)
                .map_err(|_| DocError::Malformed(format!("invalid utf-8 in {part}")))
        }
        Err(_) => Err(DocError::Malformed(format!("invalid utf-8 in {part}"))),
    }
}

/// Нормализация концов строк XML 1.0 (§2.11) сдвигом в том же буфере.
fn normalize_eol(b: &mut Vec<u8>) {
    let Some(first) = b.iter().position(|&c| c == b'\r') else {
        return;
    };
    let (mut w, mut r) = (first, first);
    while r < b.len() {
        let c = b[r];
        r += 1;
        if c == b'\r' {
            b[w] = b'\n';
            if b.get(r) == Some(&b'\n') {
                r += 1;
            }
        } else {
            b[w] = c;
        }
        w += 1;
    }
    b.truncate(w);
}

/// Рост буфера записи без удвоения сверх лимита: ёмкость не больше `left` + шаг чтения.
fn grow(buf: &mut Vec<u8>, add: usize, left: u64) {
    let need = buf.len() + add;
    if need <= buf.capacity() {
        return;
    }
    let cap_max = usize::try_from(left)
        .unwrap_or(usize::MAX)
        .saturating_add(READ_STEP);
    let want = buf
        .capacity()
        .saturating_mul(2)
        .max(need)
        .min(cap_max.max(need));
    buf.reserve_exact(want - buf.len());
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
        // один проход: перевод строки → пробел, `|` → `\|`
        let cell = std::mem::take(&mut self.cell);
        let mut c = String::with_capacity(cell.len());
        for ch in cell.chars() {
            match ch {
                '\n' => c.push(' '),
                '|' => c.push_str("\\|"),
                ch => c.push(ch),
            }
        }
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
