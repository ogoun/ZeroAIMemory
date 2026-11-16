//! `zam idea` — журнал «Идеи» (M5.7 §7.3, M6.5 §5, §7): `list`, `new`, `queue`, `review`,
//! `propose`, `accept`, `defer`, `decline`.
//!
//! - `list [--status S]… [--origin human|agent] [--knowledge]` — строки `w:<id> [статус]
//!   [приоритет] заголовок`, у идеи агента — `(от агента)`, у начатой — `→ k:<задача>`, у
//!   возвращённой на доработку — `— не хватает: <начало missing>`; без `--status` — `new,
//!   returned, proposed, deferred, accepted` (сервер до M6.5 отвечает на `returned` 400 — повтор
//!   без него);
//! - `queue [--limit N]` — пакет разбора `idea_review` (`POST /recall`, M6.5 §7): идеи
//!   «Непросмотрено» с историей, похожими записями решения и шагами разбора (текст сервера);
//!   сервер без флага `idea_review` — код 1, как у `review`;
//! - `review <w:id> --plan|--return|--decline|--done --summary-file F [--missing T] [--basis
//!   ID]… [--similar w:ID]… [--task k:ID | --task-title T --task-body-file F --task-priority
//!   P] [--key K]` — разбор идеи агентом (`POST /entries/{id}/review`, M6.5 §5): «Разобрано
//!   w:<id>: <исход>[ → задача k:<id>]», повтор по `--key` (`Idempotency-Key` одного разбора) —
//!   с « (повтор)». Разбор — всегда действие агента: `--by` нет. Сервер без флага
//!   `idea_review` в `/health` — код 1 с [`OLD_SERVER_REVIEW`];
//! - `new <заголовок> [--body T | --file F.md] [--priority …] [--horizon YYYY-MM-DD] [--tag X]…
//!   [--origin human|agent] [--knowledge]` — новая идея (`<id>@<rev>`);
//! - `propose <w:id> [--text T] [--space GUID]` — предложить человеку: вопрос-предложение
//!   «начать / отложить / отказаться» (`Предложено w:<id> → вопрос q:<id>`); у принятой идеи
//!   со снятой или удалённой задачей — вопрос «вернуть / отказаться», `--text` обязателен
//!   (`Предложено вернуть w:<id> → вопрос q:<id>`, M5.7 §16);
//! - `accept|defer|decline <w:id> [--by human|agent] [--channel chat|cli]` — решение человека:
//!   `accept [--space GUID]` создаёт задачу (повтор — код 4 с той же задачей), `defer [--until
//!   YYYY-MM-DD]` — отсрочка, `decline [--reason T]` — отказ.
//!
//! `--origin` и `--by` — по правилу `zam question answer --by`
//! ([`super::question::resolve_human_agent`]): запустил агент — флаг обязателен, иначе код 6
//! до обращения к серверу; человек в терминале — `human`. Даты `--horizon` и `--until` — строго
//! `YYYY-MM-DD` (иначе код 6 без запроса); «позже сегодняшней даты UTC» проверяет сервер (422).
//!
//! Пространство — решение из `solution.toml`, `--knowledge` — Knowledge. Ссылка — `w:<id>`,
//! `e:<id>` или голый id; иначе код 6.
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::log::read_text;
use super::{Ctx, emit, emit_json, post_entry, solution_guid};
use crate::client::{CliError, EXIT_CONFLICT, read_json};
use crate::target::is_node_id;

/// Статусы ленты по умолчанию: живые идеи (как умолчания поиска WebUI, спецификация M5.7 §4;
/// `returned` — M6.5 §4).
const ACTIVE: &str = "new,returned,proposed,deferred,accepted";

/// Умолчание ленты у сервера до M6.5 (`returned` нет): он разбирает статусы строго и на
/// [`ACTIVE`] отвечает 400 «status: неизвестное значение» — `list` без `--status` повторяет
/// запрос с этим списком (клиент новее сервера, `docs/api-compat.md`).
const ACTIVE_M57: &str = "new,proposed,deferred,accepted";

/// Статусы идеи (спецификация M5.7 §5; `returned` — M6.5 §4).
const STATUSES: [&str; 7] = [
    "new", "returned", "proposed", "accepted", "deferred", "declined", "done",
];

/// Сообщение для сервера без разбора идей (нет флага `idea_review` в `GET /health` `features`,
/// M6.5 §10).
pub const OLD_SERVER_REVIEW: &str =
    "сервер не поддерживает разбор идей: нужен zam-server 0.0.12-m6.5 или новее";

