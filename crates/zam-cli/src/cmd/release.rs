//! `zam release add <version> [--date D] [--tag T] [--notes-file F] [--from-git [FROM..TO]]`,
//! `zam release compare <version|id> [--with prev|<version>|<id>]` (§7.4) и
//! `zam release list [--limit N | --all]` (M6.6 §9, замечание 14).
//!
//! `--from-git` собирает коммиты `git log --format=%H%x09%s FROM..TO` в `data.commits`;
//! без диапазона — от предыдущего тега до `HEAD` (тег, указывающий на сам `HEAD`, не
//! считается предыдущим; тегов нет — вся история). Сравнение печатает changelog по видам,
//! дифф описаний и коммиты. Лента релизов (`list`, поиск по версии) — общий обход
//! [`list_entries`].
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

use super::log::read_text;
use super::{Ctx, Paging, emit, emit_json, list_entries, more_hint, post_entry, solution_guid};
use crate::client::{CliError, EXIT_CONFIG, EXIT_NOT_FOUND, read_json};

/// Предел числа коммитов релиза на сервере.
const COMMITS_MAX: usize = 5000;
/// Фильтр ленты записей — релизы.
const RELEASES: [(&str, &str); 1] = [("kind", "release")];
/// Релизов в `zam release list` без `--limit` и `--all` — как страница ленты сервера по
/// умолчанию (`LIST_DEFAULT` в `zam-server`, `api/entries.rs`).
const LIST_DEFAULT: usize = 50;

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
    if spec.starts_with('-') {
        return Err(CliError::input(
            "--from-git: диапазон не может начинаться с «-»",
        ));
    }
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

/// Id релиза по версии (точное совпадение `data.version`; лента целиком — [`list_entries`] с
/// `all`) или по id записи.
fn resolve_release(ctx: &Ctx, guid: &str, raw: &str) -> Result<String, CliError> {
    let raw = raw.trim();
    let v = list_entries(ctx, guid, &RELEASES, None, true)?;
    if let Some(e) = v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|e| e["data"]["version"] == raw)
    {
        return Ok(e["id"].as_str().unwrap_or_default().to_string());
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

/// `zam release list`: релизы решения, новые первыми — по дате релиза (`data.date`), при равной
/// — по времени создания. Сервер режет страницы ленты по времени создания, поэтому лента
/// читается целиком ([`list_entries`] с `all`; релизов единицы–десятки), сортируется и только
/// потом режется: `--limit` (по умолчанию [`LIST_DEFAULT`]) — первые новейшие, отрезано —
/// строка «есть ещё», как у лент журналов; `--all` — все. `--json` — записи сервера в том же
/// порядке, `next_cursor: null` (ревью задачи 9 M6.6, Important 1).
pub fn list(ctx: &Ctx, solution: Option<&str>, page: Paging) -> Result<(), CliError> {
    let guid = solution_guid(ctx, solution)?;
    let mut v = list_entries(ctx, &guid, &RELEASES, None, true)?;
    if let Some(items) = v["items"].as_array_mut() {
        let limit = (!page.all).then(|| page.limit.map_or(LIST_DEFAULT, |l| l as usize));
        if newest_first(items, limit) {
            eprintln!("{}", more_hint(items.len()));
        }
    }
    if ctx.json {
        return emit_json(&v);
    }
    let out: String = v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| format!("{}\n", list_line(e)))
        .collect();
    emit(out.as_bytes())
}

/// Релизы ленты сервера (новые созданные первыми) — новые по дате релиза первыми (сортировка
/// устойчивая: при равной дате — порядок создания) и первые `limit`; `true` — отрезано.
fn newest_first(items: &mut Vec<Value>, limit: Option<usize>) -> bool {
    items.sort_by(|a, b| b["data"]["date"].as_str().cmp(&a["data"]["date"].as_str()));
    match limit {
        Some(n) if items.len() > n => {
            items.truncate(n);
            true
        }
        _ => false,
    }
}

/// Строка ленты релизов: `<id> <версия> <дата>[ тег <tag>][ · коммитов K]`.
fn list_line(e: &Value) -> String {
    let d = &e["data"];
    let mut s = format!(
        "{} {} {}",
        e["id"].as_str().unwrap_or("?"),
        version(e),
        d["date"].as_str().unwrap_or("?")
    );
    if let Some(t) = d["git_tag"].as_str() {
        s.push_str(&format!(" тег {t}"));
    }
    if let Some(n) = d["commits"].as_array().map(Vec::len).filter(|&n| n > 0) {
        s.push_str(&format!(" · коммитов {n}"));
    }
    s
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
        assert!(git_range("--output=x").is_err());
    }

    #[test]
    fn list_line_forms() {
        let e = json!({"id": "01A", "data": {"version": "1.2", "date": "2026-03-01",
                       "git_tag": "v1.2", "commits": [{"sha": "a"}, {"sha": "b"}]}});
        assert_eq!(list_line(&e), "01A 1.2 2026-03-01 тег v1.2 · коммитов 2");
        let e =
            json!({"id": "01B", "data": {"version": "0.9", "date": "2025-12-01", "commits": []}});
        assert_eq!(list_line(&e), "01B 0.9 2025-12-01");
    }

    /// Ревью задачи 9 M6.6, Important 1: порядок — по дате релиза поверх порядка создания, а
    /// `limit` режет уже отсортированную ленту (новейшие по дате, а не последние созданные).
    #[test]
    fn newest_first_sorts_then_limits() {
        // лента сервера — новые созданные первыми: 0.9.0 заведён после 1.1.0
        let feed = || -> Vec<Value> {
            [
                ("1.2.1", "2026-03-01"),
                ("1.2.0", "2026-03-01"),
                ("0.9.0", "2025-12-01"),
                ("1.1.0", "2026-02-01"),
            ]
            .iter()
            .map(|(v, d)| json!({"data": {"version": v, "date": d}}))
            .collect()
        };
        let versions = |items: &[Value]| -> Vec<String> {
            items.iter().map(|e| version(e).to_string()).collect()
        };
        let mut all = feed();
        assert!(!newest_first(&mut all, None));
        assert_eq!(versions(&all), ["1.2.1", "1.2.0", "1.1.0", "0.9.0"]);
        let mut three = feed();
        assert!(newest_first(&mut three, Some(3)));
        assert_eq!(versions(&three), ["1.2.1", "1.2.0", "1.1.0"]);
        let mut whole = feed();
        assert!(!newest_first(&mut whole, Some(4)));
        assert_eq!(whole.len(), 4);
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
