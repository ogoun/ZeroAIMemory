//! `zam task` — журнал задач (M4.5 §3): `list`, `show`, `new`, `status`, `claim`, `release`.
//!
//! - `list [--status S,…] [--global] [--solution GUID] [--limit N | --all] [--parent k:<id> |
//!   --top]` — строки `<id> [статус] «заголовок» · далее: …`; без `--status` — `open,
//!   in_progress, blocked` (с `--parent` — все статусы). У общей задачи — прогресс подзадач « ·
//!   8/13», у `done` — длительность. `--parent` — прямые подзадачи в порядке шагов, `--top` — без
//!   подзадач (M7 §5.4 п. 4; сервер без признака `task_subtasks` — код 1). Страница — 200, обход и
//!   подсказка о продолжении — [`super::list_entries`];
//! - `show <k:id>` — карточка: статус, «в работе без обновлений N ч», «часть: k:<общая> …»,
//!   шаги, захват, времена, подзадачи (сводка и строки), план (тело);
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
    Ctx, LIST_LIMIT_MAX, Paging, agent_time, emit, emit_json, has_feature, list_entries,
    post_entry, solution_guid,
};
use crate::client::{CliError, EXIT_CONFLICT, read_json};
// `k:<id>` / `e:<id>` / `t:<id>` … → голый id (буквы — общий источник `target`)
use crate::target::bare_entry_id as bare_id;

/// Статусы ленты по умолчанию.
const ACTIVE: &str = "open,in_progress,blocked";
/// Признак сервера: подзадачи `part_of` — поля задачи и фильтры ленты (M7 §5.4).
const FEATURE_SUBTASKS: &str = "task_subtasks";
/// Код 1 у сервера без [`FEATURE_SUBTASKS`]: неизвестные параметры ленты он молча пропустил бы и
/// вернул бы все задачи.
const OLD_SERVER_SUBTASKS: &str =
    "сервер не поддерживает подзадачи (--parent, --top): обновите zam-server";
/// Строк подзадач в карточке; дальше — «… и ещё N: zam task list --parent k:<id>».
const CARD_SUBTASKS_MAX: usize = 50;

/// Прогресс сводки подзадач `{total, done, dropped}` — «done/(total − dropped)» (M7 §5.4 п. 2:
/// снятые в знаменатель не входят); сводки нет или все сняты — `None`.
fn progress(s: &Value) -> Option<String> {
    let total = s["total"].as_u64()?;
    let den = total.saturating_sub(s["dropped"].as_u64().unwrap_or(0));
    (den > 0).then(|| format!("{}/{den}", s["done"].as_u64().unwrap_or(0)))
}

