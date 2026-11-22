//! Код в тексте Markdown без разбора comrak — **один модуль на проект** (круг правок 2 задачи 5
//! M7.1, решение координатора): правило флагов сканера и `zam-markdown` (сегментация
//! предложений, маска прозы M6.6 §6.1) берут участки кода отсюда — каждый своей функцией (ниже).
//!
//! Функций две (круг правок 5, решение координатора), потому что у потребителей разная цена
//! ошибки:
//! - [`code_regions`] — как можно ближе к CommonMark (без полного разбора контейнеров:
//!   эвристика пунктов списков — [`fence_blocks`]); её берёт `zam-markdown`: лишний код там
//!   прячет от поиска имена терминов и границы предложений;
//! - [`mask_code_regions`] — для маски секретов: **объединение** [`code_regions`], правил круга 2
//!   ([`code_regions_round2`], до учёта списков) и широкого правила ([`code_regions_loose`]:
//!   забор после префиксов контейнеров в любом порядке — `10) >````, цитата в пункте). Эвристика
//!   списков сходится к CommonMark плохо, а для секрета лишняя маска безопаснее утечки:
//!   объединение не теряет кода против круга 2 и — на генераторе сверки `tests/commonmark.rs`
//!   (каждая строка блоков comrak на его входах) — против CommonMark. Три прохода и слияние интервалов
//!   — линейно; правило флагов проходит наборы по отдельности ([`mask_code_region_sets`]).
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
//! - в пункте списка отступ забора считается от колонки содержимого пункта (`- ` — 2, `1. ` — 3,
//!   вложенные — больше; [`fence_blocks`], круги правок 3–5 задачи 5 M7.1; у круга 2 — от края);
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
    fence_run(t)
}

/// Серия ограждения в начале `t` (отступ уже снят): знак и длина (правила — у [`fence_of`]).
fn fence_run(t: &str) -> Option<(u8, usize)> {
    let c = *t.as_bytes().first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let n = t.bytes().take_while(|&b| b == c).count();
    (n >= 3 && (c == b'~' || !t.as_bytes()[n..].contains(&b'`'))).then_some((c, n))
}

/// Отступ строки в колонках (табуляция — до следующей колонки, кратной 4) и остаток без него.
fn indent_of(s: &str) -> (usize, &str) {
    let mut col = 0;
    let mut i = 0;
    for b in s.bytes() {
        match b {
            b' ' => col += 1,
            b'\t' => col += 4 - col % 4,
            _ => break,
        }
        i += 1;
    }
    (col, &s[i..])
}

