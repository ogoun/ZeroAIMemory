//! Код в тексте Markdown без разбора comrak — **одно определение на проект** (круг правок 2
//! задачи 5 M7.1, решение координатора): правило флагов сканера ([`code_regions`]) и
//! `zam-markdown` (сегментация предложений, маска прозы M6.6 §6.1) берут участки кода отсюда.
//!
//! Правила (по CommonMark, без полного разбора контейнеров):
//! - строки — с концами `\n`, `\r\n` и одиночным `\r` ([`lines_of`]);
//! - блок в ограждении ([`fence_of`]): серия из трёх и больше ```` ` ```` или `~` с отступом не
//!   больше трёх пробелов; строка информации после обратных кавычек не содержит обратных кавычек
//!   («```x```» в начале строки — инлайн-код), после тильд — может; закрывает блок строка из
//!   серии того же знака не короче открывающей (только она и пробелы); незакрытый блок идёт до
//!   конца текста;
//! - ограждение внутри цитаты (`> ```sh`, вложенные `> >`) — блок той же цитаты: строки блока
//!   несут те же маркеры `>`, строка с меньшей глубиной цитаты блок закрывает;
//! - инлайн-код: серия из n обратных кавычек — до ближайшей следующей серии той же длины, если
//!   между ними нет пустой строки (спан может быть многострочным); серия внутри блока инлайн-код
//!   не открывает, но закрыть может ([`code_spans`]);
//! - блок с отступом в четыре пробела кодом **не** считается: в списках это продолжение абзаца, а
//!   отличить одно от другого без разбора контейнеров нельзя.
//!
//! Время — линейное по длине текста (серии и пустые строки собираются одним проходом, следующая
//! серия той же длины — по таблице, ревью задачи 5 M6.6, M5).

use std::collections::HashMap;
use std::ops::Range;

/// Строка: содержимое и перевод строки после него (может быть пустым).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub content: Range<usize>,
    pub eol: Range<usize>,
}

