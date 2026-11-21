//! `zam task` — журнал задач (M4.5 §3): `list`, `show`, `new`, `status`, `claim`, `release`.
//!
//! - `list [--status S,…] [--global] [--solution GUID] [--limit N | --all]` — строки `<id>
//!   [статус] «заголовок» · далее: …`; без `--status` — `open, in_progress, blocked`. У `done` —
//!   длительность. Страница — 200, обход и подсказка о продолжении — [`super::list_entries`];
//! - `show <k:id>` — карточка: статус, шаги, захват, времена, план (тело);
//! - `new --title T [--next S] [--owner agent|human] [--estimate МИН] [--link L]… [--priority
//!   low|normal|high] [--body-file F | stdin]` — новая задача (`<id>@<rev>`); поля — в `data`
//!   (`owner`, `estimate_min`, `links`, `priority`; M6.6 §9), статус при создании не
//!   передаётся (сервер — `open`);
//! - `status <k:id> <статус> [--next S] [--blocked-by S] [--reason R]` — ревизию берёт из
//!   `GET` и шлёт `If-Match`; переоткрытие `done`/`dropped` → `open` — с `--reason`;
//! - `claim <k:id> [--ttl 120]` / `release <k:id>` — захват сессией агента (`X-Zam-Session`:
//!   `ZAM_SESSION` или сессия хуков проекта). Задача занята — код 4 и сведения о захвате.
//!
//! Ссылка — `k:<id>`, `e:<id>` или голый id.
use std::path::Path;

use serde_json::{Value, json};

use super::log::read_text;
use super::{
    Ctx, LIST_LIMIT_MAX, Paging, agent_time, emit, emit_json, list_entries, post_entry,
    solution_guid,
};
use crate::client::{CliError, EXIT_CONFLICT, read_json};
// `k:<id>` / `e:<id>` / `t:<id>` … → голый id (буквы — общий источник `target`)
use crate::target::bare_entry_id as bare_id;

/// Статусы ленты по умолчанию.
const ACTIVE: &str = "open,in_progress,blocked";

/// Строка ленты: `<id> [статус] «заголовок»`, далее — шаг, блокер, длительность.
fn line(e: &Value) -> String {
    let mut s = format!(
        "{} [{}] «{}»",
        e["id"].as_str().unwrap_or("?"),
        e["status"].as_str().unwrap_or("?"),
        e["title"].as_str().unwrap_or("")
    );
    let d = &e["data"];
    if let Some(n) = d["next_step"].as_str() {
        s.push_str(&format!(" · далее: {n}"));
    }
    if e["status"] == "blocked"
        && let Some(b) = d["blocked_by"].as_str()
    {
        s.push_str(&format!(" · блокер: {b}"));
    }
    if let Some(m) = e["duration_min"].as_i64() {
        s.push_str(&format!(" · {m} мин"));
    }
    s
}

/// Карточка задачи.
fn card(e: &Value) -> String {
    let d = &e["data"];
    let mut out = format!("{}  rev{}\n", line(e), e["rev"]);
    if let Some(c) = d["claimed_by"].as_object() {
        out.push_str(&format!(
            "захвачена: сессия {} до {}{}\n",
            c.get("session").and_then(Value::as_str).unwrap_or("?"),
            agent_time(c.get("until").and_then(Value::as_str).unwrap_or("?")),
            c.get("agent")
                .and_then(Value::as_str)
                .map(|a| format!(" ({a})"))
                .unwrap_or_default()
        ));
    }
    for (k, label) in [
        ("started_at", "начата"),
        ("done_at", "завершена"),
        ("owner", "ведёт"),
    ] {
        if let Some(x) = d[k].as_str() {
            // метки времени — в тексте агенту `yyyy-MM-dd HH:mm:ss` UTC, `owner` — как есть
            out.push_str(&format!("{label}: {}\n", agent_time(x)));
        }
    }
    if let Some(n) = d["estimate_min"].as_i64() {
        out.push_str(&format!("оценка: {n} мин\n"));
    }
    for l in d["links"].as_array().into_iter().flatten() {
        out.push_str(&format!("ссылка: {}\n", l.as_str().unwrap_or("")));
    }
    let body = e["body"].as_str().unwrap_or("").trim();
    if !body.is_empty() {
        out.push('\n');
        out.push_str(body);
        out.push('\n');
    }
    out
}

fn get_task(ctx: &Ctx, id: &str) -> Result<Value, CliError> {
    read_json(ctx.client.get(&format!("/entries/{id}"), &[])?)
}

