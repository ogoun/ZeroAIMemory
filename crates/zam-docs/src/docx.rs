//! DOCX: тело `word/document.xml` со стилями заголовков, таблицы, сноски и колонтитулы.
//!
//! Единица — блок абзацев под одним путём заголовков (до мягкого предела), `Locator::Para(n)` —
//! номер первого абзаца блока: порядковый номер `w:p` тела с 1, включая абзацы ячеек таблиц
//! (абзацы надписей входят в абзац, где стоит надпись). Сноски, концевые сноски и колонтитулы —
//! отдельные единицы в конце.

use std::collections::HashMap;

use crate::ctx::{Ctx, UNIT_SOFT_CHARS, Units};
use crate::ooxml::{Ev, Package, Table, Xml, attr_ns, local, on};
use crate::{DocError, Locator, Unit};

const HEADINGS: usize = 6;

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
        .unwrap_or_else(|| "word/document.xml".to_string());
    let rels = pkg.rels(&main, ctx)?;
    let target = |kind: &str| -> Vec<String> {
        rels.iter()
            .filter(|r| r.kind == kind)
            .map(|r| r.target.clone())
            .collect()
    };
    let styles = match target("styles").first() {
        Some(s) => styles(&mut pkg, s, ctx)?,
        None => HashMap::new(),
    };
    let Some(doc) = pkg.read(&main, ctx)? else {
        if ctx.stopped() {
            return Ok(Vec::new());
        }
        return Err(DocError::Malformed(format!("нет части {main}")));
    };
    let mut body = Body::new(&styles);
    body.walk(&mut Xml::new(&doc, ctx), ctx)?;
    drop(doc);
    let Body {
        mut units,
        note_refs,
        ..
    } = body;
    units.flush();

    for (kind, title) in [("footnotes", "Сноски"), ("endnotes", "Концевые сноски")]
    {
        for t in target(kind) {
            let notes = containers(&mut pkg, &t, &["footnote", "endnote"], ctx)?;
            let lines = notes
                .into_iter()
                .filter(|(id, _)| {
                    id.as_deref()
                        .is_some_and(|i| i.parse::<i64>().is_ok_and(|n| n > 0))
                })
                .map(|(id, text)| {
                    let id = id.unwrap_or_default();
                    let at = note_refs.get(&(kind, id.clone())).copied().unwrap_or(1);
                    (at, format!("[{id}] {text}"))
                });
            push_grouped(&mut units, lines, title, ctx);
        }
    }
    let mut seen = Vec::new();
    let mut hf = Vec::new();
    for t in target("header").into_iter().chain(target("footer")) {
        for (_, text) in containers(&mut pkg, &t, &[], ctx)? {
            if !seen.contains(&text) {
                seen.push(text.clone());
                hf.push((1, text));
            }
        }
    }
    push_grouped(&mut units, hf.into_iter(), "Колонтитулы", ctx);
    Ok(units.finish())
}

/// Строки (абзац, текст) в единицы с путём `[title]` до мягкого предела.
fn push_grouped(
    units: &mut Units,
    lines: impl Iterator<Item = (u32, String)>,
    title: &str,
    ctx: &mut Ctx,
) {
    let mut cur: Option<(u32, String, usize)> = None;
    for (at, line) in lines {
        let n = line.chars().count();
        if let Some((a, buf, len)) = &mut cur {
            if *len + n < UNIT_SOFT_CHARS {
                buf.push('\n');
                buf.push_str(&line);
                *len += n + 1;
                continue;
            }
            let (a, buf) = (*a, std::mem::take(buf));
            units.push(Locator::Para(a), vec![title.to_string()], &buf, ctx);
        }
        cur = Some((at, line, n));
    }
    if let Some((a, buf, _)) = cur {
        units.push(Locator::Para(a), vec![title.to_string()], &buf, ctx);
    }
}

