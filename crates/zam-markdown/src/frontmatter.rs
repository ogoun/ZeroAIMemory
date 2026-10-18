//! Разбор полей frontmatter.
//!
//! Нужны только плоские ключи верхнего уровня (`tags`, `description`, `l0`,
//! `index`), поэтому вместо YAML-библиотеки — узкое подмножество YAML:
//! скаляры (простые, `"…"`, `'…'`), булевы значения, списки `[a, b]` и
//! блочные списки `- a`. Остальные ключи сохраняются только в `raw`.

use crate::doc::Frontmatter;

/// Строит [`Frontmatter`] из блока, который выделил comrak
/// (с разделителями `---` и хвостовой пустой строкой).
pub(crate) fn parse(block: &str, delimiter: &str) -> Frontmatter {
    let raw = inner_yaml(block, delimiter);
    let mut fm = Frontmatter {
        raw: raw.to_string(),
        tags: Vec::new(),
        description: None,
        l0: None,
        index: None,
    };
    let lines: Vec<&str> = raw.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if line.starts_with([' ', '\t', '#', '-']) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = strip_comment(value).trim();
        match key.trim() {
            "tags" => {
                fm.tags = if value.is_empty() {
                    let (items, next) = block_list(&lines, i);
                    i = next;
                    items
                } else if let Some(inner) =
                    value.strip_prefix('[').and_then(|v| v.strip_suffix(']'))
                {
                    flow_list(inner)
                } else {
                    vec![scalar(value)]
                };
                fm.tags.retain(|t| !t.is_empty());
            }
            "description" => fm.description = non_empty(scalar(value)),
            "l0" => fm.l0 = non_empty(scalar(value)),
            "index" => fm.index = boolean(value),
            _ => {}
        }
    }
    fm
}

/// Текст между строками-разделителями.
fn inner_yaml<'a>(block: &'a str, delimiter: &str) -> &'a str {
    // первая строка — разделитель
    let Some(nl) = block.find('\n') else {
        return "";
    };
    let body = &block[nl + 1..];
    let mut pos = 0;
    for line in body.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == delimiter {
            return &body[..pos];
        }
        pos += line.len();
    }
    body
}

/// Элементы блочного списка `- a`, начиная со строки `from`; возвращает и индекс следующей строки.
fn block_list(lines: &[&str], from: usize) -> (Vec<String>, usize) {
    let mut items = Vec::new();
    let mut i = from;
    while let Some(line) = lines.get(i) {
        let t = line.trim_start();
        if let Some(item) = t
            .strip_prefix('-')
            .filter(|r| r.is_empty() || r.starts_with(' '))
        {
            items.push(scalar(strip_comment(item).trim()));
        } else if !t.is_empty() && !t.starts_with('#') {
            break;
        }
        i += 1;
    }
    (items, i)
}

/// Элементы `a, "b, c", 'd'` внутри `[...]`.
fn flow_list(inner: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in inner.chars() {
        match (quote, c) {
            (None, ',') => {
                items.push(scalar(cur.trim()));
                cur.clear();
            }
            (None, '"' | '\'') => {
                quote = Some(c);
                cur.push(c);
            }
            (Some(q), _) if c == q => {
                quote = None;
                cur.push(c);
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        items.push(scalar(cur.trim()));
    }
    items
}

/// Значение скаляра: снимает кавычки и экранирование.
fn scalar(v: &str) -> String {
    if let Some(s) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            }
        }
        out
    } else if let Some(s) = v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        s.replace("''", "'")
    } else {
        v.to_string()
    }
}

/// Отрезает комментарий ` #…` вне кавычек.
fn strip_comment(v: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut prev_space = true;
    for (i, c) in v.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') if prev_space => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, '#') if prev_space => return &v[..i],
            _ => {}
        }
        prev_space = c.is_whitespace();
    }
    v
}

fn boolean(v: &str) -> Option<bool> {
    match v {
        "true" | "True" | "TRUE" => Some(true),
        "false" | "False" | "FALSE" => Some(false),
        _ => None,
    }
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inner_and_fields() {
        let fm = parse(
            "---\r\ntags: [a, 'b, c'] # коммент\r\nl0: \"x \\\"y\\\"\"\r\n---\r\n\r\n",
            "---",
        );
        assert_eq!(
            fm.raw,
            "tags: [a, 'b, c'] # коммент\r\nl0: \"x \\\"y\\\"\"\r\n"
        );
        assert_eq!(fm.tags, ["a", "b, c"]);
        assert_eq!(fm.l0.as_deref(), Some("x \"y\""));
    }

    #[test]
    fn scalar_tag_and_hash_inside_value() {
        let fm = parse("---\ntags: rust\ndescription: C# и F#\n---\n", "---");
        assert_eq!(fm.tags, ["rust"]);
        assert_eq!(fm.description.as_deref(), Some("C# и F#"));
    }
}
