//! ODS (OpenDocument Spreadsheet): свой потоковый разбор `content.xml` на `ooxml`.
//!
//! Не через `calamine`: он разбирает все листы при открытии и разворачивает повторы
//! `number-columns-repeated`/`number-rows-repeated` в сетку, копируя значение ячейки в каждую
//! копию (замер — журнал этапа, задача 2). Здесь повтор пустых строк и ячеек — только сдвиг
//! номера, а непустые повторы выдаются по одной копии в [`Feed`] в пределах `max_cells` и
//! `max_chars`. Текст ячейки — отображаемый (`text:p`), без него — значение `office:value`.

use crate::DocError;
use crate::ctx::Ctx;
use crate::ooxml::{Ev, Package, Xml, attr_ns, local};
use crate::sheet::{Feed, Sink};

pub(crate) fn read<S: Sink>(
    bytes: &[u8],
    feed: &mut Feed<'_, S>,
    ctx: &mut Ctx,
) -> Result<(), DocError> {
    let Some(mut pkg) = Package::open(bytes, ctx)? else {
        return Ok(());
    };
    if let Some(m) = pkg.read("META-INF/manifest.xml", ctx)?
        && m.text.contains("encryption-data")
    {
        ctx.warn("skipped: content.xml".into());
        return Ok(());
    }
    let Some(part) = pkg.read("content.xml", ctx)? else {
        if ctx.stopped() {
            return Ok(());
        }
        return Err(DocError::Malformed("ods: нет content.xml".into()));
    };
    let mut xml = Xml::new(&part, ctx);
    let mut row = Row::default();
    let mut in_table = false;
    while let Some(ev) = xml.next(ctx)? {
        if ctx.stopped() {
            break;
        }
        match ev {
            Ev::Start(e) if local(&e) == "table" && !in_table => {
                let name = attr_ns(&e, "name").unwrap_or_default();
                if feed.sheet(&name, ctx) {
                    in_table = true;
                } else {
                    xml.skip(ctx)?;
                }
            }
            Ev::End(n) if n == "table" && in_table => {
                feed.end_sheet(ctx);
                in_table = false;
            }
            Ev::Start(e) if in_table && (!feed.live(ctx) || local(&e) == "table") => {
                // лист исчерпан (`max_cells`) или вложенная таблица: поддерево пропускается
                xml.skip(ctx)?;
            }
            Ev::Start(e) if in_table && local(&e) == "table-row" => {
                row.begin(repeat(&e, "number-rows-repeated"));
                read_row(&mut xml, &mut row, ctx)?;
                row.replay(feed, ctx);
            }
            Ev::Empty(e) if in_table && local(&e) == "table-row" => {
                feed.skip_rows(repeat(&e, "number-rows-repeated"), ctx);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Число повторов из атрибута (`table:number-…-repeated`), не меньше 1.
fn repeat(e: &quick_xml::events::BytesStart<'_>, name: &str) -> u32 {
    attr_ns(e, name)
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(1)
        .max(1)
}

/// Строка ODS до развёртки повторов: ячейки (повтор, текст) и общий объём текста.
#[derive(Default)]
struct Row {
    repeat: u32,
    cells: Vec<(u32, String)>,
    chars: u64,
    full: bool,
}

impl Row {
    fn begin(&mut self, repeat: u32) {
        self.repeat = repeat;
        self.cells.clear();
        self.chars = 0;
        self.full = false;
    }

    /// Текст в последнюю ячейку строки: строка не длиннее `max_chars` символов.
    fn push(&mut self, s: &str, ctx: &mut Ctx) {
        if self.full || s.is_empty() {
            return;
        }
        let Some((_, cell)) = self.cells.last_mut() else {
            return;
        };
        let left = ctx.limits.max_chars.saturating_sub(self.chars);
        let cut = if s.len() as u64 <= left {
            s.len()
        } else {
            s.char_indices()
                .nth(left as usize)
                .map_or(s.len(), |(i, _)| i)
        };
        cell.push_str(&s[..cut]);
        self.chars += s[..cut].chars().count() as u64;
        if cut < s.len() {
            self.full = true;
            ctx.truncated("max_chars");
        }
    }

    /// Повторы строки и ячеек — в приёмник; пустая строка — только сдвиг номера.
    fn replay<S: Sink>(&mut self, feed: &mut Feed<'_, S>, ctx: &mut Ctx) {
        if self.cells.iter().all(|(_, t)| t.is_empty()) {
            feed.skip_rows(self.repeat, ctx);
            return;
        }
        for _ in 0..self.repeat {
            if !feed.live(ctx) || !ctx.tick() {
                return;
            }
            feed.row(None, ctx);
            for (n, text) in &self.cells {
                if text.is_empty() {
                    feed.skip_cols(*n);
                    continue;
                }
                for _ in 0..(*n).min(feed.cols_left()) {
                    feed.put(None, text, ctx);
                    if !feed.live(ctx) {
                        return;
                    }
                }
            }
            feed.end_row(ctx);
        }
    }
}

/// Ячейки строки до её конца: текст абзацев `text:p` (через перевод строки), пробелы
/// `text:s`, табуляция, разрыв строки; примечания и вложенные таблицы пропускаются.
fn read_row(xml: &mut Xml<'_>, row: &mut Row, ctx: &mut Ctx) -> Result<(), DocError> {
    let mut depth = 1u32;
    let mut value: Option<String> = None;
    let mut in_cell = false;
    let mut in_p = false;
    let mut paras = 0u32;
    while let Some(ev) = xml.next(ctx)? {
        match ev {
            Ev::Start(e) => {
                depth += 1;
                match local(&e) {
                    "table-cell" | "covered-table-cell" if !in_cell => {
                        row.cells
                            .push((repeat(&e, "number-columns-repeated"), String::new()));
                        value = cell_value(&e);
                        in_cell = true;
                        paras = 0;
                    }
                    "p" | "h" if in_cell => {
                        if paras > 0 {
                            row.push("\n", ctx);
                        }
                        paras += 1;
                        in_p = true;
                    }
                    "annotation" | "table" => {
                        xml.skip(ctx)?;
                        depth -= 1;
                    }
                    _ => {}
                }
            }
            Ev::Empty(e) => match local(&e) {
                "table-cell" | "covered-table-cell" if !in_cell => {
                    row.cells
                        .push((repeat(&e, "number-columns-repeated"), String::new()));
                    if let Some(v) = cell_value(&e) {
                        row.push(&v, ctx);
                    }
                }
                "s" if in_cell => {
                    let n = attr_ns(&e, "c")
                        .and_then(|c| c.parse::<usize>().ok())
                        .unwrap_or(1)
                        .clamp(1, 1024);
                    row.push(&" ".repeat(n), ctx);
                }
                "tab" if in_cell => row.push("\t", ctx),
                "line-break" if in_cell => row.push("\n", ctx),
                _ => {}
            },
            Ev::Text(t) if in_p => row.push(&t, ctx),
            Ev::End(n) => {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
                if in_p && (n == "p" || n == "h") {
                    in_p = false;
                }
                if in_cell && (n == "table-cell" || n == "covered-table-cell") {
                    in_p = false;
                    in_cell = false;
                    if paras == 0
                        && let Some(v) = value.take()
                    {
                        row.push(&v, ctx);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Значение ячейки из атрибутов по `office:value-type` (когда нет отображаемого текста).
fn cell_value(e: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    let key = match attr_ns(e, "value-type").as_deref() {
        Some("float" | "percentage" | "currency") => "value",
        Some("date") => "date-value",
        Some("time") => "time-value",
        Some("boolean") => "boolean-value",
        Some("string") => "string-value",
        _ => return None,
    };
    attr_ns(e, key)
}
