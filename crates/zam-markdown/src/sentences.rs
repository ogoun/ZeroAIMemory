//! Сегментация текста на предложения (исследование 05 §10).
//!
//! Основа — границы UAX #29 (`unicode-segmentation`). Поверх них:
//! - одиночный перевод строки внутри абзаца — не граница (мягкий перенос);
//!   пустая строка, начало пункта списка, заголовка, цитаты, строки таблицы
//!   и ограждения кода — граница;
//! - сокращения («т. е.», «г.», «e.g.», «Dr.» …), инициалы («А. С.»), числа
//!   и версии («1.5», «v1.2.3») не завершают предложение;
//! - внутри `` `кода` ``, fenced-блоков, URL и путей границ нет;
//! - граница без пробела после знака («foo.Bar», «a?b») — не граница.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

/// Сокращения, за которыми предложение не кончается: дальше идёт имя,
/// номер или продолжение мысли. Строчными, без пробелов между частями.
const PREFIX_ABBREVS: &[&str] = &[
    "г.",
    "ул.",
    "рис.",
    "см.",
    "стр.",
    "им.",
    "проф.",
    "акад.",
    "напр.",
    "e.g.",
    "i.e.",
    "dr.",
    "mr.",
    "mrs.",
    "ms.",
    "prof.",
    "vs.",
    "cf.",
    "fig.",
];

/// Сокращения, которыми предложение может закончиться: склейка, только если
/// следующий сегмент начинается не с заглавной буквы и не с цифры.
const TERMINAL_ABBREVS: &[&str] = &["т.е.", "т.д.", "т.п.", "т.к.", "др.", "пр.", "etc."];

/// Знаки, отрезаемые с конца URL или пути: обычно это пунктуация предложения.
const TRAILING_PUNCT: &[char] = &['.', ',', ';', ':', '!', '?', ')', ']', '}', '»', '"', '\''];

/// Предложения `text` — байтовые диапазоны по порядку, без краевых пробелов;
/// пустые сегменты отбрасываются. Не паникует на любом входе.
pub fn split_sentences(text: &str) -> Vec<Range<usize>> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let lines = lines_of(text);
    let fenced = fenced_lines(text, &lines);
    let norm = soften_newlines(text, &lines, &fenced);
    let protected = protected_spans(text, &lines, &fenced);

    let mut out = Vec::new();
    let mut cur: Option<Range<usize>> = None;
    for (i, seg) in norm.split_sentence_bound_indices() {
        let next = i..i + seg.len();
        cur = Some(match cur {
            None => next,
            Some(c) if glue(text, &norm, &c, &next, &protected) => c.start..next.end,
            Some(c) => {
                push_trimmed(text, c, &mut out);
                next
            }
        });
    }
    if let Some(c) = cur {
        push_trimmed(text, c, &mut out);
    }
    out
}

fn push_trimmed(text: &str, r: Range<usize>, out: &mut Vec<Range<usize>>) {
    let s = &text[r.clone()];
    let start = r.start + (s.len() - s.trim_start().len());
    let end = r.start + s.trim_end().len();
    if start < end {
        out.push(start..end);
    }
}

/// Строка: содержимое и перевод строки после него (может быть пустым).
struct Line {
    content: Range<usize>,
    eol: Range<usize>,
}

