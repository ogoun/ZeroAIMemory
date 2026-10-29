//! Вопросы к человеку — `zam question` ([`super::question`]).
//!
//! `zam log changelog --kind K --title T [--body-file F | stdin] [--ref KIND:VALUE]…
//! [--area A] [--solution GUID]` — запись в журнал изменений решения (§7.3).
//!
//! `--ref`: `commit:SHA`, `file:PATH` (или просто путь), `note:ID`, `issue:X` → `data.refs`.
//! Файлы (M4.5 §6.1) ещё и ссылками `refs.files` с `{path, hash, commit}`: путь — относительно
//! корня проекта (абсолютный внутри корня тоже), нормализованный по правилам сервера
//! ([`super::refs::ref_path`]), в `data.refs.files` — тот же путь. Текст — из `--body-file`
//! или stdin (если stdin не терминал). Успех → `<id>@<rev>`.
//!
//! `zam log term --title T --definition D [--alias A]… [--code C]… [--not-to-confuse N]…
//! [--area A] [--solution GUID] [--key K] [--body-file F | stdin]` — термин словаря
//! (M4.5 §2): имя, определение (до 300 символов), псевдонимы, идентификаторы кода, «не путать
//! с», подробности — в теле. Занятое имя или псевдоним — 409 `term-conflict`: список
//! пересечений в stderr, код 4.
use std::io::{IsTerminal, Read};
use std::path::Path;

use serde_json::{Map, Value, json};

use super::{Ctx, post_entry, solution_guid};
use crate::client::CliError;

pub struct Term<'a> {
    pub title: &'a str,
    pub definition: &'a str,
    pub aliases: &'a [String],
    pub code: &'a [String],
    pub not_to_confuse: &'a [String],
    pub body_file: Option<&'a Path>,
    pub area: Option<&'a str>,
    pub solution: Option<&'a str>,
    pub key: Option<&'a str>,
}

pub struct Changelog<'a> {
    pub kind: &'a str,
    pub title: &'a str,
    pub body_file: Option<&'a Path>,
    pub refs: &'a [String],
    pub area: Option<&'a str>,
    pub solution: Option<&'a str>,
    pub key: Option<&'a str>,
}

/// `commit:SHA` → (`commits`, `SHA`); путь без вида (`src/a.rs`, `C:\p\a.rs`) — файл;
/// неизвестный вид → код 6.
pub fn parse_ref(raw: &str) -> Result<(&'static str, String), CliError> {
    let bad = || {
        CliError::input(format!(
            "--ref «{raw}»: формат commit:SHA, file:PATH (или путь), note:ID или issue:X"
        ))
    };
    let Some((k, v)) = raw.split_once(':') else {
        return if raw.trim().is_empty() {
            Err(bad())
        } else {
            Ok(("files", raw.to_string()))
        };
    };
    // буква диска или путь с двоеточием в имени (`a/b:c`) — файл
    if k.len() == 1 && k.bytes().all(|b| b.is_ascii_alphabetic()) || k.contains(['/', '\\']) {
        return Ok(("files", raw.to_string()));
    }
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
    let mut data = changelog_data(o.kind, o.refs)?;
    let files: Vec<String> = data
        .pointer("/refs/files")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let refs = super::refs::request_refs(&files)?;
    if let Some(r) = &refs {
        // в `data.refs.files` — нормализованные пути (те же, что у ссылок)
        data["refs"]["files"] = Value::Array(
            r["files"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|f| f["path"].clone())
                .collect(),
        );
    }
    let guid = solution_guid(ctx, o.solution)?;
    let body = read_text(o.body_file)?;
    let mut req = json!({ "kind": "changelog", "title": o.title, "body": body, "data": data });
    if let Some(r) = refs {
        req["refs"] = r;
    }
    post_entry(ctx, &guid, &req, o.key, o.area)
}

