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
/// Длина `Id`/`Type`/`Target` отношения, байт: длиннее — отношение пропускается.
pub(crate) const REL_ATTR_MAX: usize = 1024;
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
    /// Открывает zip. `Ok(None)` — записей больше `max_entries` или каталог больше
    /// `max_central_dir` (предупреждение уже записано); несогласованный конец каталога
    /// ([`zip_directory`]) — `Malformed`.
    pub fn open(bytes: &'a [u8], ctx: &mut Ctx) -> Result<Option<Self>, DocError> {
        let max = u64::from(ctx.limits.max_entries);
        // строгая проверка каталога — до `ZipArchive` (подделки, откат `zip` к ранней EOCD)
        let dir = match zip_directory_of(bytes, ctx.limits)
            .map_err(|e| DocError::Malformed(format!("zip: {e}")))?
        {
            ZipCheck::Fits(d) => d,
            ZipCheck::TooManyEntries => {
                ctx.stop("max_entries");
                return Ok(None);
            }
            ZipCheck::DirTooLarge => {
                ctx.stop("max_central_dir");
                return Ok(None);
            }
        };
        let read_bytes = Rc::new(Cell::new(0));
        let reader = Counting {
            inner: Cursor::new(bytes),
            n: read_bytes.clone(),
        };
        let mut zip = open_archive(reader).map_err(|e| DocError::Malformed(format!("zip: {e}")))?;
        if zip.len() as u64 > max {
            ctx.stop("max_entries");
            return Ok(None);
        }
        // `zip` складывает записи с одинаковым именем в одну: их число меньше заявленного
        if dir.entries > zip.len() as u64 {
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
        self.read_inner(name, None, ctx)
    }

    /// Служебная часть со своим потолком `cap` байт (стили книги): общий бюджет
    /// `max_unpacked` она не расходует; больше `cap` или сверх `max_ratio` — `skipped: <часть>`
    /// и `Ok(None)`, разбор документа продолжается без неё.
    pub fn read_side(
        &mut self,
        name: &str,
        cap: u64,
        ctx: &mut Ctx,
    ) -> Result<Option<Part>, DocError> {
        self.read_inner(name, Some(cap), ctx)
    }

    fn read_inner(
        &mut self,
        name: &str,
        side: Option<u64>,
        ctx: &mut Ctx,
    ) -> Result<Option<Part>, DocError> {
        if ctx.stopped() {
            return Ok(None);
        }
        let Some(real) = self.entry_name(name).map(str::to_string) else {
            return Ok(None);
        };
        let left = match side {
            Some(cap) => cap,
            None => ctx.limits.max_unpacked.saturating_sub(self.unpacked),
        };
        if left == 0 {
            ctx.stop("max_unpacked");
            return Ok(None);
        }
        if side.is_some() && self.zip.by_name(&real).is_ok_and(|f| f.size() > left) {
            ctx.warn(format!("skipped: {real}"));
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
            Err(e) => return Err(DocError::Malformed(format!("zip ({real}): {e}"))),
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
                Err(e) => return Err(DocError::Malformed(format!("zip ({real}): {e}"))),
            };
            raw += n as u64;
            // выход UTF-8 (UTF-16 раскодируется по ходу) и вход ограничены одним `left`
            sink.feed(&chunk[..n], &mut buf, left);
            if side.is_some() && (raw > left || buf.len() as u64 > left) {
                // служебная часть сверх своего потолка — без неё
                ctx.warn(format!("skipped: {real}"));
                return Ok(None);
            }
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
                if side.is_some() {
                    ctx.warn(format!("skipped: {real}"));
                    return Ok(None);
                }
                ctx.truncated("max_ratio");
                truncated = true;
                break;
            }
        }
        drop(file);
        sink.finish(&mut buf, left);
        if sink.bad_utf16 && !truncated {
            return Err(DocError::Malformed(format!(
                "encoding ({real}): invalid utf-16"
            )));
        }
        if side.is_none() {
            self.unpacked += raw.max(buf.len() as u64);
        }
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
                if [&id, &ty, &target].iter().any(|a| a.len() > REL_ATTR_MAX) {
                    continue;
                }
                // отношений к частям не больше записей архива: больше частей не бывает;
                // внешние (гиперссылки) пропущены выше и в потолок не входят — их бывает
                // больше `max_entries`, а память держит обрезка атрибутов `REL_ATTR_MAX`
                if out.len() as u64 >= u64::from(ctx.limits.max_entries) {
                    ctx.truncated("max_entries");
                    break;
                }
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

