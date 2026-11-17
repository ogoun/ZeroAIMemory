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

/// Ограждение кода: символ и длина серии, если строка открывает или закрывает блок. Строка
/// информации после обратных кавычек не содержит обратных кавычек (CommonMark: «```x```» в начале
/// строки — инлайн-код, а не блок; ревью задачи 5, M2), после тильд — может.
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
    (n >= 3 && (c == b'~' || !t.as_bytes()[n..].contains(&b'`'))).then_some((c, n))
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

/// Участки кода `text`: сначала fenced-блоки ([`fence_spans`]), затем инлайн-код
/// ([`inline_spans`]). Инлайн-код открывается только вне блоков, но может закрыться внутри блока —
/// тогда участки пересекаются.
pub(crate) fn code_spans(text: &str, lines: &[Line], fenced: &[bool]) -> Vec<Range<usize>> {
    let mut spans = fence_spans(lines, fenced);
    let inline = inline_spans(text.as_bytes(), &spans);
    spans.extend(inline);
    spans
}

/// Участки fenced-блоков: от начала первой строки до конца содержимого последней, по порядку.
fn fence_spans(lines: &[Line], fenced: &[bool]) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
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
    spans
}

/// Инлайн-код вне блоков `fences` (упорядочены), по порядку: серия из n обратных кавычек — до
/// ближайшей следующей серии той же длины, если между ними нет пустой строки (`\n\n`,
/// `\n\r\n`); серия внутри блока инлайн-код не открывает, но закрыть может; после участка
/// следующая открывающая серия ищется за его концом, после незакрытой — за ней самой.
///
/// Один проход (ревью задачи 5, M5): серии и пустые строки собраны заранее, следующая серия той
/// же длины — по таблице, указатели на блоки и пустые строки идут только вперёд. Прежний поиск
/// закрывающей серии от каждой открывающей стоил O(n·k) на текстах с многими незакрытыми
/// сериями разной длины; результат тот же (тесты `inline_scan_matches_oracle*`).
fn inline_spans(b: &[u8], fences: &[Range<usize>]) -> Vec<Range<usize>> {
    // серии обратных кавычек (начало, длина) и начала пустых строк — по порядку
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut blanks: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'`' => {
                let n = b[i..].iter().take_while(|&&c| c == b'`').count();
                runs.push((i, n));
                i += n;
            }
            b'\n' => {
                if b[i + 1..].starts_with(b"\n") || b[i + 1..].starts_with(b"\r\n") {
                    blanks.push(i);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    // следующая серия той же длины (или нет)
    let mut next_same: Vec<Option<usize>> = vec![None; runs.len()];
    let mut last: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for k in (0..runs.len()).rev() {
        next_same[k] = last.insert(runs[k].1, k);
    }
    let mut out = Vec::new();
    // `p` — откуда ищется следующая открывающая серия; `fi`, `bi` — указатели на ближайшие блок
    // и пустую строку, не закончившиеся до текущей серии
    let (mut k, mut p, mut fi, mut bi) = (0, 0, 0, 0);
    while k < runs.len() {
        let (start, n) = runs[k];
        k += 1;
        if start < p {
            continue;
        }
        while fences.get(fi).is_some_and(|f| f.end <= start) {
            fi += 1;
        }
        if fences.get(fi).is_some_and(|f| f.start <= start) {
            continue;
        }
        let open_end = start + n;
        while blanks.get(bi).is_some_and(|&q| q < open_end) {
            bi += 1;
        }
        match next_same[k - 1] {
            Some(j) if blanks.get(bi).is_none_or(|&q| q >= runs[j].0) => {
                let end = runs[j].0 + runs[j].1;
                out.push(start..end);
                p = end;
                k = j + 1;
            }
            _ => p = open_end,
        }
    }
    out
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

    /// Ревью задачи 5, M2 (CommonMark): строка информации после ограждения обратными кавычками
    /// не содержит обратных кавычек — «```x``` …» в начале строки — инлайн-код, а не блок до
    /// конца текста; у `~~~` — может.
    #[test]
    fn backtick_fence_info_has_no_backticks() {
        assert_eq!(fence_of("```x```"), None);
        assert_eq!(fence_of("```zam code map``` и дальше"), None);
        assert_eq!(fence_of("```rust"), Some((b'`', 3)));
        assert_eq!(fence_of("````"), Some((b'`', 4)));
        assert_eq!(fence_of("~~~ x`y"), Some((b'~', 3)));
        assert_eq!(
            prose_mask("```x``` hooks\nтекст"),
            format!("{} hooks\nтекст", sp(7))
        );
    }

    /// Прежний поиск инлайн-кода (квадратичный в худшем случае, до ревью задачи 5) — образец
    /// для [`inline_spans`].
    fn oracle_inline(b: &[u8], fences: &[Range<usize>]) -> Vec<Range<usize>> {
        let mut spans = Vec::new();
        let mut p = 0;
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

    /// Инлайн-код текста: (новый поиск, образец).
    fn both(text: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
        let lines = lines_of(text);
        let fenced = fenced_lines(text, &lines);
        let fences = fence_spans(&lines, &fenced);
        (
            inline_spans(text.as_bytes(), &fences),
            oracle_inline(text.as_bytes(), &fences),
        )
    }

    /// Ревью задачи 5, M5: линейный поиск инлайн-кода совпадает с прежним.
    #[test]
    fn inline_scan_matches_oracle_cases() {
        for text in [
            "",
            "`",
            "``",
            "a `b` c",
            "a ``b`c`` d",
            "a `b\n\nc` d",
            "a `b\n\r\nc` d",
            "a `b\nc` d",
            "a `b\n```\nc`\n```",
            "```\n`a\n```\nb` c",
            "` `` ``` `` `",
            "`a ``b ```c ```` d` e`` f``` g",
            "x `ёж` y ``ё`ж`` z",
            "```x``` y `z`",
            "~~~\n`\n~~~\n`a`",
            "`\n\n`\n\n`",
            "`a\n```\n\n```\nb`",
        ] {
            let (got, want) = both(text);
            assert_eq!(got, want, "{text:?}");
        }
        // худший случай прежнего поиска — незакрытые серии разной длины без пустых строк
        let text: String = (1..400).map(|n| format!("{} x ", "`".repeat(n))).collect();
        let (got, want) = both(&text);
        assert_eq!(got, want);
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(1024))]
        /// Ревью задачи 5, M5: на случайных текстах из обратных кавычек, тильд, переводов строк
        /// и букв линейный поиск инлайн-кода совпадает с прежним.
        #[test]
        fn inline_scan_matches_oracle(
            chars in proptest::collection::vec(
                proptest::sample::select(vec!['`', '`', '`', '~', 'a', ' ', '\n', '\r', 'ё']),
                0..160,
            )
        ) {
            let text: String = chars.into_iter().collect();
            let (got, want) = both(&text);
            proptest::prop_assert_eq!(got, want, "{:?}", text);
        }
    }
}
