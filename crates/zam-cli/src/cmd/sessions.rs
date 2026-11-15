//! Сессии агентов и выданное им (M6 §8, ТЗ §10.8):
//!
//! - `zam sessions tokens [<id> | --last N] [--by-holdout]` — таблица «событие · вызовов ·
//!   токенов · уже выдавалось» (`session_tokens` сервера: события `/recall` — `session_start`,
//!   `recovery`, резерв Rules этих событий `rules`, `prompt`, `post_tool`, `brief`; хуки с
//!   локальным выводом — `pre_tool`, `post_tool_local`, `subagent_local`; `search` и `read` —
//!   REST и CLI с сессией; `mcp` — инструменты MCP; `mcp_read` — из них чтения `/read`, токенов 0)
//!   и строка доли «уже выдавалось»: у `read` — вызовов `/read` с пометкой из вызовов `/read`, у
//!   MCP — чтений MCP с пометкой из чтений MCP (`mcp_read`). `<id>` — одна сессия; `--last N` — сумма по N последним (свежие первыми, 1..=500); без
//!   обоих — сессия агента (`ZAM_SESSION`, сессия хуков проекта), иначе последняя.
//!   `--by-holdout` — суммы отдельно по контрольной группе A/B (`holdout`, M6 §12) и остальным.
//!   Время — UTC `yyyy-MM-dd HH:mm:ss` ([`agent_time`]); `--json` — сессии с `tokens` и суммы.
use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::hook::encode_segment;
use super::{Ctx, agent_time, emit, emit_json};
use crate::client::{CliError, read_json};

/// Порядок строк таблицы: события сервера, затем хуков с локальным выводом; прочие — по имени.
const ORDER: [&str; 13] = [
    "session_start",
    "rules",
    "recovery",
    "prompt",
    "post_tool",
    "brief",
    "pre_tool",
    "post_tool_local",
    "subagent_local",
    "search",
    "read",
    "mcp",
    "mcp_read",
];

/// Выбор сессий `zam sessions tokens`.
pub enum Pick<'a> {
    /// Одна сессия по id.
    Id(&'a str),
    /// N последних.
    Last(u32),
    /// Сессия агента, иначе последняя.
    Current,
}

/// Сумма по событию.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
struct Stat {
    calls: u64,
    tokens: u64,
    reread: u64,
}

/// Суммы `tokens` сессий по событию.
fn sum<'a>(sessions: impl IntoIterator<Item = &'a Value>) -> BTreeMap<String, Stat> {
    let mut out: BTreeMap<String, Stat> = BTreeMap::new();
    for s in sessions {
        let Some(t) = s["tokens"].as_object() else {
            continue;
        };
        for (event, v) in t {
            let e = out.entry(event.clone()).or_default();
            e.calls += v["calls"].as_u64().unwrap_or(0);
            e.tokens += v["tokens"].as_u64().unwrap_or(0);
            e.reread += v["reread_calls"].as_u64().unwrap_or(0);
        }
    }
    out
}

/// Ранг события в таблице.
fn rank(event: &str) -> usize {
    ORDER
        .iter()
        .position(|e| *e == event)
        .unwrap_or(ORDER.len())
}

/// Доля «уже выдавалось» события: `N из M (P %)`.
fn share(s: &Stat) -> String {
    let pct = if s.calls == 0 {
        0
    } else {
        ((s.reread as f64 / s.calls as f64) * 100.0).round() as u64
    };
    format!("{} из {} ({pct} %)", s.reread, s.calls)
}

/// Таблица «событие · вызовов · токенов · уже выдавалось» с итогом и строкой доли повторов.
fn table(stats: &BTreeMap<String, Stat>) -> String {
    let mut rows: Vec<(&String, &Stat)> = stats.iter().collect();
    rows.sort_by(|a, b| rank(a.0).cmp(&rank(b.0)).then_with(|| a.0.cmp(b.0)));
    let mut s = String::from("событие\tвызовов\tтокенов\tуже выдавалось\n");
    for (event, st) in &rows {
        let reread = if st.reread > 0 {
            st.reread.to_string()
        } else {
            String::new()
        };
        s.push_str(&format!("{event}\t{}\t{}\t{reread}\n", st.calls, st.tokens));
    }
    let tokens: u64 = rows.iter().map(|(_, st)| st.tokens).sum();
    s.push_str(&format!("итого\t\t{tokens}\t\n"));
    let mut shares = Vec::new();
    // у MCP — из них чтения (`mcp_read`): доля к чтениям MCP, а не ко всем вызовам MCP
    for (event, title) in [("read", "read"), ("mcp_read", "чтения MCP")] {
        if let Some(st) = stats.get(event).filter(|st| st.calls > 0) {
            shares.push(format!("{title} {}", share(st)));
        }
    }
    if !shares.is_empty() {
        s.push_str(&format!("уже выдавалось: {}\n", shares.join("; ")));
    }
    s
}

