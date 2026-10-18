//! HTML-рендер заметки (ТЗ §5.3, §15.2): comrak → собственные проходы по AST
//! (callout, вики-ссылки, вложения, формулы, mermaid, подсветка) → `ammonia`.
//!
//! Один разбор даёт и HTML, и оглавление, и ссылки. Разметка, которую пишут
//! проходы, проверяется тем же allowlist, что и вывод comrak: санитайзер не
//! доверяет ни рендеру, ни [`Resolver`].

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::{self, Write as _};

use comrak::html::{
    ChildRendering, Context, dangerous_url, format_document_with_formatter, format_node_default,
    render_sourcepos,
};
use comrak::nodes::{AstNode, NodeAlert, NodeCodeBlock, NodeLink, NodeValue};
use comrak::options::Plugins;
use comrak::{Arena, parse_document};
use serde::Serialize;

use crate::doc::{Link, OutlineItem, doc_from_root, outline, split_anchor};
use crate::highlight::{HIGHLIGHT_BUDGET_BYTES, Highlighted, highlight};
use crate::options::{HEADER_ID_PREFIX, comrak_options};
use crate::sanitize::{EXTERNAL_REL, sanitize_html};

/// Версия вывода рендера: часть ключа кэша `(content_hash, render_version)`.
/// Растёт при любом изменении HTML.
pub const RENDER_VERSION: u32 = 3;

/// Разрешённая вики-ссылка.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkResolution {
    /// Относительный href без схемы.
    pub href: String,
    /// `false` — ссылка ведёт туда, где заметки пока нет (рисуется как битая, но с href).
    pub exists: bool,
}

/// Разрешение ссылок при рендере; реализует сервер.
pub trait Resolver {
    /// Вики-ссылка `[[target#anchor]]` → href (относительный, без схемы) или None (битая).
    fn wiki(&self, target: &str, anchor: Option<&str>) -> Option<LinkResolution>;
    /// Ссылка на вложение `_attachments/<sha16>/<имя>` → URL блоба.
    fn attachment(&self, sha16: &str, filename: &str) -> Option<String>;
}

/// Резолвер без данных: все вики-ссылки и вложения не разрешены.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoResolve;

impl Resolver for NoResolve {
    fn wiki(&self, _target: &str, _anchor: Option<&str>) -> Option<LinkResolution> {
        None
    }
    fn attachment(&self, _sha16: &str, _filename: &str) -> Option<String> {
        None
    }
}

/// Результат рендера.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rendered {
    /// Санитизированный HTML.
    pub html: String,
    /// Оглавление H1–H3 (`outline(&parse(src))`).
    pub toc: Vec<OutlineItem>,
    /// Ссылки документа (`parse(src).links`).
    pub links: Vec<Link>,
    /// Битые вики-ссылки и неразрешённые вложения, по порядку документа.
    pub warnings: Vec<String>,
}

/// Рендерит заметку в санитизированный HTML. Не паникует на любом входе.
pub fn render_html(src: &str, resolver: &dyn Resolver) -> Rendered {
    let arena = Arena::new();
    let options = comrak_options();
    let root = parse_document(&arena, src, &options);
    // Doc строится до проходов: они меняют текстовые узлы callout
    let doc = doc_from_root(src, root);

    let mut warnings = Vec::new();
    let state = prepare(&arena, root, resolver, &mut warnings);
    let mut raw = String::new();
    // запись в String не возвращает ошибок; на всякий случай — пустой вывод
    if format_document_with_formatter(
        root,
        &options,
        &mut raw,
        &Plugins::default(),
        format_node,
        state,
    )
    .is_err()
    {
        raw.clear();
    }

    Rendered {
        html: sanitize_html(&raw),
        toc: outline(&doc),
        links: doc.links,
        warnings,
    }
}

// ---------- Подготовка: разрешение ссылок и callout ----------

/// Сворачивание callout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fold {
    None,
    Open,
    Closed,
}

impl Fold {
    fn as_str(self) -> &'static str {
        match self {
            Fold::None => "none",
            Fold::Open => "open",
            Fold::Closed => "closed",
        }
    }
}

