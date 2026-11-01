//! Раскладка текста страницы PDF без движка: символы с прямоугольниками → фрагменты строк →
//! порядок чтения рекурсивным XY-cut → строки; колонтитулы по повторам на страницах.
//!
//! Координаты — пространство страницы PDF (ось y вверх), единица — пункт.
//!
//! - **Фрагмент** — подряд идущие в потоке символы одной строки без большого разрыва:
//!   разрыв больше высоты фрагмента, смена строки или перевод строки начинают новый фрагмент.
//! - **XY-cut**: в области ищутся вертикальные просветы (столбцы, по оси x) не уже
//!   [`COLUMN_GAP`] × медианной высоты; есть — область режется по всем таким просветам слева
//!   направо. Иначе — горизонтальные просветы (полосы по оси y): если ни в одной полосе нет
//!   столбцов, полосы и есть строки (сверху вниз); иначе область режется по самому широкому
//!   просвету (при равных — по верхнему), и половины разбираются заново — так заголовок над
//!   двумя колонками отделяется до разреза колонок.
//! - **Колонтитулы**: строка целиком в верхних или нижних [`EDGE_ZONE`] высоты страницы,
//!   повторяющаяся (цифры не различаются — номера страниц) на ≥ [`REPEAT_SHARE`] страниц.
//!
//! Работа ограничена сроком: [`Clock::expired`] сверяется в циклах.

use std::collections::HashMap;
use std::time::Instant;

/// Ширина просвета-столбца в медианных высотах фрагмента.
pub(crate) const COLUMN_GAP: f32 = 0.8;
/// Доля высоты страницы сверху и снизу, где ищутся колонтитулы.
pub(crate) const EDGE_ZONE: f32 = 0.08;
/// Доля страниц, на которых строка-колонтитул должна повториться.
pub(crate) const REPEAT_SHARE: f64 = 0.6;

/// Символ страницы с прямоугольником (`x0 < x1`, `y0 < y1`; у пробелов может быть пустым).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Glyph {
    pub c: char,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// Строка в порядке чтения: текст и вертикальные границы.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Row {
    pub text: String,
    pub y0: f32,
    pub y1: f32,
}

/// Срок: проверка раз в 1024 вызова.
pub(crate) struct Clock {
    deadline: Instant,
    n: u32,
    expired: bool,
}

impl Clock {
    pub fn new(deadline: Instant) -> Self {
        Clock {
            deadline,
            n: 0,
            expired: false,
        }
    }

    /// `true` — срок истёк (один раз истёкший остаётся истёкшим).
    pub fn expired(&mut self) -> bool {
        if !self.expired {
            self.n = self.n.wrapping_add(1);
            if self.n.is_multiple_of(1024) && Instant::now() >= self.deadline {
                self.expired = true;
            }
        }
        self.expired
    }
}

