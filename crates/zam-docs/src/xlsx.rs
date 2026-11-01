//! XLSX (SpreadsheetML): свой потоковый разбор на `ooxml` — книга, общие строки, листы.
//!
//! Не через `calamine`: он открывает zip сам (в обход подсчёта сжатия и перекрытий записей
//! `ooxml::Package`), а `worksheet_range` строит сетку по границам ячеек и копирует общую
//! строку в каждую ссылающуюся ячейку (замер — журнал этапа, задача 2). Здесь части читаются
//! через `Package` с его лимитами, общие строки хранятся один раз ([`Strings`]), а ячейки
//! листа идут в [`Feed`] по мере разбора: `dimension`, пустые ячейки и строки памяти не
//! занимают. Формулы не вычисляются — берётся сохранённое значение `<v>`; даты остаются
//! числами (стили `numFmt` не разбираются).

use std::collections::{HashMap, HashSet};

use crate::DocError;
use crate::ctx::Ctx;
use crate::ooxml::{Ev, Package, Xml, attr, attr_ns, local};
use crate::sheet::{Feed, Sink, Strings, col_of, number};

/// Значение `<v>` числа или номера общей строки длиннее этого — текст как есть.
const VALUE_MAX: usize = 64;

pub(crate) fn read<S: Sink>(
    bytes: &[u8],
    feed: &mut Feed<'_, S>,
    ctx: &mut Ctx,
) -> Result<(), DocError> {
    let Some(mut pkg) = Package::open(bytes, ctx)? else {
        return Ok(());
    };
    let workbook = pkg
        .rels("", ctx)?
        .into_iter()
        .find(|r| r.kind == "officeDocument")
        .map_or_else(|| "xl/workbook.xml".to_string(), |r| r.target);
    let rels = pkg.rels(&workbook, ctx)?;
    let shared = rels
        .iter()
        .find(|r| r.kind == "sharedStrings")
        .map_or_else(|| "xl/sharedStrings.xml".to_string(), |r| r.target.clone());
    let targets: HashMap<String, String> = rels
        .into_iter()
        .filter(|r| r.kind == "worksheet")
        .map(|r| (r.id, r.target))
        .collect();
    let sheets = sheet_list(&mut pkg, &workbook, &targets, ctx)?;
    let mut strings: Option<Strings> = None;
    let mut seen = HashSet::new();
    for (name, target) in sheets {
        if ctx.stopped() {
            break;
        }
        // лист, на который ссылаются дважды, — один раз (иначе часть читалась бы повторно)
        if !seen.insert(target.to_ascii_lowercase()) || !pkg.has(&target) {
            continue;
        }
        if !feed.sheet(&name, ctx) {
            continue;
        }
        if strings.is_none() {
            strings = Some(read_strings(&mut pkg, &shared, ctx)?);
        }
        let Some(part) = pkg.read(&target, ctx)? else {
            continue;
        };
        sheet(
            &part,
            strings.as_ref().unwrap_or(&Strings::default()),
            feed,
            ctx,
        )?;
        feed.end_sheet(ctx);
    }
    Ok(())
}

/// Листы книги по порядку `workbook.xml`: (имя, часть). Листы диаграмм пропускаются.
fn sheet_list(
    pkg: &mut Package<'_>,
    workbook: &str,
    targets: &HashMap<String, String>,
    ctx: &mut Ctx,
) -> Result<Vec<(String, String)>, DocError> {
    let Some(part) = pkg.read(workbook, ctx)? else {
        return Err(DocError::Malformed("xlsx: нет книги workbook.xml".into()));
    };
    let mut out = Vec::new();
    let mut xml = Xml::new(&part, ctx);
    while let Some(ev) = xml.next(ctx)? {
        if let Ev::Start(e) | Ev::Empty(e) = ev
            && local(&e) == "sheet"
        {
            let (Some(name), Some(id)) = (attr(&e, "name"), attr_ns(&e, "id")) else {
                continue;
            };
            if let Some(t) = targets.get(&id) {
                if out.len() as u64 >= u64::from(ctx.limits.max_entries) {
                    ctx.truncated("max_entries");
                    break;
                }
                out.push((name, t.clone()));
            }
        }
    }
    Ok(out)
}

