//! Код в тексте Markdown без разбора comrak: блоки в ограждениях (```` ``` ````, `~~~`) и
//! инлайн-код `` `…` ``. Одна логика на сегментацию предложений ([`crate::split_sentences`]:
//! внутри кода границ нет) и маску прозы ([`prose_mask`], M6.6 §6.1: имена и псевдонимы
//! терминов в коде не ищутся). Блоки с отступом в 4 пробела кодом не считаются.

use std::ops::Range;

/// Строка: содержимое и перевод строки после него (может быть пустым).
pub(crate) struct Line {
    pub(crate) content: Range<usize>,
    pub(crate) eol: Range<usize>,
}

/// Строки с концами `\n`, `\r\n` и одиночным `\r` (как в [`crate::LineIndex`]).
pub(crate) fn lines_of(text: &str) -> Vec<Line> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < b.len() {
        let eol_len = match b[i] {
            b'\n' => 1,
            b'\r' if b.get(i + 1) == Some(&b'\n') => 2,
            b'\r' => 1,
            _ => 0,
        };
        if eol_len > 0 {
            out.push(Line {
                content: start..i,
                eol: i..i + eol_len,
            });
            i += eol_len;
            start = i;
        } else {
            i += 1;
        }
    }
    out.push(Line {
        content: start..b.len(),
        eol: b.len()..b.len(),
    });
    out
}

/// Ограждение кода: символ и длина серии, если строка открывает или закрывает блок.
pub(crate) fn fence_of(line: &str) -> Option<(u8, usize)> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    let c = *t.as_bytes().first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let n = t.bytes().take_while(|&b| b == c).count();
    (n >= 3).then_some((c, n))
}

/// Для каждой строки — принадлежит ли она fenced-блоку (включая ограждения). Незакрытый блок
/// идёт до конца текста.
pub(crate) fn fenced_lines(text: &str, lines: &[Line]) -> Vec<bool> {
    let mut out = vec![false; lines.len()];
    let mut open: Option<(u8, usize)> = None;
    for (i, l) in lines.iter().enumerate() {
        let s = &text[l.content.clone()];
        match open {
            None => {
                if let Some(f) = fence_of(s) {
                    open = Some(f);
                    out[i] = true;
                }
            }
            Some((c, n)) => {
                out[i] = true;
                let closes = fence_of(s)
                    .is_some_and(|(c2, n2)| c2 == c && n2 >= n && s.trim().bytes().all(|b| b == c));
                if closes {
                    open = None;
                }
            }
        }
    }
    out
}

/// Участки кода `text`: сначала fenced-блоки (от начала первой строки до конца содержимого
/// последней, по порядку), затем инлайн-код — серия из n обратных кавычек до такой же серии, не
/// через пустую строку (по порядку). Инлайн-код открывается только вне блоков, но может
/// закрыться внутри блока — тогда участки пересекаются.
pub(crate) fn code_spans(text: &str, lines: &[Line], fenced: &[bool]) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    // fenced-блоки — от первой строки до конца содержимого последней
    let mut i = 0;
    while i < lines.len() {
        if fenced[i] {
            let start = lines[i].content.start;
            let mut j = i;
            while j + 1 < lines.len() && fenced[j + 1] {
                j += 1;
            }
            spans.push(start..lines[j].content.end);
            i = j + 1;
        } else {
            i += 1;
        }
    }
    let fences = spans.clone();

    // инлайн-код: серия из n обратных кавычек до такой же серии, не через пустую строку
    let b = text.as_bytes();
    let mut p = 0;
    // fenced-участки упорядочены: указатель на ближайший, не закончившийся до `p`
    let mut fi = 0;
    while p < b.len() {
        while fences.get(fi).is_some_and(|f| f.end <= p) {
            fi += 1;
        }
        if let Some(f) = fences.get(fi).filter(|f| f.start <= p) {
            p = f.end;
            continue;
        }
        if b[p] != b'`' {
            p += 1;
            continue;
        }
        let n = b[p..].iter().take_while(|&&c| c == b'`').count();
        let mut q = p + n;
        let mut found = None;
        while q < b.len() {
            if b[q] == b'`' {
                let m = b[q..].iter().take_while(|&&c| c == b'`').count();
                if m == n {
                    found = Some(q + m);
                    break;
                }
                q += m;
            // побайтно: `q` идёт по байтам и может стоять внутри многобайтового символа
            } else if b[q..].starts_with(b"\n\n") || b[q..].starts_with(b"\n\r\n") {
                break;
            } else {
                q += 1;
            }
        }
        match found {
            Some(end) => {
                spans.push(p..end);
                p = end;
            }
            None => p += n,
        }
    }
    spans
}