/// Вид встраиваемого вложения по MIME имени файла.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Media {
    Image,
    Video,
    Audio,
    File,
}

/// Решение для узла ссылки, принятое до рендера.
#[derive(Debug, Clone)]
enum Plan {
    /// Вики-ссылка: href (None — битая), существует ли цель, исходная цель.
    Wiki {
        href: Option<String>,
        exists: bool,
        target: String,
    },
    /// Разрешённое вложение.
    Attachment { media: Media, url: String },
    /// Неразрешённое вложение: выводится только текст.
    Unresolved,
}

/// Данные проходов для форматтера; узлы — по адресу в арене.
struct State {
    /// Цитата → callout `(тип, сворачивание)`.
    callouts: HashMap<usize, (String, Fold)>,
    /// Абзац-заголовок callout → сворачивание.
    titles: HashMap<usize, Fold>,
    /// Ссылки, картинки и вики-ссылки с особым выводом.
    plans: HashMap<usize, Plan>,
    /// Остаток бюджета подсветки документа, байт исходника.
    highlight_budget: usize,
}

impl Default for State {
    fn default() -> Self {
        Self {
            callouts: HashMap::new(),
            titles: HashMap::new(),
            plans: HashMap::new(),
            highlight_budget: HIGHLIGHT_BUDGET_BYTES,
        }
    }
}

fn key(node: &AstNode<'_>) -> usize {
    std::ptr::from_ref(node) as usize
}

fn prepare<'a>(
    arena: &'a Arena<'a>,
    root: &'a AstNode<'a>,
    resolver: &dyn Resolver,
    warnings: &mut Vec<String>,
) -> State {
    let mut state = State::default();
    let nodes: Vec<&'a AstNode<'a>> = root.descendants().collect();
    let mut quotes = Vec::new();
    for node in nodes {
        let plan = match &node.data().value {
            NodeValue::WikiLink(w) => Some(plan_wiki(&w.url, resolver, warnings)),
            NodeValue::Image(l) | NodeValue::Link(l) => plan_attachment(&l.url, resolver, warnings),
            NodeValue::BlockQuote => {
                quotes.push(node);
                None
            }
            _ => None,
        };
        if let Some(plan) = plan {
            state.plans.insert(key(node), plan);
        }
    }
    for bq in quotes {
        if let Some((kind, fold, title)) = make_callout(arena, bq) {
            state.callouts.insert(key(bq), (kind, fold));
            state.titles.insert(key(title), fold);
        }
    }
    state
}

fn plan_wiki(url: &str, resolver: &dyn Resolver, warnings: &mut Vec<String>) -> Plan {
    let (target, anchor) = split_anchor(url);
    let res = resolver.wiki(target, anchor);
    match &res {
        None => warnings.push(format!("битая вики-ссылка [[{url}]]")),
        Some(r) if !r.exists => warnings.push(format!("нет заметки для вики-ссылки [[{url}]]")),
        Some(_) => {}
    }
    Plan::Wiki {
        exists: res.as_ref().is_some_and(|r| r.exists),
        href: res.map(|r| r.href),
        target: url.to_string(),
    }
}

/// `None` — не ссылка на вложение, обычный вывод comrak.
fn plan_attachment(url: &str, resolver: &dyn Resolver, warnings: &mut Vec<String>) -> Option<Plan> {
    let (sha16, name) = attachment_ref(url)?;
    let resolved = name.and_then(|name| {
        resolver
            .attachment(&sha16, &name)
            .map(|url| (media_of(&name), url))
    });
    Some(match resolved {
        Some((media, url)) => Plan::Attachment { media, url },
        None => {
            warnings.push(format!("вложение не найдено: {url}"));
            Plan::Unresolved
        }
    })
}

