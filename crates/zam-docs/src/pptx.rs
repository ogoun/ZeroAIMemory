//! PPTX: слайды по порядку `p:sldIdLst` из `ppt/presentation.xml`, текст фигур и таблиц;
//! заметки докладчика — отдельная единица слайда с путём `[заголовок, «Заметки докладчика»]`.

use std::collections::{HashMap, HashSet};

use crate::ctx::{Ctx, Units, path_label, trim_in_place};
use crate::ooxml::{Ev, Package, Table, Xml, attr, attr_ns, local};
use crate::{DocError, Locator, Unit};

/// Последний элемент `heading_path` единицы заметок докладчика.
pub(crate) const NOTES: &str = "Заметки докладчика";

/// Заполнители заметок, не несущие текста заметки (миниатюра слайда, номер, колонтитулы).
const NOTES_SKIP: &[&str] = &["sldImg", "sldNum", "hdr", "ftr", "dt"];

pub(crate) fn extract(bytes: &[u8], ctx: &mut Ctx) -> Result<Vec<Unit>, DocError> {
    let Some(mut pkg) = Package::open(bytes, ctx)? else {
        return Ok(Vec::new());
    };
    let main = pkg
        .rels("", ctx)?
        .into_iter()
        .find(|r| r.kind == "officeDocument")
        .map(|r| r.target)
        .filter(|t| pkg.has(t))
        .unwrap_or_else(|| "ppt/presentation.xml".to_string());
    let slides = slide_order(&mut pkg, &main, ctx)?;
    let mut units = Units::default();
    for (i, slide) in slides.iter().enumerate() {
        if ctx.chars_full() {
            ctx.stop("max_chars");
        }
        if !ctx.tick() {
            break;
        }
        let n = u32::try_from(i + 1).unwrap_or(u32::MAX);
        let Some(text) = shapes(&mut pkg, slide, &[], ctx)? else {
            continue;
        };
        let title: Vec<String> = text.title.into_iter().collect();
        units.push(Locator::Slide(n), title.clone(), text.body, ctx);
        let notes = pkg
            .rels(slide, ctx)?
            .into_iter()
            .find(|r| r.kind == "notesSlide")
            .map(|r| r.target);
        if let Some(notes) = notes
            && let Some(t) = shapes(&mut pkg, &notes, NOTES_SKIP, ctx)?
        {
            let mut path = title;
            path.push(NOTES.to_string());
            units.push(Locator::Slide(n), path, t.body, ctx);
        }
    }
    Ok(units.finish(ctx))
}

/// Части слайдов в порядке показа; без `presentation.xml` — `ppt/slides/slideN.xml` по N.
fn slide_order(pkg: &mut Package, main: &str, ctx: &mut Ctx) -> Result<Vec<String>, DocError> {
    let rels = pkg.rels(main, ctx)?;
    let slides: HashMap<&str, &str> = rels
        .iter()
        .filter(|r| r.kind == "slide")
        .map(|r| (r.id.as_str(), r.target.as_str()))
        .collect();
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    if let Some(part) = pkg.read(main, ctx)? {
        let mut xml = Xml::new(&part, ctx);
        while let Some(ev) = xml.next(ctx)? {
            if let Ev::Start(e) | Ev::Empty(e) = ev
                && local(&e) == "sldId"
                && let Some(id) = attr_ns(&e, "id")
                && let Some(target) = slides.get(id.as_str())
                && seen.insert((*target).to_string())
            {
                out.push((*target).to_string());
            }
        }
    }
    if out.is_empty() && !ctx.stopped() {
        out = rels
            .iter()
            .filter(|r| r.kind == "slide" && seen.insert(r.target.clone()))
            .map(|r| r.target.clone())
            .collect();
        out.sort_by_key(|t| slide_number(t));
    }
    Ok(out)
}

/// Номер из имени `…/slideN.xml` (запасной порядок).
fn slide_number(name: &str) -> u32 {
    let digits: String = name
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap_or(u32::MAX)
}

/// Текст слайда: заголовок (заполнитель `title`/`ctrTitle`) и строки всех фигур по порядку.
struct SlideText {
    title: Option<String>,
    body: String,
}