/// Проза `text` без кода (M6.6 §6.1): блоки в ограждениях (```` ``` ````, `~~~`, вместе со
/// строками ограждений; незакрытый — до конца текста) и инлайн-код `` `…` `` заменены пробелами
/// той же длины в байтах, а переводы строк (`\n`, `\r`) внутри кода остаются на месте. Длина в
/// байтах, смещения и число строк сохраняются: позиция в маске — позиция в исходнике. Границы
/// кода — те же, что у [`crate::split_sentences`].
pub fn prose_mask(text: &str) -> String {
    let lines = lines_of(text);
    let fenced = fenced_lines(text, &lines);
    let mut spans = code_spans(text, &lines, &fenced);
    if spans.is_empty() {
        return text.to_string();
    }
    spans.sort_by_key(|r| r.start);
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for r in spans {
        // границы участков — на ASCII-знаках (обратная кавычка, перевод строки) или на краях
        // строк: срезы не режут UTF-8; пересечение участков — продолжение с конца прежнего
        let start = r.start.max(pos);
        if start >= r.end {
            continue;
        }
        out.push_str(&text[pos..start]);
        out.extend(text.as_bytes()[start..r.end].iter().map(|&b| match b {
            b'\n' | b'\r' => b as char,
            _ => ' ',
        }));
        pos = r.end;
    }
    out.push_str(&text[pos..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(n: usize) -> String {
        " ".repeat(n)
    }

    /// M6.6 §6.1: код — пробелы той же длины в байтах, переводы строк в ограждениях — на месте,
    /// проза — как была; незакрытая обратная кавычка и кавычка через пустую строку — не код.
    #[test]
    fn prose_mask_keeps_offsets() {
        let text =
            "a `ёж` b ``c`d`` e\n```rs\nlet ё = 1;\n```\nf ~~~ g\n~~~\nh\n~~~\nk `open\n\nl` m";
        let expect = format!(
            "a {} b {} e\n{}\n{}\n{}\nf ~~~ g\n{}\n{}\n{}\nk `open\n\nl` m",
            sp("`ёж`".len()),
            sp("``c`d``".len()),
            sp("```rs".len()),
            sp("let ё = 1;".len()),
            sp(3),
            sp(3),
            sp(1),
            sp(3),
        );
        let m = prose_mask(text);
        assert_eq!(m, expect);
        assert_eq!(m.len(), text.len());
        assert_eq!(m.lines().count(), text.lines().count());
        // CRLF: оба байта перевода строки остаются
        assert_eq!(
            prose_mask("a\r\n```\r\nx\r\n```\r\nb"),
            "a\r\n   \r\n \r\n   \r\nb"
        );
        // незакрытое ограждение — код до конца текста
        assert_eq!(prose_mask("a\n```\nzam"), "a\n   \n   ");
        // инлайн-код через одиночный перевод строки — код, перевод строки на месте
        assert_eq!(prose_mask("x `a\nb` y"), "x   \n   y");
        // инлайн-код, закрытый внутри блока, — пересечение участков
        assert_eq!(prose_mask("a `b\n```\nc`\n```"), "a   \n   \n  \n   ");
        assert_eq!(prose_mask(""), "");
        assert_eq!(prose_mask("проза без кода"), "проза без кода");
    }
}