/// Разбирает `_attachments/<16 hex>/<имя>` (допустимы ведущие `./`, `../`, `/`).
/// Хэш приводится к нижнему регистру; суффиксы `?…` и `#…` отбрасываются;
/// имя декодируется из percent-encoding. `Some((sha, None))` — ссылка на
/// вложение с недопустимым именем; `None` — не ссылка на вложение.
pub fn attachment_ref(url: &str) -> Option<(String, Option<String>)> {
    let mut u = url;
    while let Some(r) = u.strip_prefix("./").or_else(|| u.strip_prefix("../")) {
        u = r;
    }
    let u = u.strip_prefix('/').unwrap_or(u);
    let (sha16, name) = u.strip_prefix("_attachments/")?.split_once('/')?;
    if sha16.len() != 16 || !sha16.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let name = name.split(['?', '#']).next().unwrap_or_default();
    let name = percent_decode(name)
        .filter(|n| !n.is_empty() && !n.contains(['/', '\\', '\0']) && n != "." && n != "..");
    Some((sha16.to_ascii_lowercase(), name))
}

/// Декодирует `%XX`; `None` — неверная последовательность или не UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            // from_str_radix допускает знак: `%+1` — не percent-последовательность
            if !hex.bytes().all(|h| h.is_ascii_hexdigit()) {
                return None;
            }
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn media_of(name: &str) -> Media {
    match mime_guess::from_path(name).first() {
        Some(m) if m.type_() == mime_guess::mime::IMAGE => Media::Image,
        Some(m) if m.type_() == mime_guess::mime::VIDEO => Media::Video,
        Some(m) if m.type_() == mime_guess::mime::AUDIO => Media::Audio,
        _ => Media::File,
    }
}

/// Маркер Obsidian `[!тип]` и знак сворачивания в начале строки: `(тип, fold, длина)`.
fn parse_marker(s: &str) -> Option<(String, Fold, usize)> {
    let rest = s.strip_prefix("[!")?;
    let end = rest.find(']')?;
    let kind = &rest[..end];
    if !is_callout_kind(kind) {
        return None;
    }
    let after = &rest[end + 1..];
    let (fold, sign) = match after.as_bytes().first() {
        Some(b'-') => (Fold::Closed, 1),
        Some(b'+') => (Fold::Open, 1),
        _ => (Fold::None, 0),
    };
    if !after[sign..].is_empty() && !after[sign..].starts_with(char::is_whitespace) {
        return None;
    }
    Some((kind.to_lowercase(), fold, 2 + end + 1 + sign))
}

/// Допустимый тип callout: буквы и цифры любого алфавита (`заметка`, `note`), `-`, `_`.
/// Тот же набор проверяет санитайзер в `data-callout`.
pub(crate) fn is_callout_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

/// Тип с заглавной первой буквой — заголовок callout по умолчанию.
fn default_title(kind: &str) -> String {
    let mut c = kind.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

fn text_of<'a>(node: &'a AstNode<'a>) -> Option<String> {
    match &node.data().value {
        NodeValue::Text(t) => Some(t.to_string()),
        _ => None,
    }
}

fn set_text(node: &AstNode<'_>, text: String) {
    node.data_mut().value = NodeValue::Text(Cow::Owned(text));
}

