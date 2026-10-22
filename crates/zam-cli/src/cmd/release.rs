//! `zam release add <version> [--date D] [--tag T] [--notes-file F] [--from-git [FROM..TO]]`
//! и `zam release compare <version|id> [--with prev|<version>|<id>]` (§7.4).
//!
//! `--from-git` собирает коммиты `git log --format=%H%x09%s FROM..TO` в `data.commits`;
//! без диапазона — от предыдущего тега до `HEAD` (тег, указывающий на сам `HEAD`, не
//! считается предыдущим; тегов нет — вся история). Сравнение печатает changelog по видам,
//! дифф описаний и коммиты.
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

use super::log::read_text;
use super::{Ctx, emit, emit_json, post_entry, solution_guid};
use crate::client::{CliError, EXIT_CONFIG, EXIT_NOT_FOUND, read_json};

/// Предел числа коммитов релиза на сервере.
const COMMITS_MAX: usize = 5000;
/// Страница при поиске релиза по версии.
const PAGE: &str = "200";

pub struct Add<'a> {
    pub version: &'a str,
    pub date: Option<&'a str>,
    pub tag: Option<&'a str>,
    pub notes_file: Option<&'a Path>,
    /// `Some("")` — флаг без диапазона.
    pub from_git: Option<&'a str>,
    pub solution: Option<&'a str>,
    pub key: Option<&'a str>,
}

/// `git <args>` в текущем каталоге: stdout; сбой → код 6 (git не найден — код 1).
fn git(args: &[&str]) -> Result<String, CliError> {
    let o = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| CliError::new(EXIT_CONFIG, format!("не удалось запустить git: {e}")))?;
    if !o.status.success() {
        return Err(CliError::input(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// Предыдущий тег для `HEAD`: ближайший достижимый тег, не указывающий на сам `HEAD`.
fn previous_tag() -> Result<Option<String>, CliError> {
    let head = git(&["rev-parse", "HEAD"])?.trim().to_string();
    let Ok(t) = git(&["describe", "--tags", "--abbrev=0", "HEAD"]) else {
        return Ok(None);
    };
    let t = t.trim().to_string();
    let at = git(&["rev-parse", &format!("{t}^{{commit}}")])?;
    if at.trim() != head {
        return Ok(Some(t));
    }
    // HEAD сам помечен тегом: ищем выше по истории
    Ok(git(&["describe", "--tags", "--abbrev=0", "HEAD^"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty()))
}

/// Диапазон для `git log`: заданный (`FROM..TO`, `FROM` → `FROM..HEAD`) или от предыдущего
/// тега до `HEAD`.
fn git_range(spec: &str) -> Result<String, CliError> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Ok(match previous_tag()? {
            Some(t) => format!("{t}..HEAD"),
            None => "HEAD".to_string(),
        });
    }
    if spec.contains("..") {
        return Ok(spec.to_string());
    }
    Ok(format!("{spec}..HEAD"))
}

/// Строки `sha<TAB>subject` → `[{sha, subject}]`.
pub fn parse_commits(log: &str) -> Vec<Value> {
    log.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (sha, subject) = l.split_once('\t').unwrap_or((l, ""));
            json!({ "sha": sha.trim(), "subject": subject.trim() })
        })
        .collect()
}

fn commits_from_git(spec: &str) -> Result<Vec<Value>, CliError> {
    let range = git_range(spec)?;
    let out = git(&["log", "--format=%H%x09%s", &range, "--"])?;
    let mut commits = parse_commits(&out);
    if commits.len() > COMMITS_MAX {
        eprintln!(
            "коммитов {} — в релиз записаны первые {COMMITS_MAX}",
            commits.len()
        );
        commits.truncate(COMMITS_MAX);
    }
    Ok(commits)
}

pub fn add(ctx: &Ctx, o: &Add) -> Result<(), CliError> {
    let guid = solution_guid(ctx, o.solution)?;
    let mut data = json!({ "version": o.version });
    if let Some(d) = o.date {
        data["date"] = json!(d);
    }
    if let Some(t) = o.tag {
        data["git_tag"] = json!(t);
    }
    if let Some(spec) = o.from_git {
        data["commits"] = json!(commits_from_git(spec)?);
    }
    let notes = match o.notes_file {
        Some(p) => read_text(Some(p))?,
        None => String::new(),
    };
    let req = json!({
        "kind": "release",
        "title": format!("Релиз {}", o.version),
        "body": notes,
        "data": data,
    });
    post_entry(ctx, &guid, &req, o.key, None)
}