/// 409 `task-claimed` → «задача захвачена сессией …», код 4.
fn explain_claimed(e: CliError) -> CliError {
    let Some(c) = e
        .problem
        .as_ref()
        .filter(|p| p["type"] == "/problems/task-claimed")
        .map(|p| &p["claimed_by"])
    else {
        return e;
    };
    CliError::with_problem(
        EXIT_CONFLICT,
        format!(
            "задача уже захвачена сессией {} до {}",
            c["session"].as_str().unwrap_or("?"),
            agent_time(c["until"].as_str().unwrap_or("?"))
        ),
        e.problem,
    )
}

/// `zam task list`; без `--limit` страница прежняя — 200.
pub fn list(
    ctx: &Ctx,
    status: Option<&str>,
    global: bool,
    solution: Option<&str>,
    page: Paging,
) -> Result<(), CliError> {
    let space = if global {
        "knowledge".to_string()
    } else {
        solution_guid(ctx, solution)?
    };
    let st = status.unwrap_or(ACTIVE);
    let v = list_entries(
        ctx,
        &space,
        &[("kind", "task"), ("status", st)],
        page.limit.or(Some(LIST_LIMIT_MAX)),
        page.all,
    )?;
    if ctx.json {
        return emit_json(&v);
    }
    let mut out = String::new();
    for e in v["items"].as_array().into_iter().flatten() {
        out.push_str(&line(e));
        out.push('\n');
    }
    emit(out.as_bytes())
}

/// `zam task show`.
pub fn show(ctx: &Ctx, id: &str) -> Result<(), CliError> {
    let e = get_task(ctx, bare_id(id))?;
    if ctx.json {
        return emit_json(&e);
    }
    emit(card(&e).as_bytes())
}

/// Параметры `zam task new`.
pub struct New<'a> {
    pub title: &'a str,
    pub next: Option<&'a str>,
    /// `agent | human` (проверил clap).
    pub owner: Option<&'a str>,
    /// Оценка, минут (1–525600, проверил clap).
    pub estimate: Option<u32>,
    pub links: &'a [String],
    /// `low | normal | high` (проверил clap).
    pub priority: Option<&'a str>,
    pub body_file: Option<&'a Path>,
    pub area: Option<&'a str>,
    pub global: bool,
    pub solution: Option<&'a str>,
    pub key: Option<&'a str>,
}

/// `data` новой задачи: только заданные поля клиента (`next_step`, `owner`, `estimate_min`,
/// `links`, `priority`); `status` не передаётся — при создании сервер его не принимает (422).
fn new_data(o: &New) -> Value {
    let mut data = json!({});
    for (k, v) in [
        ("next_step", o.next),
        ("owner", o.owner),
        ("priority", o.priority),
    ] {
        if let Some(v) = v {
            data[k] = json!(v);
        }
    }
    if let Some(m) = o.estimate {
        data["estimate_min"] = json!(m);
    }
    if !o.links.is_empty() {
        data["links"] = json!(o.links);
    }
    data
}

/// `zam task new`.
pub fn new(ctx: &Ctx, o: &New) -> Result<(), CliError> {
    let space = if o.global {
        "knowledge".to_string()
    } else {
        solution_guid(ctx, o.solution)?
    };
    let body = read_text(o.body_file)?;
    let req = json!({ "kind": "task", "title": o.title, "body": body, "data": new_data(o) });
    post_entry(ctx, &space, &req, o.key, o.area)
}

/// Тело `PATCH` смены статуса.
pub fn status_body(
    status: &str,
    next: Option<&str>,
    blocked_by: Option<&str>,
    reason: Option<&str>,
) -> Value {
    let mut b = json!({ "status": status });
    for (k, v) in [
        ("next_step", next),
        ("blocked_by", blocked_by),
        ("reason", reason),
    ] {
        if let Some(v) = v {
            b[k] = json!(v);
        }
    }
    b
}

/// `zam task status`.
pub fn status(
    ctx: &Ctx,
    id: &str,
    status: &str,
    next: Option<&str>,
    blocked_by: Option<&str>,
    reason: Option<&str>,
) -> Result<(), CliError> {
    let id = bare_id(id);
    let rev = get_task(ctx, id)?["rev"]
        .as_i64()
        .ok_or_else(|| CliError::new(crate::client::EXIT_UNREACHABLE, "в ответе нет rev записи"))?;
    let r = ctx
        .client
        .patch_json_if(
            &format!("/entries/{id}"),
            &status_body(status, next, blocked_by, reason),
            rev,
        )
        .map_err(|e| {
            if e.problem.as_ref().is_some_and(|p| p["status"] == 409) {
                CliError::with_problem(
                    EXIT_CONFLICT,
                    format!("запись изменилась (rev {rev}): перечитайте и повторите"),
                    e.problem,
                )
            } else {
                e
            }
        })?;
    let e = read_json(r)?;
    ctx.note_write("task");
    if ctx.json {
        return emit_json(&e);
    }
    emit(format!("{}\n", line(&e)).as_bytes())
}

