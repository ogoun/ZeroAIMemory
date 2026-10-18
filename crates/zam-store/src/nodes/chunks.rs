//! Метаданные заметки (frontmatter, L0) и чанки секций (§8.2, §8.3 ТЗ).
//!
//! M1: один чанк на секцию H1–H3 (`level = 'section'`) и на непустую преамбулу.
//! Текст чанка — собственные байты секции без строки заголовка и без вложенных
//! подсекций H1–H3: чанки не перекрываются, а вместе покрывают всё тело заметки.

use rusqlite::{Connection, params};
use zam_core::Id;
use zam_markdown::{Doc, first_paragraph, parse};

use super::hex;
use crate::StoreError;

/// Предел длины автоматического L0, символов.
pub(crate) const L0_AUTO_MAX_CHARS: usize = 200;

/// Что извлечено из исходника заметки при записи.
#[derive(Debug)]
pub(crate) struct NoteMeta {
    pub doc: Doc,
    /// JSON для `nodes.frontmatter`.
    pub frontmatter: Option<serde_json::Value>,
    pub l0: Option<String>,
    pub l0_source: Option<&'static str>,
    /// `frontmatter.index != false`.
    pub index: bool,
}

/// Разбор заметки и выбор L0: `agent_l0` → `l0` из frontmatter → `description` →
/// первый абзац, обрезанный по границе слова.
pub(crate) fn note_meta(content: &str, agent_l0: Option<&str>) -> NoteMeta {
    let doc = parse(content);
    let fm = doc.frontmatter.as_ref();
    let frontmatter = fm.map(|f| {
        let mut m = serde_json::Map::new();
        m.insert("tags".into(), serde_json::json!(f.tags));
        if let Some(d) = &f.description {
            m.insert("description".into(), d.clone().into());
        }
        if let Some(l0) = &f.l0 {
            m.insert("l0".into(), l0.clone().into());
        }
        if let Some(i) = f.index {
            m.insert("index".into(), i.into());
        }
        serde_json::Value::Object(m)
    });
    let nonempty = |s: Option<&str>| {
        s.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let (l0, l0_source) = if let Some(s) = nonempty(agent_l0) {
        (Some(s), Some("agent"))
    } else if let Some(s) = nonempty(fm.and_then(|f| f.l0.as_deref())) {
        (Some(s), Some("manual"))
    } else if let Some(s) = nonempty(fm.and_then(|f| f.description.as_deref())) {
        (Some(s), Some("manual"))
    } else if let Some(s) = first_paragraph(content, &doc) {
        (Some(truncate_words(&s, L0_AUTO_MAX_CHARS)), Some("auto"))
    } else {
        (None, None)
    };
    let index = fm.and_then(|f| f.index) != Some(false);
    NoteMeta {
        doc,
        frontmatter,
        l0,
        l0_source,
        index,
    }
}

/// Не длиннее `max` символов; если строку пришлось резать — по последней границе слова.
pub(crate) fn truncate_words(s: &str, max: usize) -> String {
    let Some((cut, _)) = s.char_indices().nth(max) else {
        return s.to_string();
    };
    let head = &s[..cut];
    let at_boundary = s[cut..].starts_with(char::is_whitespace);
    let head = if at_boundary {
        head
    } else {
        // слово разрезано: отбрасываем его хвост, если до него было хоть одно слово
        head.rfind(char::is_whitespace).map_or(head, |i| &head[..i])
    };
    head.trim_end().to_string()
}

/// `[<space>/<path>] <имя> > <heading_path> | <тег1>, <тег2>` (§8.2 п.6);
/// ` > …` опускается при пустом пути заголовков, ` | …` — без тегов.
pub(crate) fn context_prefix(
    space: &str,
    path: &str,
    name: &str,
    heading_path: &str,
    tags: &[String],
) -> String {
    let mut s = format!("[{space}/{path}] {name}");
    if !heading_path.is_empty() {
        s.push_str(" > ");
        s.push_str(heading_path);
    }
    if !tags.is_empty() {
        s.push_str(" | ");
        s.push_str(&tags.join(", "));
    }
    s
}

/// Чанк секции до вставки в БД.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SectionChunk {
    pub heading_path: String,
    pub ordinal: i64,
    pub text: String,
    pub text_hash: String,
}

/// Чанки секций документа: по одному на секцию с непустым собственным текстом.
pub(crate) fn section_chunks(src: &str, doc: &Doc) -> Vec<SectionChunk> {
    let mut out = Vec::with_capacity(doc.sections.len());
    for (i, s) in doc.sections.iter().enumerate() {
        // собственный текст — до начала следующей секции (вложенной или соседней)
        let end = doc
            .sections
            .get(i + 1)
            .map_or(s.body.end, |n| n.range.start)
            .clamp(s.body.start, s.body.end);
        let Some(text) = src.get(s.body.start..end) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        out.push(SectionChunk {
            heading_path: s.path.join(" > "),
            ordinal: out.len() as i64,
            text: text.to_string(),
            text_hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
        });
    }
    out
}

