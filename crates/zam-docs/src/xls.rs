//! XLS (BIFF8 — Excel 97–2003, BIFF5 — Excel 5/95): свой разбор записей потока `Workbook`.
//!
//! Не через `calamine`: он разбирает все листы при открытии и копирует строку `SST` в каждую
//! ячейку `LABELSST`, а сетку листа строит по границам ячеек (замер — журнал этапа, задача 2).
//! Здесь общие строки хранятся один раз ([`Strings`]), а ячейки идут в [`Feed`] по мере
//! чтения записей. Берутся текст (`LABELSST`, `LABEL`, `RSTRING`), числа (`NUMBER`, `RK`,
//! `MULRK`), логические и ошибки (`BOOLERR`), сохранённые результаты формул (`FORMULA`,
//! `STRING`); даты остаются числами. Зашифрованная книга (`FILEPASS`) — `skipped: Workbook`.

use std::time::Instant;

use encoding_rs::Encoding;

use crate::DocError;
use crate::cfb::Cfb;
use crate::ctx::Ctx;
use crate::sheet::{Feed, Sink, Strings, number};

const BOF: u16 = 0x0809;
const EOF: u16 = 0x000A;
const FILEPASS: u16 = 0x002F;
const CODEPAGE: u16 = 0x0042;
const BOUNDSHEET: u16 = 0x0085;
const SST: u16 = 0x00FC;
const CONTINUE: u16 = 0x003C;
const LABELSST: u16 = 0x00FD;
const LABEL: u16 = 0x0204;
const RSTRING: u16 = 0x00D6;
const NUMBER: u16 = 0x0203;
const RK: u16 = 0x027E;
const MULRK: u16 = 0x00BD;
const BOOLERR: u16 = 0x0205;
const FORMULA: u16 = 0x0006;
const STRING: u16 = 0x0207;

fn bad(why: &str) -> DocError {
    DocError::Malformed(format!("xls: {why}"))
}

/// Записи BIFF подряд: (тип, данные). Запись за концом потока — конец.
#[derive(Clone)]
struct Records<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Iterator for Records<'a> {
    type Item = (u16, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let h = self.b.get(self.at..self.at + 4)?;
        let ty = u16::from_le_bytes([h[0], h[1]]);
        let len = usize::from(u16::from_le_bytes([h[2], h[3]]));
        let data = self.b.get(self.at + 4..self.at + 4 + len)?;
        self.at += 4 + len;
        Some((ty, data))
    }
}

