//! Разбор полей frontmatter.
//!
//! Нужны только плоские ключи верхнего уровня (`tags`, `description`, `l0`,
//! `index`, `certainty`, `evidence`), поэтому вместо YAML-библиотеки — узкое подмножество YAML:
//! скаляры (простые, `"…"`, `'…'`, в том числе многострочные), блочные
//! скаляры `|` и `>` с модификаторами `-`/`+` и цифрой отступа, `~`/`null`,
//! булевы значения, списки `[a, b]` и блочные списки `- a`.
//! Остальные ключи сохраняются только в `raw`.

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
        certainty: None,
        evidence: None,
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
                    plain_or_null(value).into_iter().collect()
                };
                fm.tags.retain(|t| !t.is_empty());
            }
            "description" => fm.description = string_value(value, &lines, &mut i),
            "l0" => fm.l0 = string_value(value, &lines, &mut i),
            "index" => fm.index = boolean(value),
            "certainty" => {
                fm.certainty = string_value(value, &lines, &mut i).map(|c| c.trim().to_string())
            }
            "evidence" => fm.evidence = string_value(value, &lines, &mut i),
            _ => {}
        }
    }
    fm
}

/// Ключи `certainty`/`evidence` верхнего уровня в нераспознаваемом написании (в кавычках,
/// с заглавными буквами и т. п.): такой ключ не задаёт статус, и
/// автор, скорее всего, ошибся. Возвращает канонические имена найденных ключей без повторов.
pub fn unrecognized_certainty_keys(raw: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    for line in raw.lines() {
        if line.starts_with([' ', '\t', '#', '-']) {
            continue;
        }
        let Some((key, _)) = line.split_once(':') else {
            continue;
        };
        let norm = key.trim().trim_matches(['"', '\'']).trim().to_lowercase();
        for want in ["certainty", "evidence"] {
            if norm == want && key.trim() != want && !out.contains(&want) {
                out.push(want);
            }
        }
    }
    out
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
            items.extend(plain_or_null(strip_comment(item).trim()));
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
                items.extend(plain_or_null(cur.trim()));
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
        items.extend(plain_or_null(cur.trim()));
    }
    items
}

/// Строковое значение ключа. `value` — остаток строки после `:` без комментария;
/// `i` — индекс следующей строки, сдвигается за строки продолжения.
/// Пустое значение, `~`/`null` и блок с неизвестным заголовком — `None`.
fn string_value(value: &str, lines: &[&str], i: &mut usize) -> Option<String> {
    let cont = take_indented(lines, i);
    let text = if let Some(header) = value.strip_prefix('|') {
        block_scalar(header, false, cont)?
    } else if let Some(header) = value.strip_prefix('>') {
        block_scalar(header, true, cont)?
    } else if value.starts_with(['"', '\'']) {
        // кавычки могут закрываться на одной из следующих строк
        let mut parts = vec![value];
        for line in cont {
            if is_closed_quote(&fold(&parts)) {
                break;
            }
            parts.push(line.trim());
        }
        scalar(&fold(&parts))
    } else {
        // простой скаляр: строки продолжения сворачиваются, комментарий его завершает
        let mut parts = vec![value];
        for line in cont {
            let t = strip_comment(line);
            parts.push(t.trim());
            if t.len() != line.len() {
                break;
            }
        }
        let text = fold(&parts);
        if is_null(&text) {
            return None;
        }
        text
    };
    non_empty(text)
}

