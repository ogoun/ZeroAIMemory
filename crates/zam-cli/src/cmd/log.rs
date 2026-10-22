//! `zam log changelog --kind K --title T [--body-file F | stdin] [--ref KIND:VALUE]…
//! [--area A] [--solution GUID]` — запись в журнал изменений решения (§7.3).
//!
//! `--ref`: `commit:SHA`, `file:PATH`, `note:ID`, `issue:X` → `data.refs`. Текст — из
//! `--body-file` или stdin (если stdin не терминал). Успех → `<id>@<rev>`.
use std::io::{IsTerminal, Read};
use std::path::Path;

use serde_json::{Map, Value, json};

use super::{Ctx, post_entry, solution_guid};
use crate::client::CliError;

pub struct Changelog<'a> {
    pub kind: &'a str,
    pub title: &'a str,
    pub body_file: Option<&'a Path>,
    pub refs: &'a [String],
    pub area: Option<&'a str>,
    pub solution: Option<&'a str>,
    pub key: Option<&'a str>,
}

/// `commit:SHA` → (`commits`, `SHA`); неизвестный вид → код 6.
pub fn parse_ref(raw: &str) -> Result<(&'static str, String), CliError> {
    let bad = || {
        CliError::input(format!(
            "--ref «{raw}»: формат commit:SHA, file:PATH, note:ID или issue:X"
        ))
    };
    let (k, v) = raw.split_once(':').ok_or_else(bad)?;
    let key = match k {
        "commit" => "commits",
        "file" => "files",
        "note" => "notes",
        "issue" => "issues",
        _ => return Err(bad()),
    };
    if v.is_empty() {
        return Err(bad());
    }
    Ok((key, v.to_string()))
}

/// Текст из файла или stdin; терминал без файла — пустой текст.
pub fn read_text(file: Option<&Path>) -> Result<String, CliError> {
    let bytes = match file {
        Some(p) => {
            std::fs::read(p).map_err(|e| CliError::input(format!("чтение {}: {e}", p.display())))?
        }
        None if std::io::stdin().is_terminal() => Vec::new(),
        None => {
            let mut b = Vec::new();
            std::io::stdin()
                .lock()
                .read_to_end(&mut b)
                .map_err(|e| CliError::input(format!("чтение stdin: {e}")))?;
            b
        }
    };
    String::from_utf8(bytes).map_err(|e| {
        CliError::input(format!(
            "текст — не UTF-8 (байт {})",
            e.utf8_error().valid_up_to()
        ))
    })
}

/// `data` записи: вид и сгруппированные ссылки.
pub fn changelog_data(kind: &str, refs: &[String]) -> Result<Value, CliError> {
    let mut data = json!({ "kind": kind });
    if !refs.is_empty() {
        let mut groups: Map<String, Value> = Map::new();
        for r in refs {
            let (k, v) = parse_ref(r)?;
            groups
                .entry(k)
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .expect("массив")
                .push(json!(v));
        }
        data["refs"] = Value::Object(groups);
    }
    Ok(data)
}

pub fn changelog(ctx: &Ctx, o: &Changelog) -> Result<(), CliError> {
    let data = changelog_data(o.kind, o.refs)?;
    let guid = solution_guid(ctx, o.solution)?;
    let body = read_text(o.body_file)?;
    let req = json!({ "kind": "changelog", "title": o.title, "body": body, "data": data });
    post_entry(ctx, &guid, &req, o.key, o.area)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_grouped() {
        let d = changelog_data(
            "feat",
            &[
                "commit:abc".into(),
                "file:src/a.rs".into(),
                "commit:def".into(),
                "issue:42".into(),
                "note:01ABC".into(),
            ],
        )
        .unwrap();
        assert_eq!(d["refs"]["commits"], json!(["abc", "def"]));
        assert_eq!(d["refs"]["files"], json!(["src/a.rs"]));
        assert_eq!(d["refs"]["issues"], json!(["42"]));
        assert_eq!(d["refs"]["notes"], json!(["01ABC"]));
        assert!(changelog_data("fix", &[]).unwrap().get("refs").is_none());
    }

    #[test]
    fn bad_refs() {
        for r in ["abc", "url:x", "commit:", ":x"] {
            assert_eq!(
                parse_ref(r).unwrap_err().code,
                crate::client::EXIT_INPUT,
                "{r}"
            );
        }
        // двоеточие внутри значения сохраняется
        assert_eq!(parse_ref("file:C:/a").unwrap(), ("files", "C:/a".into()));
    }
}
