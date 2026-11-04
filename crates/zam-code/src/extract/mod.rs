//! Экстрактор символов и ссылок на tree-sitter (фича `parse`; спецификация M5.6 §3, §12, §13).
//!
//! Порядок: размер → эвристики [`sniff`] → разбор с отсечкой по времени → один обход дерева
//! курсором (определения по виду узла, контекст `qual`/`export` — стеком, без рекурсии и без
//! `Node::parent()`) → запрос ссылок с тем же дедлайном → символы (`qual`, `sig`, `body_hash`,
//! `exported`, суффиксы `#N`) и агрегированные ссылки → пределы. Всё под `catch_unwind`: паника —
//! `parser_crash`, наружу не выходит.

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
/// UTF-8 разбирается по байтам (имена — с заменой). Отсечка `parse_timeout` считается после
/// компиляции запроса ссылок (один раз на процесс и язык).
pub fn extract(lang: Lang, src: &[u8], lim: &Limits) -> Extracted {
    if lang::refs_query(lang).is_err() {
        return Extracted::skipped(SkipReason::ParserCrash);
    }
    guarded(lang, src, lim, true, Instant::now() + lim.parse_timeout)
}

/// Хэши тел символов одного файла для хуков (§8): только определения, без запроса (компиляции
/// нет); дедлайн — абсолютный, от вызова. Файл пропущен или обрезан пределом (`partial`) —
/// `None`: неполный список хук принял бы за удаление символов.
pub fn symbol_hashes(lang: Lang, src: &[u8], lim: &Limits) -> Option<Vec<SymbolHash>> {
    let out = guarded(lang, src, lim, false, Instant::now() + lim.parse_timeout);
    if out.state != ParseState::Ok {
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
fn guarded(lang: Lang, src: &[u8], lim: &Limits, refs: bool, deadline: Instant) -> Extracted {
    if src.len() as u64 > lim.max_file_bytes {
        return Extracted::skipped(SkipReason::TooLarge);
    }
    // BOM срезается до эвристик и разбора: строки и смещения считаются без него (лексер
    // tree-sitter пропускает BOM и сам — проверено тестом с мутацией, — срез оставлен явным)
    let src = src.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(src);
    if let Some(reason) = sniff(src) {
        return Extracted::skipped(reason);
    }
    catch_unwind(AssertUnwindSafe(|| run(lang, src, lim, refs, deadline)))
        .unwrap_or_else(|_| Extracted::skipped(SkipReason::ParserCrash))
}

/// Определение из обхода: узел определения, узел имени, вид и контекст обхода.
struct RawDef<'t> {
    node: Node<'t>,
    name: Node<'t>,
    kind: SymbolKind,
    ctx: Ctx,
}

/// Ссылка из запроса: узел имени и вид.
struct RawRef<'t> {
    name: Node<'t>,
    kind: RefKind,
}

/// Ближайший предок-контейнер или функция: решает, метод ли функция Rust/Python.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Near {
    None,
    Class,
    Module,
    Func,
}

/// Контекст узла в обходе: цепочка контейнеров (индекс в [`Chain`]), ближайший контейнер или
/// функция, `export` TS/JS, «внутри функции или тела класса».
#[derive(Clone, Copy)]
struct Ctx {
    chain: Option<usize>,
    near: Near,
    export: bool,
    in_body: bool,
}

/// Звено цепочки контейнеров: готовый `qual` контейнера (`None` — длиннее [`QUAL_MAX_CHARS`]).
struct Link {
    qual: Option<String>,
}

/// Цепочки контейнеров: звено хранит полный `qual`, поэтому символ не поднимается к корню, а
/// длинная цепочка обрезается на пределе (глубина вложенности не умножает работу).
#[derive(Default)]
struct Chain {
    links: Vec<Link>,
}

impl Chain {
    fn qual(&self, at: Option<usize>) -> Option<&str> {
        match at {
            None => Some(""),
            Some(i) => self.links[i].qual.as_deref(),
        }
    }