/// Строки после `*i`, принадлежащие значению ключа: пустые и с отступом.
/// Хвостовые пустые строки тоже входят — они нужны для `|+`.
fn take_indented<'a>(lines: &'a [&'a str], i: &mut usize) -> &'a [&'a str] {
    let from = (*i).min(lines.len());
    let n = lines[from..]
        .iter()
        .take_while(|l| l.trim().is_empty() || l.starts_with([' ', '\t']))
        .count();
    *i = from + n;
    &lines[from..from + n]
}

/// Свёртка многострочного скаляра: соседние строки — через пробел,
/// `n` пустых строк — `n` переводов строки.
fn fold(parts: &[&str]) -> String {
    let mut out = String::new();
    let mut breaks = 0;
    for p in parts {
        if p.is_empty() {
            breaks += 1;
            continue;
        }
        if !out.is_empty() {
            if breaks > 0 {
                out.push_str(&"\n".repeat(breaks));
            } else {
                out.push(' ');
            }
        }
        breaks = 0;
        out.push_str(p);
    }
    out
}

/// Блочный скаляр (`|` — буквальный, `>` — свёрнутый). `header` — индикаторы
/// после `|`/`>`: модификатор `-`/`+` и цифра отступа в любом порядке;
/// неизвестный заголовок — `None`.
fn block_scalar(header: &str, folded: bool, cont: &[&str]) -> Option<String> {
    let mut chomp = None;
    let mut indent = None;
    for c in strip_comment(header).trim().chars() {
        match c {
            '-' | '+' if chomp.is_none() => chomp = Some(c),
            '1'..='9' if indent.is_none() => indent = c.to_digit(10).map(|d| d as usize),
            _ => return None,
        }
    }
    let spaces = |l: &str| l.len() - l.trim_start_matches(' ').len();
    let Some(indent) = indent.or_else(|| {
        cont.iter()
            .find(|l| !l.trim().is_empty())
            .map(|l| spaces(l))
    }) else {
        return Some(String::new());
    };
    let mut body: Vec<&str> = Vec::new();
    for line in cont {
        if line.trim().is_empty() {
            body.push("");
        } else if spaces(line) >= indent {
            body.push(&line[indent..]);
        } else {
            break;
        }
    }
    let trailing = body.iter().rev().take_while(|l| l.is_empty()).count();
    let content = &body[..body.len() - trailing];
    let mut text = if folded {
        fold_block(content)
    } else {
        content.join("\n")
    };
    match chomp {
        Some('-') => {}
        Some(_) => {
            let n = trailing + usize::from(!content.is_empty());
            text.push_str(&"\n".repeat(n));
        }
        None if !content.is_empty() => text.push('\n'),
        None => {}
    }
    Some(text)
}

/// Свёртка строк блока `>`: перевод между обычными строками становится пробелом,
/// рядом со строками с дополнительным отступом — сохраняется.
fn fold_block(lines: &[&str]) -> String {
    let more = |l: &str| l.starts_with([' ', '\t']);
    let mut out = String::new();
    let mut prev: Option<&str> = None;
    let mut breaks = 0;
    for &l in lines {
        if l.is_empty() {
            breaks += 1;
            continue;
        }
        match prev {
            Some(p) if more(p) || more(l) => out.push_str(&"\n".repeat(breaks + 1)),
            Some(_) if breaks > 0 => out.push_str(&"\n".repeat(breaks)),
            Some(_) => out.push(' '),
            None => out.push_str(&"\n".repeat(breaks)),
        }
        breaks = 0;
        out.push_str(l);
        prev = Some(l);
    }
    out
}

/// Строка целиком — закрытый скаляр в кавычках.
fn is_closed_quote(v: &str) -> bool {
    let Some(rest) = v.get(1..) else {
        return false;
    };
    if v.starts_with('\'') {
        // `''` внутри — экранированная кавычка: закрыта при нечётном хвосте из `'`
        let tail = rest.len() - rest.trim_end_matches('\'').len();
        tail % 2 == 1
    } else if v.starts_with('"') {
        rest.strip_suffix('"').is_some_and(|inner| {
            let slashes = inner.len() - inner.trim_end_matches('\\').len();
            slashes % 2 == 0
        })
    } else {
        false
    }
}

fn is_null(v: &str) -> bool {
    matches!(v, "" | "~" | "null" | "Null" | "NULL")
}

/// Простой скаляр (кавычки снимаются) или `None` для пустого значения и `~`/`null`.
fn plain_or_null(v: &str) -> Option<String> {
    (!is_null(v)).then(|| scalar(v))
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

    fn fm(yaml: &str) -> Frontmatter {
        parse(&format!("---\n{yaml}---\n"), "---")
    }

    #[test]
    fn literal_block_scalar_with_chomping() {
        let y = "l0: |\n  строка один\n    отступ\n\n  строка два\n\n\nindex: true\n";
        assert_eq!(
            fm(y).l0.as_deref(),
            Some("строка один\n  отступ\n\nстрока два\n")
        );
        assert_eq!(fm(y).index, Some(true));
        let y = "l0: |-\n  a\n  b\n\n";
        assert_eq!(fm(y).l0.as_deref(), Some("a\nb"));
        let y = "l0: |+\n  a\n\n\ndescription: x\n";
        assert_eq!(fm(y).l0.as_deref(), Some("a\n\n\n"));
        assert_eq!(fm(y).description.as_deref(), Some("x"));
        // явный индикатор отступа и комментарий в заголовке
        let y = "l0: |2- # коммент\n    a\n   b\n";
        assert_eq!(fm(y).l0.as_deref(), Some("  a\n b"));
    }

    #[test]
    fn folded_block_scalar() {
        let y = "description: >-\n  первая\n  вторая\n\n  третья\n    код\n  конец\n";
        assert_eq!(
            fm(y).description.as_deref(),
            Some("первая вторая\nтретья\n  код\nконец")
        );
        let y = "description: >\n  a\n  b\n";
        assert_eq!(fm(y).description.as_deref(), Some("a b\n"));
    }

    #[test]
    fn empty_or_unknown_block_header_is_none() {
        assert_eq!(fm("l0: |\nindex: true\n").l0, None);
        let f = fm("l0: |x\n  текст\ndescription: >!\n  y\nindex: false\n");
        assert_eq!(f.l0, None);
        assert_eq!(f.description, None);
        assert_eq!(f.index, Some(false));
    }

    #[test]
    fn multiline_plain_and_quoted_scalars() {
        let y = "description: длинное\n  продолжение\n\n  абзац # коммент\nl0:\n  на следующей\n  строке\n";
        let f = fm(y);
        assert_eq!(f.description.as_deref(), Some("длинное продолжение\nабзац"));
        assert_eq!(f.l0.as_deref(), Some("на следующей строке"));
        let f = fm("description: \"раз\n  два\"\nl0: 'it''s\n  ok'\n");
        assert_eq!(f.description.as_deref(), Some("раз два"));
        assert_eq!(f.l0.as_deref(), Some("it's ok"));
    }

    #[test]
    fn frontmatter_certainty_and_evidence() {
        let f = fm("certainty: theorem
evidence: |
  проверено на CI
  метод: прогон
");
        assert_eq!(f.certainty.as_deref(), Some("theorem"));
        assert_eq!(
            f.evidence.as_deref(),
            Some(
                "проверено на CI
метод: прогон
"
            )
        );
        let f = fm("certainty: 'axiom' # человек
evidence: \"тест\"
");
        assert_eq!(f.certainty.as_deref(), Some("axiom"));
        assert_eq!(f.evidence.as_deref(), Some("тест"));
        // без поля и с null — статуса нет
        let f = fm("tags: [a]
");
        assert_eq!((f.certainty, f.evidence), (None, None));
        let f = fm("certainty: ~
evidence:
");
        assert_eq!((f.certainty, f.evidence), (None, None));
    }

    #[test]
    fn misspelled_certainty_keys_detected() {
        assert!(unrecognized_certainty_keys("certainty: axiom\nevidence: x\n").is_empty());
        assert_eq!(
            unrecognized_certainty_keys("\"certainty\": axiom\nEvidence: x\ntags: [a]\n"),
            vec!["certainty", "evidence"]
        );
        // пробел перед двоеточием разбор принимает
        assert!(unrecognized_certainty_keys("certainty : theorem\n").is_empty());
        // вложенные ключи и списки не считаются
        assert!(unrecognized_certainty_keys("meta:\n  Certainty: x\n- certainty\n").is_empty());
        // распознанный ключ действительно задаёт поле, нераспознанный — нет
        assert_eq!(fm("'certainty': axiom\n").certainty, None);
    }

    #[test]
    fn null_values_are_absent() {
        let f = fm("tags: ~\ndescription: null\nl0: Null\nindex: ~\n");
        assert!(f.tags.is_empty());
        assert_eq!(f.description, None);
        assert_eq!(f.l0, None);
        assert_eq!(f.index, None);
        assert!(fm("tags: [a, ~, null, b]\n").tags == ["a", "b"]);
        assert!(fm("tags:\n  - ~\n  - x\n").tags == ["x"]);
        assert_eq!(fm("description: '~'\n").description.as_deref(), Some("~"));
    }
}