#[derive(Debug, Clone)]
struct Frag {
    text: String,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Frag {
    fn h(&self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }
}

/// Символ — разделитель строк в потоке pdfium.
fn is_newline(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// Символ не попадает в текст: управляющие (кроме пробельных), «не символы» Юникода.
fn is_junk(c: char) -> bool {
    (c.is_control() && !c.is_whitespace()) || matches!(c, '\u{FFFE}' | '\u{FFFF}')
}

/// Фрагменты строк из символов в порядке потока.
fn fragments(glyphs: &[Glyph], clock: &mut Clock) -> Vec<Frag> {
    let mut out: Vec<Frag> = Vec::new();
    let mut cur: Option<Frag> = None;
    let mut space = false;
    for g in glyphs {
        if clock.expired() {
            break;
        }
        if is_newline(g.c) {
            out.extend(cur.take());
            space = false;
            continue;
        }
        if g.c.is_whitespace() {
            space = cur.is_some();
            continue;
        }
        if is_junk(g.c) || !(g.x0.is_finite() && g.y0.is_finite()) {
            continue;
        }
        let (x0, x1) = (g.x0.min(g.x1), g.x0.max(g.x1));
        let (y0, y1) = (g.y0.min(g.y1), g.y0.max(g.y1));
        if let Some(f) = &mut cur {
            let h = f.h().max(y1 - y0).max(1.0);
            let overlap = f.y1.min(y1) - f.y0.max(y0);
            let center = (y0 + y1) / 2.0;
            let same_line = overlap >= 0.5 * f.h().min(y1 - y0) || (f.y0..=f.y1).contains(&center);
            let gap = x0 - f.x1;
            if same_line && gap > -0.5 * h && gap <= h {
                // пробелы — только из потока (pdfium сам добавляет их по разрывам): просвет
                // между плотными прямоугольниками узких букв (`i`, `l`) не означает пробела
                if space {
                    f.text.push(' ');
                }
                f.text.push(g.c);
                f.x0 = f.x0.min(x0);
                f.x1 = f.x1.max(x1);
                f.y0 = f.y0.min(y0);
                f.y1 = f.y1.max(y1);
                space = false;
                continue;
            }
            out.extend(cur.take());
        }
        space = false;
        cur = Some(Frag {
            text: g.c.to_string(),
            x0,
            y0,
            x1,
            y1,
        });
    }
    out.extend(cur);
    out
}

/// Просветы проекции отрезков `[a, b]` шириной не меньше `min`: (ширина, середина).
fn gaps(mut spans: Vec<(f32, f32)>, min: f32) -> Vec<(f32, f32)> {
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = Vec::new();
    let mut end = f32::NEG_INFINITY;
    for (a, b) in spans {
        if end > f32::NEG_INFINITY && a - end >= min && a - end > 0.0 {
            out.push((a - end, (a + end) / 2.0));
        }
        end = end.max(b);
    }
    out
}

/// Медианная высота фрагментов (не меньше 1 пункта).
fn median_h(frags: &[Frag]) -> f32 {
    let mut hs: Vec<f32> = frags.iter().map(Frag::h).collect();
    if hs.is_empty() {
        return 1.0;
    }
    let mid = hs.len() / 2;
    hs.select_nth_unstable_by(mid, f32::total_cmp);
    hs[mid].max(1.0)
}

/// Полосы области по горизонтальным просветам, сверху вниз.
fn bands(frags: &[Frag], idx: &[usize]) -> Vec<Vec<usize>> {
    let mut sorted = idx.to_vec();
    // сверху вниз: по верхней границе убывающе
    sorted.sort_by(|&a, &b| frags[b].y1.total_cmp(&frags[a].y1));
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut low = f32::INFINITY;
    for i in sorted {
        let f = &frags[i];
        match out.last_mut() {
            Some(band) if f.y1 > low => {
                band.push(i);
                low = low.min(f.y0);
            }
            _ => {
                out.push(vec![i]);
                low = f.y0;
            }
        }
    }
    out
}

fn has_column_gap(frags: &[Frag], idx: &[usize], min: f32) -> bool {
    idx.len() > 1
        && !gaps(
            idx.iter().map(|&i| (frags[i].x0, frags[i].x1)).collect(),
            min,
        )
        .is_empty()
}

/// Порядок чтения фрагментов: строки (фрагменты строки слева направо) сверху вниз по XY-cut.
fn xy_cut(frags: &[Frag], clock: &mut Clock) -> Vec<Vec<usize>> {
    let min_col = COLUMN_GAP * median_h(frags);
    let mut rows: Vec<Vec<usize>> = Vec::new();
    // стек областей; дети кладутся в обратном порядке, чтобы выходить по порядку чтения
    let mut stack: Vec<Vec<usize>> = vec![(0..frags.len()).collect()];
    while let Some(region) = stack.pop() {
        if clock.expired() {
            // остаток — как есть, полосами
            rows.extend(bands(frags, &region));
            continue;
        }
        if region.len() <= 1 {
            rows.extend((!region.is_empty()).then_some(region));
            continue;
        }
        // столбцы: все вертикальные просветы шире порога
        let cols = gaps(
            region.iter().map(|&i| (frags[i].x0, frags[i].x1)).collect(),
            min_col,
        );
        if !cols.is_empty() {
            let mut parts: Vec<Vec<usize>> = vec![Vec::new(); cols.len() + 1];
            for &i in &region {
                let cx = frags[i].x0;
                let k = cols.iter().take_while(|(_, mid)| cx > *mid).count();
                parts[k].push(i);
            }
            stack.extend(parts.into_iter().rev().filter(|p| !p.is_empty()));
            continue;
        }
        let bs = bands(frags, &region);
        if bs.len() == 1 || !bs.iter().any(|b| has_column_gap(frags, b, min_col)) {
            for mut b in bs {
                b.sort_by(|&a, &c| frags[a].x0.total_cmp(&frags[c].x0));
                rows.push(b);
            }
            continue;
        }
        // самый широкий просвет между полосами (при равных — верхний)
        let mut best = (f32::NEG_INFINITY, 1usize);
        for k in 1..bs.len() {
            let low = bs[k - 1]
                .iter()
                .map(|&i| frags[i].y0)
                .fold(f32::INFINITY, f32::min);
            let high = bs[k]
                .iter()
                .map(|&i| frags[i].y1)
                .fold(f32::NEG_INFINITY, f32::max);
            let gap = low - high;
            if gap > best.0 + 1e-3 {
                best = (gap, k);
            }
        }
        let (top, bottom) = bs.split_at(best.1);
        stack.push(bottom.concat());
        stack.push(top.concat());
    }
    rows
}

/// Строки страницы в порядке чтения.
pub(crate) fn page_rows(glyphs: &[Glyph], clock: &mut Clock) -> Vec<Row> {
    let frags = fragments(glyphs, clock);
    let order = xy_cut(&frags, clock);
    let mut out = Vec::with_capacity(order.len());
    for row in order {
        let mut text = String::new();
        let (mut y0, mut y1) = (f32::INFINITY, f32::NEG_INFINITY);
        for i in row {
            let f = &frags[i];
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&f.text);
            y0 = y0.min(f.y0);
            y1 = y1.max(f.y1);
        }
        out.push(Row { text, y0, y1 });
    }
    out
}

/// Ключ строки для поиска колонтитулов: пробелы схлопнуты, цифры не различаются.
fn edge_key(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    for w in text.split_whitespace() {
        if !s.is_empty() {
            s.push(' ');
        }
        s.extend(w.chars().map(|c| if c.is_numeric() { '#' } else { c }));
    }
    s
}

/// Строка целиком в верхней или нижней зоне страницы высотой `[bottom, top]`.
fn in_edge(row: &Row, bottom: f32, top: f32) -> bool {
    let zone = (top - bottom) * EDGE_ZONE;
    row.y0 >= top - zone || row.y1 <= bottom + zone
}

/// Сколько строк зон колонтитулов страницы запоминается (ближайшие к краям — первыми).
pub(crate) const EDGE_ROWS_MAX: usize = 32;

/// Текст страницы и строки-кандидаты в колонтитулы: байтовый диапазон в тексте и ключ.
#[derive(Debug, Default)]
pub(crate) struct PageText {
    pub text: String,
    pub edges: Vec<(std::ops::Range<usize>, String)>,
}

/// Текст страницы из строк: `\n` между строками, пустая строка — при переходе вверх (новая
/// колонка) или при просвете больше высоты строки (абзац). Строки в зонах колонтитулов страницы
/// `[bottom, top]` запоминаются (не больше [`EDGE_ROWS_MAX`]).
pub(crate) fn join_rows(rows: &[Row], bottom: f32, top: f32) -> PageText {
    let mut out = PageText::default();
    let mut cands: Vec<(f32, std::ops::Range<usize>)> = Vec::new();
    let mut prev: Option<&Row> = None;
    for r in rows {
        if let Some(p) = prev {
            let h = (p.y1 - p.y0).max(r.y1 - r.y0).max(1.0);
            let gap = p.y0 - r.y1;
            out.text.push('\n');
            if r.y1 > p.y1 || gap > h {
                out.text.push('\n');
            }
        }
        let start = out.text.len();
        out.text.push_str(&r.text);
        if in_edge(r, bottom, top) {
            // расстояние до ближайшего края страницы
            let dist = (top - r.y1).min(r.y0 - bottom).max(0.0);
            cands.push((dist, start..out.text.len()));
        }
        prev = Some(r);
    }
    cands.sort_by(|a, b| a.0.total_cmp(&b.0));
    cands.truncate(EDGE_ROWS_MAX);
    cands.sort_by_key(|(_, r)| r.start);
    out.edges = cands
        .into_iter()
        .map(|(_, r)| {
            let key = edge_key(&out.text[r.clone()]);
            (r, key)
        })
        .collect();
    out
}

/// Ключи строк, повторяющихся в зонах колонтитулов на ≥ [`REPEAT_SHARE`] страниц (из `pages`
/// страниц документа, не меньше двух).
pub(crate) fn repeated_edges<'a>(
    pages: impl IntoIterator<Item = &'a PageText>,
    total: usize,
) -> HashMap<String, usize> {
    let mut count: HashMap<String, usize> = HashMap::new();
    if total < 2 {
        return count;
    }
    let need = (total as f64 * REPEAT_SHARE).ceil() as usize;
    for p in pages {
        let mut seen: Vec<&str> = Vec::new();
        for (_, k) in &p.edges {
            if !k.is_empty() && !seen.contains(&k.as_str()) {
                seen.push(k);
            }
        }
        for k in seen {
            *count.entry(k.to_string()).or_default() += 1;
        }
    }
    count.retain(|_, n| *n >= need);
    count
}