/// Центральный каталог zip по концу архива ([`zip_directory`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ZipDir {
    /// Число записей каталога.
    pub entries: u64,
    /// Смещение каталога от начала файла.
    pub cd_offset: u64,
    /// Размер каталога, байт.
    pub cd_size: u64,
}

/// Итог [`zip_directory`] для согласованного конца каталога.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ZipCheck {
    /// Каталог в пределах лимитов и пройден запись за записью: его можно отдать `zip`.
    Fits(ZipDir),
    /// Записей больше `max_entries` (каталог не читался).
    TooManyEntries,
    /// Каталог больше `max_central_dir` (не читался).
    DirTooLarge,
}

/// Наименьшая запись центрального каталога: сигнатура и 42 байта полей, без имени.
const CDFH_MIN: u64 = 46;
/// Запись EOCD64 без расширяемых данных: 12 байт (сигнатура и размер) + 44.
const EOCD64_LEN: u64 = 56;
/// Окно поиска EOCD у `zip` 9.0.5 с запасом: `get_metadata` ищет сигнатуру не раньше
/// `file_len − (size_of::<Zip32CentralDirectoryEnd>() + 4 + 65 536)`; размер структуры в памяти
/// (32 байта на 64-битных целях), а не 22 байта записи — окно `zip` 65 572 байта. Сверяется
/// тестом `eocd_window_covers_zip_search` (при обновлении `zip` перепроверить).
pub(crate) const EOCD_WINDOW: u64 = 65_536 + 128;

const EOCD: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
const LOC64: [u8; 4] = [0x50, 0x4b, 0x06, 0x07];
const EOCD64: [u8; 4] = [0x50, 0x4b, 0x06, 0x06];
const CDFH: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];

fn le16(b: &[u8], at: usize) -> u64 {
    u64::from(u16::from_le_bytes([b[at], b[at + 1]]))
}

fn le32(b: &[u8], at: usize) -> u64 {
    u64::from(u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]))
}

fn le64(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(a)
}

/// Чтение по абсолютному смещению файла: ровно `len` байт или `None`.
type ReadAt<'a> = dyn FnMut(u64, usize) -> Option<Vec<u8>> + 'a;

fn read_exact_at(read_at: &mut ReadAt<'_>, off: u64, len: usize) -> Option<Vec<u8>> {
    read_at(off, len).filter(|b| b.len() == len)
}

/// Поля EOCD64: `(позиция, записей на диске, записей, размер каталога, смещение каталога)`.
type Eocd64 = (u64, u64, u64, u64, u64);

