//! Экстрактор символов и ссылок на tree-sitter (фича `parse`; спецификация M5.6 §3, §12, §13).
//!
//! Порядок: размер → эвристики [`sniff`] → разбор с отсечкой по времени → запрос тегов с тем же
//! дедлайном → символы (`qual` по контейнерам, `sig`, `body_hash`, `exported`, суффиксы `#N`) и
//! агрегированные ссылки → пределы. Всё под `catch_unwind`: паника — `parser_crash`, наружу не
//! выходит. Обход дерева — только `Node::parent()` и явный стек, без рекурсии.

mod lang;
mod sniff;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, ParseOptions, Parser, QueryCursor, QueryCursorOptions};

pub use sniff::sniff;

use crate::proto::{
    Lang, NAME_MAX_CHARS, ParseState, QUAL_MAX_CHARS, RefAgg, RefKind, SkipReason, Symbol,
    SymbolHash, SymbolKind, body_hash, sig_line,
};

/// Пределы разбора одного файла (клиент `[code]`, спецификация §12).
#[derive(Debug, Clone)]
pub struct Limits {
    /// Больше — `too_large` без чтения дерева.
    pub max_file_bytes: u64,
    /// Отсечка разбора и запроса тегов — `timeout`.
    pub parse_timeout: Duration,
    /// Больше символов — первые по порядку, `partial` + `limit`.
    pub max_symbols: usize,
    /// Больше агрегированных ссылок — первые по порядку, `partial` + `limit`.
    pub max_refs: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 1_048_576,
            parse_timeout: Duration::from_millis(200),
            max_symbols: 5000,
            max_refs: 20_000,
        }
    }
}

/// Итог разбора файла.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub state: ParseState,
    pub skip: Option<SkipReason>,
    pub symbols: Vec<Symbol>,
    pub refs: Vec<RefAgg>,
}

impl Extracted {
    fn skipped(reason: SkipReason) -> Self {
        Self {
            state: ParseState::Skipped,
            skip: Some(reason),
            symbols: vec![],
            refs: vec![],
        }
    }
}

/// Символы и ссылки файла. Никогда не паникует наружу; BOM срезается до разбора, невалидный
/// UTF-8 разбирается по байтам (имена — с заменой).
pub fn extract(lang: Lang, src: &[u8], lim: &Limits) -> Extracted {
    guarded(lang, src, lim, lang::Mode::Full)
}

/// Хэши тел символов одного файла для хуков (§8): запрос только определений (компилируется
/// в разы быстрее полного, замер журнала M5.6); файл пропущен — `None`.
pub fn symbol_hashes(lang: Lang, src: &[u8], lim: &Limits) -> Option<Vec<SymbolHash>> {
    let out = guarded(lang, src, lim, lang::Mode::Defs);
    if out.state == ParseState::Skipped {
        return None;
    }
    Some(
        out.symbols
            .into_iter()
            .map(|s| SymbolHash {
                qual: s.qual,
                body_hash: s.body_hash,
            })
            .collect(),
    )
}

/// Пределы, эвристики и `catch_unwind` вокруг разбора.
fn guarded(lang: Lang, src: &[u8], lim: &Limits, mode: lang::Mode) -> Extracted {
    if src.len() as u64 > lim.max_file_bytes {
        return Extracted::skipped(SkipReason::TooLarge);
    }
    // BOM срезается до эвристик и разбора: строки и смещения считаются без него (лексер
    // tree-sitter пропускает BOM и сам — проверено тестом с мутацией, — срез оставлен явным)
    let src = src.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(src);
    if let Some(reason) = sniff(src) {
        return Extracted::skipped(reason);
    }
    catch_unwind(AssertUnwindSafe(|| run(lang, src, lim, mode)))
        .unwrap_or_else(|_| Extracted::skipped(SkipReason::ParserCrash))
}

/// Определение из запроса: узел определения, узел имени, суффикс захвата.
struct RawDef<'t> {
    node: Node<'t>,
    name: Node<'t>,
    cap: &'static str,
}

/// Ссылка из запроса: узел имени и вид.
struct RawRef<'t> {
    name: Node<'t>,
    kind: RefKind,
}