    fn push(&mut self, parent: Option<usize>, segs: &[String]) -> usize {
        let qual = self.qual(parent).and_then(|p| {
            let q = join_qual(p, segs);
            (q.chars().count() <= QUAL_MAX_CHARS).then_some(q)
        });
        self.links.push(Link { qual });
        self.links.len() - 1
    }
}

fn join_qual(prefix: &str, segs: &[String]) -> String {
    let mut q = String::from(prefix);
    for s in segs {
        if !q.is_empty() {
            q.push_str("::");
        }
        q.push_str(s);
    }
    q
}

/// Узлов между проверками дедлайна в обходе.
const DEADLINE_EVERY: usize = 1024;

fn run(lang: Lang, src: &[u8], lim: &Limits, with_refs: bool, deadline: Instant) -> Extracted {
    let mut parser = Parser::new();
    if parser.set_language(&lang::language(lang)).is_err() {
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

    let mut chain = Chain::default();
    let Some(defs) = walk_defs(lang, &tree, src, &mut chain, deadline) else {
        return Extracted::skipped(SkipReason::Timeout);
    };
    let refs = if with_refs {
        let Ok(query) = lang::refs_query(lang) else {
            return Extracted::skipped(SkipReason::ParserCrash);
        };
        match query_refs(query, &tree, src, deadline) {
            Some(r) => r,
            None => return Extracted::skipped(SkipReason::Timeout),
        }
    } else {
        Vec::new()
    };
    let Some(mut symbols) = build_symbols(lang, src, &defs, &chain, deadline) else {
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

/// Один обход дерева в глубину курсором (без рекурсии и без `Node::parent()`, который в
/// tree-sitter 0.27 спускается от корня): определения с контекстом. Стек контекстов идёт
/// параллельно пути курсора. Дедлайн — раз в [`DEADLINE_EVERY`] узлов; вышел — `None`.
fn walk_defs<'t>(
    lang: Lang,
    tree: &'t tree_sitter::Tree,
    src: &[u8],
    chain: &mut Chain,
    deadline: Instant,
) -> Option<Vec<RawDef<'t>>> {
    let root_ctx = Ctx {
        chain: None,
        near: Near::None,
        export: false,
        in_body: false,
    };
    let mut cursor = tree.walk();
    // контекст и вид каждого предка на пути курсора (контекст — для его детей)
    let mut stack: Vec<(Ctx, &'t str)> = Vec::new();
    let mut defs = Vec::new();
    let mut visited = 0usize;
    loop {
        let node = cursor.node();
        visited += 1;
        if visited.is_multiple_of(DEADLINE_EVERY) && Instant::now() >= deadline {
            return None;
        }
        let ctx = stack.last().map_or(root_ctx, |s| s.0);
        let place = lang::Place {
            parent: stack.last().map(|s| s.1),
            grand: stack.len().checked_sub(2).map(|i| stack[i].1),
            in_body: ctx.in_body,
        };
        let kind = node.kind();
        // `export` TS/JS: сам узел под `export_statement` экспортирован
        let node_ctx = Ctx {
            export: ctx.export || place.parent == Some("export_statement"),
            ..ctx
        };
        for (name, k) in lang::defs_of(lang, node, place, src) {
            defs.push(RawDef {
                node,
                name,
                kind: k,
                ctx: node_ctx,
            });
        }
        if cursor.goto_first_child() {
            stack.push((child_ctx(lang, node, kind, node_ctx, src, chain), kind));
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Some(defs);
            }
            stack.pop();
        }
    }
}

/// Контекст детей узла: контейнер добавляет звено цепочки, функция и тело класса — «внутри»,
/// граница экспорта TS/JS снимает `export`.
fn child_ctx(lang: Lang, node: Node, kind: &str, ctx: Ctx, src: &[u8], chain: &mut Chain) -> Ctx {
    let mut out = ctx;
    if let Some((segs, class_like)) = lang::container(lang, node, src) {
        out.chain = Some(chain.push(ctx.chain, &segs));
        out.near = if class_like {
            Near::Class
        } else {
            Near::Module
        };
        out.in_body |= class_like;
    } else if lang::is_function_like(kind) {
        out.near = Near::Func;
        out.in_body = true;
    }
    if matches!(lang, Lang::TypeScript | Lang::Tsx | Lang::JavaScript)
        && lang::is_export_boundary(kind)
    {
        out.export = false;
    }
    out
}

/// Ссылки запросом с дедлайном; вышел — `None`.
fn query_refs<'t>(
    query: &'static tree_sitter::Query,
    tree: &'t tree_sitter::Tree,
    src: &[u8],
    deadline: Instant,
) -> Option<Vec<RawRef<'t>>> {
    let names = query.capture_names();
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
        query,
        tree.root_node(),
        src,
        QueryCursorOptions::new().progress_callback(&mut on_query),
    );
    let mut refs = Vec::new();
    while let Some(m) = matches.next() {
        let (mut name, mut rf) = (None, None);
        for c in m.captures() {
            let cap: &'static str = names[c.index as usize];
            if cap == "name" {
                name = Some(c.node);
            } else if let Some(k) = cap.strip_prefix("reference.") {
                rf = Some((c.node, k));
            }
        }
        if let (Some(name), Some((node, cap))) = (name, rf) {
            refs.push(RawRef {
                name,
                kind: lang::ref_kind(node, cap),
            });
        }
    }
    drop(matches);
    if timed_out.get() || Instant::now() >= deadline {
        return None;
    }
    Some(refs)
}