/// Превращает цитату `> [!тип]± заголовок` в callout: удаляет маркер, отделяет
/// первую строку абзаца как заголовок. Возвращает `(тип, fold, абзац-заголовок)`.
fn make_callout<'a>(
    arena: &'a Arena<'a>,
    bq: &'a AstNode<'a>,
) -> Option<(String, Fold, &'a AstNode<'a>)> {
    let para = bq.first_child()?;
    if !matches!(para.data().value, NodeValue::Paragraph) {
        return None;
    }
    // ведущие текстовые узлы: comrak может разбить `[!info]` на несколько
    let mut lead = String::new();
    let mut texts = Vec::new();
    let mut cur = para.first_child();
    while let Some(n) = cur {
        let Some(t) = text_of(n) else { break };
        lead.push_str(&t);
        texts.push((n, t.len()));
        cur = n.next_sibling();
    }
    let (kind, fold, marker_len) = parse_marker(&lead)?;

    // удалить маркер
    let mut rest = marker_len;
    for (n, len) in texts {
        if rest == 0 {
            break;
        }
        if len <= rest {
            n.detach();
            rest -= len;
        } else {
            let t = text_of(n).unwrap_or_default();
            set_text(n, t[rest..].to_string());
            rest = 0;
        }
    }
    if let Some(first) = para.first_child()
        && let Some(t) = text_of(first)
    {
        let trimmed = t.trim_start();
        if trimmed.is_empty() {
            first.detach();
        } else if trimmed.len() != t.len() {
            set_text(first, trimmed.to_string());
        }
    }

    // остаток абзаца после первого переноса — отдельный абзац тела
    let brk = para
        .children()
        .find(|n| matches!(n.data().value, NodeValue::SoftBreak | NodeValue::LineBreak));
    if let Some(brk) = brk {
        let body = arena.alloc(AstNode::from(NodeValue::Paragraph));
        while let Some(n) = brk.next_sibling() {
            body.append(n);
        }
        brk.detach();
        if let Some(first) = body.first_child() {
            // позиции: заголовок — до конца его последнего узла, тело — от первого своего
            let mut sp = para.data().sourcepos;
            let start = first.data().sourcepos.start;
            if start.line > 0 {
                sp.start = start;
            }
            body.data_mut().sourcepos = sp;
            if let Some(last) = para.last_child() {
                let end = last.data().sourcepos.end;
                if end.line > 0 {
                    para.data_mut().sourcepos.end = end;
                }
            }
            para.insert_after(body);
        }
    }
    if para.first_child().is_none() {
        let title = arena.alloc(AstNode::from(NodeValue::Text(Cow::Owned(default_title(
            &kind,
        )))));
        para.append(title);
    }
    Some((kind, fold, para))
}

// ---------- Форматтер ----------

type Ctx<'o, 'c> = Context<'o, 'c, State>;
type Res = Result<ChildRendering, fmt::Error>;

fn format_node<'a>(ctx: &mut Ctx<'_, '_>, node: &'a AstNode<'a>, entering: bool) -> Res {
    let data = node.data();
    match &data.value {
        NodeValue::Heading(h) => heading(ctx, node, entering, h.level),
        NodeValue::CodeBlock(cb) => {
            if entering {
                code_block(ctx, node, cb)?;
            }
            Ok(ChildRendering::HTML)
        }
        NodeValue::Math(m) => {
            if entering {
                ctx.write_str("<span class=\"math\" data-math-style=\"")?;
                ctx.write_str(if m.display_math { "display" } else { "inline" })?;
                ctx.write_str("\"")?;
                render_sourcepos(ctx, node)?;
                ctx.write_str(">")?;
                ctx.escape(&m.literal)?;
                ctx.write_str("</span>")?;
            }
            Ok(ChildRendering::HTML)
        }
        NodeValue::BlockQuote => match ctx.user.callouts.get(&key(node)).cloned() {
            Some((kind, fold)) => {
                callout_container(ctx, node, entering, &kind, fold)?;
                Ok(ChildRendering::HTML)
            }
            None => format_node_default(ctx, node, entering),
        },
        NodeValue::Paragraph => match ctx.user.titles.get(&key(node)).copied() {
            Some(fold) => {
                callout_title(ctx, node, entering, fold)?;
                Ok(ChildRendering::HTML)
            }
            None => format_node_default(ctx, node, entering),
        },
        NodeValue::Alert(a) => {
            alert(ctx, node, entering, a)?;
            Ok(ChildRendering::HTML)
        }
        NodeValue::WikiLink(_) => match ctx.user.plans.get(&key(node)).cloned() {
            Some(Plan::Wiki {
                href,
                exists,
                target,
            }) => {
                wiki_link(ctx, node, entering, href.as_deref(), exists, &target)?;
                Ok(ChildRendering::HTML)
            }
            _ => format_node_default(ctx, node, entering),
        },
        NodeValue::Image(l) => match ctx.user.plans.get(&key(node)).cloned() {
            Some(plan) => attachment_image(ctx, node, entering, l, plan),
            None => format_node_default(ctx, node, entering),
        },
        NodeValue::Link(l) => match ctx.user.plans.get(&key(node)).cloned() {
            Some(plan) => {
                attachment_link(ctx, node, entering, plan)?;
                Ok(ChildRendering::HTML)
            }
            None => {
                link(ctx, node, entering, l)?;
                Ok(ChildRendering::HTML)
            }
        },
        _ => format_node_default(ctx, node, entering),
    }
}