/// Тело запроса термина (`POST /spaces/{guid}/entries`, `kind: term`).
pub fn term_body(o: &Term, body: &str) -> Value {
    json!({
        "kind": "term",
        "title": o.title,
        "body": body,
        "data": {
            "definition": o.definition,
            "aliases": o.aliases,
            "code": o.code,
            "not_to_confuse": o.not_to_confuse,
        },
    })
}

/// 409 `term-conflict` → пересечения построчно, код 4; прочие ошибки — как есть.
fn explain_term_conflict(e: CliError) -> CliError {
    let Some(p) = e
        .problem
        .as_ref()
        .filter(|p| p["type"] == "/problems/term-conflict")
    else {
        return e;
    };
    let mut msg = String::from("имя или псевдоним уже занят действующим термином:\n");
    for c in p["conflicts"].as_array().into_iter().flatten() {
        msg.push_str(&format!(
            "  «{}» — термин «{}» ({})\n",
            c["matched"].as_str().unwrap_or(""),
            c["title"].as_str().unwrap_or(""),
            c["id"].as_str().unwrap_or("?"),
        ));
    }
    msg.push_str(
        "что дальше: выберите другое имя или псевдоним либо замените существующий термин \
         (zam glossary <имя> покажет карточку)",
    );
    CliError {
        code: crate::client::EXIT_CONFLICT,
        msg,
        problem: e.problem,
    }
}

/// `zam log term`.
pub fn term(ctx: &Ctx, o: &Term) -> Result<(), CliError> {
    let guid = solution_guid(ctx, o.solution)?;
    let body = read_text(o.body_file)?;
    let req = term_body(o, &body);
    post_entry(ctx, &guid, &req, o.key, o.area).map_err(explain_term_conflict)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn term_request_body() {
        let aliases = vec!["summary".to_string()];
        let o = Term {
            title: "Выжимка",
            definition: "Короткая сводка записи",
            aliases: &aliases,
            code: &[],
            not_to_confuse: &[],
            body_file: None,
            area: None,
            solution: None,
            key: None,
        };
        let b = term_body(&o, "подробности");
        assert_eq!(b["kind"], "term");
        assert_eq!(b["title"], "Выжимка");
        assert_eq!(b["body"], "подробности");
        assert_eq!(b["data"]["definition"], "Короткая сводка записи");
        assert_eq!(b["data"]["aliases"], json!(["summary"]));
        assert_eq!(b["data"]["code"], json!([]));
    }

    #[test]
    fn term_conflict_explained() {
        let e = CliError {
            code: 4,
            msg: "x".into(),
            problem: Some(json!({
                "type": "/problems/term-conflict",
                "conflicts": [{"id": "i1", "title": "Выжимка", "matched": "summary"}],
            })),
        };
        let r = explain_term_conflict(e);
        assert_eq!(r.code, crate::client::EXIT_CONFLICT);
        assert!(
            r.msg.contains("«summary» — термин «Выжимка» (i1)"),
            "{}",
            r.msg
        );
        // чужая ошибка не меняется
        let other = CliError {
            code: 4,
            msg: "иное".into(),
            problem: None,
        };
        assert_eq!(explain_term_conflict(other).msg, "иное");
    }

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
        for r in ["url:x", "commit:", ":x", "", "file:"] {
            assert_eq!(
                parse_ref(r).unwrap_err().code,
                crate::client::EXIT_INPUT,
                "{r}"
            );
        }
        // двоеточие внутри значения сохраняется
        assert_eq!(parse_ref("file:C:/a").unwrap(), ("files", "C:/a".into()));
        // путь без вида — файл (в том числе с буквой диска и с `\`)
        assert_eq!(parse_ref("src/a.rs").unwrap(), ("files", "src/a.rs".into()));
        assert_eq!(
            parse_ref("C:\\p\\a.rs").unwrap(),
            ("files", "C:\\p\\a.rs".into())
        );
        assert_eq!(parse_ref("abc").unwrap(), ("files", "abc".into()));
        assert_eq!(parse_ref("dir/a:b").unwrap(), ("files", "dir/a:b".into()));
    }
}