/// Строки с концами `\n`, `\r\n` и одиночным `\r` (как в CommonMark и comrak).
pub fn lines_of(text: &str) -> Vec<Line> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < b.len() {
        let eol_len = eol_at(b, i);
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

/// Длина перевода строки с позиции `i` (`\n`, `\r\n`, одиночный `\r`) или 0.
fn eol_at(b: &[u8], i: usize) -> usize {
    match b[i] {
        b'\n' => 1,
        b'\r' if b.get(i + 1) == Some(&b'\n') => 2,
        b'\r' => 1,
        _ => 0,
    }
}

/// Ограждение кода: символ и длина серии, если строка (без маркеров цитаты) открывает или
/// закрывает блок. Отступ — не больше трёх пробелов; строка информации после обратных кавычек не
/// содержит обратных кавычек (CommonMark: «```x```» в начале строки — инлайн-код, а не блок;
/// ревью задачи 5 M6.6, M2), после тильд — может.
pub fn fence_of(line: &str) -> Option<(u8, usize)> {
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

/// Маркеры цитаты в начале строки — не больше `max`: до трёх пробелов, `>` и один необязательный
/// пробел, столько раз, сколько их подряд. Глубина и остаток строки.
fn strip_quotes(line: &str, max: usize) -> (usize, &str) {
    let mut rest = line;
    let mut depth = 0;
    while depth < max {
        let t = rest.trim_start_matches(' ');
        if rest.len() - t.len() > 3 || !t.starts_with('>') {
            break;
        }
        rest = t[1..].strip_prefix(' ').unwrap_or(&t[1..]);
        depth += 1;
    }
    (depth, rest)
}

/// Блок в ограждении: строки `open..=last` (с ограждениями), `closed` — последняя строка
/// закрывает блок.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FenceBlock {
    pub open: usize,
    pub last: usize,
    pub closed: bool,
}

/// Блоки в ограждениях по строкам `lines` (см. модуль: цитаты, закрытие, незакрытый — до конца).
pub fn fence_blocks(text: &str, lines: &[Line]) -> Vec<FenceBlock> {
    let mut out = Vec::new();
    // открытый блок: знак, длина серии, глубина цитаты, строка открытия
    let mut open: Option<(u8, usize, usize, usize)> = None;
    for (i, l) in lines.iter().enumerate() {
        let s = &text[l.content.clone()];
        if let Some((c, n, d, from)) = open {
            let (q, rest) = strip_quotes(s, d);
            if q == d {
                let closes = fence_of(rest).is_some_and(|(c2, n2)| {
                    c2 == c && n2 >= n && rest.trim().bytes().all(|b| b == c)
                });
                if closes {
                    out.push(FenceBlock {
                        open: from,
                        last: i,
                        closed: true,
                    });
                    open = None;
                }
                continue;
            }
            // цитата кончилась — блок закрыт до этой строки, сама строка разбирается заново
            out.push(FenceBlock {
                open: from,
                last: i - 1,
                closed: false,
            });
            open = None;
        }
        let (d, rest) = strip_quotes(s, usize::MAX);
        if let Some((c, n)) = fence_of(rest) {
            open = Some((c, n, d, i));
        }
    }
    if let Some((_, _, _, from)) = open {
        out.push(FenceBlock {
            open: from,
            last: lines.len() - 1,
            closed: false,
        });
    }
    out
}

/// Для каждой строки — принадлежит ли она блоку в ограждении (включая ограждения).
pub fn fenced_lines(text: &str, lines: &[Line]) -> Vec<bool> {
    let mut out = vec![false; lines.len()];
    for b in fence_blocks(text, lines) {
        out[b.open..=b.last].iter_mut().for_each(|x| *x = true);
    }
    out
}

/// Участки кода `text`: сначала блоки в ограждениях (строки целиком, с ограждениями, подряд
/// идущие — одним участком), затем инлайн-код (с обратными кавычками). Инлайн-код открывается
/// только вне блоков, но может закрыться внутри блока — тогда участки пересекаются.
pub fn code_spans(text: &str, lines: &[Line], fenced: &[bool]) -> Vec<Range<usize>> {
    code_spans_of(text, lines, fenced, false)
}

/// [`code_spans`]; `open_tail` — `text` лишь начало более длинного текста: инлайн-код без
/// закрывающей серии и без пустой строки до конца `text` может закрыться дальше — он код до конца
/// `text` (окно маски головы промпта, повторное ревью задачи 5 M6.6).
pub fn code_spans_of(
    text: &str,
    lines: &[Line],
    fenced: &[bool],
    open_tail: bool,
) -> Vec<Range<usize>> {
    let mut spans = fence_spans(lines, fenced);
    let inline = inline_spans(text.as_bytes(), &spans, open_tail);
    spans.extend(inline);
    spans
}

/// Участки блоков в ограждениях: от начала первой строки до конца содержимого последней, по
/// порядку (подряд идущие строки блоков — одним участком).
pub fn fence_spans(lines: &[Line], fenced: &[bool]) -> Vec<Range<usize>> {
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
/// ближайшей следующей серии той же длины, если между ними нет пустой строки (после перевода
/// строки — сразу ещё один: `\n\n`, `\n\r\n`, `\r\r`, `\r\n\r\n`, …); серия внутри блока
/// инлайн-код не открывает, но закрыть может; после участка следующая открывающая серия ищется за
/// его концом, после незакрытой — за ней самой.
///
/// Один проход (ревью задачи 5 M6.6, M5): серии и пустые строки собраны заранее, следующая серия
/// той же длины — по таблице, указатели на блоки и пустые строки идут только вперёд.
pub fn inline_spans(b: &[u8], fences: &[Range<usize>], open_tail: bool) -> Vec<Range<usize>> {
    // серии обратных кавычек (начало, длина) и пустые строки (позиция перевода строки перед
    // ней) — по порядку
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
            b'\n' | b'\r' => {
                let next = i + eol_at(b, i);
                if matches!(b.get(next), Some(b'\n' | b'\r')) {
                    blanks.push(i);
                }
                i = next;
            }
            _ => i += 1,
        }
    }
    // следующая серия той же длины (или нет)
    let mut next_same: Vec<Option<usize>> = vec![None; runs.len()];
    let mut last: HashMap<usize, usize> = HashMap::new();
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
            // ни закрывающей серии, ни пустой строки до конца: дальше начала текста не видно
            _ if open_tail && bi == blanks.len() => {
                out.push(start..b.len());
                break;
            }
            _ => p = open_end,
        }
    }
    out
}

/// Вид участка кода [`code_regions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeKind {
    /// Строки блока в ограждении (между ограждениями; у незакрытого — до конца текста).
    Fenced,
    /// Содержимое инлайн-кода (между сериями обратных кавычек; может быть многострочным).
    Inline,
}