/// Строки с концами `\n`, `\r\n` и одиночным `\r` (как в [`crate::LineIndex`]).
fn lines_of(text: &str) -> Vec<Line> {
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
fn fence_of(line: &str) -> Option<(u8, usize)> {
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

/// Для каждой строки — принадлежит ли она fenced-блоку (включая ограждения).
fn fenced_lines(text: &str, lines: &[Line]) -> Vec<bool> {
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

/// Строка начинает блок Markdown: заголовок, цитату, таблицу, ограждение, пункт списка.
fn is_block_start(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with(['#', '>', '|']) || fence_of(line).is_some() {
        return true;
    }
    let mut chars = t.chars();
    match chars.next() {
        Some('-' | '*' | '+') => chars.next().is_none_or(char::is_whitespace),
        Some(c) if c.is_ascii_digit() => {
            let digits = t.bytes().take_while(u8::is_ascii_digit).count();
            let rest = &t[digits..];
            digits <= 9
                && (rest.starts_with(". ") || rest.starts_with(") ") || rest == "." || rest == ")")
        }
        _ => false,
    }
}

/// Копия текста той же длины, где мягкие переводы строк заменены пробелами.
fn soften_newlines(text: &str, lines: &[Line], fenced: &[bool]) -> String {
    let mut bytes = text.as_bytes().to_vec();
    for i in 0..lines.len().saturating_sub(1) {
        let cur = &text[lines[i].content.clone()];
        let next = &text[lines[i + 1].content.clone()];
        let hard = cur.trim().is_empty()
            || next.trim().is_empty()
            || fenced[i]
            || fenced[i + 1]
            || is_block_start(next)
            || cur.trim_start().starts_with(['#', '|']);
        if !hard {
            bytes[lines[i].eol.clone()].fill(b' ');
        }
    }
    // заменялись только ASCII-байты переводов строк: UTF-8 сохраняется
    String::from_utf8(bytes).unwrap_or_else(|_| text.to_string())
}

/// Участки, внутри которых нет границ: fenced-блоки, `` `код` ``, URL и пути.
/// Результат отсортирован и без пересечений.
fn protected_spans(text: &str, lines: &[Line], fenced: &[bool]) -> Vec<Range<usize>> {
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
            } else if text[q..].starts_with("\n\n") || text[q..].starts_with("\n\r\n") {
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

    // URL и пути: непробельные токены со схемой, `www.`, `/` или `\`
    let mut start = None;
    for (i, ch) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        match (ch.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                let tok = &text[s..i];
                if tok.contains("://")
                    || tok.starts_with("www.")
                    || tok.contains('/')
                    || tok.contains('\\')
                {
                    let t = tok.trim_end_matches(TRAILING_PUNCT);
                    if !t.is_empty() {
                        spans.push(s..s + t.len());
                    }
                }
                start = None;
            }
            _ => {}
        }
    }

    spans.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(spans.len());
    for r in spans {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
}

/// Граница `b` строго внутри защищённого участка.
fn inside_protected(spans: &[Range<usize>], b: usize) -> bool {
    let k = spans.partition_point(|r| r.start < b);
    k > 0 && spans[k - 1].end > b
}

/// Склеивать ли текущее предложение `cur` со следующим сегментом `next`.
fn glue(
    text: &str,
    norm: &str,
    cur: &Range<usize>,
    next: &Range<usize>,
    protected: &[Range<usize>],
) -> bool {
    let a = &text[cur.clone()];
    if a.trim().is_empty() || inside_protected(protected, next.start) {
        return true;
    }
    // жёсткий перевод строки (абзац, пункт списка, ограждение) — всегда граница
    if norm[cur.clone()].ends_with(['\n', '\r']) {
        return false;
    }
    // граница без пробела: «foo.Bar», «a?b», «v1.2.Beta»
    if !a.ends_with(char::is_whitespace) {
        return true;
    }
    let a = a.trim_end();
    let b = text[next.clone()].trim_start();
    let b_first = b.chars().next();
    let b_strong = b_first.is_some_and(|c| c.is_uppercase() || c.is_ascii_digit());

    for abbr in PREFIX_ABBREVS {
        if let Some(start) = abbrev_start(a, abbr) {
            // «2020 г.» в конце фразы — как завершающее сокращение
            let after_number = *abbr == "г."
                && a[..start]
                    .trim_end()
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_digit());
            return !(after_number && b_strong);
        }
    }
    if TERMINAL_ABBREVS
        .iter()
        .any(|abbr| abbrev_start(a, abbr).is_some())
    {
        return !b_strong;
    }
    if let Some(i) = trailing_initial(a) {
        let before = a[..i].trim_end();
        let prev_tok = before.rsplit(char::is_whitespace).next().unwrap_or("");
        return starts_with_initial(b)
            || before.is_empty()
            || is_initial(prev_tok)
            || PREFIX_ABBREVS
                .iter()
                .any(|abbr| abbrev_start(prev_tok, abbr).is_some());
    }
    false
}

/// Начало сокращения `abbr` в конце `a`: регистр не важен, между частями
/// сокращения («т. е.») допускаются пробелы, перед ним — не буква и не цифра.
fn abbrev_start(a: &str, abbr: &str) -> Option<usize> {
    let mut chars = a.char_indices().rev().peekable();
    let mut start = a.len();
    for (k, ac) in abbr.chars().rev().enumerate() {
        if k > 0 && ac == '.' {
            while chars.peek().is_some_and(|&(_, c)| c.is_whitespace()) {
                chars.next();
            }
        }
        let (i, c) = chars.next()?;
        if !c.to_lowercase().eq(ac.to_lowercase()) {
            return None;
        }
        start = i;
    }
    match chars.next() {
        Some((_, c)) if c.is_alphanumeric() => None,
        _ => Some(start),
    }
}

/// Инициал в конце `a` («… А.»): байт начала буквы.
fn trailing_initial(a: &str) -> Option<usize> {
    let mut it = a.char_indices().rev();
    let (_, dot) = it.next()?;
    let (i, letter) = it.next()?;
    let before_ok = it.next().is_none_or(|(_, c)| !c.is_alphanumeric());
    (dot == '.' && letter.is_uppercase() && before_ok).then_some(i)
}

/// Токен — инициал: заглавная буква и точка.
fn is_initial(tok: &str) -> bool {
    let mut c = tok.chars();
    matches!((c.next(), c.next(), c.next()), (Some(l), Some('.'), None) if l.is_uppercase())
}

/// Сегмент начинается с инициала («С. Пушкин»).
fn starts_with_initial(b: &str) -> bool {
    let mut c = b.chars();
    matches!(
        (c.next(), c.next(), c.next()),
        (Some(l), Some('.'), next) if l.is_uppercase() && next.is_none_or(char::is_whitespace)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbrev_matching() {
        assert_eq!(abbrev_start("это т. е.", "т.е."), Some(7));
        assert_eq!(abbrev_start("Dr.", "dr."), Some(0));
        assert_eq!(abbrev_start("друг.", "г."), None);
        assert_eq!(abbrev_start("в г.", "г."), Some(3));
    }

    #[test]
    fn initials() {
        assert_eq!(trailing_initial("Поэт А."), Some("Поэт ".len()));
        assert_eq!(trailing_initial("plan AB."), None);
        assert!(is_initial("J.") && !is_initial("Jr."));
        assert!(starts_with_initial("С. Пушкин") && !starts_with_initial("Св."));
    }

    #[test]
    fn block_starts() {
        for l in [
            "- a", "* b", "+", "1. x", "10) y", "# h", "> q", "| t |", "```rs",
        ] {
            assert!(is_block_start(l), "{l}");
        }
        for l in ["-a", "1.5 раза", "текст", "2020 г."] {
            assert!(!is_block_start(l), "{l}");
        }
    }
}