/// Заголовок с `id` по алгоритму comrak (тот же [`comrak::Anchorizer`] и
/// порядок, что в `parse`), без ссылки-якоря comrak: якоря добавляет WebUI.
fn heading<'a>(ctx: &mut Ctx<'_, '_>, node: &'a AstNode<'a>, entering: bool, level: u8) -> Res {
    if entering {
        ctx.cr()?;
        let id = ctx.anchorizer.anchorize(&node.collect_text());
        write!(ctx, "<h{level} id=\"{HEADER_ID_PREFIX}")?;
        ctx.escape(&id)?;
        ctx.write_str("\"")?;
        render_sourcepos(ctx, node)?;
        ctx.write_str(">")?;
    } else {
        write!(ctx, "</h{level}>")?;
        ctx.lf()?;
    }
    Ok(ChildRendering::HTML)
}

/// Ограждённый и отступный код: `math` — формула, `mermaid` — исходник
/// диаграммы без подсветки, остальное — syntect с классами `hl-*`.
fn code_block<'a>(ctx: &mut Ctx<'_, '_>, node: &'a AstNode<'a>, cb: &NodeCodeBlock) -> fmt::Result {
    let lang = cb.info.split_ascii_whitespace().next().unwrap_or("");
    ctx.cr()?;
    if lang == "math" {
        ctx.write_str("<div class=\"math\" data-math-style=\"display\"")?;
        render_sourcepos(ctx, node)?;
        ctx.write_str(">")?;
        ctx.escape(&cb.literal)?;
        ctx.write_str("</div>")?;
    } else if lang.eq_ignore_ascii_case("mermaid") {
        ctx.write_str("<pre class=\"mermaid-src\"")?;
        render_sourcepos(ctx, node)?;
        ctx.write_str("><code>")?;
        ctx.escape(&cb.literal)?;
        ctx.write_str("</code></pre>")?;
    } else {
        match highlight(lang, &cb.literal, &mut ctx.user.highlight_budget) {
            Highlighted::Html(html) => {
                ctx.write_str("<pre class=\"hl-code\"")?;
                render_sourcepos(ctx, node)?;
                ctx.write_str("><code>")?;
                ctx.write_str(&html)?;
                ctx.write_str("</code></pre>")?;
            }
            // бюджет подсветки документа исчерпан (ТЗ §17)
            Highlighted::Skipped => plain_code(ctx, node, &cb.literal, " class=\"hl-skipped\"")?,
            Highlighted::Plain => plain_code(ctx, node, &cb.literal, "")?,
        }
    }
    ctx.lf()
}

/// Блок кода экранированным текстом; `class_attr` — готовый атрибут или пусто.
fn plain_code<'a>(
    ctx: &mut Ctx<'_, '_>,
    node: &'a AstNode<'a>,
    literal: &str,
    class_attr: &str,
) -> fmt::Result {
    ctx.write_str("<pre")?;
    ctx.write_str(class_attr)?;
    render_sourcepos(ctx, node)?;
    ctx.write_str("><code>")?;
    ctx.escape(literal)?;
    ctx.write_str("</code></pre>")
}

fn callout_open<'a>(
    ctx: &mut Ctx<'_, '_>,
    node: &'a AstNode<'a>,
    kind: &str,
    fold: Fold,
) -> fmt::Result {
    ctx.cr()?;
    ctx.write_str(if fold == Fold::None {
        "<div"
    } else {
        "<details"
    })?;
    ctx.write_str(" class=\"callout\" data-callout=\"")?;
    ctx.escape(kind)?;
    ctx.write_str("\" data-fold=\"")?;
    ctx.write_str(fold.as_str())?;
    ctx.write_str("\"")?;
    render_sourcepos(ctx, node)?;
    if fold == Fold::Open {
        ctx.write_str(" open")?;
    }
    ctx.write_str(">")?;
    ctx.lf()
}