/// Id релиза по версии (точное совпадение `data.version`) или по id записи.
fn resolve_release(ctx: &Ctx, guid: &str, raw: &str) -> Result<String, CliError> {
    let raw = raw.trim();
    let mut cursor: Option<String> = None;
    loop {
        let mut q = vec![("kind", "release"), ("limit", PAGE)];
        if let Some(c) = cursor.as_deref() {
            q.push(("cursor", c));
        }
        let page = read_json(ctx.client.get(&format!("/spaces/{guid}/entries"), &q)?)?;
        let items = page["items"].as_array().cloned().unwrap_or_default();
        if let Some(e) = items.iter().find(|e| e["data"]["version"] == raw) {
            return Ok(e["id"].as_str().unwrap_or_default().to_string());
        }
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    if raw.len() == 26 && raw.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Ok(raw.to_string());
    }
    Err(CliError::new(
        EXIT_NOT_FOUND,
        format!("релиз «{raw}» не найден"),
    ))
}

pub fn compare(
    ctx: &Ctx,
    release: &str,
    with: Option<&str>,
    solution: Option<&str>,
) -> Result<(), CliError> {
    let guid = solution_guid(ctx, solution)?;
    let to = resolve_release(ctx, &guid, release)?;
    let with = match with.map(str::trim) {
        None | Some("") | Some("prev") => "prev".to_string(),
        Some(w) => resolve_release(ctx, &guid, w)?,
    };
    let v = read_json(
        ctx.client
            .get(&format!("/releases/{to}/compare"), &[("with", &with)])?,
    )?;
    if ctx.json {
        return emit_json(&v);
    }
    emit(render(&v).as_bytes())
}

fn version(e: &Value) -> &str {
    e["data"]["version"].as_str().unwrap_or("?")
}

/// Текст сравнения: заголовок, changelog по видам, дифф описаний, коммиты.
pub fn render(v: &Value) -> String {
    let mut s = String::new();
    let from = if v["from"].is_null() {
        "(начало журнала)".to_string()
    } else {
        version(&v["from"]).to_string()
    };
    s.push_str(&format!("# {} → {}\n", from, version(&v["to"])));
    if let Some(groups) = v["changelog"].as_object() {
        for (kind, items) in groups {
            s.push_str(&format!("\n## {kind}\n"));
            for e in items.as_array().into_iter().flatten() {
                s.push_str(&format!("- {}\n", e["title"].as_str().unwrap_or("?")));
            }
        }
    }
    if let Some(d) = v["notes_diff"].as_str().filter(|d| !d.is_empty()) {
        s.push_str("\n## Описание\n");
        s.push_str(d);
        if !d.ends_with('\n') {
            s.push('\n');
        }
    }
    let commits = v["commits"].as_array().cloned().unwrap_or_default();
    if !commits.is_empty() {
        s.push_str("\n## Коммиты\n");
        for c in &commits {
            let sha = c["sha"].as_str().unwrap_or("?");
            let short: String = sha.chars().take(10).collect();
            s.push_str(&format!(
                "{short} {}\n",
                c["subject"].as_str().unwrap_or("")
            ));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commits_parsed() {
        let c = parse_commits("abc\tfeat: x\n\ndef\tfix: y\tz\nghi\n");
        assert_eq!(c.len(), 3);
        assert_eq!(c[0], json!({"sha": "abc", "subject": "feat: x"}));
        assert_eq!(c[1]["subject"], "fix: y\tz");
        assert_eq!(c[2]["subject"], "");
    }

    #[test]
    fn explicit_ranges() {
        assert_eq!(git_range("v1..v2").unwrap(), "v1..v2");
        assert_eq!(git_range("v1").unwrap(), "v1..HEAD");
        assert_eq!(git_range("..v2").unwrap(), "..v2");
    }

    #[test]
    fn render_compare() {
        let v = json!({
            "from": null,
            "to": {"data": {"version": "1.0"}},
            "changelog": {"fix": [{"title": "Починка"}]},
            "notes_diff": "+a",
            "commits": [{"sha": "0123456789abcdef", "subject": "feat"}],
        });
        let r = render(&v);
        assert!(r.starts_with("# (начало журнала) → 1.0\n"));
        assert!(r.contains("## fix\n- Починка\n"));
        assert!(r.contains("+a\n"));
        assert!(r.contains("0123456789 feat\n"));
    }
}
