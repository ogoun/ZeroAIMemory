//! `zam hook subagent-start` (`SubagentStart` Claude Code и Codex, бюджет 300 мс; M4.5 §9.4).
//!
//! - stdin: `session_id` (сессия родителя), `agent_id`, `agent_type`;
//! - задача — старейшая запись очереди `spawn-<сессия>.json` с тем же `agent_type` (её пишет
//!   `pre-tool` на `Agent`/`Task`, [`cache::take_spawn`]); нет — пакет без `task`
//!   (мини-дайджест сервера);
//! - параллельно в общем бюджете: `POST /sessions {id: "<session>:<agent_id>", parent_id,
//!   agent_type, agent, solution}` и `POST /recall {event: "brief", session_id, parent_id,
//!   solution, task}`. Id длиннее 128 символов — `sub-<32 hex blake3>`;
//! - журнал сессии родителя — `subagent {agent_type, agent_id}` (ключ `session_journal`);
//! - вывод — `additionalContext` с `hookEventName: SubagentStart`. У Claude текст попадает в
//!   контекст субагента (документировано); у Codex адресат не подтверждён — ⚠️ проверить на
//!   машине владельца.
//!
//! Ключ `subagent_start` выключен — тишина и без сети (запись очереди берётся всё равно, чтобы
//! не досталась следующему субагенту). Сервер недоступен, тайм-аут, нет сессии — тишина.
use serde_json::{Value, json};

use super::cache;
use super::input::Input;
use super::journal::{self, Ev};
use super::output::Output;
use super::{Env, Flag, Kind, Settings};
use crate::client::read_json;

/// Предел id сессии на сервере, символов.
const SESSION_ID_MAX: usize = 128;

/// Id сессии субагента: `<session>:<agent_id>`, длиннее предела — `sub-<32 hex blake3>`.
pub fn subagent_session_id(session: &str, agent_id: &str) -> String {
    let id = format!("{session}:{agent_id}");
    if id.chars().count() <= SESSION_ID_MAX {
        return id;
    }
    let h = blake3::hash(id.as_bytes()).to_hex();
    format!("sub-{}", &h[..32])
}

/// Строковое поле stdin без пробелов по краям, до 128 символов без управляющих; пусто — `None`.
fn field(inp: &Input, key: &str) -> Option<String> {
    let v: String = inp
        .str(key)?
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(SESSION_ID_MAX)
        .collect();
    (!v.is_empty()).then_some(v)
}

pub fn run(env: &Env, inp: &Input, s: &Settings) -> Output {
    let Some(session) = inp.session_id.as_deref() else {
        return Output::Empty;
    };
    let agent_type = field(inp, "agent_type").unwrap_or_default();
    let agent_id = field(inp, "agent_id");
    if s.flag_on(Flag::SessionJournal) {
        let ev = Ev::Subagent {
            agent_type: agent_type.clone(),
            agent_id: agent_id.clone().unwrap_or_default(),
        };
        if let Err(e) = journal::append(&env.root, session, &ev) {
            env.log(&format!("журнал сессии: {e}"));
        }
    }
    let spawn = cache::take_spawn(env, session, &agent_type);
    if !s.on(Kind::SubagentStart) {
        return Output::Empty;
    }
    let Some(client) = env.client() else {
        return Output::Empty;
    };
    let solution = env.solution.as_ref().map(|x| x.id.clone());
    let sub = agent_id.as_deref().map(|a| subagent_session_id(session, a));
    let agent_type = (!agent_type.is_empty()).then_some(agent_type);
    let (reg, recall) = std::thread::scope(|sc| {
        let reg = sub.as_ref().map(|id| {
            let body = json!({"id": id, "parent_id": session, "agent_type": agent_type,
                              "agent": env.agent.name(), "solution": solution});
            let c = &client;
            sc.spawn(move || c.post_json("/sessions", &body).map(|_| ()))
        });
        let body = json!({
            "event": "brief",
            // без `agent_id` сессии субагента нет — показ учитывается в сессии родителя
            "session_id": sub.as_deref().unwrap_or(session),
            "parent_id": session,
            "solution": solution,
            "task": spawn.as_ref().map(|x| x.task.as_str()).filter(|t| !t.trim().is_empty()),
        });
        let c = &client;
        let recall = sc.spawn(move || c.post_json("/recall", &body).and_then(read_json));
        (reg.map(|h| h.join()), recall.join())
    });
    match reg {
        Some(Ok(Err(e))) => env.log(&format!("регистрация сессии субагента: {e}")),
        Some(Err(_)) => env.log("регистрация сессии субагента: паника"),
        _ => {}
    }
    let v: Value = match recall {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            env.log(&format!("recall(brief): {e}"));
            return Output::Empty;
        }
        Err(_) => {
            env.log("recall(brief): паника");
            return Output::Empty;
        }
    };
    for w in cache::strings(&v["warnings"]) {
        env.log(&format!("recall(brief): предупреждение сервера: {w}"));
    }
    Output::context(&inp.event, v["text"].as_str().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subagent_id_within_limit() {
        assert_eq!(subagent_session_id("s1", "a7"), "s1:a7");
        let long = "x".repeat(128);
        let id = subagent_session_id(&long, "a7");
        assert!(id.starts_with("sub-") && id.len() == 36, "{id}");
        assert_eq!(id, subagent_session_id(&long, "a7"));
        assert_ne!(id, subagent_session_id(&long, "a8"));
    }
}