/// Строка ленты: `<id> [статус] «заголовок»`, у общей — прогресс подзадач, далее — шаг, блокер,
/// длительность.
fn line(e: &Value) -> String {
    let mut s = format!(
        "{} [{}] «{}»",
        e["id"].as_str().unwrap_or("?"),
        e["status"].as_str().unwrap_or("?"),
        e["title"].as_str().unwrap_or("")
    );
    if let Some(p) = progress(&e["subtasks"]) {
        s.push_str(&format!(" · {p}"));
    }
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

/// Строки подзадач карточки (M7 §5.4 п. 4): у задачи «в работе» без обновлений — «в работе без
/// обновлений N ч (последнее — <время>)» (`stale` сервера; часы — от `last_activity_at` до `now`,
/// секунд Unix), у подзадачи — «часть: k:<общая> «…» (8/13)». Полей нет (сервер без
/// `task_subtasks`) — пусто: карточка прежняя.
fn subtask_head(e: &Value, now: i64) -> String {
    let mut out = String::new();
    if e["stale"] == true
        && let Some(last) = e["last_activity_at"].as_str()
    {
        let hours = zam_core::time::unix_secs(last).map_or(0, |t| (now - t).max(0) / 3600);
        out.push_str(&format!(
            "в работе без обновлений {hours} ч (последнее — {})\n",
            agent_time(last)
        ));
    }
    if let Some(p) = e["parent"].as_object() {
        let pr = p
            .get("subtasks")
            .and_then(progress)
            .map(|x| format!(" ({x})"))
            .unwrap_or_default();
        out.push_str(&format!(
            "часть: k:{} «{}»{pr}\n",
            p.get("id").and_then(Value::as_str).unwrap_or("?"),
            p.get("title").and_then(Value::as_str).unwrap_or("")
        ));
    }
    out
}

/// Сводка и строки подзадач общей задачи (M7 §5.4 п. 4): «подзадачи: 8/13 (в работе 2, открыто
/// 2, заблокировано 1, снято 1)» (все сняты — «подзадачи: снято N»), затем до
/// [`CARD_SUBTASKS_MAX`] строк `k:<id> [статус] «заголовок»` в порядке шагов и «… и ещё N: zam
/// task list --parent k:<id>». Сводки нет — пусто.
fn subtask_list(e: &Value) -> String {
    let s = &e["subtasks"];
    let Some(total) = s["total"].as_u64() else {
        return String::new();
    };
    let n = |k: &str| s[k].as_u64().unwrap_or(0);
    let parts: Vec<String> = [
        ("в работе", n("in_progress")),
        ("открыто", n("open")),
        ("заблокировано", n("blocked")),
        ("снято", n("dropped")),
    ]
    .into_iter()
    .filter(|(_, c)| *c > 0)
    .map(|(w, c)| format!("{w} {c}"))
    .collect();
    let mut out = match progress(s) {
        Some(p) if parts.is_empty() => format!("подзадачи: {p}\n"),
        Some(p) => format!("подзадачи: {p} ({})\n", parts.join(", ")),
        None => format!("подзадачи: снято {}\n", n("dropped")),
    };
    let items = s["items"].as_array().map_or(&[][..], Vec::as_slice);
    let shown = items.len().min(CARD_SUBTASKS_MAX);
    for it in &items[..shown] {
        out.push_str(&format!(
            "  k:{} [{}] «{}»\n",
            it["id"].as_str().unwrap_or("?"),
            it["status"].as_str().unwrap_or("?"),
            it["title"].as_str().unwrap_or("")
        ));
    }
    let rest = total.saturating_sub(shown as u64);
    if rest > 0 {
        out.push_str(&format!(
            "  … и ещё {rest}: zam task list --parent k:{}\n",
            e["id"].as_str().unwrap_or("?")
        ));
    }
    out
}

/// Карточка задачи на момент `now` (секунд Unix — для «без обновлений N ч»).
fn card(e: &Value, now: i64) -> String {
    let d = &e["data"];
    let mut out = format!("{}  rev{}\n", line(e), e["rev"]);
    out.push_str(&subtask_head(e, now));
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
    out.push_str(&subtask_list(e));
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

/// Подзадачи в ленте `zam task list` (M7 §5.4 п. 4): `--parent k:<id>` или `--top` (clap
/// не даёт задать оба).
#[derive(Debug, Clone, Copy, Default)]
pub struct Subtasks<'a> {
    /// Прямые подзадачи задачи `k:<id>` в порядке шагов.
    pub parent: Option<&'a str>,
    /// Без подзадач.
    pub top: bool,
}

/// Параметры ленты задач: `kind`, статусы (`status` или по умолчанию [`ACTIVE`]; с `--parent` —
/// все статусы: продолжение карточки показывает и выполненные шаги **[решение исполнителя,
/// задача 5 M7.2]**), `top_level`, `parent`.
fn list_query<'a>(status: Option<&'a str>, sub: Subtasks<'a>) -> Vec<(&'static str, &'a str)> {
    let mut q = vec![("kind", "task")];
    match (status, sub.parent) {
        (Some(s), _) => q.push(("status", s)),
        (None, None) => q.push(("status", ACTIVE)),
        (None, Some(_)) => {}
    }
    if sub.top {
        q.push(("top_level", "1"));
    }
    if let Some(p) = sub.parent {
        q.push(("parent", bare_id(p)));
    }
    q
}