fn run(lang: Lang, src: &[u8], lim: &Limits, mode: lang::Mode) -> Extracted {
    let Ok(spec) = lang::spec(lang, mode) else {
        return Extracted::skipped(SkipReason::ParserCrash);
    };
    let deadline = Instant::now() + lim.parse_timeout;
    let mut parser = Parser::new();
    if parser.set_language(&spec.language).is_err() {
        return Extracted::skipped(SkipReason::ParserCrash);
    }
    let mut on_parse = |_: &tree_sitter::ParseState| {
        if Instant::now() >= deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let len = src.len();
    let tree = parser.parse_with_options(
        &mut |i, _| &src[i.min(len)..],
        None,
        Some(ParseOptions::new().progress_callback(&mut on_parse)),
    );
    let Some(tree) = tree else {
        return Extracted::skipped(SkipReason::Timeout);
    };

    let names = spec.query.capture_names();
    let timed_out = Cell::new(false);
    let mut on_query = |_: &tree_sitter::QueryCursorState| {
        if Instant::now() >= deadline {
            timed_out.set(true);
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches_with_options(
        &spec.query,
        tree.root_node(),
        src,
        QueryCursorOptions::new().progress_callback(&mut on_query),
    );
    let mut defs: Vec<RawDef> = Vec::new();
    let mut refs: Vec<RawRef> = Vec::new();
    while let Some(m) = matches.next() {
        let (mut name, mut def, mut rf) = (None, None, None);
        for c in m.captures() {
            let cap: &'static str = names[c.index as usize];
            if cap == "name" {
                name = Some(c.node);
            } else if let Some(k) = cap.strip_prefix("definition.") {
                def = Some((c.node, k));
            } else if let Some(k) = cap.strip_prefix("reference.") {
                rf = Some((c.node, k));
            }
        }
        let Some(name) = name else { continue };
        if let Some((node, cap)) = def {
            defs.push(RawDef { node, name, cap });
        } else if let Some((node, cap)) = rf {
            refs.push(RawRef {
                name,
                kind: lang::ref_kind(node, cap),
            });
        }
    }
    drop(matches);
    if timed_out.get() || Instant::now() >= deadline {
        return Extracted::skipped(SkipReason::Timeout);
    }

    let Some(mut symbols) = build_symbols(lang, src, defs.as_slice(), deadline) else {
        return Extracted::skipped(SkipReason::Timeout);
    };
    let def_names: HashSet<usize> = defs.iter().map(|d| d.name.id()).collect();
    let mut refs = aggregate_refs(src, refs, &def_names);

    let mut state = ParseState::Ok;
    if symbols.len() > lim.max_symbols {
        symbols.truncate(lim.max_symbols);
        state = ParseState::Partial;
    }
    if refs.len() > lim.max_refs {
        refs.truncate(lim.max_refs);
        state = ParseState::Partial;
    }
    let skip = (state == ParseState::Partial).then_some(SkipReason::Limit);
    Extracted {
        state,
        skip,
        symbols,
        refs,
    }
}

/// Символы в порядке файла с суффиксами одноимённых; дедлайн вышел — `None`.
fn build_symbols(
    lang: Lang,
    src: &[u8],
    defs: &[RawDef],
    deadline: Instant,
) -> Option<Vec<Symbol>> {
    let mut seen = HashSet::new();
    let mut items: Vec<(usize, usize, Symbol)> = Vec::new();
    for d in defs {
        if !seen.insert(d.node.id()) {
            continue;
        }
        if Instant::now() >= deadline {
            return None;
        }
        if let Some(sym) = make_symbol(lang, src, d) {
            items.push((d.node.start_byte(), usize::MAX - d.node.end_byte(), sym));
        }
    }
    items.sort_by_key(|(start, rev_end, _)| (*start, *rev_end));
    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut out = Vec::with_capacity(items.len());
    for (_, _, mut sym) in items {
        let n = counts.entry(sym.qual.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            sym.qual = format!("{}#{n}", sym.qual);
        }
        if sym.qual.chars().count() <= QUAL_MAX_CHARS {
            out.push(sym);
        }
    }
    Some(out)
}

fn make_symbol(lang: Lang, src: &[u8], d: &RawDef) -> Option<Symbol> {
    let mut kind = lang::def_kind(lang, d.node, d.cap)?;
    let parts = lang::name_parts(d.name, src);
    let name = parts.last()?.clone();
    if !valid_name(&name) {
        return None;
    }
    // цепочка контейнеров снизу вверх; первый встреченный контейнер или функция решает,
    // метод ли функция (Rust `impl`/`trait`, класс Python)
    let mut chain: Vec<Vec<String>> = Vec::new();
    let mut nearest_class: Option<bool> = None;
    if lang == Lang::Go && d.node.kind() == "method_declaration" {
        chain.push(vec![lang::go_receiver(d.node, src)?]);
    }
    let mut cur = d.node.parent();
    while let Some(n) = cur {
        if let Some((segs, class_like)) = lang::container(lang, n, src) {
            nearest_class.get_or_insert(class_like);
            chain.push(segs);
        } else if lang::is_function_like(n.kind()) {
            nearest_class.get_or_insert(false);
        }
        cur = n.parent();
    }
    if kind == SymbolKind::Function
        && matches!(d.node.kind(), "function_item" | "function_definition")
        && nearest_class == Some(true)
    {
        kind = SymbolKind::Method;
    }
    let mut qual_parts: Vec<String> = chain.into_iter().rev().flatten().collect();
    qual_parts.extend(parts);
    let qual = qual_parts.join("::");

    let start = d.node.start_byte();
    let end = d.node.end_byte().min(src.len());
    let def_bytes = src.get(start..end)?;
    let def_text = String::from_utf8_lossy(def_bytes);
    // сигнатура — со строки имени (атрибуты, декораторы и аннотации выше неё не входят)
    let name_start = d.name.start_byte().min(end);
    let line_start = src[..name_start]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
    let sig_from = line_start.max(start);
    let sig_bytes = src.get(sig_from..end).unwrap_or_default();
    let sig_text = String::from_utf8_lossy(sig_bytes);
    let sig = sig_line(sig_text.trim_start());

    let line = d.name.start_position().row as u32 + 1;
    let endp = d.node.end_position();
    let mut end_line = endp.row as u32 + 1;
    if endp.column == 0 && endp.row > d.node.start_position().row {
        end_line -= 1;
    }
    Some(Symbol {
        exported: lang::exported(lang, d.node, &name, src),
        name,
        qual,
        kind,
        line,
        end_line: end_line.max(line),
        sig,
        body_hash: body_hash(&def_text),
    })
}

/// Имя символа или ссылки: непустое, ≤ [`NAME_MAX_CHARS`], без управляющих символов.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= NAME_MAX_CHARS
        && !name.chars().any(char::is_control)
}

/// Приоритет вида при нескольких захватах одного узла имени.
fn ref_priority(k: RefKind) -> u8 {
    match k {
        RefKind::Impl => 4,
        RefKind::Call => 3,
        RefKind::Import => 2,
        RefKind::Type => 1,
        RefKind::Other => 0,
    }
}

/// `(name, kind)` → число и первая строка; порядок — по первому вхождению. Узел имени
/// определения и `Self` — не ссылки; один узел — один вид (по приоритету).
fn aggregate_refs(src: &[u8], refs: Vec<RawRef>, def_names: &HashSet<usize>) -> Vec<RefAgg> {
    let mut by_node: HashMap<usize, (usize, RawRef)> = HashMap::new();
    for r in refs {
        if def_names.contains(&r.name.id()) {
            continue;
        }
        match by_node.get(&r.name.id()) {
            Some((_, old)) if ref_priority(old.kind) >= ref_priority(r.kind) => {}
            _ => {
                by_node.insert(r.name.id(), (r.name.start_byte(), r));
            }
        }
    }
    let mut ordered: Vec<(usize, RawRef)> = by_node.into_values().collect();
    ordered.sort_by_key(|(start, r)| (*start, r.name.end_byte()));
    let mut index: HashMap<(String, RefKind), usize> = HashMap::new();
    let mut out: Vec<RefAgg> = Vec::new();
    for (_, r) in ordered {
        let raw = lang::text(r.name, src);
        let name = ref_name(&raw, r.name.kind());
        if !valid_name(&name) || name == "Self" {
            continue;
        }
        let line = r.name.start_position().row as u32 + 1;
        match index.get(&(name.clone(), r.kind)) {
            Some(&i) => out[i].count = out[i].count.saturating_add(1),
            None => {
                index.insert((name.clone(), r.kind), out.len());
                out.push(RefAgg {
                    name,
                    kind: r.kind,
                    count: 1,
                    first_line: line,
                });
            }
        }
    }
    out
}

/// Имя ссылки: последняя часть пути импорта (`net/http` → `http`), без кавычек и пробелов.
fn ref_name(raw: &str, node_kind: &str) -> String {
    let t = raw
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`');
    let t = if node_kind.contains("string") {
        t.rsplit('/').next().unwrap_or(t)
    } else {
        t
    };
    t.split_whitespace().collect()
}