/// Флаг разбора идей в `GET /health` `features` (`docs/api-compat.md`).
const FEATURE_REVIEW: &str = "idea_review";

/// Символов начала `missing` в строке ленты возвращённой идеи.
const MISSING_SHOWN: usize = 80;

/// Голый id идеи из `w:<id>`, `e:<id>` или id (канонический вид); иначе код 6.
pub fn idea_id(raw: &str) -> Result<String, CliError> {
    let t = raw.trim();
    let id = match t.split_once(':') {
        Some(("w" | "e", id)) => id,
        Some(_) => "",
        None => t,
    };
    if is_node_id(id) {
        Ok(id.to_string())
    } else {
        Err(CliError::input(format!(
            "«{raw}»: ссылка на идею — w:<id>, e:<id> или id записи"
        )))
    }
}

/// Дата календаря строго `YYYY-MM-DD` (месяц и день существуют).
fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u32> {
        let p = &s[r];
        p.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| p.parse().ok())?
    };
    let (Some(y), Some(m), Some(d)) = (num(0..4), num(5..7), num(8..10)) else {
        return false;
    };
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&d)
}

/// Дата «не раньше» (`--horizon`, `--until`): строго `YYYY-MM-DD`, иначе код 6. «Позже
/// сегодняшней даты UTC» проверяет только сервер (422 → код 6): часы клиента и сервера могут
/// расходиться около полуночи UTC.
fn check_horizon<'a>(raw: &'a str, flag: &str) -> Result<&'a str, CliError> {
    let s = raw.trim();
    if !is_date(s) {
        return Err(CliError::input(format!("{flag} «{raw}»: дата YYYY-MM-DD")));
    }
    Ok(s)
}

/// Строка ленты: `w:<id> [статус] [приоритет] заголовок`, `(от агента)`, `→ k:<задача>`; у
/// возвращённой на доработку — `— не хватает: <первая строка missing>` (до [`MISSING_SHOWN`]
/// символов, дальше есть текст — `…`): что человеку дописать, без `zam read` (как строка
/// решения ленты WebUI, M6.5 §9).
fn line(e: &Value) -> String {
    let d = &e["data"];
    let mut s = format!(
        "w:{} [{}] [{}] {}",
        e["id"].as_str().unwrap_or("?"),
        e["status"].as_str().unwrap_or("?"),
        d["priority"].as_str().unwrap_or("normal"),
        e["title"].as_str().unwrap_or("")
    );
    if d["origin"] == "agent" {
        s.push_str(" (от агента)");
    }
    if let Some(t) = d["task_id"].as_str() {
        s.push_str(&format!(" → k:{t}"));
    }
    if e["status"] == "returned"
        && let Some(m) = d["review"]["missing"].as_str()
    {
        let mut lines = m.lines().map(str::trim).filter(|l| !l.is_empty());
        if let Some(first) = lines.next() {
            let mut shown: String = first.chars().take(MISSING_SHOWN).collect();
            if first.chars().count() > MISSING_SHOWN || lines.next().is_some() {
                shown.push('…');
            }
            s.push_str(&format!(" — не хватает: {shown}"));
        }
    }
    s
}

/// Пространство команды: Knowledge или решение из `solution.toml`.
fn space(ctx: &Ctx, knowledge: bool) -> Result<String, CliError> {
    if knowledge {
        Ok("knowledge".to_string())
    } else {
        solution_guid(ctx, None)
    }
}

/// Параметры `zam idea list`.
pub struct List<'a> {
    /// Статусы (повторяемый флаг, каждый — через запятую).
    pub status: &'a [String],
    pub origin: Option<&'a str>,
    pub knowledge: bool,
}

/// Статусы ленты через запятую; не заданы — `None` (умолчание [`ACTIVE`]); неизвестный статус
/// идеи — код 6.
fn statuses(raw: &[String]) -> Result<Option<String>, CliError> {
    let mut out: Vec<&str> = Vec::new();
    for s in raw.iter().flat_map(|r| r.split(',')).map(str::trim) {
        if s.is_empty() {
            continue;
        }
        if !STATUSES.contains(&s) {
            return Err(CliError::input(format!(
                "--status {s}: статус идеи — {}",
                STATUSES.join(", ")
            )));
        }
        if !out.contains(&s) {
            out.push(s);
        }
    }
    Ok((!out.is_empty()).then(|| out.join(",")))
}

