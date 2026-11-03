//! Упаковка карты кода в бюджет токенов (спецификация M5.6 §7).
//!
//! Карта — Markdown: заголовок снимка, затем файлы по рангу (строка `path`), под каждым —
//! символы по рангу строками [`map_line`]. Бинарный поиск по длине префикса в порядке ранга
//! находит наибольший префикс, при котором текст вместе с хвостом «… ещё N файлов, M символов»
//! укладывается в бюджет; итог не превышает бюджета никогда.

use crate::SymbolKind;

/// Символ карты.
#[derive(Debug, Clone, PartialEq)]
pub struct MapSym {
    pub kind: SymbolKind,
    pub qual: String,
    pub sig: Option<String>,
    pub line: u32,
}

/// Файл карты; `syms` уже упорядочены по рангу, сами файлы — по рангу файла.
#[derive(Debug, Clone, PartialEq)]
pub struct MapFile {
    pub path: String,
    pub syms: Vec<MapSym>,
}

/// Упакованная карта.
#[derive(Debug, Clone, PartialEq)]
pub struct Packed {
    pub text: String,
    /// `count(text)`.
    pub tokens: usize,
    /// Файлов с показанным путём (частично показанный файл — показан).
    pub files_shown: usize,
    pub symbols_shown: usize,
    /// Файлов, не показанных совсем.
    pub files_left: usize,
    /// Символов, не показанных (в том числе хвост частично показанного файла).
    pub symbols_left: usize,
}

/// Строка символа: `  <kind> <sig или qual> :<line>`.
pub fn map_line(s: &MapSym) -> String {
    let what = s.sig.as_deref().unwrap_or(&s.qual);
    format!("  {} {what} :{}", s.kind.as_str(), s.line)
}

/// Русская форма числительного: 1 файл, 2 файла, 5 файлов.
fn plural<'a>(n: usize, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    match (n % 10, n % 100) {
        (_, 11..=14) => many,
        (1, _) => one,
        (2..=4, _) => few,
        _ => many,
    }
}

/// Хвост карты: сколько не показано и как получить остальное.
fn tail_line(files_left: usize, symbols_left: usize) -> String {
    format!(
        "… ещё {files_left} {}, {symbols_left} {} — `memory_code action=map focus=…`",
        plural(files_left, "файл", "файла", "файлов"),
        plural(symbols_left, "символ", "символа", "символов"),
    )
}

/// Единиц префикса у файла: число символов, у файла без символов — 1 (сам путь).
fn units(f: &MapFile) -> usize {
    f.syms.len().max(1)
}

/// Карта из первых `k` единиц (символов в порядке ранга; файл без символов — одна единица);
/// `rest` — `(файлов, символов)` за пределами `files`, они входят в хвост.
fn build(header: &str, files: &[MapFile], rest: (usize, usize), k: usize) -> Packed {
    let mut lines: Vec<String> = Vec::new();
    if !header.is_empty() {
        lines.push(header.to_string());
    }
    let (mut files_shown, mut symbols_shown, mut left) = (0, 0, k);
    for f in files {
        if left == 0 {
            break;
        }
        files_shown += 1;
        lines.push(f.path.clone());
        let take = left.min(f.syms.len());
        lines.extend(f.syms[..take].iter().map(map_line));
        symbols_shown += take;
        left -= units(f).min(left);
    }
    let total_syms: usize = files.iter().map(|f| f.syms.len()).sum();
    let files_left = files.len() - files_shown + rest.0;
    let symbols_left = total_syms - symbols_shown + rest.1;
    if files_left > 0 || symbols_left > 0 {
        lines.push(tail_line(files_left, symbols_left));
    }
    Packed {
        text: lines.join("\n"),
        tokens: 0,
        files_shown,
        symbols_shown,
        files_left,
        symbols_left,
    }
}

/// Наибольшее `x` в `0..=hi`, при котором `fits(x)`, если `fits(0)`. Поиск экспоненциальный, затем
/// двоичный: счётчик видит тексты не длиннее удвоенного ответа (точный токенизатор сервера на
/// полной карте в 50 000 файлов дорог). Итог всегда проверен `fits` — немонотонный счётчик за
/// бюджет не выводит.
fn max_fitting(hi: usize, fits: impl Fn(usize) -> bool) -> usize {
    let (mut lo, mut hi, mut step) = (0, hi, 1);
    while lo < hi {
        let probe = lo + step.min(hi - lo);
        if fits(probe) {
            lo = probe;
            step *= 2;
        } else {
            hi = probe - 1;
            break;
        }
    }
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// Префикс строки из `n` символов.
fn prefix(s: &str, n: usize) -> &str {
    s.char_indices().nth(n).map_or(s, |(i, _)| &s[..i])
}

/// Карта в бюджете `budget` токенов счётчиком `count`: заголовок, файлы и символы наибольшим
/// префиксом в порядке ранга и хвост «… ещё …» (входит в бюджет). Заголовок длиннее бюджета
/// обрезается по символам (с хвостом, если тот влезает). `tokens ≤ budget` всегда, если
/// `count("") ≤ budget`; иначе текст пустой.
pub fn render_map(
    header: &str,
    files: &[MapFile],
    budget: usize,
    count: &dyn Fn(&str) -> usize,
) -> Packed {
    render_map_rest(header, files, (0, 0), budget, count)
}

/// [`render_map`] по первым файлам карты: `rest` — `(файлов, символов)`, которые вызывающий не
/// передал (сервер читает символы только первых файлов). Они не показываются, но входят в
/// хвост и в `files_left`/`symbols_left`. Если показаны не все `files`, итог равен карте
/// целиком; иначе вызывающему нужно передать больше файлов.
pub fn render_map_rest(
    header: &str,
    files: &[MapFile],
    rest: (usize, usize),
    budget: usize,
    count: &dyn Fn(&str) -> usize,
) -> Packed {
    let fits = |text: &str| count(text) <= budget;
    let total: usize = files.iter().map(units).sum();
    let mut packed = if fits(&build(header, files, rest, 0).text) {
        let k = max_fitting(total, |k| fits(&build(header, files, rest, k).text));
        build(header, files, rest, k)
    } else {
        // заголовок не влезает и без символов: обрезка по символам, сначала — с хвостом
        let empty = build("", files, rest, 0);
        let tail = empty.text.clone();
        let with_tail = |h: usize| {
            let head = prefix(header, h);
            match (head.is_empty(), tail.is_empty()) {
                (_, true) => head.to_string(),
                (true, false) => tail.clone(),
                (false, false) => format!("{head}\n{tail}"),
            }
        };
        let hchars = header.chars().count();
        let text = if fits(&tail) {
            with_tail(max_fitting(hchars, |h| fits(&with_tail(h))))
        } else {
            prefix(header, max_fitting(hchars, |h| fits(prefix(header, h)))).to_string()
        };
        Packed { text, ..empty }
    };
    packed.tokens = count(&packed.text);
    packed
}