/// EOCD64 по локатору перед EOCD в позиции `pos`: `(позиция EOCD64, записей на диске, записей,
/// размер, смещение каталога)`. `Ok(None)` — локатора нет; `Err` — локатор есть, а EOCD64 не та
/// (не ровно перед локатором, с расширяемыми данными, многотомная).
///
/// Строже `zip` 9.0.5: тот принимает любые расширяемые данные (`record_size + 12 ==
/// позиция локатора − позиция EOCD64`) и копирует их в память целиком — до размера файла. Здесь
/// `record_size` ровно 44 (раунд 4, решено оставить): расширяемые данные (zip64 v2 — шифрование
/// каталога PKWARE) не пишут ни Office/LibreOffice, ни Python `zipfile`, Info-ZIP, 7-Zip, libzip,
/// .NET; а при единственной EOCD в окне ([`zip_directory`]) строгость даёт только `Malformed`,
/// не откат `zip` к другому каталогу.
fn eocd64_at(read_at: &mut ReadAt<'_>, pos: u64) -> Result<Option<Eocd64>, &'static str> {
    let Some(loc_pos) = pos.checked_sub(20) else {
        return Ok(None);
    };
    let Some(l) = read_exact_at(read_at, loc_pos, 20).filter(|l| l[..4] == LOC64) else {
        return Ok(None);
    };
    if le32(&l, 4) != 0 || le32(&l, 16) > 1 {
        return Err("многотомный архив zip64");
    }
    let at = le64(&l, 8);
    if at.checked_add(EOCD64_LEN) != Some(loc_pos) {
        return Err("EOCD64 не перед локатором");
    }
    let r = read_exact_at(read_at, at, EOCD64_LEN as usize).ok_or("нет EOCD64")?;
    if r[..4] != EOCD64 || le64(&r, 4) != EOCD64_LEN - 12 {
        return Err("неверная EOCD64");
    }
    if le32(&r, 16) != 0 || le32(&r, 20) != 0 {
        return Err("многотомный архив zip64");
    }
    Ok(Some((
        at,
        le64(&r, 24),
        le64(&r, 32),
        le64(&r, 40),
        le64(&r, 48),
    )))
}

/// Строгая проверка центрального каталога zip до `ZipArchive` (раунды правок 2–4 задачи 4 M5).
///
/// `zip` 9.0.5 перебирает кандидатов EOCD в окне от конца файла ([`EOCD_WINDOW`]) и молча
/// пропускает несогласованные, а если не разобрался каталог выбранной EOCD (битое extra, имя и
/// т. п.) — откатывается к более ранней. Догонять все причины сбоя разбора бесполезно, поэтому
/// класс атак закрыт безотказным правилом (раунд 4):
/// 1. **В окне ровно одна сигнатура EOCD** `PK\x05\x06`. Любая вторая — отказ архива. С
///    `ArchiveOffset::Known(0)` EOCD64 `zip` берёт только по локатору перед этой EOCD, так что
///    других кандидатов у него нет: при сбое разбора каталога он возвращает ошибку, а не
///    откатывается. Цена — ложный отказ при случайной `PK\x05\x06` в последних 64 КиБ сжатых
///    данных (≈ 2¹⁶·2⁻³² ≈ 1,5·10⁻⁵ на файл); Office, LibreOffice, Google Docs и Python
///    `zipfile` второй EOCD не пишут.
/// 2. **Эта EOCD согласована целиком**: кончается с комментарием ровно на конце файла; один
///    диск; записей на диске = всего; при локаторе за 20 байт до EOCD — EOCD64 ровно перед
///    локатором ([`eocd64_at`]), поля каталога из неё, а поля EOCD, кроме `0xFFFF…`, совпадают с
///    ней (так пишет и Python `zipfile` с принудительным zip64-концом при реальных полях EOCD);
///    каталог кончается ровно у EOCD (EOCD64): `cd_offset + cd_size`; `entries × 46 ≤ cd_size`;
///    число записей и размер каталога — против `max_entries`/`max_central_dir`.
/// 3. **Каталог пройден запись за записью** (только если в пределах лимитов): сигнатура
///    каждой записи, имя, extra и комментарий внутри каталога, смещение локального заголовка до
///    каталога; сумма длин записей — ровно `cd_size`, записей — ровно `entries`.
///
/// Открывать архив — только [`open_archive`] (`ArchiveOffset::Known(0)`: без сканирования файла
/// в поисках сигнатур). Архивы с префиксом (самораспаковывающиеся) не проходят слой 1.
/// `tail` — конец файла длиной `file_len` со смещения `tail_start`; `read_at` — байты по
/// абсолютному смещению. `Err` — причина без содержимого файла.
pub(crate) fn zip_directory(
    tail: &[u8],
    tail_start: u64,
    file_len: u64,
    limits: &crate::Limits,
    read_at: &mut ReadAt<'_>,
) -> Result<ZipCheck, &'static str> {
    let max_entries = u64::from(limits.max_entries);
    let max_dir = limits.max_central_dir;
    let window = file_len.saturating_sub(EOCD_WINDOW);
    let lo = usize::try_from(window.saturating_sub(tail_start))
        .unwrap_or(usize::MAX)
        .min(tail.len());
    // правило 1: в окне поиска `zip` ровно одна сигнатура EOCD (в том числе неполная у конца
    // файла) — откатываться `zip` некуда
    let mut sigs = (lo..tail.len().saturating_sub(3))
        .rev()
        .filter(|&i| tail[i..i + 4] == EOCD);
    let rel = sigs.next().ok_or("нет конца центрального каталога")?;
    if sigs.next().is_some() {
        return Err("вторая сигнатура конца каталога в окне поиска zip");
    }
    if rel + 22 > tail.len() {
        return Err("конец каталога обрезан");
    }
    // правило 2: единственная EOCD согласована целиком
    let e = &tail[rel..rel + 22];
    let pos = tail_start + rel as u64;
    if pos + 22 + le16(e, 20) != file_len {
        return Err("конец каталога не на конце файла");
    }
    if le16(e, 4) != 0 || le16(e, 6) != 0 {
        return Err("многотомный архив");
    }
    let (n_disk, n, size, off) = (le16(e, 8), le16(e, 10), le32(e, 12), le32(e, 16));
    let (entries, entries_disk, cd_size, cd_offset, end) = match eocd64_at(read_at, pos)? {
        None => (n, n_disk, size, off, pos),
        Some((at, n64_disk, n64, size64, off64)) => {
            // поля EOCD, кроме «смотри zip64», совпадают с EOCD64
            let agree = |v32: u64, sentinel: u64, v64: u64| v32 == sentinel || v32 == v64;
            if !(agree(n_disk, 0xFFFF, n64_disk)
                && agree(n, 0xFFFF, n64)
                && agree(size, 0xFFFF_FFFF, size64)
                && agree(off, 0xFFFF_FFFF, off64))
            {
                return Err("поля EOCD и EOCD64 расходятся");
            }
            (n64, n64_disk, size64, off64, at)
        }
    };
    if entries_disk != entries {
        return Err("число записей на диске и всего различается");
    }
    if cd_offset.checked_add(cd_size) != Some(end) {
        return Err("каталог не кончается у EOCD (префикс или подделка)");
    }
    if entries.checked_mul(CDFH_MIN).is_none_or(|m| m > cd_size) {
        return Err("записей больше, чем вмещает каталог");
    }
    if entries > max_entries {
        return Ok(ZipCheck::TooManyEntries);
    }
    if cd_size > max_dir {
        return Ok(ZipCheck::DirTooLarge);
    }
    // правило 3: запись за записью
    let mut at = cd_offset;
    for _ in 0..entries {
        let h = read_exact_at(read_at, at, CDFH_MIN as usize).ok_or("запись каталога обрезана")?;
        if h[..4] != CDFH {
            return Err("нет сигнатуры записи каталога");
        }
        let next = at + CDFH_MIN + le16(&h, 28) + le16(&h, 30) + le16(&h, 32);
        if next > end {
            return Err("запись каталога выходит за каталог");
        }
        let lfh = le32(&h, 42);
        if lfh != 0xFFFF_FFFF && lfh >= cd_offset {
            return Err("локальный заголовок не перед каталогом");
        }
        at = next;
    }
    if at != end {
        return Err("записи каталога не заполняют его ровно");
    }
    Ok(ZipCheck::Fits(ZipDir {
        entries,
        cd_offset,
        cd_size,
    }))
}