impl<'a> Records<'a> {
    fn peek(&self) -> Option<u16> {
        let h = self.b.get(self.at..self.at + 2)?;
        Some(u16::from_le_bytes([h[0], h[1]]))
    }
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// Книга: версия BIFF, кодировка BIFF5, листы (смещение, имя), общие строки.
struct Book {
    biff8: bool,
    enc: &'static Encoding,
    sheets: Vec<(usize, String)>,
    strings: Strings,
}

pub(crate) fn read<S: Sink>(
    bytes: &[u8],
    feed: &mut Feed<'_, S>,
    ctx: &mut Ctx,
) -> Result<(), DocError> {
    let cfb = Cfb::open(bytes, ctx)?;
    if cfb.find(&["EncryptedPackage"]).is_some() {
        // зашифрованный OOXML лежит в OLE: пароль неизвестен
        ctx.warn("skipped: EncryptedPackage".into());
        return Ok(());
    }
    let Some(i) = cfb.find(&["Workbook", "Book"]) else {
        return Err(bad("нет потока Workbook"));
    };
    let wb = cfb.read(i, ctx)?;
    drop(cfb);
    let Some(book) = globals(&wb, ctx)? else {
        return Ok(());
    };
    for (at, name) in &book.sheets {
        if ctx.stopped() {
            break;
        }
        if !feed.sheet(name, ctx) {
            continue;
        }
        sheet(&wb, *at, &book, feed, ctx);
        feed.end_sheet(ctx);
    }
    Ok(())
}

/// Глобальный поток книги до первого `EOF`; `None` — книга зашифрована.
fn globals(wb: &[u8], ctx: &mut Ctx) -> Result<Option<Book>, DocError> {
    let mut recs = Records { b: wb, at: 0 };
    let Some((BOF, bof)) = recs.next() else {
        return Err(bad("нет BOF"));
    };
    let biff8 = match u16_at(bof, 0) {
        Some(0x0600) => true,
        Some(0x0500) => false,
        _ => return Err(bad("версия BIFF не поддерживается")),
    };
    let mut book = Book {
        biff8,
        enc: encoding_rs::WINDOWS_1252,
        sheets: Vec::new(),
        strings: Strings::default(),
    };
    while let Some((ty, data)) = recs.next() {
        if !ctx.tick() {
            break;
        }
        match ty {
            EOF => break,
            FILEPASS => {
                ctx.warn("skipped: Workbook".into());
                return Ok(None);
            }
            CODEPAGE => {
                if let Some(cp) = u16_at(data, 0) {
                    book.enc = codepage(cp);
                }
            }
            BOUNDSHEET => {
                // только листы с ячейками (0); макросы, диаграммы, модули VB — нет
                let (Some(at), Some(&kind)) = (u32_at(data, 0), data.get(5)) else {
                    continue;
                };
                if kind != 0 {
                    continue;
                }
                if book.sheets.len() as u64 >= u64::from(ctx.limits.max_entries) {
                    ctx.truncated("max_entries");
                    continue;
                }
                let name = short_string(data.get(6..).unwrap_or_default(), &book);
                book.sheets.push((at as usize, name));
            }
            SST if biff8 => {
                let first = data.get(8..).unwrap_or_default();
                recs = read_sst(recs.clone(), first, &mut book.strings, ctx);
                book.strings.shrink();
            }
            _ => {}
        }
    }
    Ok(Some(book))
}

/// Строки листа от его `BOF` до парного `EOF`; вложенные потоки (диаграммы) пропускаются.
fn sheet<S: Sink>(wb: &[u8], at: usize, book: &Book, feed: &mut Feed<'_, S>, ctx: &mut Ctx) {
    let mut recs = Records { b: wb, at };
    if !matches!(recs.next(), Some((BOF, _))) {
        return;
    }
    let mut depth = 1u32;
    let mut num = String::new();
    let mut text = String::new();
    // ячейка формулы, чей строковый результат — в следующей записи `STRING`
    let mut formula: Option<(u32, u32)> = None;
    for (ty, d) in recs {
        if !feed.live(ctx) || !ctx.tick() {
            return;
        }
        match ty {
            BOF => depth += 1,
            EOF => {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
            _ if depth != 1 => {}
            LABELSST => {
                if let (Some((r, c)), Some(i)) = (rc(d), u32_at(d, 6)) {
                    feed.put_at(r, c, book.strings.get(i as usize), ctx);
                }
            }
            LABEL | RSTRING => {
                if let Some((r, c)) = rc(d) {
                    long_string(d.get(6..).unwrap_or_default(), book, &mut text);
                    feed.put_at(r, c, &text, ctx);
                }
            }
            NUMBER => {
                if let (Some((r, c)), Some(v)) = (rc(d), d.get(6..14)) {
                    number(
                        f64::from_le_bytes(v.try_into().unwrap_or_default()),
                        &mut num,
                    );
                    feed.put_at(r, c, &num, ctx);
                }
            }
            RK => {
                if let (Some((r, c)), Some(v)) = (rc(d), u32_at(d, 6)) {
                    number(rk(v), &mut num);
                    feed.put_at(r, c, &num, ctx);
                }
            }
            MULRK => {
                let (Some((r, c0)), true) = (rc(d), d.len() >= 6) else {
                    continue;
                };
                let n = (d.len() - 6) / 6;
                for k in 0..n {
                    if let Some(v) = u32_at(d, 4 + k * 6 + 2) {
                        number(rk(v), &mut num);
                        feed.put_at(r, c0 + k as u32, &num, ctx);
                    }
                }
            }
            BOOLERR => {
                if let (Some((r, c)), Some(&v), Some(&err)) = (rc(d), d.get(6), d.get(7)) {
                    feed.put_at(r, c, bool_err(v, err != 0), ctx);
                }
            }
            FORMULA => {
                let (Some((r, c)), Some(v)) = (rc(d), d.get(6..14)) else {
                    continue;
                };
                formula = None;
                if v[6..8] == [0xFF, 0xFF] {
                    match v[0] {
                        0 => formula = Some((r, c)),
                        1 => feed.put_at(r, c, bool_err(v[2], false), ctx),
                        2 => feed.put_at(r, c, bool_err(v[2], true), ctx),
                        _ => {}
                    }
                } else {
                    number(
                        f64::from_le_bytes(v.try_into().unwrap_or_default()),
                        &mut num,
                    );
                    feed.put_at(r, c, &num, ctx);
                }
            }
            STRING => {
                if let Some((r, c)) = formula.take() {
                    long_string(d, book, &mut text);
                    feed.put_at(r, c, &text, ctx);
                }
            }
            _ => {}
        }
    }
}

/// Строка (с 1) и столбец (с 0) записи ячейки.
fn rc(d: &[u8]) -> Option<(u32, u32)> {
    Some((u32::from(u16_at(d, 0)?) + 1, u32::from(u16_at(d, 2)?)))
}

/// Число RK: целое со сдвигом или старшие 30 бит `f64`; бит 0 — деление на 100.
fn rk(v: u32) -> f64 {
    let x = if v & 2 != 0 {
        f64::from((v as i32) >> 2)
    } else {
        f64::from_bits(u64::from(v & 0xFFFF_FFFC) << 32)
    };
    if v & 1 != 0 { x / 100.0 } else { x }
}

fn bool_err(v: u8, err: bool) -> &'static str {
    match (err, v) {
        (false, 0) => "FALSE",
        (false, _) => "TRUE",
        (true, 0x00) => "#NULL!",
        (true, 0x07) => "#DIV/0!",
        (true, 0x0F) => "#VALUE!",
        (true, 0x17) => "#REF!",
        (true, 0x1D) => "#NAME?",
        (true, 0x24) => "#NUM!",
        _ => "#N/A",
    }
}

/// Символы BIFF8: 1 байт (Latin-1, `high = false`) или UTF-16LE на символ.
fn push_chars(b: &[u8], high: bool, out: &mut String) {
    if high {
        let units = b.as_chunks::<2>().0.iter().map(|&p| u16::from_le_bytes(p));
        out.extend(char::decode_utf16(units).map(|c| c.unwrap_or('\u{FFFD}')));
    } else {
        out.extend(b.iter().map(|&c| char::from(c)));
    }
}

/// Имя листа `BOUNDSHEET`: BIFF8 — `cch` u8, флаги, символы; BIFF5 — `cch` u8 и байты.
fn short_string(d: &[u8], book: &Book) -> String {
    let mut out = String::new();
    let Some(&cch) = d.first() else {
        return out;
    };
    let cch = usize::from(cch);
    if book.biff8 {
        let high = d.get(1).is_some_and(|f| f & 1 != 0);
        let w = if high { 2 } else { 1 };
        let end = (2 + cch * w).min(d.len());
        push_chars(d.get(2..end).unwrap_or_default(), high, &mut out);
    } else {
        let end = (1 + cch).min(d.len());
        out = book
            .enc
            .decode_without_bom_handling(&d[1..end])
            .0
            .into_owned();
    }
    out
}

/// Строка ячейки (`LABEL`, `STRING`): `cch` u16, в BIFF8 — флаги и символы, в BIFF5 — байты в
/// кодовой странице книги. Длина — не больше данных записи.
fn long_string(d: &[u8], book: &Book, out: &mut String) {
    out.clear();
    let Some(cch) = u16_at(d, 0) else {
        return;
    };
    let cch = usize::from(cch);
    if book.biff8 {
        let Some(&flags) = d.get(2) else {
            return;
        };
        let high = flags & 1 != 0;
        let mut at = 3;
        if flags & 8 != 0 {
            at += 2;
        }
        if flags & 4 != 0 {
            at += 4;
        }
        let w = if high { 2 } else { 1 };
        let end = (at + cch * w).min(d.len());
        push_chars(d.get(at..end).unwrap_or_default(), high, out);
    } else {
        let end = (2 + cch).min(d.len());
        out.push_str(&book.enc.decode_without_bom_handling(&d[2..end]).0);
    }
}

/// Чтение `SST` с продолжениями `CONTINUE`: на границе записи внутри символов строки первым
/// байтом продолжения идут флаги (ширина символов меняется); остальное — сплошные байты.
///
/// Продолжения берутся лениво из курсора записей: память O(1) при любом числе `CONTINUE`
/// (в том числе пустых); срок сверяется раз в 1024 перехода.
struct Segs<'a> {
    recs: Records<'a>,
    cur: &'a [u8],
    pos: usize,
    deadline: Instant,
    steps: u64,
    expired: bool,
}

