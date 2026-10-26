//! Разрешение ссылок заметок (§8.5, §12 ТЗ) — общее для рёбер графа и рендера HTML.
//!
//! **Ключ ссылки** ([`link_key`], он же `edges.dst_raw` выведенных рёбер):
//! - вики-ссылка `[[Цель#Якорь|текст]]` → `[[Цель]]`: цель декодируется из percent-encoding и
//!   обрезается по краям, якорь хранится отдельно;
//! - Markdown-ссылка `[текст](путь)` на относительный путь → путь от корня пространства:
//!   от папки заметки-источника, `/…` — от корня, сегменты `.` и `..` сворачиваются (выход за
//!   корень — ссылки нет); внешние URL (со схемой или `//`), чистые якоря `#…`, вложения
//!   `_attachments/…` и картинки рёбер не дают;
//! - цель `relations:` во frontmatter — `[[Имя]]` или путь от корня пространства
//!   **[решение исполнителя]**.
//!
//! **Разрешение ключа** ([`resolve_key`]) — только в пространстве источника, только живые
//! заметки:
//! - `[[Цель]]` с `/` — путь (`nodes_live_path`), иначе имя — первая по `path` (как
//!   `find_by_name` и рендер до M3);
//! - путь — точное совпадение, затем без суффикса `.md` (ссылки в стиле Obsidian/GitHub).
//!
//! Рендер (`zam-server::api::render`) разрешает вики-ссылки этим же кодом: граф и HTML не
//! расходятся.

use rusqlite::{Connection, OptionalExtension, params};
use zam_core::Id;
use zam_markdown::{Doc, Link, LinkKind, attachment_ref};

use crate::graph::EdgeType;

/// Ключ ссылки `link` заметки по пути `from_path` (см. модуль); `None` — ссылка ребра не даёт.
pub fn link_key(from_path: &str, link: &Link) -> Option<String> {
    match link.kind {
        LinkKind::Wiki => wiki_key(&link.target),
        LinkKind::Markdown => markdown_key(from_path, &link.target),
        LinkKind::Image => None,
    }
}

/// `[[цель]]` по сырой цели вики-ссылки; пустая — `None`.
pub fn wiki_key(raw: &str) -> Option<String> {
    let t = percent_decode(raw);
    let t = t.trim();
    (!t.is_empty()).then(|| format!("[[{t}]]"))
}

/// Путь от корня пространства для Markdown-ссылки `target` заметки `from_path`.
fn markdown_key(from_path: &str, target: &str) -> Option<String> {
    let t = target.trim();
    if t.is_empty() || t.starts_with("//") || has_scheme(t) || attachment_ref(t).is_some() {
        return None;
    }
    // параметры запроса в путях заметок не бывают
    let t = t.split('?').next().unwrap_or(t);
    let t = percent_decode(t);
    let (base, rel): (Vec<&str>, &str) = match t.strip_prefix('/') {
        Some(rest) => (Vec::new(), rest),
        None => {
            let mut dir: Vec<&str> = from_path.split('/').collect();
            dir.pop();
            (dir, t.as_str())
        }
    };
    normalize(base, rel)
}

/// Свёртка `.` и `..` пути `rel` относительно сегментов `base`; выход за корень или пустой
/// итог — `None`.
fn normalize<'a>(mut segs: Vec<&'a str>, rel: &'a str) -> Option<String> {
    for s in rel.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                segs.pop()?;
            }
            s => segs.push(s),
        }
    }
    (!segs.is_empty()).then(|| segs.join("/"))
}

/// `scheme:` по RFC 3986 (`https:`, `mailto:`, `zam:` …): буква, затем буквы, цифры, `+-.`.
fn has_scheme(s: &str) -> bool {
    let Some((scheme, _)) = s.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Разрешает ссылку `link` заметки `from_path` в пространстве `space`: id живой заметки или
/// `None` (битая ссылка, внешний URL, картинка). Общее для графа и рендера.
pub fn resolve_link(
    c: &Connection,
    space: Id,
    from_path: &str,
    link: &Link,
) -> rusqlite::Result<Option<Id>> {
    match link_key(from_path, link) {
        Some(k) => resolve_key(c, space, &k),
        None => Ok(None),
    }
}

/// Разрешает ключ ссылки ([`link_key`]) в пространстве `space` (см. модуль).
pub fn resolve_key(c: &Connection, space: Id, key: &str) -> rusqlite::Result<Option<Id>> {
    if let Some(inner) = key.strip_prefix("[[").and_then(|k| k.strip_suffix("]]")) {
        if inner.contains('/') {
            return note_by_path(c, space, inner);
        }
        return c
            .prepare_cached(
                "SELECT id FROM nodes
                 WHERE space_id = ?1 AND name = ?2 AND kind = 'note' AND deleted_at IS NULL
                 ORDER BY path LIMIT 1",
            )?
            .query_row(params![space, inner], |r| r.get(0))
            .optional();
    }
    if let Some(id) = note_by_path(c, space, key)? {
        return Ok(Some(id));
    }
    match key.strip_suffix(".md") {
        Some(p) if !p.is_empty() && !p.ends_with('/') => note_by_path(c, space, p),
        _ => Ok(None),
    }
}

fn note_by_path(c: &Connection, space: Id, path: &str) -> rusqlite::Result<Option<Id>> {
    c.prepare_cached(
        "SELECT id FROM nodes
         WHERE space_id = ?1 AND path = ?2 AND kind = 'note' AND deleted_at IS NULL",
    )?
    .query_row(params![space, path], |r| r.get(0))
    .optional()
}

/// Ключи ссылок, которые может разрешить заметка с именем `name` по пути `path`: по ним
/// после создания, переноса, удаления и восстановления узла перепроверяются выведенные рёбра.
pub(crate) fn keys_of(name: &str, path: &str) -> [String; 4] {
    [
        format!("[[{name}]]"),
        format!("[[{path}]]"),
        path.to_string(),
        format!("{path}.md"),
    ]
}

/// Разобранная строка `relations:` frontmatter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub kind: EdgeType,
    /// Ключ цели ([`link_key`]).
    pub key: String,
    pub anchor: Option<String>,
}