/// Убирает из текста страницы колонтитулы (`repeated`), пустые строки по краям и лишние
/// пустые строки на месте вырезанного — без новой строки, если вырезать нечего.
pub(crate) fn strip_edges(page: PageText, repeated: &HashMap<String, usize>) -> String {
    let cut: Vec<&std::ops::Range<usize>> = page
        .edges
        .iter()
        .filter(|(_, k)| repeated.contains_key(k))
        .map(|(r, _)| r)
        .collect();
    if cut.is_empty() {
        return page.text;
    }
    let mut out = String::with_capacity(page.text.len());
    let mut at = 0;
    for r in cut {
        out.push_str(&page.text[at..r.start]);
        at = r.end;
    }
    out.push_str(&page.text[at..]);
    // схлопнуть 3+ переводов строки в 2
    let mut res = String::with_capacity(out.len());
    let mut nl = 0;
    for c in out.chars() {
        if c == '\n' {
            nl += 1;
            if nl <= 2 {
                res.push(c);
            }
        } else {
            nl = 0;
            res.push(c);
        }
    }
    res.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Слово шрифтом высоты 10 с шириной символа 6 начиная с `(x, y)` (низ строки).
    fn word(out: &mut Vec<Glyph>, s: &str, x: f32, y: f32) {
        for (i, c) in s.chars().enumerate() {
            let x0 = x + i as f32 * 6.0;
            out.push(Glyph {
                c,
                x0,
                y0: y,
                x1: x0 + 5.0,
                y1: y + 10.0,
            });
        }
    }

    fn clock() -> Clock {
        Clock::new(Instant::now() + Duration::from_secs(60))
    }

    fn texts(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn spaces_inside_row_and_newlines_split() {
        let mut g = Vec::new();
        word(&mut g, "ab", 0.0, 100.0);
        g.push(Glyph {
            c: ' ',
            x0: 0.0,
            y0: 0.0,
            x1: 0.0,
            y1: 0.0,
        });
        word(&mut g, "cd", 15.0, 100.0);
        g.push(Glyph {
            c: '\n',
            x0: 0.0,
            y0: 0.0,
            x1: 0.0,
            y1: 0.0,
        });
        word(&mut g, "ef", 0.0, 80.0);
        let rows = page_rows(&g, &mut clock());
        assert_eq!(texts(&rows), ["ab cd", "ef"]);
    }

    #[test]
    fn interleaved_columns_read_column_by_column() {
        // поток: левая строка, правая строка, … — как у вёрстки построчно через обе колонки
        let mut g = Vec::new();
        // заголовок шире просвета между колонками: сначала отрезается он, затем колонки
        word(&mut g, "Wide_title_over_both_columns", 0.0, 200.0);
        for k in 0..3 {
            let y = 170.0 - 14.0 * k as f32;
            word(&mut g, &format!("L{k}"), 0.0, y);
            word(&mut g, &format!("R{k}"), 100.0, y);
        }
        let rows = page_rows(&g, &mut clock());
        assert_eq!(
            texts(&rows),
            [
                "Wide_title_over_both_columns",
                "L0",
                "L1",
                "L2",
                "R0",
                "R1",
                "R2"
            ]
        );
    }

    #[test]
    fn headers_repeating_on_pages_are_stripped() {
        let mk = |n: usize| {
            let rows = vec![
                Row {
                    text: "Report 2026".into(),
                    y0: 780.0,
                    y1: 790.0,
                },
                Row {
                    text: format!("body {n}"),
                    y0: 400.0,
                    y1: 410.0,
                },
                Row {
                    text: format!("Page {n}"),
                    y0: 10.0,
                    y1: 20.0,
                },
            ];
            join_rows(&rows, 0.0, 800.0)
        };
        let pages: Vec<PageText> = (1..=5).map(mk).collect();
        let rep = repeated_edges(&pages, pages.len());
        assert_eq!(rep.len(), 2);
        for (n, p) in pages.into_iter().enumerate() {
            assert_eq!(strip_edges(p, &rep), format!("body {}", n + 1));
        }
        // одна страница — колонтитулов нет
        let one = [mk(1)];
        assert!(repeated_edges(&one, 1).is_empty());
    }

    #[test]
    fn expired_clock_still_returns_rows() {
        let mut g = Vec::new();
        for k in 0..3000 {
            word(&mut g, "x", (k % 50) as f32 * 7.0, (k / 50) as f32 * 12.0);
        }
        let mut c = Clock::new(Instant::now());
        let rows = page_rows(&g, &mut c);
        assert!(c.expired);
        let _ = rows;
    }
}