impl<'a> Segs<'a> {
    /// Следующая запись `CONTINUE`; `None` — её нет, она за концом потока или истёк срок.
    fn next_seg(&mut self) -> Option<()> {
        self.steps += 1;
        if self.steps.is_multiple_of(1024) && Instant::now() >= self.deadline {
            self.expired = true;
            return None;
        }
        match self.recs.peek() {
            Some(CONTINUE) => {
                let (_, d) = self.recs.next()?;
                self.cur = d;
                self.pos = 0;
                Some(())
            }
            _ => None,
        }
    }

    /// Следующий байт сплошного потока.
    fn byte(&mut self) -> Option<u8> {
        loop {
            if let Some(&b) = self.cur.get(self.pos) {
                self.pos += 1;
                return Some(b);
            }
            self.next_seg()?;
        }
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes([self.byte()?, self.byte()?]))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes([
            self.byte()?,
            self.byte()?,
            self.byte()?,
            self.byte()?,
        ]))
    }

    fn skip(&mut self, mut n: usize) -> Option<()> {
        loop {
            let k = (self.cur.len() - self.pos).min(n);
            n -= k;
            self.pos += k;
            if n == 0 {
                return Some(());
            }
            self.next_seg()?;
        }
    }

    /// `n` символов строки шириной `high`; на новой записи — новый байт флагов.
    fn chars(
        &mut self,
        mut n: usize,
        mut high: bool,
        f: &mut impl FnMut(&[u8], bool),
    ) -> Option<()> {
        while n > 0 {
            if self.pos >= self.cur.len() {
                self.next_seg()?;
                high = self.cur.first()? & 1 != 0;
                self.pos = 1;
                continue;
            }
            let w = if high { 2 } else { 1 };
            let k = ((self.cur.len() - self.pos) / w).min(n);
            if k == 0 {
                // половина символа UTF-16 на границе записи — файл повреждён
                return None;
            }
            f(&self.cur[self.pos..self.pos + k * w], high);
            self.pos += k * w;
            n -= k;
        }
        Some(())
    }
}

