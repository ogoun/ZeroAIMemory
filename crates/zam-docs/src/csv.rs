//! CSV (RFC 4180, с поблажками): кодировка, разделитель, записи — один лист [`CSV_SHEET`].
//!
//! Кодировка: BOM UTF-8 снимается; BOM UTF-16 — раскодирование целиком (копия в пределах
//! `max_unpacked`); UTF-8 (неполный символ в конце или до 0,1 % неверных байт — замена U+FFFD)
//! — как есть; иначе Windows-1251 (`encoding_rs`, по кускам поля — без копии файла).
//! Разделитель — `,`, `;` или табуляция: тот, что встречается одинаковое ненулевое число раз в
//! наибольшем числе первых записей. Поля в кавычках (`""` — кавычка) могут содержать
//! разделители и переводы строк; кавычка внутри поля без кавычек и текст после закрывающей —
//! как есть. Поле передаётся в [`Feed`] срезами входа: длинная строка не копируется.

use std::borrow::Cow;

use encoding_rs::{Encoding, UTF_16BE, UTF_16LE, WINDOWS_1251};

use crate::DocError;
use crate::ctx::Ctx;
use crate::sheet::{CSV_SHEET, Feed, Sink};

/// Сколько первых записей смотреть при выборе разделителя.
const SNIFF_RECORDS: usize = 20;
/// Сколько байт смотреть при выборе разделителя.
const SNIFF_BYTES: usize = 64 * 1024;
/// Кусок поля Windows-1251 при раскодировании.
const DECODE_STEP: usize = 64 * 1024;

pub(crate) fn read<S: Sink>(
    bytes: &[u8],
    feed: &mut Feed<'_, S>,
    ctx: &mut Ctx,
) -> Result<(), DocError> {
    let (text, enc) = decode(bytes, ctx);
    let b: &[u8] = &text;
    let delim = delimiter(&b[..b.len().min(SNIFF_BYTES)]);
    if !feed.sheet(CSV_SHEET, ctx) {
        return Ok(());
    }
    let mut i = 0usize;
    while i < b.len() {
        if !feed.live(ctx) || !ctx.tick() {
            break;
        }
        feed.row(None, ctx);
        // поля записи
        loop {
            if !ctx.tick() {
                break;
            }
            feed.cell(None, ctx);
            if b.get(i) == Some(&b'"') {
                i += 1;
                loop {
                    match b[i..].iter().position(|&c| c == b'"') {
                        None => {
                            emit(feed, &b[i..], enc, ctx);
                            i = b.len();
                            break;
                        }
                        Some(q) => {
                            emit(feed, &b[i..i + q], enc, ctx);
                            i += q + 1;
                            if b.get(i) == Some(&b'"') {
                                emit(feed, b"\"", enc, ctx);
                                i += 1;
                            } else {
                                break;
                            }
                        }
                    }
                }
            }
            // поле без кавычек или хвост после закрывающей кавычки
            let end = b[i..]
                .iter()
                .position(|&c| c == delim || c == b'\n' || c == b'\r')
                .map_or(b.len(), |k| i + k);
            emit(feed, &b[i..end], enc, ctx);
            feed.end_cell(ctx);
            i = end;
            match b.get(i) {
                Some(&c) if c == delim => i += 1,
                Some(b'\r') => {
                    i += 1;
                    if b.get(i) == Some(&b'\n') {
                        i += 1;
                    }
                    break;
                }
                Some(b'\n') => {
                    i += 1;
                    break;
                }
                _ => break,
            }
            if !feed.live(ctx) {
                break;
            }
        }
        feed.end_row(ctx);
    }
    Ok(())
}