fn callout_close(ctx: &mut Ctx<'_, '_>, fold: Fold) -> fmt::Result {
    ctx.cr()?;
    // закрывается и обёртка тела, открытая после заголовка
    ctx.write_str("</div>")?;
    ctx.write_str(if fold == Fold::None {
        "</div>"
    } else {
        "</details>"
    })?;
    ctx.lf()
}

fn title_tag(fold: Fold) -> &'static str {
    if fold == Fold::None { "div" } else { "summary" }
}

fn callout_container<'a>(
    ctx: &mut Ctx<'_, '_>,
    node: &'a AstNode<'a>,
    entering: bool,
    kind: &str,
    fold: Fold,
) -> fmt::Result {
    if entering {
        callout_open(ctx, node, kind, fold)
    } else {
        callout_close(ctx, fold)
    }
}

fn callout_title<'a>(
    ctx: &mut Ctx<'_, '_>,
    node: &'a AstNode<'a>,
    entering: bool,
    fold: Fold,
) -> fmt::Result {
    let tag = title_tag(fold);
    if entering {
        write!(ctx, "<{tag} class=\"callout-title\"")?;
        render_sourcepos(ctx, node)?;
        ctx.write_str(">")
    } else {
        write!(ctx, "</{tag}>")?;
        ctx.lf()?;
        ctx.write_str("<div class=\"callout-content\">")?;
        ctx.lf()
    }
}

/// GitHub alert — та же разметка, что у callout. Заголовок comrak после
/// `[!NOTE]` может начинаться со знака сворачивания Obsidian.
fn alert<'a>(
    ctx: &mut Ctx<'_, '_>,
    node: &'a AstNode<'a>,
    entering: bool,
    a: &NodeAlert,
) -> fmt::Result {
    let raw = a.title.as_deref().unwrap_or("");
    let (fold, title) = match raw.as_bytes().first() {
        Some(b'-') => (Fold::Closed, raw[1..].trim()),
        Some(b'+') => (Fold::Open, raw[1..].trim()),
        _ => (Fold::None, raw),
    };
    if !entering {
        return callout_close(ctx, fold);
    }
    let kind = a.alert_type.default_title().to_ascii_lowercase();
    callout_open(ctx, node, &kind, fold)?;
    let tag = title_tag(fold);
    write!(ctx, "<{tag} class=\"callout-title\">")?;
    if title.is_empty() {
        ctx.escape(a.alert_type.default_title())?;
    } else {
        ctx.escape(title)?;
    }
    write!(ctx, "</{tag}>")?;
    ctx.lf()?;
    ctx.write_str("<div class=\"callout-content\">")?;
    ctx.lf()
}

fn write_href(ctx: &mut Ctx<'_, '_>, url: &str) -> fmt::Result {
    ctx.write_str(" href=\"")?;
    if !dangerous_url(url) {
        ctx.escape_href(url)?;
    }
    ctx.write_str("\"")
}

fn wiki_link<'a>(
    ctx: &mut Ctx<'_, '_>,
    node: &'a AstNode<'a>,
    entering: bool,
    href: Option<&str>,
    exists: bool,
    target: &str,
) -> fmt::Result {
    if !entering {
        return ctx.write_str("</a>");
    }
    ctx.write_str(if exists {
        "<a class=\"wikilink\""
    } else {
        "<a class=\"wikilink broken\""
    })?;
    if let Some(href) = href {
        write_href(ctx, href)?;
    }
    if !exists {
        ctx.write_str(" data-target=\"")?;
        ctx.escape(target)?;
        ctx.write_str("\"")?;
    }
    render_sourcepos(ctx, node)?;
    ctx.write_str(">")
}

/// Внешняя ссылка — абсолютный http(s)-URL или URL без схемы `//host`.
fn is_external(url: &str) -> bool {
    let u = url.trim_start().as_bytes();
    let starts = |p: &str| u.len() >= p.len() && u[..p.len()].eq_ignore_ascii_case(p.as_bytes());
    starts("http://") || starts("https://") || starts("//")
}