/// Участок кода: содержимое без ограждений и обратных кавычек.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRegion {
    pub range: Range<usize>,
    pub kind: CodeKind,
}

/// Содержимое кода `text` по порядку, без пересечений: строки блоков в ограждениях (без самих
/// ограждений; маркеры цитаты остаются) и содержимое инлайн-кода (без обратных кавычек; спан,
/// закрытый внутри блока, обрезается до начала блока). Пустые участки не входят.
pub fn code_regions(text: &str) -> Vec<CodeRegion> {
    let lines = lines_of(text);
    let blocks = fence_blocks(text, &lines);
    let mut fenced = vec![false; lines.len()];
    let mut out: Vec<CodeRegion> = Vec::new();
    for b in &blocks {
        fenced[b.open..=b.last].iter_mut().for_each(|x| *x = true);
        let last = if b.closed {
            b.last.checked_sub(1)
        } else {
            Some(b.last)
        };
        if let Some(last) = last.filter(|&l| l > b.open) {
            let r = lines[b.open + 1].content.start..lines[last].content.end;
            if !r.is_empty() {
                out.push(CodeRegion {
                    range: r,
                    kind: CodeKind::Fenced,
                });
            }
        }
    }
    let fences = fence_spans(&lines, &fenced);
    let bytes = text.as_bytes();
    let mut fi = 0;
    for s in inline_spans(bytes, &fences, false) {
        let n = bytes[s.start..].iter().take_while(|&&c| c == b'`').count();
        let mut end = s.end.saturating_sub(n).max(s.start + n);
        // закрылся внутри блока: часть в блоке — его участок
        while fences.get(fi).is_some_and(|f| f.end <= s.start) {
            fi += 1;
        }
        if let Some(f) = fences.get(fi).filter(|f| f.start < end) {
            end = f.start;
        }
        let r = s.start + n..end;
        if r.start < r.end {
            out.push(CodeRegion {
                range: r,
                kind: CodeKind::Inline,
            });
        }
    }
    out.sort_by_key(|r| r.range.start);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Содержимое участков кода как строки и вид.
    fn regions(t: &str) -> Vec<(String, CodeKind)> {
        code_regions(t)
            .into_iter()
            .map(|r| (t[r.range].to_string(), r.kind))
            .collect()
    }

    fn fenced(t: &str) -> Vec<String> {
        regions(t)
            .into_iter()
            .filter(|(_, k)| *k == CodeKind::Fenced)
            .map(|(s, _)| s)
            .collect()
    }

    fn inline(t: &str) -> Vec<String> {
        regions(t)
            .into_iter()
            .filter(|(_, k)| *k == CodeKind::Inline)
            .map(|(s, _)| s)
            .collect()
    }

    /// Блоки: язык и без, тильды, незакрытый, закрытие той же серией не короче, отступ забора до
    /// трёх пробелов (четыре — не забор), цитаты (и вложенные), конец цитаты закрывает блок,
    /// одиночный `\r`, CRLF.
    #[test]
    fn fenced_regions() {
        assert_eq!(fenced("a\n```sh\nx -y\nz\n```\nb"), ["x -y\nz"]);
        assert_eq!(fenced("~~~\nk\n~~~~\n"), ["k"]);
        assert_eq!(fenced("1. a\n   ```sh\n   y\n   ```\n"), ["   y"]);
        assert_eq!(fenced("```\nоткрыт\nдо конца"), ["открыт\nдо конца"]);
        assert_eq!(fenced("````\nx\n```\nв блоке\n````\n"), ["x\n```\nв блоке"]);
        assert_eq!(
            fenced("```\nx\n``` не закрытие\ny\n```\n"),
            ["x\n``` не закрытие\ny"]
        );
        assert!(
            fenced("para\n    ```\nmysql\n    ```\n").is_empty(),
            "отступ 4 — не забор"
        );
        assert!(
            fenced("Text\n\n    mysql -p x\n\nmore").is_empty(),
            "блок с отступом — не код"
        );
        assert_eq!(fenced("> ```sh\n> mysql -px\n> ```\n"), ["> mysql -px"]);
        assert_eq!(fenced("> > ```\n> > a\n> > ```\n"), ["> > a"]);
        assert_eq!(
            fenced("> ```\n> a\nb\n"),
            ["> a"],
            "конец цитаты закрывает блок"
        );
        assert_eq!(fenced("```\ra\rb\r```\r"), ["a\rb"]);
        assert_eq!(fenced("```sh\r\nx\r\n```\r\n"), ["x"]);
        assert!(fenced("```\n```\n").is_empty());
        assert!(
            fenced("```x``` и дальше\nтекст").is_empty(),
            "инлайн, не блок"
        );
    }

    /// Инлайн-код: любая длина серии, многострочный до пустой строки (в том числе с одиночным
    /// `\r`), непарная кавычка — не код, закрытый внутри блока — до начала блока.
    #[test]
    fn inline_regions() {
        assert_eq!(inline("а `b` и ``c ` d`` и `"), ["b", "c ` d"]);
        assert_eq!(inline("run `mysql -uroot\n-px` now"), ["mysql -uroot\n-px"]);
        assert!(inline("a `b\n\nc` d").is_empty());
        assert!(
            inline("a `b\r\rc` d").is_empty(),
            "пустая строка с одиночным CR"
        );
        assert!(inline("a `b\r\n\r\nc` d").is_empty());
        assert!(inline("одна ` кавычка").is_empty());
        assert_eq!(inline("text ```mysql -px``` text"), ["mysql -px"]);
        assert_eq!(inline("a `b\n```\nc`\n```"), ["b\n"]);
        assert_eq!(inline("\\`a\\` `x`"), ["a\\", "x"]);
    }

    /// Прежний поиск инлайн-кода (квадратичный в худшем случае, до ревью задачи 5 M6.6) —
    /// образец для [`inline_spans`]; пустая строка — та же, что у него (после перевода строки —
    /// сразу ещё один).
    fn oracle_inline(b: &[u8], fences: &[Range<usize>]) -> Vec<Range<usize>> {
        let blank_at = |q: usize| {
            let e = eol_at(b, q);
            e > 0 && matches!(b.get(q + e), Some(b'\n' | b'\r'))
        };
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
                } else if blank_at(q) {
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
            inline_spans(text.as_bytes(), &fences, false),
            oracle_inline(text.as_bytes(), &fences),
        )
    }

    /// Ревью задачи 5 M6.6, M5: линейный поиск инлайн-кода совпадает с прежним (перенесено из
    /// `zam-markdown` вместе с реализацией, ожидания те же).
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
            "a `b\r\rc` `d`",
            "> ```\n> `a\n> ```\n`b`",
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
        /// Ревью задачи 5 M6.6, M5: на случайных текстах из обратных кавычек, тильд, маркеров
        /// цитаты, переводов строк и букв линейный поиск инлайн-кода совпадает с прежним, а
        /// участки [`code_regions`] упорядочены, не пересекаются и лежат на границах символов.
        #[test]
        fn inline_scan_matches_oracle(
            chars in proptest::collection::vec(
                proptest::sample::select(vec!['`', '`', '`', '~', 'a', ' ', '\n', '\r', 'ё', '>']),
                0..160,
            )
        ) {
            let text: String = chars.into_iter().collect();
            let (got, want) = both(&text);
            proptest::prop_assert_eq!(got, want, "{:?}", text);
            let rs = code_regions(&text);
            for w in rs.windows(2) {
                proptest::prop_assert!(w[0].range.end <= w[1].range.start, "{:?}", text);
            }
            for r in &rs {
                proptest::prop_assert!(text.is_char_boundary(r.range.start) && text.is_char_boundary(r.range.end));
            }
        }
    }

    /// Линейность: серии обратных кавычек растущей длины в одной строке на 1 МБ (худший случай
    /// прежнего правила флагов, 196 мс) — release: `cargo test -p zam-secrets --release --lib --
    /// --ignored bench_code_regions --nocapture`.
    #[test]
    #[ignore]
    fn bench_code_regions_backtick_runs() {
        let mut t = String::new();
        let mut k = 1;
        while t.len() < 1024 * 1024 {
            t.push_str(&"`".repeat(k));
            t.push_str(" x ");
            k = k % 969 + 1;
        }
        let t0 = std::time::Instant::now();
        let n = 5;
        for _ in 0..n {
            std::hint::black_box(code_regions(&t));
        }
        let per = t0.elapsed() / n;
        println!("code_regions (серии кавычек, {} байт): {per:?}", t.len());
        assert!(per < std::time::Duration::from_millis(20), "{per:?}");
    }
}
