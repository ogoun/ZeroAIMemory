//! Кэш выжимок (§9.3 шаг 3, §9.4, §13.2):
//!
//! - `zam cache put --query Q --sources ID[#СЕКЦИЯ][@REV],… [--promote note:<путь>|insight:<вид>]
//!   [--solution GUID] [--file F] < summary.md` — выжимка из `--file` или stdin; источники через
//!   запятую (`n:`/`i:`/`c:`/`r:`/голый id или путь `<пространство>:/<путь>`, секция после `#`,
//!   ревизия после `@`). Решение — `--solution` или `solution.toml` (заголовок
//!   `X-Zam-Solution`), без него — Knowledge. Печатает `c_<id>` (с `--promote` — ещё ссылку
//!   на созданную запись через табуляцию); `--json` — выжимку;
//! - `zam cache list [--q Q] [--state valid|stale] [--solution GUID] [--limit N]` — строки
//!   `c_<id>\t<valid|stale>\t<дата>\t<запрос>`;
//! - `zam cache show <id>` — запрос, состояние, источники (изменившиеся — с причиной) и текст;
//! - `zam cache rm <id>` — мягкое удаление (автор, владелец пространства или администратор).
use std::path::Path;

use serde_json::{Value, json};

use super::log::read_text;
use super::{Ctx, emit, emit_json};
use crate::client::{CliError, read_json};

/// Источник `ID[#СЕКЦИЯ][@REV]` → `{id, section?, rev?}`.
fn parse_source(raw: &str) -> Result<Value, CliError> {
    let raw = raw.trim();
    let (rest, rev) = match raw.rsplit_once('@') {
        Some((r, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            let n: i64 = n
                .parse()
                .map_err(|_| CliError::input(format!("источник «{raw}»: ревизия")))?;
            (r, Some(n))
        }
        _ => (raw, None),
    };
    let (id, section) = match rest.split_once('#') {
        Some((i, s)) if !s.trim().is_empty() => (i.trim(), Some(s.trim())),
        _ => (rest.trim(), None),
    };
    if id.is_empty() {
        return Err(CliError::input(format!("источник «{raw}»: пустой id")));
    }
    let mut v = json!({ "id": id });
    if let Some(r) = rev {
        v["rev"] = json!(r);
    }
    if let Some(s) = section {
        v["section"] = json!(s);
    }
    Ok(v)
}

/// `--promote note:<путь>|insight:<вид>` → `{as, path?|kind?}`.
fn parse_promote(raw: &str) -> Result<Value, CliError> {
    let (to, arg) = raw.split_once(':').unwrap_or((raw, ""));
    let arg = arg.trim();
    match to.trim() {
        "note" if arg.is_empty() => Ok(json!({"as": "note"})),
        "note" => Ok(json!({"as": "note", "path": arg})),
        "insight" if arg.is_empty() => Ok(json!({"as": "insight"})),
        "insight" => Ok(json!({"as": "insight", "kind": arg})),
        _ => Err(CliError::input("--promote: note:<путь> или insight:<вид>")),
    }
}

/// Параметры `zam cache put`.
pub struct PutOpts<'a> {
    pub query: &'a str,
    pub sources: &'a [String],
    pub promote: Option<&'a str>,
    pub solution: Option<&'a str>,
    pub file: Option<&'a Path>,
}

/// `zam cache put`.
pub fn put(ctx: &Ctx, o: &PutOpts) -> Result<(), CliError> {
    let summary = read_text(o.file)?;
    if summary.trim().is_empty() {
        return Err(CliError::input("выжимка пуста: --file или stdin"));
    }
    let sources: Vec<Value> = o
        .sources
        .iter()
        .flat_map(|s| s.split(','))
        .filter(|s| !s.trim().is_empty())
        .map(parse_source)
        .collect::<Result<_, _>>()?;
    if sources.is_empty() {
        return Err(CliError::input("--sources: хотя бы один источник"));
    }
    let mut body = json!({"query": o.query, "summary": summary, "sources": sources});
    if let Some(p) = o.promote {
        body["promote"] = parse_promote(p)?;
    }
    if let Some(g) = o.solution {
        body["solution"] = json!(super::solution_guid(ctx, Some(g))?);
    }
    let v = read_json(ctx.client.post_json("/cache", &body)?)?;
    if ctx.json {
        return emit_json(&v);
    }
    let mut line = v["ref"].as_str().unwrap_or("?").to_string();
    if let Some(p) = v["promoted"].as_str() {
        line.push('\t');
        line.push_str(p);
    }
    line.push('\n');
    emit(line.as_bytes())
}

/// Параметры `zam cache list`.
pub struct ListOpts<'a> {
    pub q: Option<&'a str>,
    pub state: Option<&'a str>,
    pub solution: Option<&'a str>,
    pub limit: Option<u32>,
}