/// `zam idea list`; без `--status` у сервера до M6.5 (400 на [`ACTIVE`]) — повтор с
/// [`ACTIVE_M57`]; явный `--status returned` такому серверу — его 400 (код 6).
pub fn list(ctx: &Ctx, o: &List) -> Result<(), CliError> {
    let st = statuses(o.status)?;
    let space = space(ctx, o.knowledge)?;
    let path = format!("/spaces/{space}/entries");
    let get = |status: &str| {
        let mut q = vec![("kind", "idea"), ("status", status), ("limit", "200")];
        if let Some(origin) = o.origin {
            q.push(("origin", origin));
        }
        ctx.client.get(&path, &q).and_then(read_json)
    };
    let v = match (get(st.as_deref().unwrap_or(ACTIVE)), &st) {
        (Err(e), None) if e.http_status() == Some(400) => get(ACTIVE_M57)?,
        (r, _) => r?,
    };
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

/// Параметры `zam idea new`.
pub struct New<'a> {
    pub title: &'a str,
    pub body: Option<&'a str>,
    pub file: Option<&'a Path>,
    pub priority: Option<&'a str>,
    pub horizon: Option<&'a str>,
    pub tags: &'a [String],
    pub knowledge: bool,
}

/// Тело запроса идеи (`POST /spaces/{space}/entries`, `kind: idea`); `origin` — уже разрешён.
pub fn new_body(o: &New, body: &str, origin: &str) -> Result<Value, CliError> {
    let mut data = json!({ "origin": origin });
    if let Some(p) = o.priority {
        data["priority"] = json!(p);
    }
    if let Some(h) = o.horizon {
        data["horizon"] = json!(check_horizon(h, "--horizon")?);
    }
    let mut req = json!({ "kind": "idea", "title": o.title, "body": body, "data": data });
    if !o.tags.is_empty() {
        req["tags"] = json!(o.tags);
    }
    Ok(req)
}

/// `zam idea new`; `origin` разрешён до подключения ([`super::question::resolve_human_agent`]).
pub fn new(ctx: &Ctx, o: &New, origin: &str) -> Result<(), CliError> {
    let body = match (o.body, o.file) {
        (Some(b), _) => b.to_string(),
        (None, Some(f)) => read_text(Some(f))?,
        (None, None) => String::new(),
    };
    let req = new_body(o, &body, origin)?;
    let space = space(ctx, o.knowledge)?;
    post_entry(ctx, &space, &req, None, None)
}

/// `zam idea propose`.
pub fn propose(
    ctx: &Ctx,
    id: &str,
    text: Option<&str>,
    space: Option<&str>,
) -> Result<(), CliError> {
    let id = idea_id(id)?;
    let mut b = json!({});
    if let Some(t) = text {
        b["text"] = json!(t);
    }
    if let Some(s) = space {
        b["space"] = json!(s);
    }
    let v = read_json(
        ctx.client
            .post_json(&format!("/entries/{id}/propose"), &b)?,
    )?;
    ctx.note_write("idea");
    if ctx.json {
        return emit_json(&v);
    }
    // вопрос «вернуть» идею со снятой задачей (§16)
    let what = if v["question"]["data"]["proposal"] == "reopen" {
        "Предложено вернуть"
    } else {
        "Предложено"
    };
    emit(
        format!(
            "{what} w:{id} → вопрос q:{}\n",
            v["question"]["id"].as_str().unwrap_or("?")
        )
        .as_bytes(),
    )
}

/// Ошибка `e` запроса разбора идей: HTTP-статус `status` — так отвечает сервер до M6.5 (404 —
/// нет маршрута `/review`, 400 — неизвестное событие `idea_review` у `/recall`). В `GET /health`
/// `features` нет [`FEATURE_REVIEW`] (у сервера до M6 нет и самого поля) — код 1 с
/// [`OLD_SERVER_REVIEW`]; флаг есть или `/health` не ответил — `e` как есть (404 — нет такой
/// идеи, код 5).
fn old_server(ctx: &Ctx, e: CliError, status: u16) -> CliError {
    if e.http_status() != Some(status) {
        return e;
    }
    match ctx.client.get("/health", &[]).and_then(read_json) {
        Ok(h) if !has_feature(&h, FEATURE_REVIEW) => CliError::config(OLD_SERVER_REVIEW),
        _ => e,
    }
}

