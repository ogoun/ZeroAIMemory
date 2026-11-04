//! `zam idea` — журнал «Идеи» (M5.7 §7.3): `list`, `new`, `propose`, `accept`, `defer`,
//! `decline`.
//!
//! - `list [--status S]… [--origin human|agent] [--knowledge]` — строки `w:<id> [статус]
//!   [приоритет] заголовок`, у идеи агента — `(от агента)`, у начатой — `→ k:<задача>`; без
//!   `--status` — `new, proposed, deferred, accepted`;
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
use std::path::Path;

use serde_json::{Value, json};

use super::log::read_text;
use super::{Ctx, emit, emit_json, post_entry, solution_guid};
use crate::client::{CliError, EXIT_CONFLICT, read_json};
use crate::target::is_node_id;

/// Статусы ленты по умолчанию: живые идеи (как умолчания поиска WebUI, спецификация §4).
const ACTIVE: &str = "new,proposed,deferred,accepted";

/// Статусы идеи (спецификация §5).
const STATUSES: [&str; 6] = [
    "new", "proposed", "accepted", "deferred", "declined", "done",
];

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

/// Строка ленты: `w:<id> [статус] [приоритет] заголовок`, `(от агента)`, `→ k:<задача>`.
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

/// Статусы ленты через запятую; неизвестный статус идеи — код 6.
fn statuses(raw: &[String]) -> Result<String, CliError> {
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
    Ok(if out.is_empty() {
        ACTIVE.to_string()
    } else {
        out.join(",")
    })
}

/// `zam idea list`.
pub fn list(ctx: &Ctx, o: &List) -> Result<(), CliError> {
    let st = statuses(o.status)?;
    let space = space(ctx, o.knowledge)?;
    let mut q = vec![("kind", "idea"), ("status", st.as_str()), ("limit", "200")];
    if let Some(origin) = o.origin {
        q.push(("origin", origin));
    }
    let v = read_json(ctx.client.get(&format!("/spaces/{space}/entries"), &q)?)?;
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
    CliError {
        code: EXIT_CONFLICT,
        msg: format!(
            "идея уже начата → задача k:{}",
            p["task_id"].as_str().unwrap_or("?")
        ),
        problem: e.problem,
    }
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
        assert_eq!(statuses(&[]).unwrap(), ACTIVE);
        assert_eq!(
            statuses(&["deferred,new".into(), "new".into()]).unwrap(),
            "deferred,new"
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
        let e = CliError {
            code: 4,
            msg: "x".into(),
            problem: Some(json!({"type": "/problems/already-decided", "task_id": "01T"})),
        };
        let r = explain_decided(e);
        assert_eq!(
            (r.code, r.msg.as_str()),
            (EXIT_CONFLICT, "идея уже начата → задача k:01T")
        );
    }
}