/// `zam cache list`.
pub fn list(ctx: &Ctx, o: &ListOpts) -> Result<(), CliError> {
    let limit = o.limit.map(|l| l.to_string());
    let solution = o
        .solution
        .map(|g| super::solution_guid(ctx, Some(g)))
        .transpose()?;
    let mut q: Vec<(&str, &str)> = Vec::new();
    for (k, v) in [
        ("q", o.q),
        ("state", o.state),
        ("solution", solution.as_deref()),
        ("limit", limit.as_deref()),
    ] {
        if let Some(v) = v {
            q.push((k, v));
        }
    }
    let v = read_json(ctx.client.get("/cache", &q)?)?;
    if ctx.json {
        return emit_json(&v);
    }
    let s: String = v["items"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|c| {
            format!(
                "{}\t{}\t{}\t{}\n",
                c["ref"].as_str().unwrap_or("?"),
                c["state"].as_str().unwrap_or("?"),
                c["created_at"]
                    .as_str()
                    .unwrap_or("?")
                    .get(..10)
                    .unwrap_or("?"),
                one_line(c["query"].as_str().unwrap_or("")),
            )
        })
        .collect();
    emit(s.as_bytes())
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `c_<id>` или голый id → путь API.
fn cache_path(id: &str) -> String {
    format!("/cache/{}", id.trim())
}

/// `zam cache show <id>`.
pub fn show(ctx: &Ctx, id: &str) -> Result<(), CliError> {
    let v = read_json(ctx.client.get(&cache_path(id), &[])?)?;
    if ctx.json {
        return emit_json(&v);
    }
    let mut s = format!(
        "{} «{}» — {}, {}",
        v["ref"].as_str().unwrap_or("?"),
        one_line(v["query"].as_str().unwrap_or("")),
        v["state"].as_str().unwrap_or("?"),
        v["created_at"].as_str().unwrap_or("?"),
    );
    if let Some(a) = v["author"].as_str() {
        s.push_str(&format!(" · автор {a}"));
    }
    if let Some(m) = v["machine"].as_str() {
        s.push_str(&format!(", машина {m}"));
    }
    if let Some(p) = v["promoted_to"].as_str() {
        s.push_str(&format!(" · превращена в {p}"));
    }
    s.push('\n');
    let stale = v["stale"].as_array().cloned().unwrap_or_default();
    for d in v["sources"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let target = d["id"].as_str().unwrap_or("?");
        let mut line = format!("  {target}");
        if let Some(sec) = d["section"].as_str() {
            line.push_str(&format!(" › {sec}"));
        }
        line.push_str(&format!(" rev{}", d["rev"]));
        let why: Vec<&str> = stale
            .iter()
            .filter(|x| x["target"] == d["id"] && x["section"] == d["section"])
            .filter_map(|x| x["reason"].as_str())
            .collect();
        if !why.is_empty() {
            line.push_str(&format!(" ({})", why.join(", ")));
        }
        s.push_str(&line);
        s.push('\n');
    }
    for x in stale.iter().filter(|x| x["reason"] == "new_sources") {
        s.push_str(&format!(
            "  + {} (new_sources)\n",
            x["target"].as_str().unwrap_or("?")
        ));
    }
    s.push('\n');
    s.push_str(v["summary"].as_str().unwrap_or(""));
    if !s.ends_with('\n') {
        s.push('\n');
    }
    emit(s.as_bytes())
}

/// `zam cache rm <id>`.
pub fn remove(ctx: &Ctx, id: &str) -> Result<(), CliError> {
    ctx.client.delete(&cache_path(id))?;
    if ctx.json {
        return emit_json(&json!({ "deleted": id.trim() }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_forms() {
        assert_eq!(parse_source("n:ABC").unwrap(), json!({"id": "n:ABC"}));
        assert_eq!(
            parse_source(" n:ABC@3 ").unwrap(),
            json!({"id": "n:ABC", "rev": 3})
        );
        assert_eq!(
            parse_source("knowledge:/Инструменты/Bash#Bash > Windows@2").unwrap(),
            json!({"id": "knowledge:/Инструменты/Bash", "section": "Bash > Windows", "rev": 2})
        );
        assert_eq!(
            parse_source("user@host").unwrap(),
            json!({"id": "user@host"})
        );
        assert!(parse_source("@3").is_err());
    }

    #[test]
    fn promote_forms() {
        assert_eq!(
            parse_promote("note:Выжимки/Деплой").unwrap(),
            json!({"as": "note", "path": "Выжимки/Деплой"})
        );
        assert_eq!(
            parse_promote("insight:gotcha").unwrap(),
            json!({"as": "insight", "kind": "gotcha"})
        );
        assert_eq!(parse_promote("note").unwrap(), json!({"as": "note"}));
        assert!(parse_promote("folder:x").is_err());
    }
}