/// Флаг `f` в `features` ответа `/health`.
fn has_feature(health: &Value, f: &str) -> bool {
    health["features"]
        .as_array()
        .is_some_and(|a| a.iter().any(|x| x == f))
}

/// `zam idea queue`: пакет разбора `POST /recall {event: idea_review, limit}` решения из
/// `solution.toml` — готовый текст сервера (пустая очередь, режим `propose`, нет права записи —
/// строка-уведомление); `--json` — ответ сервера целиком (`items` с `reason: idea_review`).
pub fn queue(ctx: &Ctx, limit: u32) -> Result<(), CliError> {
    let solution = solution_guid(ctx, None)?;
    let body = json!({ "event": "idea_review", "solution": solution, "limit": limit });
    let v = ctx
        .client
        .post_json("/recall", &body)
        .and_then(read_json)
        .map_err(|e| old_server(ctx, e, 400))?;
    if ctx.json {
        return emit_json(&v);
    }
    let mut out = v["text"]
        .as_str()
        .unwrap_or_default()
        .trim_end()
        .to_string();
    out.push('\n');
    emit(out.as_bytes())
}

/// Параметры `zam idea review` (M6.5 §5); исход и сочетания флагов проверил clap.
pub struct ReviewOpts {
    /// `w:<id>`, `e:<id>` или id идеи.
    pub id: String,
    /// `plan | return | decline | done`.
    pub outcome: &'static str,
    /// Разбор для человека (Markdown).
    pub summary_file: PathBuf,
    /// Что дописать человеку (`return`).
    pub missing: Option<String>,
    /// На что опирался разбор: ссылки с префиксом (`k:`, `file:`, `commit:` …).
    pub basis: Vec<String>,
    /// Похожие идеи `w:<id>`.
    pub similar: Vec<String>,
    /// Уже открытая задача плана `k:<id>` (`plan`).
    pub task: Option<String>,
    /// Заголовок новой задачи (`plan`).
    pub task_title: Option<String>,
    /// План шагов новой задачи (`plan`).
    pub task_body_file: Option<PathBuf>,
    /// Приоритет новой задачи `low | normal | high` (`plan`).
    pub task_priority: Option<String>,
    /// `Idempotency-Key` этого разбора (ruling 5: ключ — на один вызов, неявных ключей нет).
    pub key: Option<String>,
}

/// Ссылка `k:<id>` на задачу из `k:<id>` или голого id; иначе код 6.
fn task_ref(raw: &str) -> Result<String, CliError> {
    let t = raw.trim();
    let id = match t.split_once(':') {
        Some(("k", id)) => id,
        Some(_) => "",
        None => t,
    };
    if is_node_id(id) {
        Ok(format!("k:{id}"))
    } else {
        Err(CliError::input(format!(
            "--task «{raw}»: ссылка на задачу — k:<id> или id задачи"
        )))
    }
}

/// Тело `POST /entries/{id}/review`: исход, разбор, поля исхода; `task_body` — прочитанный
/// `--task-body-file`. Задача — `{id}` открытой (`--task`) или `{title, body, priority}` новой.
fn review_body(o: &ReviewOpts, summary: &str, task_body: Option<&str>) -> Result<Value, CliError> {
    let mut b = json!({ "outcome": o.outcome, "summary": summary });
    if let Some(m) = &o.missing {
        b["missing"] = json!(m);
    }
    if !o.basis.is_empty() {
        b["basis"] = json!(o.basis);
    }
    if !o.similar.is_empty() {
        b["similar"] = json!(o.similar);
    }
    if let Some(t) = &o.task {
        b["task"] = json!({ "id": task_ref(t)? });
    } else {
        let mut t = serde_json::Map::new();
        for (k, v) in [
            ("title", o.task_title.as_deref()),
            ("body", task_body),
            ("priority", o.task_priority.as_deref()),
        ] {
            if let Some(v) = v {
                t.insert(k.to_string(), json!(v));
            }
        }
        if !t.is_empty() {
            b["task"] = Value::Object(t);
        }
    }
    Ok(b)
}

/// Строка итога разбора: «Разобрано w:<id>: <исход>[ → задача k:<id>]», повтор по ключу —
/// « (повтор)».
fn review_line(id: &str, outcome: &str, v: &Value) -> String {
    let mut s = format!("Разобрано w:{id}: {outcome}");
    if let Some(t) = v["task"]["id"].as_str() {
        s.push_str(&format!(" → задача k:{t}"));
    }
    if v["replay"] == true {
        s.push_str(" (повтор)");
    }
    s.push('\n');
    s
}

