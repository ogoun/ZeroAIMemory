//! Разобранный документ: заголовки, секции, ссылки, frontmatter.

use std::ops::Range;

use comrak::nodes::{AstNode, NodeValue, Sourcepos};
use comrak::{Anchorizer, Arena, parse_document};
use serde::Serialize;

use crate::options::{FRONT_MATTER_DELIMITER, comrak_options};
use crate::pos::LineIndex;

const BOM: &str = "\u{FEFF}";

/// Результат разбора заметки. Все диапазоны — байтовые оффсеты в исходнике.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Doc {
    /// Сырой YAML и разобранные поля.
    pub frontmatter: Option<Frontmatter>,
    /// Все заголовки верхнего уровня документа, по порядку.
    pub headings: Vec<Heading>,
    /// Секции H1–H3 верхнего уровня (и преамбула `level = 0`, если она непустая).
    pub sections: Vec<Section>,
    /// Ссылки всех видов, по порядку.
    pub links: Vec<Link>,
    /// Байт после frontmatter (и BOM).
    pub body_start: usize,
}

/// Frontmatter заметки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Frontmatter {
    /// YAML между разделителями `---`, как в исходнике.
    pub raw: String,
    pub tags: Vec<String>,
    pub description: Option<String>,
    pub l0: Option<String>,
    pub index: Option<bool>,
    /// Эпистемический статус (§7.5): `axiom` | `theorem` | `hypothesis` как в исходнике
    /// (проверку значения делает хранилище); без поля — статуса нет.
    pub certainty: Option<String>,
    /// Доказательство для `theorem` — свободный текст (скаляр или блок `|`).
    pub evidence: Option<String>,
}

/// Заголовок верхнего уровня.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Heading {
    pub level: u8,
    /// Текст без разметки.
    pub text: String,
    /// Уникальный в документе якорь без префикса (как `id` у comrak).
    pub anchor: String,
    /// Строка(и) заголовка: от первого символа (`#` или текста setext) до конца
    /// последней строки, включая перевод строки.
    pub range: Range<usize>,
}

/// Секция: заголовок H1–H3 и всё до следующего заголовка того же или высшего уровня.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Section {
    /// 1–3; 0 — преамбула до первого заголовка.
    pub level: u8,
    pub title: String,
    /// Заголовки от корня до этой секции включительно.
    pub path: Vec<String>,
    pub anchor: String,
    /// От начала строки заголовка (с отступом) до начала строки следующего
    /// заголовка того же или высшего уровня.
    pub range: Range<usize>,
    /// Содержимое после строк заголовка.
    pub body: Range<usize>,
}

/// Вид ссылки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum LinkKind {
    /// `[[Заметка#Секция|текст]]`
    Wiki,
    /// `[текст](путь)` и автоссылки.
    Markdown,
    /// `![alt](путь)`
    Image,
}

/// Ссылка из документа.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Link {
    pub kind: LinkKind,
    /// Путь или имя без якоря.
    pub target: String,
    /// Часть после `#`.
    pub anchor: Option<String>,
    /// Текст ссылки (для вики-ссылки — только явный, после `|`; для картинки — alt).
    pub label: Option<String>,
    pub range: Range<usize>,
}

/// Пункт оглавления.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlineItem {
    pub level: u8,
    pub title: String,
    pub anchor: String,
    pub path: Vec<String>,
}

/// Разбирает исходник заметки. Не паникует на любом входе.
pub fn parse(src: &str) -> Doc {
    let arena = Arena::new();
    let options = comrak_options();
    let root = parse_document(&arena, src, &options);
    doc_from_root(src, root)
}

/// [`Doc`] по уже разобранному AST (`root` — результат `parse_document`
/// с [`comrak_options`]). Общий путь для [`parse`] и рендера: один разбор.
pub(crate) fn doc_from_root<'a>(src: &str, root: &'a AstNode<'a>) -> Doc {
    let li = LineIndex::new(src);
    let bom = if src.starts_with(BOM) { BOM.len() } else { 0 };

    let mut frontmatter = None;
    let mut body_start = bom;
    let mut headings = Vec::new();
    let mut links = Vec::new();
    // якоря считаются по всем заголовкам в порядке документа — как в HTML comrak
    let mut anchorizer = Anchorizer::new();

    for node in root.descendants() {
        let data = node.data();
        match &data.value {
            NodeValue::FrontMatter(block) => {
                let end = bom + block.len();
                if end <= src.len() && src.is_char_boundary(end) {
                    body_start = end;
                }
                frontmatter = Some(crate::frontmatter::parse(block, FRONT_MATTER_DELIMITER));
            }
            NodeValue::Heading(h) => {
                let text = node.collect_text();
                let anchor = anchorizer.anchorize(&text);
                let top = node.parent().is_some_and(|p| p.same_node(root));
                if top {
                    let sp = data.sourcepos;
                    let start =
                        floor_char(src, li.offset(sp.start.line, sp.start.column)).max(body_start);
                    let end = li.line_end(sp.end.line).max(start);
                    headings.push(Heading {
                        level: h.level,
                        text,
                        anchor,
                        range: start..end,
                    });
                }
            }
            NodeValue::Link(l) => {
                let range = inline_range(src, &li, data.sourcepos);
                links.push(make_link(LinkKind::Markdown, &l.url, label_of(node), range));
            }
            NodeValue::Image(l) => {
                let range = inline_range(src, &li, data.sourcepos);
                links.push(make_link(LinkKind::Image, &l.url, label_of(node), range));
            }
            NodeValue::WikiLink(w) => {
                let range = inline_range(src, &li, data.sourcepos);
                // текст вики-ссылки явный, только если в исходнике был `|`
                let label = src[range.clone()]
                    .contains('|')
                    .then(|| label_of(node))
                    .flatten();
                links.push(make_link(LinkKind::Wiki, &w.url, label, range));
            }
            _ => {}
        }
    }

    let sections = build_sections(src, body_start, &headings);
    Doc {
        frontmatter,
        headings,
        sections,
        links,
        body_start,
    }
}