/// Общие строки: текст `<t>` внутри `<si>` (и его прогонов `<r>`), без фонетики `<rPh>`.
fn read_strings(pkg: &mut Package<'_>, name: &str, ctx: &mut Ctx) -> Result<Strings, DocError> {
    let mut s = Strings::default();
    let Some(part) = pkg.read(name, ctx)? else {
        return Ok(s);
    };
    let mut xml = Xml::new(&part, ctx);
    let (mut in_si, mut in_t) = (false, false);
    while let Some(ev) = xml.next(ctx)? {
        match ev {
            Ev::Start(e) => match local(&e) {
                "si" => {
                    if !s.begin(ctx) {
                        break;
                    }
                    in_si = true;
                }
                "t" if in_si => in_t = true,
                "rPh" => xml.skip(ctx)?,
                _ => {}
            },
            Ev::Empty(e) if local(&e) == "si" => {
                if !s.begin(ctx) {
                    break;
                }
                s.end();
            }
            Ev::End(n) => match n.rsplit(':').next().unwrap_or_default() {
                "si" => {
                    s.end();
                    in_si = false;
                }
                "t" => in_t = false,
                _ => {}
            },
            Ev::Text(t) if in_t => s.push(&t, ctx),
            _ => {}
        }
    }
    s.end();
    s.shrink();
    Ok(s)
}

/// Тип значения ячейки (`t`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ty {
    Number,
    Shared,
    Bool,
    /// Текст значения идёт как есть: `str`, `e`, `d`, `inlineStr`.
    Text,
}

/// Лист: `<sheetData>` → строки → ячейки; остальное (`mergeCells`, `drawing`) не нужно.
fn sheet<S: Sink>(
    part: &crate::ooxml::Part,
    strings: &Strings,
    feed: &mut Feed<'_, S>,
    ctx: &mut Ctx,
) -> Result<(), DocError> {
    let mut xml = Xml::new(part, ctx);
    let mut ty = Ty::Number;
    let (mut in_v, mut in_is, mut in_t) = (false, false, false);
    let mut value = String::new();
    let mut long = false;
    let mut num = String::new();
    while let Some(ev) = xml.next(ctx)? {
        if !feed.live(ctx) {
            break;
        }
        match ev {
            Ev::Start(e) => match local(&e) {
                "row" => feed.row(attr(&e, "r").and_then(|r| r.parse().ok()), ctx),
                "c" => {
                    ty = cell_type(attr(&e, "t").as_deref());
                    feed.cell(attr(&e, "r").as_deref().and_then(col_of), ctx);
                }
                "v" => {
                    in_v = true;
                    value.clear();
                    long = false;
                }
                "is" => in_is = true,
                "t" if in_is => in_t = true,
                "f" | "rPh" | "extLst" => xml.skip(ctx)?,
                _ => {}
            },
            Ev::Empty(e) => match local(&e) {
                "row" => {
                    feed.row(attr(&e, "r").and_then(|r| r.parse().ok()), ctx);
                    feed.end_row(ctx);
                }
                "c" => {
                    feed.cell(attr(&e, "r").as_deref().and_then(col_of), ctx);
                    feed.end_cell(ctx);
                }
                _ => {}
            },
            Ev::Text(t) => {
                if in_t || (in_v && ty == Ty::Text) {
                    feed.text(&t, ctx);
                } else if in_v {
                    if long || value.len() + t.len() > VALUE_MAX {
                        // не число и не номер строки: выдаётся как есть, без накопления
                        if !long {
                            feed.text(&value, ctx);
                            long = true;
                        }
                        feed.text(&t, ctx);
                    } else {
                        value.push_str(&t);
                    }
                }
            }
            Ev::End(n) => match n.as_str() {
                "v" => {
                    in_v = false;
                    if !long {
                        emit_value(ty, &value, strings, &mut num, feed, ctx);
                    }
                }
                "c" => feed.end_cell(ctx),
                "row" => feed.end_row(ctx),
                "is" => in_is = false,
                "t" => in_t = false,
                "sheetData" => break,
                _ => {}
            },
        }
    }
    Ok(())
}

fn cell_type(t: Option<&str>) -> Ty {
    match t {
        Some("s") => Ty::Shared,
        Some("b") => Ty::Bool,
        None | Some("n") => Ty::Number,
        Some(_) => Ty::Text,
    }
}

fn emit_value<S: Sink>(
    ty: Ty,
    v: &str,
    strings: &Strings,
    num: &mut String,
    feed: &mut Feed<'_, S>,
    ctx: &mut Ctx,
) {
    let v = v.trim();
    match ty {
        Ty::Shared => {
            if let Ok(i) = v.parse::<usize>() {
                feed.text(strings.get(i), ctx);
            }
        }
        Ty::Bool => feed.text(if v == "1" { "TRUE" } else { "FALSE" }, ctx),
        Ty::Number => match v.parse::<f64>() {
            Ok(f) if f.is_finite() => {
                number(f, num);
                feed.text(num, ctx);
            }
            _ => feed.text(v, ctx),
        },
        Ty::Text => {}
    }
}