/// `zam idea review`: ссылки, файлы и тело проверяются до запроса (код 6); 404 — проверка
/// старого сервера ([`old_server`]).
pub fn review(ctx: &Ctx, o: ReviewOpts) -> Result<(), CliError> {
    let id = idea_id(&o.id)?;
    let summary = read_text(Some(&o.summary_file))?;
    let task_body = o
        .task_body_file
        .as_deref()
        .map(|f| read_text(Some(f)))
        .transpose()?;
    let body = review_body(&o, &summary, task_body.as_deref())?;
    let headers: Vec<(&str, &str)> = o
        .key
        .as_deref()
        .map(|k| ("Idempotency-Key", k))
        .into_iter()
        .collect();
    let v = ctx
        .client
        .post_json_with(&format!("/entries/{id}/review"), &body, &headers)
        .and_then(read_json)
        .map_err(|e| old_server(ctx, e, 404))?;
    // повтор по ключу ничего не записал
    if v["replay"] != true {
        ctx.note_write("idea");
    }
    if ctx.json {
        return emit_json(&v);
    }
    emit(review_line(&id, o.outcome, &v).as_bytes())
}

/// Тело `POST /entries/{id}/decide`: решение, `by`, `channel` и поля решения (`extra`).
pub fn decide_body(decision: &str, by: &str, channel: &str, extra: &[(&str, Value)]) -> Value {
    let mut b = json!({ "decision": decision, "by": by, "channel": channel });
    for (k, v) in extra {
        b[*k] = v.clone();
    }
    b
}

/// Параметры решения `accept|defer|decline`; `by` разрешён до подключения.
pub struct DecideOpts<'a> {
    pub id: &'a str,
    pub by: &'a str,
    pub channel: &'a str,
    /// Пространство задачи (`accept`).
    pub space: Option<&'a str>,
    /// Дата «не раньше» (`defer`).
    pub until: Option<&'a str>,
    /// Причина отказа (`decline`).
    pub reason: Option<&'a str>,
}

/// 409 already-decided → «идея уже начата → задача k:…», код 4.
fn explain_decided(e: CliError) -> CliError {
    let Some(p) = e
        .problem
        .as_ref()
        .filter(|p| p["type"] == "/problems/already-decided")
    else {
        return e;
    };
    CliError::with_problem(
        EXIT_CONFLICT,
        format!(
            "идея уже начата → задача k:{}",
            p["task_id"].as_str().unwrap_or("?")
        ),
        e.problem,
    )
}

/// Тело решения по параметрам команды (формат `--until` проверяется до запроса).
fn decide_req(decision: &str, o: &DecideOpts) -> Result<Value, CliError> {
    let mut extra: Vec<(&str, Value)> = Vec::new();
    if let Some(s) = o.space {
        extra.push(("space", json!(s)));
    }
    if let Some(u) = o.until {
        extra.push(("horizon", json!(check_horizon(u, "--until")?)));
    }
    if let Some(r) = o.reason {
        extra.push(("reason", json!(r)));
    }
    Ok(decide_body(decision, o.by, o.channel, &extra))
}