/// Байтовый диапазон inline-узла: конец comrak указывает на последний байт,
/// диапазон выравнивается по границам символов.
fn inline_range(src: &str, li: &LineIndex, sp: Sourcepos) -> Range<usize> {
    let start = floor_char(src, li.offset(sp.start.line, sp.start.column));
    let mut end = (li.offset(sp.end.line, sp.end.column) + 1).min(src.len());
    while !src.is_char_boundary(end) {
        end += 1;
    }
    start..end.max(start)
}

/// Ближайшая слева граница символа.
fn floor_char(src: &str, mut i: usize) -> usize {
    while !src.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn label_of<'a>(node: &'a AstNode<'a>) -> Option<String> {
    let t = node.collect_text();
    (!t.is_empty()).then_some(t)
}

/// Цель и якорь ссылки: часть после первого `#`; пустой якорь — `None`.
pub(crate) fn split_anchor(url: &str) -> (&str, Option<&str>) {
    match url.split_once('#') {
        Some((t, a)) => (t, (!a.is_empty()).then_some(a)),
        None => (url, None),
    }
}

fn make_link(kind: LinkKind, url: &str, label: Option<String>, range: Range<usize>) -> Link {
    let (target, anchor) = split_anchor(url);
    Link {
        kind,
        target: target.to_string(),
        anchor: anchor.map(str::to_string),
        label,
        range,
    }
}

fn build_sections(src: &str, body_start: usize, headings: &[Heading]) -> Vec<Section> {
    let heads: Vec<&Heading> = headings.iter().filter(|h| h.level <= 3).collect();
    let mut sections = Vec::with_capacity(heads.len() + 1);

    // секции состоят из целых строк: отступ строки заголовка (до трёх пробелов)
    // принадлежит его секции, а не предыдущей
    let first = heads
        .first()
        .map_or(src.len(), |h| line_start(src, body_start, h.range.start));
    if first > body_start && !src[body_start..first].trim().is_empty() {
        sections.push(Section {
            level: 0,
            title: String::new(),
            path: Vec::new(),
            anchor: String::new(),
            range: body_start..first,
            body: body_start..first,
        });
    }

    let mut stack: Vec<(u8, &str)> = Vec::new();
    for (i, h) in heads.iter().enumerate() {
        let end = heads[i + 1..]
            .iter()
            .find(|n| n.level <= h.level)
            .map_or(src.len(), |n| line_start(src, body_start, n.range.start));
        let start = line_start(src, body_start, h.range.start);
        while stack.last().is_some_and(|&(l, _)| l >= h.level) {
            stack.pop();
        }
        stack.push((h.level, &h.text));
        sections.push(Section {
            level: h.level,
            title: h.text.clone(),
            path: stack.iter().map(|&(_, t)| t.to_string()).collect(),
            anchor: h.anchor.clone(),
            range: start..end,
            body: h.range.end.min(end)..end,
        });
    }
    sections
}

/// Начало строки, содержащей `pos`, но не раньше `floor`.
fn line_start(src: &str, floor: usize, pos: usize) -> usize {
    src[floor..pos]
        .rfind(['\n', '\r'])
        .map_or(floor, |i| floor + i + 1)
}

/// Оглавление по секциям H1–H3.
pub fn outline(doc: &Doc) -> Vec<OutlineItem> {
    doc.sections
        .iter()
        .filter(|s| s.level > 0)
        .map(|s| OutlineItem {
            level: s.level,
            title: s.title.clone(),
            anchor: s.anchor.clone(),
            path: s.path.clone(),
        })
        .collect()
}

/// Текст первого абзаца тела верхнего уровня без разметки; пробелы схлопнуты.
pub fn first_paragraph(src: &str, doc: &Doc) -> Option<String> {
    let body = src.get(doc.body_start..).unwrap_or(src);
    let arena = Arena::new();
    let mut options = comrak_options();
    // frontmatter уже отрезан по body_start
    options.extension.front_matter_delimiter = None;
    let root = parse_document(&arena, body, &options);
    root.children()
        .find(|n| matches!(n.data().value, NodeValue::Paragraph))
        .map(|n| {
            n.collect_text()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|t| !t.is_empty())
}

/// Секция по пути вида `"Установка > Linux"`; пробелы вокруг `>` не важны.
/// При нескольких секциях с одним путём — первая.
pub fn section_by_path<'a>(doc: &'a Doc, path: &str) -> Option<&'a Section> {
    sections_by_path(doc, path).next()
}

/// Все секции с путём `path` (без преамбулы), по порядку документа.
pub(crate) fn sections_by_path<'a>(doc: &'a Doc, path: &str) -> impl Iterator<Item = &'a Section> {
    let parts: Vec<&str> = path.split('>').map(str::trim).collect();
    let valid = !parts.iter().any(|p| p.is_empty());
    doc.sections.iter().filter(move |s| {
        valid && s.level > 0 && s.path.iter().map(String::as_str).eq(parts.iter().copied())
    })
}

/// Якорь заголовка по алгоритму comrak (`header_id`), без префикса и суффикса уникальности.
/// Буквы любых алфавитов сохраняются.
pub fn slug(text: &str) -> String {
    Anchorizer::new().anchorize(text)
}