/// Кодировка полей: UTF-8 (единичные неверные байты — U+FFFD) или однобайтовая Windows-1251.
#[derive(Clone, Copy)]
enum Enc {
    Utf8,
    Single(&'static Encoding),
}

/// Байты как UTF-8 (без копии, BOM снят) или копия из UTF-16; иначе — Windows-1251.
fn decode<'b>(bytes: &'b [u8], ctx: &mut Ctx) -> (Cow<'b, [u8]>, Enc) {
    if let Some(b) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        let enc = if mostly_utf8(b) {
            Enc::Utf8
        } else {
            Enc::Single(WINDOWS_1251)
        };
        return (Cow::Borrowed(b), enc);
    }
    let utf16 = match bytes {
        [0xFF, 0xFE, ..] => Some(UTF_16LE),
        [0xFE, 0xFF, ..] => Some(UTF_16BE),
        _ => None,
    };
    if let Some(enc) = utf16 {
        // UTF-16 → UTF-8: до 1,5 байта на байт входа; копия — в пределах `max_unpacked`
        let mut src = &bytes[2..];
        let cap = ctx.limits.max_unpacked / 3 * 2;
        if src.len() as u64 > cap {
            src = &src[..cap as usize & !1];
            ctx.truncated("max_unpacked");
        }
        let s = enc.decode_without_bom_handling(src).0.into_owned();
        return (Cow::Owned(s.into_bytes()), Enc::Utf8);
    }
    if mostly_utf8(bytes) {
        (Cow::Borrowed(bytes), Enc::Utf8)
    } else {
        (Cow::Borrowed(bytes), Enc::Single(WINDOWS_1251))
    }
}

/// UTF-8 с единичными ошибками: неполный символ в конце файла (обрезка) или неверных байт не
/// больше 0,1 % — это UTF-8 с заменой, а не другая кодировка (в Windows-1251 кириллица почти
/// вся — неверный UTF-8).
fn mostly_utf8(b: &[u8]) -> bool {
    let Err(e) = std::str::from_utf8(b) else {
        return true;
    };
    if e.error_len().is_none() {
        return true;
    }
    let bad: usize = b.utf8_chunks().map(|c| c.invalid().len()).sum();
    bad.saturating_mul(1000) <= b.len()
}

/// Кусок поля — в приёмник; срез режется только по ASCII, поэтому верные символы UTF-8 целы.
fn emit<S: Sink>(feed: &mut Feed<'_, S>, piece: &[u8], enc: Enc, ctx: &mut Ctx) {
    match enc {
        Enc::Utf8 => {
            for c in piece.utf8_chunks() {
                feed.text(c.valid(), ctx);
                if !c.invalid().is_empty() {
                    feed.text("\u{FFFD}", ctx);
                }
            }
        }
        Enc::Single(e) => {
            for chunk in piece.chunks(DECODE_STEP) {
                let s = e.decode_without_bom_handling(chunk).0;
                feed.text(&s, ctx);
                if !feed.live(ctx) {
                    return;
                }
            }
        }
    }
}

/// Разделитель по первым записям (кавычки учитываются).
fn delimiter(b: &[u8]) -> u8 {
    const CANDIDATES: [u8; 3] = *b",;\t";
    let mut counts: Vec<[usize; 3]> = Vec::new();
    let mut cur = [0usize; 3];
    let mut quoted = false;
    let mut any = false;
    for &c in b {
        if counts.len() >= SNIFF_RECORDS {
            break;
        }
        match c {
            b'"' => quoted = !quoted,
            // конец записи — `\n`, `\r\n` или одиночный `\r`
            b'\n' | b'\r' if !quoted => {
                if any {
                    counts.push(cur);
                }
                cur = [0; 3];
                any = false;
                continue;
            }
            _ if !quoted => {
                if let Some(k) = CANDIDATES.iter().position(|&d| d == c) {
                    cur[k] += 1;
                }
            }
            _ => {}
        }
        any = true;
    }
    if any && counts.len() < SNIFF_RECORDS {
        counts.push(cur);
    }
    // (записей с самым частым ненулевым числом, само число) — больше лучше
    let mut best = (0usize, 0usize, b',');
    for (k, &d) in CANDIDATES.iter().enumerate() {
        let mut freq: Vec<(usize, usize)> = Vec::new();
        for c in counts.iter().map(|c| c[k]).filter(|&n| n > 0) {
            match freq.iter_mut().find(|(n, _)| *n == c) {
                Some((_, f)) => *f += 1,
                None => freq.push((c, 1)),
            }
        }
        if let Some(&(n, f)) = freq.iter().max_by_key(|&&(n, f)| (f, n))
            && (f, n) > (best.0, best.1)
        {
            best = (f, n, d);
        }
    }
    best.2
}