/// Детерминированный id чанка: первые 16 байт blake3(node ‖ ordinal ‖ text_hash), hex.
pub(crate) fn chunk_id(node: Id, ordinal: i64, text_hash: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(node.as_uuid().as_bytes());
    h.update(&ordinal.to_le_bytes());
    h.update(text_hash.as_bytes());
    hex(&h.finalize().as_bytes()[..16])
}

/// Приблизительное число токенов (слова через пробельные символы) — до появления
/// токенизатора модели эмбеддингов.
fn approx_tokens(text: &str) -> i64 {
    text.split_whitespace().count() as i64
}

/// Пересобирает чанки заметки `node_id` по ревизии `rev`: старые удаляются
/// (триггеры убирают их из FTS), новые вставляются, если `index`.
/// Вызывается внутри транзакции записи. Возвращает число вставленных чанков.
pub(crate) fn rebuild_chunks(
    c: &Connection,
    node_id: Id,
    rev: i64,
    content: &str,
    meta: &NoteMeta,
) -> Result<usize, StoreError> {
    c.execute("DELETE FROM chunks WHERE node_id = ?1", [node_id])?;
    if !meta.index {
        return Ok(0);
    }
    let (space, path, name): (String, String, String) = c.query_row(
        "SELECT s.name, n.path, n.name FROM nodes n JOIN spaces s ON s.id = n.space_id
         WHERE n.id = ?1",
        [node_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let tags = meta
        .doc
        .frontmatter
        .as_ref()
        .map_or(&[][..], |f| &f.tags[..]);
    let chunks = section_chunks(content, &meta.doc);
    let mut st = c.prepare_cached(
        "INSERT INTO chunks (id, node_id, rev, level, heading_path, ordinal, text,
                             context_prefix, token_count, text_hash)
         VALUES (?1, ?2, ?3, 'section', ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for ch in &chunks {
        st.execute(params![
            chunk_id(node_id, ch.ordinal, &ch.text_hash),
            node_id,
            rev,
            ch.heading_path,
            ch.ordinal,
            ch.text,
            context_prefix(&space, &path, &name, &ch.heading_path, tags),
            approx_tokens(&ch.text),
            ch.text_hash,
        ])?;
    }
    Ok(chunks.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_at_word_boundary() {
        assert_eq!(truncate_words("короткая строка", 200), "короткая строка");
        assert_eq!(truncate_words("ab cd ef", 5), "ab cd");
        assert_eq!(truncate_words("ab cd ef", 4), "ab");
        assert_eq!(truncate_words("ab cd ef", 6), "ab cd");
        assert_eq!(truncate_words("abcdef", 3), "abc");
        let s = "слово ".repeat(60);
        let t = truncate_words(s.trim(), 200);
        assert!(t.chars().count() <= 200 && t.ends_with("слово"));
    }

    #[test]
    fn nested_sections_do_not_overlap() {
        let src = "intro\n# A\n\na\n## B\n\nb\n# C\n\nc\n";
        let doc = parse(src);
        let ch = section_chunks(src, &doc);
        let got: Vec<_> = ch
            .iter()
            .map(|c| (c.heading_path.as_str(), c.text.trim()))
            .collect();
        assert_eq!(
            got,
            vec![("", "intro"), ("A", "a"), ("A > B", "b"), ("C", "c")]
        );
    }

    #[test]
    fn chunk_id_is_deterministic() {
        let n = Id::new();
        let a = chunk_id(n, 0, "h");
        assert_eq!(a, chunk_id(n, 0, "h"));
        assert_eq!(a.len(), 32);
        assert_ne!(a, chunk_id(n, 1, "h"));
        assert_ne!(a, chunk_id(Id::new(), 0, "h"));
    }

    #[test]
    fn prefix_format() {
        let tags = ["x".to_string(), "y z".to_string()];
        assert_eq!(context_prefix("K", "a/b", "b", "", &[]), "[K/a/b] b");
        assert_eq!(
            context_prefix("K", "a/b", "b", "", &tags),
            "[K/a/b] b | x, y z"
        );
        assert_eq!(
            context_prefix("K", "a/b", "b", "H", &tags),
            "[K/a/b] b > H | x, y z"
        );
        assert_eq!(
            context_prefix("K", "a/b", "b", "X > Y", &[]),
            "[K/a/b] b > X > Y"
        );
    }
}