/// Ошибка разбора строки `relations:`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelationError {
    /// Тип не из списка §8.5.
    UnknownType(String),
    /// Нет `:` или цели.
    Invalid,
}

/// Разбирает `"<тип>: <цель>"`, цель — `[[Имя#якорь|текст]]` или путь от корня пространства.
pub fn parse_relation(s: &str) -> Result<Relation, RelationError> {
    let (kind, target) = s.split_once(':').ok_or(RelationError::Invalid)?;
    let (kind, target) = (kind.trim(), target.trim());
    if kind.is_empty() || target.is_empty() {
        return Err(RelationError::Invalid);
    }
    let kind = EdgeType::parse(kind).ok_or_else(|| RelationError::UnknownType(kind.to_string()))?;
    let (key, anchor) = match target.strip_prefix("[[").and_then(|t| t.strip_suffix("]]")) {
        Some(inner) => {
            let inner = inner.split('|').next().unwrap_or(inner);
            let (t, a) = match inner.split_once('#') {
                Some((t, a)) => (t, (!a.trim().is_empty()).then(|| percent_decode(a.trim()))),
                None => (inner, None),
            };
            (wiki_key(t).ok_or(RelationError::Invalid)?, a)
        }
        None => {
            let (t, a) = match target.split_once('#') {
                Some((t, a)) => (t, (!a.is_empty()).then(|| percent_decode(a))),
                None => (target, None),
            };
            let t = percent_decode(t.trim());
            let path =
                normalize(Vec::new(), t.trim_start_matches('/')).ok_or(RelationError::Invalid)?;
            (path, a)
        }
    };
    Ok(Relation { kind, key, anchor })
}

/// Предупреждения записи заметки по `relations:`: `unknown relation type: <тип>` и
/// `invalid relation: <строка>` (такие строки ребра не дают).
pub fn relation_warnings(doc: &Doc) -> Vec<String> {
    let Some(fm) = &doc.frontmatter else {
        return Vec::new();
    };
    fm.relations
        .iter()
        .filter_map(|r| match parse_relation(r) {
            Ok(_) => None,
            Err(RelationError::UnknownType(t)) => Some(format!("unknown relation type: {t}")),
            Err(RelationError::Invalid) => Some(format!("invalid relation: {r}")),
        })
        .collect()
}

/// Декодирует `%XX`; некорректные последовательности и не-UTF-8 остаются как есть.
pub fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(target: &str) -> Link {
        Link {
            kind: LinkKind::Markdown,
            target: target.into(),
            anchor: None,
            label: None,
            range: 0..0,
        }
    }

    #[test]
    fn markdown_keys() {
        assert_eq!(link_key("a/b/n", &md("c.md")).as_deref(), Some("a/b/c.md"));
        assert_eq!(link_key("a/b/n", &md("../c")).as_deref(), Some("a/c"));
        assert_eq!(
            link_key("a/b/n", &md("./x/./y")).as_deref(),
            Some("a/b/x/y")
        );
        assert_eq!(link_key("a/b/n", &md("/root/x")).as_deref(), Some("root/x"));
        assert_eq!(link_key("n", &md("x%20y")).as_deref(), Some("x y"));
        assert_eq!(link_key("n", &md("../x")), None);
        for ext in [
            "https://example.com/a.md",
            "mailto:a@b.c",
            "//host/x",
            "",
            "_attachments/0123456789abcdef/a.png",
        ] {
            assert_eq!(link_key("a/n", &md(ext)), None, "{ext}");
        }
        let mut img = md("c");
        img.kind = LinkKind::Image;
        assert_eq!(link_key("a/n", &img), None);
    }

    #[test]
    fn wiki_keys_and_relations() {
        assert_eq!(
            wiki_key(" %D0%A6%D0%B5%D0%BB%D1%8C ").as_deref(),
            Some("[[Цель]]")
        );
        assert_eq!(wiki_key("  "), None);
        let r = parse_relation("depends_on: [[Сборка#Шаги|как]]").unwrap();
        assert_eq!(
            (r.kind, r.key.as_str(), r.anchor.as_deref()),
            (EdgeType::DependsOn, "[[Сборка]]", Some("Шаги"))
        );
        let r = parse_relation(" fixes :/notes/gotchas/crlf.md").unwrap();
        assert_eq!(
            (r.kind, r.key.as_str()),
            (EdgeType::Fixes, "notes/gotchas/crlf.md")
        );
        assert_eq!(
            parse_relation("blocks: [[X]]"),
            Err(RelationError::UnknownType("blocks".into()))
        );
        assert_eq!(parse_relation("relates_to"), Err(RelationError::Invalid));
        assert_eq!(
            parse_relation("relates_to: [[ ]]"),
            Err(RelationError::Invalid)
        );
    }

    #[test]
    fn scheme_detection() {
        assert!(has_scheme("https://x"));
        assert!(has_scheme("zam+x:1"));
        assert!(!has_scheme("a/b:c"));
        assert!(!has_scheme("1x:y"));
        assert!(!has_scheme("notes/a.md"));
    }

    #[test]
    fn percent_decode_cases() {
        assert_eq!(percent_decode("%D0%A6%D0%B5%D0%BB%D1%8C"), "Цель");
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%FF"), "%FF");
    }
}