/// `zam task list`; без `--limit` страница прежняя — 200. `--parent`/`--top` — только у сервера с
/// признаком [`FEATURE_SUBTASKS`] (проверка `GET /health` до ленты), иначе код 1.
pub fn list(
    ctx: &Ctx,
    status: Option<&str>,
    global: bool,
    solution: Option<&str>,
    page: Paging,
    sub: Subtasks,
) -> Result<(), CliError> {
    let space = if global {
        "knowledge".to_string()
    } else {
        solution_guid(ctx, solution)?
    };
    if (sub.parent.is_some() || sub.top)
        && !has_feature(
            &read_json(ctx.client.get("/health", &[])?)?,
            FEATURE_SUBTASKS,
        )
    {
        return Err(CliError::config(OLD_SERVER_SUBTASKS));
    }
    let v = list_entries(
        ctx,
        &space,
        &list_query(status, sub),
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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    emit(card(&e, now).as_bytes())
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
        let c = card(&e, 0);
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

    /// M7 §5.4 п. 4: прогресс `done/(total − dropped)`, все сняты — без прогресса; сводка
    /// карточки — части без нулей, «снято» последним; строк подзадач — до 50 и «… и ещё N»;
    /// у подзадачи — «часть: …» с прогрессом общей; «без обновлений N ч» — от `last_activity_at`.
    #[test]
    fn subtask_progress_and_card_lines() {
        let s = |total, done, dropped| json!({"total": total, "done": done, "dropped": dropped});
        assert_eq!(progress(&s(13, 8, 0)).as_deref(), Some("8/13"));
        assert_eq!(progress(&s(13, 8, 1)).as_deref(), Some("8/12"));
        assert_eq!(progress(&s(3, 0, 3)), None);
        assert_eq!(progress(&Value::Null), None);
        let items: Vec<Value> = (0..60)
            .map(|i| json!({"id": format!("S{i}"), "status": "open", "title": format!("Шаг {i}")}))
            .collect();
        let e = json!({"id": "01P", "status": "in_progress", "title": "Этап", "data": {},
                       "subtasks": {"total": 60, "done": 8, "dropped": 1, "open": 46,
                                    "in_progress": 2, "blocked": 3, "items": items},
                       "stale": true, "last_activity_at": "2026-11-22T00:00:00Z"});
        assert_eq!(line(&e), "01P [in_progress] «Этап» · 8/59");
        let now = zam_core::time::unix_secs("2026-11-22T14:59:00Z").unwrap();
        let c = card(&e, now);
        assert!(
            c.contains("подзадачи: 8/59 (в работе 2, открыто 46, заблокировано 3, снято 1)\n"),
            "{c}"
        );
        assert!(
            c.contains("в работе без обновлений 14 ч (последнее — 2026-11-22 00:00:00)\n"),
            "{c}"
        );
        assert_eq!(c.matches("\n  k:S").count(), 50, "{c}");
        assert!(
            c.contains("  k:S49 [open] «Шаг 49»\n") && !c.contains("k:S50"),
            "{c}"
        );
        assert!(
            c.contains("  … и ещё 10: zam task list --parent k:01P\n"),
            "{c}"
        );
        // все выполнены — без частей; все сняты — «снято N»
        let done = json!({"id": "01D", "title": "Т", "status": "done", "data": {},
                          "subtasks": {"total": 2, "done": 2, "dropped": 0}});
        assert!(card(&done, 0).contains("подзадачи: 2/2\n"));
        let gone = json!({"id": "01G", "title": "Т", "status": "dropped", "data": {},
                          "subtasks": {"total": 2, "done": 0, "dropped": 2}});
        assert!(card(&gone, 0).contains("подзадачи: снято 2\n"));
        // подзадача: часть общей с её прогрессом; без полей сервера — строк нет
        let step = json!({"id": "S1", "title": "Шаг", "status": "open", "data": {},
                          "parent": {"id": "01P", "title": "Этап", "status": "in_progress",
                                     "subtasks": {"total": 13, "done": 8, "dropped": 0}}});
        assert!(card(&step, 0).contains("часть: k:01P «Этап» (8/13)\n"));
        let old = json!({"id": "S1", "title": "Шаг", "status": "in_progress", "rev": 1,
                         "data": {}});
        assert_eq!(card(&old, now), "S1 [in_progress] «Шаг»  rev1\n");
    }

    /// Параметры ленты: без `--status` — активные, с `--parent` — все статусы; `k:` снимается.
    #[test]
    fn list_query_forms() {
        let none = Subtasks::default();
        assert_eq!(
            list_query(None, none),
            [("kind", "task"), ("status", ACTIVE)]
        );
        let top = Subtasks { top: true, ..none };
        assert_eq!(
            list_query(Some("done"), top),
            [("kind", "task"), ("status", "done"), ("top_level", "1")]
        );
        let kids = Subtasks {
            parent: Some("k:01P"),
            ..none
        };
        assert_eq!(
            list_query(None, kids),
            [("kind", "task"), ("parent", "01P")]
        );
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