/// Текст фигур части слайда или заметок; `skip` — типы заполнителей, которые пропускаются.
fn shapes(
    pkg: &mut Package,
    name: &str,
    skip: &[&str],
    ctx: &mut Ctx,
) -> Result<Option<SlideText>, DocError> {
    let Some(part) = pkg.read(name, ctx)? else {
        return Ok(None);
    };
    let mut xml = Xml::new(&part, ctx);
    // текст слайда собирается сразу в одну строку (без промежуточного списка и `join`)
    let mut body = String::new();
    let add = |body: &mut String, line: &str| {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(line);
    };
    let mut title: Option<String> = None;
    // текущая фигура: строки, заголовок ли, пропускается ли
    let mut shape: Option<(Vec<String>, bool, bool)> = None;
    let (mut para, mut in_p, mut in_t) = (String::new(), false, false);
    let mut tbl_depth = 0u32;
    let mut table = Table::default();
    let mut pending = 0u64;
    while let Some(ev) = xml.next(ctx)? {
        match ev {
            Ev::Start(e) => match local(&e) {
                "Fallback" => xml.skip(ctx)?,
                "sp" => shape = Some((Vec::new(), false, false)),
                "p" => {
                    in_p = true;
                    para.clear();
                }
                "t" => in_t = true,
                "tbl" => {
                    tbl_depth += 1;
                    if tbl_depth == 1 {
                        table = Table::default();
                    }
                }
                "ph" => placeholder(&e, &mut shape, skip),
                _ => {}
            },
            Ev::Empty(e) => match local(&e) {
                "ph" => placeholder(&e, &mut shape, skip),
                "br" if in_p => ctx.append(&mut para, "\n", &mut pending),
                _ => {}
            },
            Ev::End(n) => match n.as_str() {
                "t" => in_t = false,
                "p" if in_p => {
                    in_p = false;
                    let mut line = std::mem::take(&mut para);
                    trim_in_place(&mut line);
                    if line.is_empty() {
                    } else if tbl_depth > 0 {
                        table.text(&line);
                    } else if let Some((sl, _, _)) = &mut shape {
                        sl.push(line);
                    } else {
                        add(&mut body, &line);
                    }
                }
                "tc" if tbl_depth == 1 => table.end_cell(),
                "tr" if tbl_depth == 1 => table.end_row(),
                "tbl" => {
                    tbl_depth = tbl_depth.saturating_sub(1);
                    if tbl_depth == 0 {
                        let t = std::mem::take(&mut table).render();
                        if !t.is_empty() {
                            add(&mut body, &t);
                        }
                    }
                }
                "sp" => {
                    if let Some((sl, is_title, skipped)) = shape.take()
                        && !skipped
                        && !sl.is_empty()
                    {
                        if is_title && title.is_none() {
                            title = Some(path_label(sl.iter().map(String::as_str)));
                        }
                        for l in sl {
                            add(&mut body, &l);
                        }
                    }
                }
                _ => {}
            },
            Ev::Text(t) => {
                if in_p && in_t {
                    ctx.append(&mut para, &t, &mut pending);
                }
            }
        }
        // остаток `max_chars` исчерпан: собранное выдаётся, остальное не разбирается
        if ctx.chars_full() && !in_p && tbl_depth == 0 && shape.is_none() {
            break;
        }
    }
    // часть оборвана лимитом посреди фигуры или таблицы: собранное не теряется
    trim_in_place(&mut para);
    if tbl_depth > 0 {
        if in_p && !para.is_empty() {
            table.text(&para);
        }
        let t = table.render_partial();
        if !t.is_empty() {
            add(&mut body, &t);
        }
    } else if let Some((sl, _, false)) = shape.take() {
        for l in sl {
            add(&mut body, &l);
        }
        if in_p && !para.is_empty() {
            add(&mut body, &para);
        }
    } else if in_p && !para.is_empty() {
        add(&mut body, &para);
    }
    Ok(Some(SlideText { title, body }))
}

/// Заполнитель фигуры: заголовок слайда или пропускаемый тип.
fn placeholder(
    e: &quick_xml::events::BytesStart,
    shape: &mut Option<(Vec<String>, bool, bool)>,
    skip: &[&str],
) {
    if let Some((_, is_title, skipped)) = shape {
        let ty = attr(e, "type").unwrap_or_default();
        *is_title = matches!(ty.as_str(), "title" | "ctrTitle");
        *skipped = skip.contains(&ty.as_str());
    }
}