/// Обычная ссылка comrak; внешние получают `rel="noopener noreferrer"`.
fn link<'a>(
    ctx: &mut Ctx<'_, '_>,
    node: &'a AstNode<'a>,
    entering: bool,
    l: &NodeLink,
) -> fmt::Result {
    if !entering {
        return ctx.write_str("</a>");
    }
    ctx.write_str("<a")?;
    render_sourcepos(ctx, node)?;
    write_href(ctx, &l.url)?;
    if !l.title.is_empty() {
        ctx.write_str(" title=\"")?;
        ctx.escape(&l.title)?;
        ctx.write_str("\"")?;
    }
    if is_external(&l.url) {
        write!(ctx, " rel=\"{EXTERNAL_REL}\"")?;
    }
    ctx.write_str(">")
}

/// `[текст](_attachments/…)`: разрешённая — ссылка на скачивание,
/// неразрешённая — только текст.
fn attachment_link<'a>(
    ctx: &mut Ctx<'_, '_>,
    node: &'a AstNode<'a>,
    entering: bool,
    plan: Plan,
) -> fmt::Result {
    let Plan::Attachment { url, .. } = plan else {
        return Ok(());
    };
    if !entering {
        return ctx.write_str("</a>");
    }
    ctx.write_str("<a class=\"attachment\"")?;
    write_href(ctx, &url)?;
    ctx.write_str(" download")?;
    render_sourcepos(ctx, node)?;
    ctx.write_str(">")
}

/// `![подпись](_attachments/…)`: вид по MIME; неразрешённое — текст подписи.
fn attachment_image<'a>(
    ctx: &mut Ctx<'_, '_>,
    node: &'a AstNode<'a>,
    entering: bool,
    l: &NodeLink,
    plan: Plan,
) -> Res {
    if !entering {
        return Ok(ChildRendering::HTML);
    }
    let alt = node.collect_text();
    let Plan::Attachment { media, url } = plan else {
        // неразрешённое вложение: подпись или имя файла
        let text = if alt.is_empty() {
            l.url.rsplit('/').next().unwrap_or_default()
        } else {
            &alt
        };
        ctx.escape(text)?;
        return Ok(ChildRendering::Skip);
    };
    match media {
        Media::Image => {
            ctx.write_str("<img")?;
            write_src(ctx, &url)?;
            ctx.write_str(" alt=\"")?;
            ctx.escape(&alt)?;
            ctx.write_str("\"")?;
            if !l.title.is_empty() {
                ctx.write_str(" title=\"")?;
                ctx.escape(&l.title)?;
                ctx.write_str("\"")?;
            }
            render_sourcepos(ctx, node)?;
            ctx.write_str(" />")?;
        }
        Media::Video | Media::Audio => {
            let tag = if media == Media::Video {
                "video"
            } else {
                "audio"
            };
            write!(ctx, "<{tag} controls preload=\"metadata\"")?;
            write_src(ctx, &url)?;
            render_sourcepos(ctx, node)?;
            ctx.write_str(">")?;
            ctx.escape(&alt)?;
            write!(ctx, "</{tag}>")?;
        }
        Media::File => {
            ctx.write_str("<a class=\"attachment\"")?;
            write_href(ctx, &url)?;
            ctx.write_str(" download")?;
            render_sourcepos(ctx, node)?;
            ctx.write_str(">")?;
            let text = if alt.is_empty() {
                l.url.rsplit('/').next().unwrap_or_default()
            } else {
                &alt
            };
            ctx.escape(text)?;
            ctx.write_str("</a>")?;
        }
    }
    Ok(ChildRendering::Skip)
}

fn write_src(ctx: &mut Ctx<'_, '_>, url: &str) -> fmt::Result {
    ctx.write_str(" src=\"")?;
    if !dangerous_url(url) {
        ctx.escape_href(url)?;
    }
    ctx.write_str("\"")
}