/// [`zip_directory`] архива в памяти.
pub(crate) fn zip_directory_of(b: &[u8], limits: &crate::Limits) -> Result<ZipCheck, &'static str> {
    let mut read_at = |off: u64, len: usize| {
        let off = usize::try_from(off).ok()?;
        b.get(off..off.checked_add(len)?).map(<[u8]>::to_vec)
    };
    zip_directory(b, 0, b.len() as u64, limits, &mut read_at)
}

/// Единственный способ открыть недоверенный zip в `zam-docs` — после [`zip_directory`]:
/// `ArchiveOffset::Known(0)`, без режима `Detect` (он сканирует весь файл и хранит позицию каждой
/// сигнатуры записи каталога: 80 МБ наполнителя давали пик 256 МиБ).
pub(crate) fn open_archive<R: Read + Seek>(r: R) -> zip::result::ZipResult<ZipArchive<R>> {
    ZipArchive::with_config(
        zip::read::Config {
            archive_offset: zip::read::ArchiveOffset::Known(0),
        },
        r,
    )
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
                .map_err(|_| DocError::Malformed(format!("encoding ({part}): invalid utf-8")))
        }
        Err(_) => Err(DocError::Malformed(format!(
            "encoding ({part}): invalid utf-8"
        ))),
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
                    return Err(DocError::Malformed(format!("xml ({}): {e}", self.part)));
                }
            };
            match ev {
                Event::DocType(_) => {
                    self.done = true;
                    return Err(DocError::Malformed(format!(
                        "xml ({}): <!DOCTYPE> запрещён",
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
                                "xml ({}): неизвестная сущность &{};",
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
///
/// Каждая законченная строка сразу дописывается в итоговый текст, а ячейка экранируется при
/// записи: в памяти одна копия таблицы плюс текущая строка.
#[derive(Default)]
pub(crate) struct Table {
    out: String,
    rows: usize,
    row: Vec<String>,
    cell: String,
}

impl Table {
    /// Текст в текущую ячейку (абзацы ячейки — через пробел): перевод строки → пробел,
    /// `|` → `\|`.
    pub fn text(&mut self, s: &str) {
        let s = s.trim();
        if s.is_empty() {
            return;
        }
        if !self.cell.is_empty() {
            self.cell.push(' ');
        }
        self.cell.reserve(s.len());
        for ch in s.chars() {
            match ch {
                '\n' => self.cell.push(' '),
                '|' => self.cell.push_str("\\|"),
                ch => self.cell.push(ch),
            }
        }
    }

    pub fn end_cell(&mut self) {
        let cell = std::mem::take(&mut self.cell);
        self.row.push(cell);
    }

    pub fn end_row(&mut self) {
        let row = std::mem::take(&mut self.row);
        if !row.iter().any(|c| !c.is_empty()) {
            return;
        }
        if self.rows > 0 {
            self.out.push('\n');
        }
        self.out.push('|');
        for c in &row {
            self.out.push(' ');
            self.out.push_str(c);
            self.out.push_str(" |");
        }
        if self.rows == 0 {
            self.out.push_str("\n|");
            for _ in &row {
                self.out.push_str(" --- |");
            }
        }
        self.rows += 1;
    }

    pub fn render(self) -> String {
        self.out
    }

    /// Таблица, оборванная концом части: открытые ячейка и строка закрываются.
    pub fn render_partial(mut self) -> String {
        if !self.cell.is_empty() {
            self.end_cell();
        }
        self.end_row();
        self.out
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// Пустой архив: EOCD без записей в начале файла длиной `len`, дальше нули (`zip`
    /// допускает «мусор после комментария» — EOCD годна, если кончается не позже конца файла).
    fn opens_with_eocd_at_start(len: usize) -> bool {
        let mut b = vec![0u8; len];
        b[..4].copy_from_slice(&EOCD);
        open_archive(Cursor::new(b)).is_ok()
    }

    /// Окно [`EOCD_WINDOW`] покрывает всё окно поиска EOCD у `zip` (сейчас 9.0.5): иначе
    /// сигнатура вне нашего окна, но в окне `zip` ушла бы от правила «ровно одна EOCD».
    /// Окно `zip` замеряется на деле: EOCD в начале файла длиной `W` ещё находится, `W + 1` —
    /// уже нет. При обновлении `zip` перепроверить (`format/find_central_directory.rs`,
    /// `read/zip_archive.rs::get_metadata`).
    #[test]
    fn eocd_window_covers_zip_search() {
        // 9.0.5: size_of::<Zip32CentralDirectoryEnd>() = 32 на 64-битных целях + 4 + 65 536
        let zip_window = if cfg!(target_pointer_width = "64") {
            32 + 4 + 65_536
        } else {
            24 + 4 + 65_536
        };
        assert!(opens_with_eocd_at_start(22));
        assert!(
            opens_with_eocd_at_start(zip_window),
            "окно zip шире замера: перепроверить EOCD_WINDOW"
        );
        assert!(
            !opens_with_eocd_at_start(zip_window + 1),
            "окно zip шире ожидаемого: перепроверить EOCD_WINDOW"
        );
        assert!(EOCD_WINDOW >= zip_window as u64);
    }

    /// Правило 1: вторая сигнатура EOCD в окне — отказ, даже если она неполная или в комментарии.
    #[test]
    fn second_eocd_signature_in_window_rejected() {
        let limits = crate::Limits::default();
        let mut empty = EOCD.to_vec();
        empty.resize(22, 0);
        assert_eq!(
            zip_directory_of(&empty, &limits),
            Ok(ZipCheck::Fits(ZipDir {
                entries: 0,
                cd_offset: 0,
                cd_size: 0
            }))
        );
        // сигнатура перед EOCD (в «данных») и в комментарии
        let mut before = EOCD.to_vec();
        before.extend_from_slice(&empty);
        let mut comment = empty.clone();
        comment[20] = 4;
        comment.extend_from_slice(&EOCD);
        for b in [before, comment] {
            assert_eq!(
                zip_directory_of(&b, &limits),
                Err("вторая сигнатура конца каталога в окне поиска zip")
            );
        }
    }
}