/// Строка сессии: id, агент, начало и последнее обращение (UTC), эпоха, признак `holdout`.
fn session_line(s: &Value) -> String {
    let mut line = format!(
        "сессия {} · {} · начало {} · последнее {}",
        s["id"].as_str().unwrap_or("?"),
        s["agent"].as_str().unwrap_or("агент неизвестен"),
        agent_time(s["started_at"].as_str().unwrap_or("?")),
        agent_time(s["last_seen_at"].as_str().unwrap_or("?")),
    );
    // эпоха `session_seen` (M6 §7.2): «уже выдавалось» считается в её пределах
    if let Some(e) = s["seen_epoch"].as_u64() {
        line.push_str(&format!(" · эпоха {e}"));
    }
    if s["holdout"].as_bool() == Some(true) {
        line.push_str(" · holdout");
    }
    line.push('\n');
    line
}

/// Сессии выбора `pick` (с `tokens`).
fn fetch(ctx: &Ctx, pick: &Pick) -> Result<Vec<Value>, CliError> {
    let one = |id: &str| -> Result<Vec<Value>, CliError> {
        let v = read_json(
            ctx.client
                .get(&format!("/sessions/{}", encode_segment(id)), &[])?,
        )?;
        Ok(vec![v])
    };
    let last = |n: u32| -> Result<Vec<Value>, CliError> {
        let limit = n.to_string();
        let v = read_json(ctx.client.get(
            "/sessions",
            &[("limit", limit.as_str()), ("tokens", "true")],
        )?)?;
        let items = v["items"].as_array().cloned().unwrap_or_default();
        // сервер до M6 (задача 8b) `tokens=true` не знает и отдаёт сессии без учёта: «итого 0»
        // было бы неправдой
        if items.iter().any(|s| s.get("tokens").is_none()) {
            return Err(CliError::config(
                "сервер не поддерживает учёт по сессиям (GET /sessions?tokens=true): обновите zam-server",
            ));
        }
        Ok(items)
    };
    match pick {
        Pick::Id(id) => one(id),
        Pick::Last(n) => last(*n),
        Pick::Current => match ctx.session.as_deref() {
            Some(id) => one(id),
            None => last(1),
        },
    }
}

/// `zam sessions tokens`.
pub fn tokens(ctx: &Ctx, pick: &Pick, by_holdout: bool) -> Result<(), CliError> {
    let items = fetch(ctx, pick)?;
    let groups: Vec<(&str, Vec<&Value>)> = if by_holdout {
        let (hold, main): (Vec<&Value>, Vec<&Value>) = items
            .iter()
            .partition(|s| s["holdout"].as_bool() == Some(true));
        vec![("main", main), ("holdout", hold)]
    } else {
        vec![("all", items.iter().collect())]
    };
    if ctx.json {
        let g: serde_json::Map<String, Value> = groups
            .iter()
            .map(|(name, list)| {
                let t: serde_json::Map<String, Value> = sum(list.iter().copied())
                    .into_iter()
                    .map(|(e, s)| {
                        (
                            e,
                            json!({"calls": s.calls, "tokens": s.tokens, "reread_calls": s.reread}),
                        )
                    })
                    .collect();
                (
                    name.to_string(),
                    json!({"sessions": list.len(), "tokens": t}),
                )
            })
            .collect();
        return emit_json(&json!({ "items": items, "groups": g }));
    }
    if items.is_empty() {
        return emit("сессий нет\n".as_bytes());
    }
    let mut out = String::new();
    if items.len() == 1 {
        out.push_str(&session_line(&items[0]));
    } else {
        // свежие первыми: последняя — первая в списке
        out.push_str(&format!(
            "сессий: {} · с {} по {}\n",
            items.len(),
            agent_time(items[items.len() - 1]["started_at"].as_str().unwrap_or("?")),
            agent_time(items[0]["last_seen_at"].as_str().unwrap_or("?")),
        ));
    }
    for (name, list) in &groups {
        if by_holdout {
            let title = if *name == "holdout" {
                "контрольная группа (holdout)"
            } else {
                "основная группа"
            };
            out.push_str(&format!("\n{title}: сессий {}\n", list.len()));
        }
        if list.is_empty() {
            continue;
        }
        out.push_str(&table(&sum(list.iter().copied())));
    }
    emit(out.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_orders_events_and_shares() {
        let s = json!({"tokens": {
            "read": {"calls": 4, "tokens": 400, "reread_calls": 1},
            "session_start": {"calls": 1, "tokens": 900},
            "rules": {"calls": 1, "tokens": 300},
            "custom": {"calls": 2, "tokens": 5},
            "mcp": {"calls": 5, "tokens": 120},
            "mcp_read": {"calls": 3, "tokens": 0, "reread_calls": 2},
        }});
        let t = table(&sum([&s]));
        assert_eq!(
            t,
            "событие\tвызовов\tтокенов\tуже выдавалось\n\
             session_start\t1\t900\t\n\
             rules\t1\t300\t\n\
             read\t4\t400\t1\n\
             mcp\t5\t120\t\n\
             mcp_read\t3\t0\t2\n\
             custom\t2\t5\t\n\
             итого\t\t1725\t\n\
             уже выдавалось: read 1 из 4 (25 %); чтения MCP 2 из 3 (67 %)\n"
        );
    }

    #[test]
    fn sum_adds_sessions() {
        let a = json!({"tokens": {"read": {"calls": 1, "tokens": 10, "reread_calls": 1}}});
        let b = json!({"tokens": {"read": {"calls": 2, "tokens": 5}}});
        let m = sum([&a, &b]);
        assert_eq!(
            m["read"],
            Stat {
                calls: 3,
                tokens: 15,
                reread: 1
            }
        );
    }
}