/// `zam task claim`.
pub fn claim(ctx: &Ctx, id: &str, ttl: u32) -> Result<(), CliError> {
    let id = bare_id(id);
    let r = ctx
        .client
        .post_json(&format!("/entries/{id}/claim"), &json!({ "ttl_min": ttl }))
        .map_err(explain_claimed)?;
    let e = read_json(r)?;
    if ctx.json {
        return emit_json(&e);
    }
    let until = agent_time(e["data"]["claimed_by"]["until"].as_str().unwrap_or("?"));
    emit(format!("{id} захвачена до {until}\n").as_bytes())
}

/// `zam task release`.
pub fn release(ctx: &Ctx, id: &str) -> Result<(), CliError> {
    let id = bare_id(id);
    let r = ctx
        .client
        .post_json(&format!("/entries/{id}/release"), &json!({}))?;
    let e = read_json(r)?;
    if ctx.json {
        return emit_json(&e);
    }
    emit(format!("{id} освобождена\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        assert_eq!(bare_id(" k:01ABC "), "01ABC");
        assert_eq!(bare_id("e:01ABC"), "01ABC");
        assert_eq!(bare_id("01ABC"), "01ABC");
        assert_eq!(bare_id("x:01ABC"), "x:01ABC");
    }

    #[test]
    fn lines_and_card() {
        let e = json!({
            "id": "01A", "rev": 3, "status": "blocked", "title": "Миграция",
            "body": "План.\n",
            "data": {"next_step": "дождаться ревью", "blocked_by": "ревью",
                     "claimed_by": {"session": "s1", "until": "2026-10-28T12:00:00.000000Z",
                                    "agent": "claude"},
                     "started_at": "2026-10-28T10:00:00.000000Z", "estimate_min": 90}
        });
        assert_eq!(
            line(&e),
            "01A [blocked] «Миграция» · далее: дождаться ревью · блокер: ревью"
        );
        let c = card(&e);
        assert!(c.contains("rev3"), "{c}");
        assert!(
            c.contains("захвачена: сессия s1 до 2026-10-28 12:00:00 (claude)"),
            "{c}"
        );
        assert!(
            c.contains(
                "начата: 2026-10-28 10:00:00
"
            ),
            "{c}"
        );
        assert!(c.contains("оценка: 90 мин"), "{c}");
        assert!(c.ends_with("\nПлан.\n"), "{c}");
        let d = json!({"id": "01B", "status": "done", "title": "T", "duration_min": 42,
                       "data": {}});
        assert_eq!(line(&d), "01B [done] «T» · 42 мин");
    }

    /// M6.6 §9 (замечание 9): `data` новой задачи — только заданные поля, без `status`.
    #[test]
    fn new_data_forms() {
        let links = ["https://example.invalid/pr/1".to_string()];
        let mut o = New {
            title: "T",
            next: None,
            owner: None,
            estimate: None,
            links: &[],
            priority: None,
            body_file: None,
            area: None,
            global: false,
            solution: None,
            key: None,
        };
        assert_eq!(new_data(&o), json!({}));
        o.next = Some("n");
        o.owner = Some("human");
        o.estimate = Some(90);
        o.links = &links;
        o.priority = Some("high");
        assert_eq!(
            new_data(&o),
            json!({"next_step": "n", "owner": "human", "estimate_min": 90,
                   "links": ["https://example.invalid/pr/1"], "priority": "high"})
        );
    }

    #[test]
    fn status_body_forms() {
        assert_eq!(
            status_body("done", None, None, None),
            json!({"status": "done"})
        );
        assert_eq!(
            status_body("blocked", Some("n"), Some("b"), Some("r")),
            json!({"status": "blocked", "next_step": "n", "blocked_by": "b", "reason": "r"})
        );
        // `--next ""` (M6.6, замечание 35: снять шаг задачи, закрытой раньше) — пустая строка
        // уходит как есть, а не `null`: сервер снимает поле одинаково (`TaskData::apply_patch`,
        // тест сервера `task_done_with_explicit_next_keeps_it`)
        assert_eq!(
            status_body("done", Some(""), None, None),
            json!({"status": "done", "next_step": ""})
        );
    }
}