/// Уровни заголовков стилей абзаца: `styleId` → 0 (`Title`) или 1..=6.
fn styles(pkg: &mut Package, name: &str, ctx: &mut Ctx) -> Result<HashMap<String, u8>, DocError> {
    struct Style {
        name: Option<String>,
        based_on: Option<String>,
        outline: Option<u8>,
    }
    let Some(part) = pkg.read(name, ctx)? else {
        return Ok(HashMap::new());
    };
    let mut all: HashMap<String, Style> = HashMap::new();
    let mut cur: Option<(String, Style)> = None;
    let mut xml = Xml::new(&part, ctx);
    while let Some(ev) = xml.next(ctx)? {
        match ev {
            Ev::Start(e) | Ev::Empty(e) => match local(&e) {
                "style" => {
                    let para = attr_ns(&e, "type").is_none_or(|t| t == "paragraph");
                    cur = attr_ns(&e, "styleId").filter(|_| para).map(|id| {
                        let s = Style {
                            name: None,
                            based_on: None,
                            outline: None,
                        };
                        (id, s)
                    });
                }
                "name" => {
                    if let Some((_, s)) = &mut cur {
                        s.name = attr_ns(&e, "val");
                    }
                }
                "basedOn" => {
                    if let Some((_, s)) = &mut cur {
                        s.based_on = attr_ns(&e, "val");
                    }
                }
                "outlineLvl" => {
                    if let Some((_, s)) = &mut cur {
                        s.outline = attr_ns(&e, "val").and_then(|v| v.parse().ok());
                    }
                }
                _ => {}
            },
            Ev::End(n) if n == "style" => {
                if let Some((id, s)) = cur.take() {
                    all.insert(id, s);
                }
            }
            _ => {}
        }
    }
    let own = |id: &str, s: &Style| -> Option<u8> {
        let by_name = |n: &str| {
            let n: String = n
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .to_lowercase();
            if n == "title" {
                return Some(0);
            }
            let d = n.strip_prefix("heading")?.parse::<u8>().ok()?;
            (1..=HEADINGS as u8).contains(&d).then_some(d)
        };
        s.name
            .as_deref()
            .and_then(by_name)
            .or_else(|| by_name(id))
            .or_else(|| outline_level(s.outline))
    };
    let mut out = HashMap::new();
    for id in all.keys() {
        let mut at = id.as_str();
        // наследование `basedOn`, не глубже 16 шагов (циклы в чужом файле)
        for _ in 0..16 {
            let Some(s) = all.get(at) else { break };
            if let Some(l) = own(at, s) {
                out.insert(id.clone(), l);
                break;
            }
            match s.based_on.as_deref() {
                Some(b) => at = b,
                None => break,
            }
        }
    }
    Ok(out)
}

/// `w:outlineLvl` 0..=5 → уровень заголовка 1..=6 (9 — основной текст).
fn outline_level(v: Option<u8>) -> Option<u8> {
    v.filter(|v| usize::from(*v) < HEADINGS).map(|v| v + 1)
}

/// Разбор тела документа.
struct Body<'s> {
    styles: &'s HashMap<String, u8>,
    units: Units,
    para_no: u32,
    /// Вложенность `w:p` (абзац надписи внутри абзаца).
    in_p: u32,
    text: String,
    style: Option<String>,
    outline: Option<u8>,
    list: bool,
    in_ppr: bool,
    in_rpr: bool,
    in_t: bool,
    hidden: bool,
    tbl_depth: u32,
    table: Table,
    table_at: u32,
    /// Первый абзац со ссылкой на сноску: (`footnotes`|`endnotes`, id) → номер абзаца.
    note_refs: HashMap<(&'static str, String), u32>,
}

impl<'s> Body<'s> {
    fn new(styles: &'s HashMap<String, u8>) -> Self {
        Body {
            styles,
            units: Units::default(),
            para_no: 0,
            in_p: 0,
            text: String::new(),
            style: None,
            outline: None,
            list: false,
            in_ppr: false,
            in_rpr: false,
            in_t: false,
            hidden: false,
            tbl_depth: 0,
            table: Table::default(),
            table_at: 0,
            note_refs: HashMap::new(),
        }
    }

    fn walk(&mut self, xml: &mut Xml, ctx: &mut Ctx) -> Result<(), DocError> {
        while let Some(ev) = xml.next(ctx)? {
            match ev {
                Ev::Start(e) => match local(&e) {
                    // удалённые правки и запасные ветви `mc:AlternateContent` (дубли надписей)
                    "del" | "moveFrom" | "Fallback" => xml.skip(ctx)?,
                    "p" => self.start_p(),
                    "pPr" => self.in_ppr = true,
                    "rPr" => self.in_rpr = true,
                    "r" => self.hidden = false,
                    "t" => self.in_t = true,
                    "tbl" => {
                        self.tbl_depth += 1;
                        if self.tbl_depth == 1 {
                            self.table = Table::default();
                            self.table_at = self.para_no + 1;
                        }
                    }
                    other => self.prop(other, &e),
                },
                Ev::Empty(e) => {
                    let name = local(&e);
                    if name == "p" {
                        self.start_p();
                        self.end_p(ctx);
                    } else {
                        self.prop(name, &e);
                    }
                }
                Ev::End(n) => match n.as_str() {
                    "p" => self.end_p(ctx),
                    "pPr" => self.in_ppr = false,
                    "rPr" => self.in_rpr = false,
                    "t" => self.in_t = false,
                    "tc" if self.tbl_depth == 1 => self.table.end_cell(),
                    "tr" if self.tbl_depth == 1 => self.table.end_row(),
                    "tbl" => {
                        self.tbl_depth = self.tbl_depth.saturating_sub(1);
                        if self.tbl_depth == 0 {
                            let t = std::mem::take(&mut self.table).render();
                            self.units.block(&t, Locator::Para(self.table_at), ctx);
                        }
                    }
                    _ => {}
                },
                Ev::Text(t) => {
                    if self.in_t && self.in_p > 0 && !self.hidden {
                        self.text.push_str(&t);
                    }
                }
            }
            if ctx.stopped() {
                break;
            }
        }
        Ok(())
    }