/// `zam idea accept|defer|decline`; ссылка и формат `--until` проверяются до запроса (код 6).
pub fn decide(ctx: &Ctx, decision: &str, o: DecideOpts) -> Result<(), CliError> {
    let id = idea_id(o.id)?;
    let req = decide_req(decision, &o)?;
    let v = read_json(
        ctx.client
            .post_json(&format!("/entries/{id}/decide"), &req)
            .map_err(explain_decided)?,
    )?;
    ctx.note_write("idea");
    if ctx.json {
        return emit_json(&v);
    }
    let e = &v["entry"];
    let mut s = format!("Идея w:{id}: {}", e["status"].as_str().unwrap_or("?"));
    if let Some(t) = v["task"]["id"].as_str() {
        s.push_str(&format!(" → задача k:{t}"));
    }
    if let Some(h) = e["data"]["horizon"].as_str() {
        s.push_str(&format!(" до {h}"));
    }
    s.push('\n');
    emit(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01JABCDEFGHJKMNPQRSTVWXYZ0";

    #[test]
    fn bodies_and_lines() {
        // id
        let id = zam_core::Id::new().to_string();
        for raw in [format!("w:{id}"), format!(" e:{id} "), id.clone()] {
            assert_eq!(idea_id(&raw).unwrap(), id, "{raw}");
        }
        for bad in [
            "мусор".to_string(),
            format!("k:{id}"),
            "w:".to_string(),
            format!("w:{}", id.to_lowercase()),
        ] {
            assert_eq!(
                idea_id(&bad).unwrap_err().code,
                crate::client::EXIT_INPUT,
                "{bad}"
            );
        }

        // даты
        assert!(is_date("2099-02-28") && is_date("2096-02-29"));
        for bad in [
            "2099-02-29",
            "2100-02-29",
            "2099-13-01",
            "2099-1-01",
            "2099-01-00",
            "x",
        ] {
            assert!(!is_date(bad), "{bad}");
        }
        assert_eq!(
            check_horizon(" 2000-01-01 ", "--until").unwrap(),
            "2000-01-01",
            "прошедшая дата — дело сервера"
        );
        for bad in ["2026-11-31", "01.12.2026", "2026-11-05T00:00"] {
            let e = check_horizon(bad, "--until").unwrap_err();
            assert_eq!(e.code, crate::client::EXIT_INPUT);
            assert!(e.msg.starts_with("--until"), "{}", e.msg);
        }

        // тело идеи
        let tags = vec!["ui".to_string()];
        let o = New {
            title: "Тема",
            body: None,
            file: None,
            priority: Some("high"),
            horizon: Some("2099-01-01"),
            tags: &tags,
            knowledge: false,
        };
        assert_eq!(
            new_body(&o, "замысел", "human").unwrap(),
            json!({"kind": "idea", "title": "Тема", "body": "замысел", "tags": ["ui"],
                   "data": {"origin": "human", "priority": "high", "horizon": "2099-01-01"}})
        );
        let bare = New {
            priority: None,
            horizon: None,
            tags: &[],
            ..o
        };
        assert_eq!(
            new_body(&bare, "", "agent").unwrap(),
            json!({"kind": "idea", "title": "Тема", "body": "", "data": {"origin": "agent"}})
        );
        let bad_date = New {
            horizon: Some("2026-02-30"),
            ..bare
        };
        assert!(new_body(&bad_date, "", "human").is_err());

        // тело решения
        assert_eq!(
            decide_body("defer", "human", "cli", &[("horizon", json!("2099-01-01"))]),
            json!({"decision": "defer", "by": "human", "channel": "cli", "horizon": "2099-01-01"})
        );
        let d = DecideOpts {
            id: ID,
            by: "agent",
            channel: "chat",
            space: None,
            until: None,
            reason: Some("лишнее"),
        };
        assert_eq!(
            decide_req("decline", &d).unwrap(),
            json!({"decision": "decline", "by": "agent", "channel": "chat", "reason": "лишнее"})
        );
        let d = DecideOpts {
            until: Some("2026-1-04"),
            reason: None,
            ..d
        };
        assert!(decide_req("defer", &d).is_err());

        // статусы ленты
        assert_eq!(statuses(&[]).unwrap(), None);
        assert_eq!(statuses(&[" , ".into()]).unwrap(), None);
        assert_eq!(
            statuses(&["deferred,new".into(), "new".into()])
                .unwrap()
                .as_deref(),
            Some("deferred,new")
        );
        assert!(statuses(&["open".into()]).is_err());

        // строки ленты
        let e = json!({"id": ID, "status": "new", "title": "Тёмная тема",
                       "data": {"origin": "human", "priority": "high"}});
        assert_eq!(line(&e), format!("w:{ID} [new] [high] Тёмная тема"));
        let e = json!({"id": ID, "status": "accepted", "title": "Кэш",
                       "data": {"origin": "agent", "task_id": "01T"}});
        assert_eq!(
            line(&e),
            format!("w:{ID} [accepted] [normal] Кэш (от агента) → k:01T")
        );

        // already-decided → код 4 с задачей
        let e = CliError::with_problem(
            4,
            "x",
            Some(json!({"type": "/problems/already-decided", "task_id": "01T"})),
        );
        let r = explain_decided(e);
        assert_eq!(
            (r.code, r.msg.as_str()),
            (EXIT_CONFLICT, "идея уже начата → задача k:01T")
        );
    }

    fn opts(outcome: &'static str) -> ReviewOpts {
        ReviewOpts {
            id: ID.to_string(),
            outcome,
            summary_file: PathBuf::from("s.md"),
            missing: None,
            basis: Vec::new(),
            similar: Vec::new(),
            task: None,
            task_title: None,
            task_body_file: None,
            task_priority: None,
            key: None,
        }
    }

    /// M6.5 (задача 6): статус `returned` ленты, тело и строка разбора, ссылка на задачу, флаг
    /// старого сервера.
    #[test]
    fn review_bodies_and_lines() {
        // статусы ленты: returned — фильтр и умолчание
        assert_eq!(
            statuses(&["returned".into()]).unwrap().as_deref(),
            Some("returned")
        );
        assert!(ACTIVE.split(',').any(|s| s == "returned"));
        // умолчание сервера до M6.5 — то же без returned
        assert_eq!(
            ACTIVE_M57.split(',').collect::<Vec<_>>(),
            ACTIVE
                .split(',')
                .filter(|s| *s != "returned")
                .collect::<Vec<_>>()
        );
        let e = json!({"id": ID, "status": "returned", "title": "Тема",
                       "data": {"review": {"outcome": "return",
                                           "missing": "\n Какие экраны? \nА меню?"}}});
        assert_eq!(
            line(&e),
            format!("w:{ID} [returned] [normal] Тема — не хватает: Какие экраны?…")
        );
        let long = "я".repeat(MISSING_SHOWN + 1);
        let e = json!({"id": ID, "status": "returned", "title": "Т",
                       "data": {"review": {"missing": long}}});
        assert!(
            line(&e).ends_with(&format!(": {}…", "я".repeat(MISSING_SHOWN))),
            "{}",
            line(&e)
        );
        // `missing` прежнего разбора у новой идеи (после правки человеком) не показывается
        let e = json!({"id": ID, "status": "new", "title": "Т",
                       "data": {"review": {"missing": "что-то"}}});
        assert_eq!(line(&e), format!("w:{ID} [new] [normal] Т"));

        // тело разбора
        assert_eq!(
            review_body(&opts("decline"), "разбор", None).unwrap(),
            json!({"outcome": "decline", "summary": "разбор"})
        );
        let o = ReviewOpts {
            missing: Some("что дописать".into()),
            basis: vec!["k:01T".into(), "file:docs/a.md".into()],
            similar: vec![format!("w:{ID}")],
            ..opts("return")
        };
        assert_eq!(
            review_body(&o, "р", None).unwrap(),
            json!({"outcome": "return", "summary": "р", "missing": "что дописать",
                   "basis": ["k:01T", "file:docs/a.md"], "similar": [format!("w:{ID}")]})
        );
        let o = ReviewOpts {
            task_title: Some("Задача".into()),
            task_priority: Some("high".into()),
            ..opts("plan")
        };
        assert_eq!(
            review_body(&o, "р", Some("1. шаг")).unwrap(),
            json!({"outcome": "plan", "summary": "р",
                   "task": {"title": "Задача", "body": "1. шаг", "priority": "high"}})
        );
        for raw in [format!("k:{ID}"), format!(" {ID} ")] {
            let o = ReviewOpts {
                task: Some(raw.clone()),
                ..opts("plan")
            };
            assert_eq!(
                review_body(&o, "р", None).unwrap()["task"],
                json!({"id": format!("k:{ID}")}),
                "{raw}"
            );
        }
        for bad in [format!("w:{ID}"), "k:".into(), "мусор".into()] {
            let e = task_ref(&bad).unwrap_err();
            assert_eq!(e.code, crate::client::EXIT_INPUT, "{bad}");
            assert!(e.msg.contains("k:<id>"), "{}", e.msg);
        }

        // строка итога
        assert_eq!(
            review_line(ID, "plan", &json!({"task": {"id": "01T"}, "replay": false})),
            format!("Разобрано w:{ID}: plan → задача k:01T\n")
        );
        assert_eq!(
            review_line(ID, "decline", &json!({"entry": {}, "replay": true})),
            format!("Разобрано w:{ID}: decline (повтор)\n")
        );

        // флаг сервера
        assert!(has_feature(
            &json!({"features": ["m6", FEATURE_REVIEW]}),
            "idea_review"
        ));
        assert!(!has_feature(&json!({"features": ["m6"]}), "idea_review"));
        assert!(!has_feature(&json!({"version": "0.0.10"}), "idea_review"));
    }
}
