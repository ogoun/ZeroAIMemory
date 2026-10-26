//! `zam solution access list|requests|approve|reject|revoke` — управление доступом к
//! решению владельцем без WebUI (§11.6); `zam solution rename <имя>` — переименование
//! (`PATCH /solutions/{guid}`, владелец или администратор).
//!
//! Решение — `--solution GUID` или `agent_solution_id` ближайшего `solution.toml`.
//! Пользователь — по имени (как в списке запросов) или id.
use serde_json::{Value, json};

use super::{Ctx, emit, emit_json};
use crate::client::{CliError, EXIT_NOT_FOUND, read_json};
use crate::target::is_guid;

/// Действие над доступом.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Все запросы и участники.
    List,
    /// Только ожидающие запросы.
    Requests,
    Approve {
        user: String,
        ro: bool,
    },
    Reject {
        user: String,
        reason: Option<String>,
    },
    Revoke {
        user: String,
    },
}

/// GUID решения: флаг или `solution.toml`.
fn solution_guid(ctx: &Ctx, flag: Option<&str>) -> Result<String, CliError> {
    match flag {
        Some(g) if is_guid(g.trim()) => Ok(g.trim().to_ascii_lowercase()),
        Some(_) => Err(CliError::input("--solution: UUID с дефисами")),
        None => ctx.solution.as_ref().map(|s| s.id.clone()).ok_or_else(|| {
            CliError::config(
                "решение не задано: --solution GUID или .zeroaimemory/solution.toml в текущем каталоге или выше",
            )
        }),
    }
}

/// Id пользователя по имени или id среди запросов доступа к решению.
fn resolve_user(members: &[Value], raw: &str) -> Result<String, CliError> {
    let raw = raw.trim();
    if let Some(m) = members.iter().find(|m| m["user_id"] == raw) {
        return Ok(m["user_id"].as_str().unwrap_or(raw).to_string());
    }
    let by_name: Vec<&Value> = members
        .iter()
        .filter(|m| {
            m["user_name"]
                .as_str()
                .is_some_and(|n| n.to_lowercase() == raw.to_lowercase())
        })
        .collect();
    match by_name.as_slice() {
        [m] => Ok(m["user_id"].as_str().unwrap_or_default().to_string()),
        [] => Err(CliError::new(
            EXIT_NOT_FOUND,
            format!("пользователь «{raw}» не запрашивал доступ к решению"),
        )),
        _ => Err(CliError::input(format!(
            "имя «{raw}» у нескольких пользователей: укажите id"
        ))),
    }
}

/// Строка таблицы запроса: имя, id, состояние, режим, время запроса.
fn line(m: &Value) -> String {
    format!(
        "{}  {}  {}  {}  {}\n",
        m["user_name"].as_str().unwrap_or("?"),
        m["user_id"].as_str().unwrap_or("?"),
        m["state"].as_str().unwrap_or("?"),
        m["access"].as_str().unwrap_or("?"),
        m["requested_at"].as_str().unwrap_or("?"),
    )
}

/// `zam solution rename <имя> [--solution GUID]`: печатает `<guid>  <новое имя>`.
pub fn rename(ctx: &Ctx, flag: Option<&str>, name: &str) -> Result<(), CliError> {
    let guid = solution_guid(ctx, flag)?;
    let name = name.trim();
    if name.is_empty() {
        return Err(CliError::input("имя решения не может быть пустым"));
    }
    let r = ctx
        .client
        .patch_json(&format!("/solutions/{guid}"), &json!({ "name": name }))?;
    let v = read_json(r)?;
    if ctx.json {
        return emit_json(&v);
    }
    emit(
        format!(
            "{guid}  {}
",
            v["name"].as_str().unwrap_or(name)
        )
        .as_bytes(),
    )
}

pub fn run(ctx: &Ctx, flag: Option<&str>, action: &Action) -> Result<(), CliError> {
    let guid = solution_guid(ctx, flag)?;
    let base = format!("/solutions/{guid}/access");
    let list = read_json(ctx.client.get(&base, &[])?)?;
    let members: Vec<Value> = list["members"].as_array().cloned().unwrap_or_default();
    let (user, verb, body) = match action {
        Action::List | Action::Requests => {
            let shown: Vec<&Value> = members
                .iter()
                .filter(|m| *action == Action::List || m["state"] == "pending")
                .collect();
            if ctx.json {
                return emit_json(&json!({
                    "agent_solution_id": guid,
                    "owner": list["owner"],
                    "members": shown,
                    "pending": list["pending"],
                }));
            }
            if shown.is_empty() {
                let what = if *action == Action::List {
                    "запросов доступа нет"
                } else {
                    "ожидающих запросов нет"
                };
                eprintln!("{what}");
                return Ok(());
            }
            let text: String = shown.iter().map(|m| line(m)).collect();
            return emit(text.as_bytes());
        }
        Action::Approve { user, ro } => (
            user,
            "approve",
            json!({ "access": if *ro { "ro" } else { "rw" } }),
        ),
        Action::Reject { user, reason } => (user, "reject", json!({ "reason": reason })),
        Action::Revoke { user } => (user, "revoke", json!({})),
    };
    let id = resolve_user(&members, user)?;
    let r = ctx
        .client
        .post_json(&format!("{base}/{id}/{verb}"), &body)?;
    let entry = read_json(r)?;
    if ctx.json {
        return emit_json(&entry);
    }
    emit(line(&entry).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_by_name_or_id() {
        let m = vec![
            json!({"user_id": "01AAAAAAAAAAAAAAAAAAAAAAAA", "user_name": "Борис"}),
            json!({"user_id": "01BBBBBBBBBBBBBBBBBBBBBBBB", "user_name": "вера"}),
            json!({"user_id": "01CCCCCCCCCCCCCCCCCCCCCCCC", "user_name": "Вера"}),
        ];
        assert_eq!(
            resolve_user(&m, "борис").unwrap(),
            "01AAAAAAAAAAAAAAAAAAAAAAAA"
        );
        assert_eq!(
            resolve_user(&m, "01CCCCCCCCCCCCCCCCCCCCCCCC").unwrap(),
            "01CCCCCCCCCCCCCCCCCCCCCCCC"
        );
        assert_eq!(
            resolve_user(&m, "вера").unwrap_err().code,
            crate::client::EXIT_INPUT
        );
        assert_eq!(resolve_user(&m, "нет").unwrap_err().code, EXIT_NOT_FOUND);
    }
}