/// Маркер пункта списка в начале `t` (отступ уже снят): `-`, `+`, `*` или до девяти цифр с `.`
/// или `)`; за ним — пробелы или табуляции либо конец строки. Длина маркера и число пробелов за
/// ним.
fn list_marker(t: &str) -> Option<(usize, usize)> {
    let b = t.as_bytes();
    let m = match *b.first()? {
        b'-' | b'+' | b'*' => 1,
        c if c.is_ascii_digit() => {
            let n = b.iter().take_while(|c| c.is_ascii_digit()).count();
            if n > 9 || !matches!(b.get(n), Some(b'.' | b')')) {
                return None;
            }
            n + 1
        }
        _ => return None,
    };
    let sp = b[m..]
        .iter()
        .take_while(|&&c| c == b' ' || c == b'\t')
        .count();
    (sp > 0 || m == b.len()).then_some((m, sp))
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
///
/// Пункты списков (круг правок 3 задачи 5 M7.1, решение координатора): отступ забора считается
/// от колонки содержимого текущего пункта (`- ` — 2, `1. ` — 3, у вложенных — больше), так что
/// забор с отступом 4 и больше в пункте — забор. Колонки открытых пунктов — стеком; пункт
/// закрывается строкой левее его колонки после пустой строки или строкой, начинающей блок
/// (маркер, заголовок, ограждение); иначе строка — ленивое продолжение абзаца, и пункт остаётся
/// (в сомнении — шире код). Блок пункта:
/// - забор лишь благодаря пункту (абсолютный отступ больше 3) — блок кончается любой непустой
///   строкой левее колонки пункта (пункт кончился; круг правок 3);
/// - забор на отступе до 3 и забор на строке маркера — строка левее колонки пункта блок не
///   закрывает (как и вне списка, круг правок 4), **кроме строки, начинающей блок** (забор,
///   пункт, заголовок, цитата): по CommonMark она закрывает пункт и открывает свой блок (круг
///   правок 5, N2; `1. ~~~\n~~~\nmysql -p…` — код);
/// - закрывающий забор блока со строки маркера — с отступом от колонки содержимого пункта (круг
///   правок 5, N1: `- a\n  - ```\n    x\n    ```` — блок кончается на `    ```` `).
pub fn fence_blocks(text: &str, lines: &[Line]) -> Vec<FenceBlock> {
    let mut out = Vec::new();
    // открытый блок: знак, длина серии, глубина цитаты, закрывается ли строкой левее пункта,
    // колонка закрывающего забора, колонка пункта, строка открытия
    let mut open: Option<(u8, usize, usize, bool, usize, usize, usize)> = None;
    // колонки содержимого открытых пунктов списка, по возрастанию
    let mut items: Vec<usize> = Vec::new();
    let mut prev_blank = true;
    for (i, l) in lines.iter().enumerate() {
        let s = &text[l.content.clone()];
        if let Some((c, n, d, dedent, cbase, ibase, from)) = open {
            let (q, rest) = strip_quotes(s, d);
            let (ind, t) = indent_of(rest);
            // строка левее колонки пункта: кончает пункт (и блок), если блок так закрывается или
            // строка сама начинает блок — забор, пункт, заголовок, цитата (круг правок 5, N2: по
            // CommonMark ленивого продолжения у блока кода нет)
            let left = !t.is_empty() && ind < ibase;
            let starts =
                fence_run(t).is_some() || list_marker(t).is_some() || t.starts_with(['#', '>']);
            let ends_item = left && (dedent || starts);
            if q == d && !ends_item {
                let closes = ind.saturating_sub(cbase) <= 3
                    && fence_run(t).is_some_and(|(c2, n2)| {
                        c2 == c && n2 >= n && t.trim_end().bytes().all(|b| b == c)
                    });
                if closes {
                    out.push(FenceBlock {
                        open: from,
                        last: i,
                        closed: true,
                    });
                    open = None;
                    prev_blank = false;
                }
                continue;
            }
            // цитата или пункт кончились — блок закрыт до этой строки, сама строка разбирается
            // заново
            out.push(FenceBlock {
                open: from,
                last: i - 1,
                closed: false,
            });
            open = None;
        }
        let (d, rest) = strip_quotes(s, usize::MAX);
        let (mut ind, mut t) = indent_of(rest);
        if t.is_empty() {
            prev_blank = true;
            continue;
        }
        let marker = list_marker(t);
        // цитата (`>`) пункт не закрывает: `  > заметка` в пункте — его часть, а по строке без
        // разбора контейнеров её не отличить (круг правок 4: с `>` здесь в `- a\n  > q\n\n    ```…`
        // блок терялся; на примерах ревьюера разницы нет)
        let starts_block = marker.is_some() || t.starts_with('#') || fence_run(t).is_some();
        while items.last().is_some_and(|&col| col > ind) && (prev_blank || starts_block) {
            items.pop();
        }
        let mut base = items.last().copied().unwrap_or(0);
        let mut on_marker = false;
        let mut marker = marker;
        // маркеров на строке может быть несколько: `* - ````, `1. - ~~~` — вложенные пункты
        while let Some((m, sp)) = marker
            && ind.saturating_sub(base) <= 3
        {
            on_marker = true;
            // колонки пробелов за маркером: табуляция — до следующей колонки, кратной 4
            let mut col = ind + m;
            for b in &t.as_bytes()[m..m + sp] {
                col = if *b == b'\t' {
                    col + 4 - col % 4
                } else {
                    col + 1
                };
            }
            let sp_cols = col - ind - m;
            // колонка содержимого: за маркером и пробелами; пустой пункт (за маркером — только
            // пробелы или ничего) и код с отступом внутри — за одним пробелом
            let empty = t.len() == m + sp;
            let content = if sp == 0 || sp_cols > 4 || empty {
                m + 1
            } else {
                m + sp_cols
            };
            base = ind + content;
            items.push(base);
            t = &t[m + sp..];
            ind = col;
            marker = list_marker(t);
        }
        if let Some((c, n)) = fence_run(t)
            && ind.saturating_sub(base) <= 3
        {
            // см. описание: строкой левее пункта кончается только блок с забором лишь благодаря
            // пункту; закрывающий забор — от колонки пункта, кроме забора на отступе до 3 вне
            // строки маркера (как вне списка — от края)
            let dedent = ind > 3 && !on_marker;
            let cbase = if dedent || on_marker { base } else { 0 };
            open = Some((c, n, d, dedent, cbase, base, i));
        }
        prev_blank = false;
    }
    if let Some((_, _, _, _, _, _, from)) = open {
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

/// Содержимое кода `text` по порядку, без пересечений — как можно ближе к CommonMark
/// ([`fence_blocks`]): строки блоков в ограждениях (без самих ограждений; маркеры цитаты
/// остаются) и содержимое инлайн-кода (без обратных кавычек; спан, закрытый внутри блока,
/// обрезается до начала блока). Пустые участки не входят. Его берёт `zam-markdown`; маска
/// секретов — [`mask_code_regions`].
pub fn code_regions(text: &str) -> Vec<CodeRegion> {
    let lines = lines_of(text);
    regions_of(text, &lines, &fence_blocks(text, &lines))
}

/// Участки кода по правилам круга 2 (коммит `2c4abf03`, до учёта списков): заборы — с отступом
/// до трёх пробелов от края (после маркеров цитаты), пункты списков не разбираются
/// ([`fence_blocks_round2`]); инлайн-код — тот же. Часть [`mask_code_regions`].
pub fn code_regions_round2(text: &str) -> Vec<CodeRegion> {
    let lines = lines_of(text);
    regions_of(text, &lines, &fence_blocks_round2(text, &lines))
}

/// Участки кода по широкому правилу «в сомнении — код» ([`fence_blocks_loose`]): забор — после
/// любых префиксов контейнеров в любом порядке (отступы, `>`, маркеры пунктов: `10) >````,
/// `* - ````), пары заборов — без учёта границ контейнеров. Часть [`mask_code_regions`]: так маска
/// не теряет строк блоков comrak там, где эвристика списков [`fence_blocks`] не разбирает
/// вложение (цитата в пункте, пункт в цитате в пункте).
pub fn code_regions_loose(text: &str) -> Vec<CodeRegion> {
    let lines = lines_of(text);
    regions_of(text, &lines, &fence_blocks_loose(text, &lines))
}

/// Наборы участков маски секретов ([`mask_code_regions`]): по CommonMark, по правилам круга 2 и
/// широкий; совпадающие с первым не повторяются.
pub fn mask_code_region_sets(text: &str) -> Vec<Vec<CodeRegion>> {
    // строки — один раз; участки — только для различающихся наборов блоков (обычный текст без
    // списков: блоки всех трёх правил совпадают — один набор)
    let lines = lines_of(text);
    let mut blocks = vec![fence_blocks(text, &lines)];
    for b in [
        fence_blocks_round2(text, &lines),
        fence_blocks_loose(text, &lines),
    ] {
        if !blocks.contains(&b) {
            blocks.push(b);
        }
    }
    blocks.iter().map(|b| regions_of(text, &lines, b)).collect()
}

/// Участки кода для маски секретов (правило флагов сканера): **объединение** [`code_regions`],
/// [`code_regions_round2`] и широкого [`code_regions_loose`], пересёкшиеся — одним участком (вид
/// — инлайн, если в слиянии есть инлайн-код). Для секретов лишняя маска безопаснее утечки: маска
/// не теряет кода ни против CommonMark (строгая сверка с comrak — тесты крейта), ни против круга
/// 2. Три прохода и слияние интервалов — линейно.
pub fn mask_code_regions(text: &str) -> Vec<CodeRegion> {
    let mut sets = mask_code_region_sets(text);
    if sets.len() == 1 {
        return sets.pop().unwrap_or_default();
    }
    let mut all: Vec<CodeRegion> = sets.into_iter().flatten().collect();
    all.sort_by_key(|r| (r.range.start, std::cmp::Reverse(r.range.end)));
    let mut out: Vec<CodeRegion> = Vec::with_capacity(all.len());
    for r in all {
        match out.last_mut() {
            Some(last) if r.range.start < last.range.end => {
                last.range.end = last.range.end.max(r.range.end);
                if r.kind == CodeKind::Inline {
                    last.kind = CodeKind::Inline;
                }
            }
            _ => out.push(r),
        }
    }
    out
}

/// Блоки в ограждениях по правилам круга 2 (без пунктов списков): забор — [`fence_of`] после
/// маркеров цитаты; закрывает блок забор того же знака не короче с отступом до трёх пробелов или
/// конец цитаты; незакрытый — до конца текста.
// внутреннее правило маски: снаружи — только для сверки в тестах (не часть API)
#[doc(hidden)]
pub fn fence_blocks_round2(text: &str, lines: &[Line]) -> Vec<FenceBlock> {
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

/// Широкие блоки ([`code_regions_loose`]): строка без префиксов контейнеров ([`strip_containers`])
/// — забор ([`fence_run`], отступ не ограничен) — открывает блок, если открытого нет, и
/// закрывает открытый, если это серия того же знака не короче (и только она); незакрытый — до
/// конца текста.
// внутреннее правило маски: снаружи — только для сверки в тестах (не часть API)
#[doc(hidden)]
pub fn fence_blocks_loose(text: &str, lines: &[Line]) -> Vec<FenceBlock> {
    let mut out = Vec::new();
    let mut open: Option<(u8, usize, usize)> = None;
    for (i, l) in lines.iter().enumerate() {
        let t = strip_containers(&text[l.content.clone()]);
        let Some((c2, n2)) = fence_run(t) else {
            continue;
        };
        match open {
            Some((c, n, from)) => {
                if c2 == c && n2 >= n && t.trim_end().bytes().all(|b| b == c) {
                    out.push(FenceBlock {
                        open: from,
                        last: i,
                        closed: true,
                    });
                    open = None;
                }
            }
            None => open = Some((c2, n2, i)),
        }
    }
    if let Some((_, _, from)) = open {
        out.push(FenceBlock {
            open: from,
            last: lines.len() - 1,
            closed: false,
        });
    }
    out
}

/// Строка без префиксов контейнеров: пробелов, табуляций, маркеров цитаты и пунктов списка — в
/// любом порядке и сколько угодно.
fn strip_containers(s: &str) -> &str {
    let mut t = s;
    loop {
        let u = t.trim_start_matches([' ', '\t']);
        if let Some(r) = u.strip_prefix('>') {
            t = r;
            continue;
        }
        if let Some((m, sp)) = list_marker(u)
            && sp > 0
        {
            t = &u[m + sp..];
            continue;
        }
        return u;
    }
}

/// [`fenced_lines`] по правилам круга 2 ([`fence_blocks_round2`]) — для сверки маски прозы и
/// сегментации `zam-markdown` с кругом 2.
// хелпер тестов `zam-markdown` (не часть API)
#[doc(hidden)]
pub fn fenced_lines_round2(text: &str, lines: &[Line]) -> Vec<bool> {
    let mut out = vec![false; lines.len()];
    for b in fence_blocks_round2(text, lines) {
        out[b.open..=b.last].iter_mut().for_each(|x| *x = true);
    }
    out
}

/// Участки кода по блокам `blocks` (см. [`code_regions`]).
fn regions_of(text: &str, lines: &[Line], blocks: &[FenceBlock]) -> Vec<CodeRegion> {
    let mut fenced = vec![false; lines.len()];
    let mut out: Vec<CodeRegion> = Vec::new();
    for b in blocks {
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
    let fences = fence_spans(lines, &fenced);
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

    /// Круг правок 3 задачи 5 M7.1: забор в пункте списка — с отступом от колонки содержимого
    /// пункта (`1. ` — 3, `- ` — 2, вложенный — больше), в том числе через пустую строку и с
    /// пустой строкой внутри блока; забор на строке маркера; блок кончается строкой левее
    /// пункта; ленивое продолжение абзаца пункт не закрывает; вне списка отступ 4 — не забор.
    #[test]
    fn fenced_regions_in_lists() {
        assert_eq!(
            fenced("1. Шаг:\n\n    ```sh\n    mysql -uroot -px\n\n    mysql -px other\n    ```\n"),
            ["    mysql -uroot -px\n\n    mysql -px other"]
        );
        assert_eq!(
            fenced(
                "- Деплой:\n\n    ```bash\n    export A=1\n\n    curl -u a:b https://h\n    ```\n"
            ),
            ["    export A=1\n\n    curl -u a:b https://h"]
        );
        assert_eq!(
            fenced("- a\n  - b\n\n      ```sh\n      x\n      ```\n"),
            ["      x"]
        );
        assert_eq!(fenced("- ```sh\n  x\n  ```\n"), ["  x"]);
        assert_eq!(
            fenced("- a\n\n    ```\n    x\nпроза\n```\n"),
            ["    x"],
            "строка левее пункта закрывает блок; следующий ``` — новый пустой незакрытый"
        );
        assert_eq!(
            fenced("- a\nленивое продолжение\n\n    ```\n    x\n    ```\n"),
            ["    x"]
        );
        assert!(fenced("- a\n\nабзац вне списка\n\n    ```\n    x\n    ```\n").is_empty());
        assert!(fenced("абзац\n    ```\n    x\n    ```\n").is_empty());
        assert_eq!(
            fenced("10) пункт\n     ```\n     y\n     ```\n"),
            ["     y"]
        );
        // табуляция за маркером — колонкой до кратной 4: `-\t` — содержимое с колонки 4
        assert_eq!(
            fenced("-\tпункт\n\n      ```\n      y\n      ```\n"),
            ["      y"]
        );
        // пустой пункт (за маркером только пробелы) — содержимое за одним пробелом (круг 5,
        // случаи сверки с comrak)
        assert_eq!(fenced("-    \n    ```\n   1. x\n"), ["   1. x\n"]);
        assert_eq!(fenced("1.\t\n   ```sh\n ````\nx\n"), ["x\n"]);
    }

    /// Круг правок 4 задачи 5 M7.1: забор пункта на абсолютном отступе до 3 — блок по правилам
    /// круга 2 (до закрывающего забора или конца цитаты), строка левее колонки пункта его не
    /// закрывает; закрывающий забор левее пункта закрывает блок, а не открывает новый (примеры
    /// ревьюера).
    #[test]
    fn fenced_regions_in_lists_shallow_fence() {
        for (t, want) in [
            (
                "- Подключение:\n  ```\nmysql -uroot -px\n  ```\n",
                "mysql -uroot -px",
            ),
            (
                "1. Run:\n   ```sh\n  mysql -uroot -px\n   ```",
                "  mysql -uroot -px",
            ),
            (
                "1. Run:\n   ```sh\nmysql -uroot -px\n   ```",
                "mysql -uroot -px",
            ),
            (
                "- Шаг:\n  ```sh\n  cd /x\nmysql -uroot -px\n  ```",
                "  cd /x\nmysql -uroot -px",
            ),
            ("- a\n> цитата\n  ```\nmysql -px\n  ```", "mysql -px"),
            ("- a\n>   ```\n> mysql -px\n> ```", "> mysql -px"),
            ("- ```sh\nmysql -px\n```", "mysql -px"),
            // забор на строке маркера, за маркером колонка больше 3 — тоже по правилам круга 2
            ("-\t```sh\n  mysql -px\n  ```", "  mysql -px"),
            ("10) ```\nmysql -px\n```", "mysql -px"),
            // цитата в пункте пункт не закрывает
            (
                "- a\n  > заметка\n\n    ```sh\n    mysql -px\n    ```",
                "    mysql -px",
            ),
        ] {
            assert_eq!(fenced(t), [want], "{t:?}");
        }
        // забор левее пункта — по CommonMark конец пункта и новый блок (круг правок 5, N2; в
        // круге 4 он закрывал блок, и дальше шла проза)
        assert_eq!(fenced("- a\n  ```\n  x\n```\nпосле\n"), ["  x", "после\n"]);
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