    /// Свойства и пустые элементы: стиль, уровень, список, скрытый текст, табуляции, переносы.
    fn prop(&mut self, name: &str, e: &quick_xml::events::BytesStart) {
        let top = self.in_p == 1;
        match name {
            "pStyle" if self.in_ppr && top => self.style = attr_ns(e, "val"),
            "outlineLvl" if self.in_ppr && top => {
                self.outline = attr_ns(e, "val").and_then(|v| v.parse().ok());
            }
            "numPr" if self.in_ppr && top => self.list = true,
            "vanish" if self.in_rpr && !self.in_ppr => self.hidden = on(e),
            "tab" if !self.in_ppr && self.in_p > 0 && !self.hidden => self.text.push('\t'),
            "br" | "cr" if self.in_p > 0 && !self.hidden => self.text.push('\n'),
            "noBreakHyphen" if self.in_p > 0 && !self.hidden => self.text.push('-'),
            "footnoteReference" | "endnoteReference" => {
                let kind = if name == "footnoteReference" {
                    "footnotes"
                } else {
                    "endnotes"
                };
                if let Some(id) = attr_ns(e, "id") {
                    let at = self.para_no.max(1);
                    self.note_refs.entry((kind, id)).or_insert(at);
                }
            }
            _ => {}
        }
    }

    fn start_p(&mut self) {
        if self.in_p == 0 {
            self.para_no += 1;
            self.text.clear();
            self.style = None;
            self.outline = None;
            self.list = false;
        } else if !self.text.is_empty() {
            self.text.push(' ');
        }
        self.in_p += 1;
    }

    fn end_p(&mut self, ctx: &mut Ctx) {
        self.in_p = self.in_p.saturating_sub(1);
        if self.in_p > 0 {
            return;
        }
        let text = std::mem::take(&mut self.text);
        if self.tbl_depth > 0 {
            self.table.text(&text);
            return;
        }
        let at = Locator::Para(self.para_no);
        let level = outline_level(self.outline).or_else(|| {
            self.style
                .as_deref()
                .and_then(|s| self.styles.get(s).copied())
        });
        let one_line = || text.split_whitespace().collect::<Vec<_>>().join(" ");
        match level {
            Some(l) if !one_line().is_empty() => self.units.heading(l, &one_line(), at, ctx),
            _ if self.list => self.units.block(&format!("- {}", text.trim()), at, ctx),
            _ => self.units.block(&text, at, ctx),
        }
    }
}

/// Текст частей сносок и колонтитулов: (id контейнера, текст абзацев через `\n`).
/// `boxes` пуст — вся часть одним контейнером без id.
fn containers(
    pkg: &mut Package,
    name: &str,
    boxes: &[&str],
    ctx: &mut Ctx,
) -> Result<Vec<(Option<String>, String)>, DocError> {
    let Some(part) = pkg.read(name, ctx)? else {
        return Ok(Vec::new());
    };
    let mut out: Vec<(Option<String>, String)> = Vec::new();
    let mut cur: Option<(Option<String>, Vec<String>)> =
        boxes.is_empty().then(|| (None, Vec::new()));
    let (mut para, mut in_t, mut in_ppr, mut in_p) = (String::new(), false, false, 0u32);
    let mut xml = Xml::new(&part, ctx);
    let close = |cur: &mut Option<(Option<String>, Vec<String>)>, out: &mut Vec<_>| {
        if let Some((id, paras)) = cur.take() {
            let text = paras.join("\n");
            if !text.trim().is_empty() {
                out.push((id, text));
            }
        }
    };
    while let Some(ev) = xml.next(ctx)? {
        match ev {
            Ev::Start(e) => match local(&e) {
                n if boxes.contains(&n) => {
                    close(&mut cur, &mut out);
                    cur = Some((attr_ns(&e, "id"), Vec::new()));
                }
                "del" | "moveFrom" | "Fallback" => xml.skip(ctx)?,
                "p" => {
                    in_p += 1;
                    if in_p > 1 {
                        para.push(' ');
                    }
                }
                "pPr" => in_ppr = true,
                "t" => in_t = true,
                _ => {}
            },
            Ev::Empty(e) => match local(&e) {
                "tab" if !in_ppr && in_p > 0 => para.push('\t'),
                "br" | "cr" if in_p > 0 => para.push(' '),
                _ => {}
            },
            Ev::End(n) => match n.as_str() {
                "p" => {
                    in_p = in_p.saturating_sub(1);
                    if in_p == 0 {
                        let t = std::mem::take(&mut para);
                        let t = t.trim();
                        if let Some((_, paras)) = &mut cur
                            && !t.is_empty()
                        {
                            paras.push(t.to_string());
                        }
                    }
                }
                "pPr" => in_ppr = false,
                "t" => in_t = false,
                n if boxes.contains(&n) => close(&mut cur, &mut out),
                _ => {}
            },
            Ev::Text(t) => {
                if in_t && in_p > 0 {
                    para.push_str(&t);
                }
            }
        }
    }
    close(&mut cur, &mut out);
    Ok(out)
}