/// Строки `SST` (данные первой записи — `first`, продолжения — из `recs`); возвращает курсор
/// после прочитанных продолжений.
fn read_sst<'a>(
    recs: Records<'a>,
    first: &'a [u8],
    strings: &mut Strings,
    ctx: &mut Ctx,
) -> Records<'a> {
    let mut r = Segs {
        recs,
        cur: first,
        pos: 0,
        deadline: ctx.deadline(),
        steps: 0,
        expired: false,
    };
    let mut tmp = String::new();
    loop {
        if !ctx.tick() {
            break;
        }
        let Some(cch) = r.u16() else {
            break;
        };
        let Some(flags) = r.byte() else {
            break;
        };
        let runs = if flags & 8 != 0 { r.u16() } else { Some(0) };
        let ext = if flags & 4 != 0 { r.u32() } else { Some(0) };
        let (Some(runs), Some(ext)) = (runs, ext) else {
            break;
        };
        if !strings.begin(ctx) {
            break;
        }
        let ok = r.chars(usize::from(cch), flags & 1 != 0, &mut |b, high| {
            tmp.clear();
            push_chars(b, high, &mut tmp);
            strings.push(&tmp, ctx);
        });
        strings.end();
        if ok.is_none() || r.skip(usize::from(runs) * 4).is_none() || r.skip(ext as usize).is_none()
        {
            break;
        }
    }
    if r.expired {
        ctx.stop("timeout_ms");
    }
    r.recs
}

/// Кодовая страница BIFF5 → кодировка; неизвестная — Windows-1252.
fn codepage(cp: u16) -> &'static Encoding {
    use encoding_rs as e;
    match cp {
        866 => e::IBM866,
        874 => e::WINDOWS_874,
        932 => e::SHIFT_JIS,
        936 => e::GBK,
        949 => e::EUC_KR,
        950 => e::BIG5,
        1250 => e::WINDOWS_1250,
        1251 => e::WINDOWS_1251,
        1253 => e::WINDOWS_1253,
        1254 => e::WINDOWS_1254,
        1255 => e::WINDOWS_1255,
        1256 => e::WINDOWS_1256,
        1257 => e::WINDOWS_1257,
        1258 => e::WINDOWS_1258,
        10000 => e::MACINTOSH,
        10007 => e::X_MAC_CYRILLIC,
        20866 => e::KOI8_R,
        21866 => e::KOI8_U,
        _ => e::WINDOWS_1252,
    }
}
