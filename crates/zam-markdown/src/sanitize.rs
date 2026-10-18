//! Санитизация итогового HTML: `ammonia` с явным allowlist (ТЗ §15.2).
//!
//! Конфигурация начинается с [`ammonia::Builder::empty`], и каждый параметр
//! задан явно, чтобы умолчания ammonia не расширили список. Всё, чего нет в
//! списке, удаляется: теги (содержимое `script`/`style` — целиком),
//! атрибуты, классы без разрешённого префикса, URL с чужими схемами.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use ammonia::{Builder, UrlRelative};

use crate::render::is_callout_kind;

/// Разрешённые теги: всё, что порождают comrak (GFM, сноски, задачи) и
/// собственные проходы (callout, медиа, формулы).
const TAGS: &[&str] = &[
    "a",
    "audio",
    "blockquote",
    "br",
    "code",
    "del",
    "details",
    "div",
    "em",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "img",
    "input",
    "li",
    "ol",
    "p",
    "pre",
    "section",
    "span",
    "strong",
    "sub",
    "summary",
    "sup",
    "table",
    "tbody",
    "td",
    "th",
    "thead",
    "tr",
    "ul",
    "video",
];

/// Теги, удаляемые вместе с содержимым.
const CLEAN_CONTENT_TAGS: &[&str] = &["script", "style"];

/// Атрибуты, разрешённые на любом теге (значения проверяет [`filter`]).
const GENERIC_ATTRIBUTES: &[&str] = &[
    "class",
    "data-sourcepos",
    "data-math-style",
    "data-callout",
    "data-fold",
    "data-target",
];

/// Атрибуты отдельных тегов.
const TAG_ATTRIBUTES: &[(&str, &[&str])] = &[
    ("a", &["href", "title", "rel", "download", "id"]),
    ("img", &["src", "alt", "title"]),
    ("video", &["src", "controls", "preload"]),
    ("audio", &["src", "controls", "preload"]),
    ("input", &["type", "checked", "disabled"]),
    ("details", &["open"]),
    ("li", &["id"]),
    ("ol", &["start"]),
    ("th", &["align"]),
    ("td", &["align"]),
    ("h1", &["id"]),
    ("h2", &["id"]),
    ("h3", &["id"]),
    ("h4", &["id"]),
    ("h5", &["id"]),
    ("h6", &["id"]),
];

/// Принудительные значения: чекбокс задачи только для чтения.
const SET_ATTRIBUTES: &[(&str, &str, &str)] = &[("input", "disabled", "")];

/// Схемы абсолютных URL; относительные проходят как есть.
const URL_SCHEMES: &[&str] = &["http", "https", "mailto"];

/// Префиксы допустимых классов.
const CLASS_PREFIXES: &[&str] = &[
    "hl-",
    "callout",
    "wikilink",
    "math",
    "mermaid-src",
    "attachment",
    "footnote",
    "task-list-item",
    "broken",
];

/// `rel` внешних ссылок — единственное допустимое значение.
pub(crate) const EXTERNAL_REL: &str = "noopener noreferrer";

/// Подстроки, после которых атрибут удаляется целиком (в любом теге и
/// атрибуте, после удаления пробельных и управляющих символов).
const DANGEROUS_VALUES: &[&str] = &["javascript:", "vbscript:", "data:text/html"];

static SANITIZER: LazyLock<Builder<'static>> = LazyLock::new(|| {
    let mut b = Builder::empty();
    b.tags(TAGS.iter().copied().collect())
        .clean_content_tags(CLEAN_CONTENT_TAGS.iter().copied().collect())
        .generic_attributes(GENERIC_ATTRIBUTES.iter().copied().collect())
        .generic_attribute_prefixes(HashSet::new())
        .tag_attributes(
            TAG_ATTRIBUTES
                .iter()
                .map(|&(t, a)| (t, a.iter().copied().collect()))
                .collect(),
        )
        .tag_attribute_values(HashMap::new())
        .set_tag_attribute_values(SET_ATTRIBUTES.iter().fold(
            HashMap::<&str, HashMap<&str, &str>>::new(),
            |mut m, &(t, a, v)| {
                m.entry(t).or_default().insert(a, v);
                m
            },
        ))
        .allowed_classes(HashMap::new())
        .url_schemes(URL_SCHEMES.iter().copied().collect())
        .url_relative(UrlRelative::PassThrough)
        .link_rel(None)
        .id_prefix(None)
        .strip_comments(true)
        .attribute_filter(filter);
    b
});

/// Санитизирует HTML-фрагмент по allowlist рендера. Вызывается из
/// [`render_html`](crate::render_html); публична для тестов и для HTML,
/// собранного вне рендера.
pub fn sanitize_html(html: &str) -> String {
    SANITIZER.clean(html).to_string()
}

/// Проверка значений разрешённых атрибутов; `None` — атрибут удаляется.
fn filter<'u>(element: &str, attribute: &str, value: &'u str) -> Option<Cow<'u, str>> {
    if is_dangerous(value) {
        return None;
    }
    let keep = |ok: bool| ok.then_some(Cow::Borrowed(value));
    match (element, attribute) {
        (_, "class") => {
            let classes: Vec<&str> = value
                .split_ascii_whitespace()
                .filter(|c| CLASS_PREFIXES.iter().any(|p| c.starts_with(p)))
                .collect();
            (!classes.is_empty()).then(|| Cow::Owned(classes.join(" ")))
        }
        (_, "data-sourcepos") => keep(
            !value.is_empty()
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b == b':' || b == b'-'),
        ),
        (_, "data-math-style") => keep(matches!(value, "inline" | "display")),
        (_, "data-fold") => keep(matches!(value, "open" | "closed" | "none")),
        (_, "data-callout") => keep(is_callout_kind(value)),
        ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", "id") => keep(value.starts_with("h-")),
        ("li", "id") => keep(value.starts_with("fn-")),
        ("a", "id") => keep(value.starts_with("fnref-")),
        ("a", "rel") => keep(value == EXTERNAL_REL),
        ("a", "download") => Some(Cow::Borrowed("")),
        ("input", "type") => Some(Cow::Borrowed("checkbox")),
        ("input", "checked" | "disabled") => Some(Cow::Borrowed("")),
        ("video" | "audio", "controls") => Some(Cow::Borrowed("")),
        ("video" | "audio", "preload") => Some(Cow::Borrowed("metadata")),
        ("details", "open") => Some(Cow::Borrowed("")),
        ("ol", "start") => keep(!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())),
        ("th" | "td", "align") => keep(matches!(value, "left" | "center" | "right")),
        _ => Some(Cow::Borrowed(value)),
    }
}

/// Значение содержит схему скрипта или HTML-данных — с учётом обходов через
/// регистр, пробельные, управляющие и невидимые символы.
fn is_dangerous(value: &str) -> bool {
    let norm: String = value
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control() && !is_invisible(*c))
        .flat_map(char::to_lowercase)
        .collect();
    DANGEROUS_VALUES.iter().any(|d| norm.contains(d))
}

/// Символы нулевой ширины, которые браузер может отбросить.
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{2060}'..='\u{2064}' | '\u{feff}' | '\u{ad}')
}