/// Символы в порядке файла с суффиксами одноимённых; дедлайн вышел — `None`.
fn build_symbols(
    lang: Lang,
    src: &[u8],
    defs: &[RawDef],
    chain: &Chain,
    deadline: Instant,
) -> Option<Vec<Symbol>> {
    let mut items: Vec<(usize, usize, Symbol)> = Vec::new();
    for d in defs {
        if Instant::now() >= deadline {
            return None;
        }
        if let Some(sym) = make_symbol(lang, src, d, chain) {
            items.push((d.node.start_byte(), usize::MAX - d.node.end_byte(), sym));
        }
    }
    // устойчивая сортировка: определения одного узла (`const a, b` Go) — в порядке обхода
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

fn make_symbol(lang: Lang, src: &[u8], d: &RawDef, chain: &Chain) -> Option<Symbol> {
    let mut kind = d.kind;
    let parts = lang::name_parts(d.name, src);
    let name = parts.last()?.clone();
    if !valid_name(&name) || parts.iter().any(|p| !valid_name(p)) {
        return None;
    }
    // функция в `impl`/`trait` Rust или в классе Python — метод (ближайший — класс, не функция)
    if kind == SymbolKind::Function
        && matches!(d.node.kind(), "function_item" | "function_definition")
        && d.ctx.near == Near::Class
    {
        kind = SymbolKind::Method;
    }
    let mut prefix = chain.qual(d.ctx.chain)?.to_string();
    if lang == Lang::Go && d.node.kind() == "method_declaration" {
        let recv = lang::go_receiver(d.node, src)?;
        prefix = join_qual(&prefix, &[recv]);
    }
    let qual = join_qual(&prefix, &parts);
    if qual.chars().count() > QUAL_MAX_CHARS {
        return None;
    }

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
        exported: lang::exported(lang, d.node, &name, src, d.ctx.export),
        name,
        qual,
        kind,
        line,
        end_line: end_line.max(line),
        sig,
        body_hash: body_hash(&def_text),
    })
}

/// Имя символа или ссылки: непустое, ≤ [`NAME_MAX_CHARS`], без управляющих символов, `/` и `#`
/// (`#` и `/` ломали бы ссылку `путь#qual`).
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= NAME_MAX_CHARS
        && !name.chars().any(|c| c.is_control() || c == '/' || c == '#')
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
