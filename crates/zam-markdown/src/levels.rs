//! Уровни L1 (ТЗ §8.3): оглавление и первое предложение каждой секции.
//!
//! Формат — строки `<отступ><заголовок> — <первое предложение>`; отступ —
//! два пробела на уровень глубже самой верхней секции. Преамбула даёт строку
//! из одного предложения, секция без абзацев — только заголовок. Первое
//! предложение берётся из первого абзаца верхнего уровня в собственном тексте
//! секции (до первой подсекции), без разметки.

use std::ops::Range;

use comrak::nodes::{AstNode, NodeValue};
use comrak::{Arena, parse_document};

use crate::chunk::TokenCounter;
use crate::doc::{Doc, Section, doc_from_root};
use crate::options::comrak_options;
use crate::pos::LineIndex;
use crate::sentences::split_sentences;

/// Бюджет L1 заметки в символах по умолчанию (§8.3).
pub const NOTE_L1_MAX_CHARS: usize = 1200;
/// Бюджет L1 заметки и секции в токенах (§8.2 п. 7, §8.3).
pub const L1_MAX_TOKENS: usize = 300;
/// Предел длины первого предложения в строке оглавления, символов.
const SENTENCE_MAX_CHARS: usize = 200;

/// Пункт L1: секция и её первое предложение.
#[derive(Debug, Clone)]
pub(crate) struct L1Item {
    pub level: u8,
    pub title: String,
    pub sentence: Option<String>,
    /// Начало секции (`Section::range.start`) в исходнике.
    pub start: usize,
}

/// L1 заметки: оглавление и первое предложение каждой секции, не длиннее
/// `max_chars` символов. Целые строки отбрасываются с конца; единственная
/// слишком длинная строка обрезается с «…».
pub fn note_l1(src: &str, doc: &Doc, max_chars: usize) -> String {
    let arena = Arena::new();
    let root = parse_document(&arena, src, &comrak_options());
    let items = l1_items(src, doc, root);
    fit_lines(&render(&items), |t| t.chars().count() <= max_chars)
}

/// L1 секции `s` (с подсекциями) не длиннее `max_tokens` по `tc`.
pub fn section_l1(src: &str, s: &Section, tc: &dyn TokenCounter, max_tokens: usize) -> String {
    let Some(sub) = src.get(s.range.clone()) else {
        return String::new();
    };
    // подстрока начинается с заголовка (или с преамбулы): frontmatter в ней нет
    let arena = Arena::new();
    let mut options = comrak_options();
    options.extension.front_matter_delimiter = None;
    let root = parse_document(&arena, sub, &options);
    let doc = doc_from_root(sub, root);
    let items = l1_items(sub, &doc, root);
    fit_lines(&render(&items), |t| tc.count(t) <= max_tokens)
}

/// Пункты L1 для всех секций `doc` по AST того же исходника.
pub(crate) fn l1_items<'a>(src: &str, doc: &Doc, root: &'a AstNode<'a>) -> Vec<L1Item> {
    let li = LineIndex::new(src);
    let own = own_ranges(doc);
    let mut sentences: Vec<Option<String>> = vec![None; doc.sections.len()];
    for node in root.children() {
        if !matches!(node.data().value, NodeValue::Paragraph) {
            continue;
        }
        let pos = li.line_start(node.data().sourcepos.start.line);
        let Some(k) = own.iter().position(|r| r.contains(&pos)) else {
            continue;
        };
        if sentences[k].is_some() {
            continue;
        }
        // границы — по исходнику абзаца (код в `` ` `` виден сегментатору),
        // затем первое предложение переводится в текст без разметки
        let end = li.line_end(node.data().sourcepos.end.line).max(pos);
        let Some(raw) = src.get(pos..end) else {
            continue;
        };
        sentences[k] = split_sentences(raw)
            .first()
            .map(|r| plain_text(&raw[r.clone()]))
            .filter(|s| !s.is_empty())
            .map(|s| cap_chars(&s, SENTENCE_MAX_CHARS));
    }
    doc.sections
        .iter()
        .zip(sentences)
        .map(|(s, sentence)| L1Item {
            level: s.level,
            title: s.title.split_whitespace().collect::<Vec<_>>().join(" "),
            sentence,
            start: s.range.start,
        })
        .collect()
}

/// Собственный текст каждой секции: от конца заголовка до начала следующей секции.
pub(crate) fn own_ranges(doc: &Doc) -> Vec<Range<usize>> {
    doc.sections
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let end = doc
                .sections
                .get(i + 1)
                .map_or(s.body.end, |n| n.range.start)
                .clamp(s.body.start, s.body.end);
            s.body.start..end
        })
        .collect()
}

/// Строки L1 по пунктам.
pub(crate) fn render(items: &[L1Item]) -> Vec<String> {
    let base = items
        .iter()
        .filter(|i| i.level > 0)
        .map(|i| i.level)
        .min()
        .unwrap_or(1);
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        let line = match (it.level, it.title.is_empty(), &it.sentence) {
            (0, _, Some(s)) | (_, true, Some(s)) => s.clone(),
            (0, _, None) | (_, true, None) => continue,
            (level, false, s) => {
                let indent = "  ".repeat(usize::from(level.saturating_sub(base)));
                match s {
                    Some(s) => format!("{indent}{} — {s}", it.title),
                    None => format!("{indent}{}", it.title),
                }
            }
        };
        out.push(line);
    }
    out
}

/// Строки, склеенные через `\n`, пока выполняется `fits`. Если не входит даже
/// первая строка — её наибольший префикс с «…».
pub(crate) fn fit_lines(lines: &[String], fits: impl Fn(&str) -> bool) -> String {
    let mut out = String::new();
    for line in lines {
        let candidate = if out.is_empty() {
            line.clone()
        } else {
            format!("{out}\n{line}")
        };
        if !fits(&candidate) {
            break;
        }
        out = candidate;
    }
    if out.is_empty()
        && let Some(first) = lines.first()
    {
        // двоичный поиск по числу символов префикса
        let bounds: Vec<usize> = first.char_indices().map(|(i, _)| i).collect();
        let cand = |k: usize| {
            let prefix = bounds.get(k).map_or(first.as_str(), |&b| &first[..b]);
            format!("{}…", prefix.trim_end())
        };
        // инвариант: префикс из `lo` символов входит (0 — пустой результат)
        let (mut lo, mut hi) = (0usize, bounds.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if fits(&cand(mid)) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        if lo > 0 {
            out = cand(lo);
        }
    }
    out
}

/// Фрагмент Markdown без разметки, пробелы схлопнуты.
fn plain_text(md: &str) -> String {
    let arena = Arena::new();
    let mut options = comrak_options();
    options.extension.front_matter_delimiter = None;
    let root = parse_document(&arena, md, &options);
    root.collect_text()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Не длиннее `max` символов; обрезанное получает «…».
fn cap_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        None => s.to_string(),
        Some(_) => {
            let cut = s
                .char_indices()
                .nth(max.saturating_sub(1))
                .map_or(s.len(), |(i, _)| i);
            format!("{}…", s[..cut].trim_end())
        }
    }
}
