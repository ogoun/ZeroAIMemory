//! `zam hook session-start` (`SessionStart`, бюджет 300 мс; §10.3).
//!
//! Параллельно (бюджет общий): `POST /sessions {id, agent, solution}`, `/recall(session_start)`
//! с `source` из stdin и `agent_block_version` (R2), `GET /triggers` с `If-None-Match` (кэш
//! триггеров). Вывод — дайджест `text`; `hooks` ответа запоминаются (R5).
//!
//! Выключенный в клиенте `session_start` гасит только вывод: `/recall` вызывается всё равно —
//! ради `hooks` сервера (fix round 1, m-5), текст отбрасывается; регистрация сессии и кэш
//! триггеров нужны `checkpoint`, MCP (R13) и `pre-tool`. Выключенный на сервере — дайджест,
//! полученный в том же ответе, не выводится.
//! На `source` `compact` и `clear` (контекст агента потерян) показанное `pre-tool` в сессии
//! сбрасывается, факт правок остаётся.
use std::path::Path;

use serde_json::{Value, json};

use super::cache;
use super::input::Input;
use super::output::Output;
use super::{Env, HookFlags, Kind, Settings};
use crate::client::read_json;

/// Предел файла инструкции при поиске маркера блока, байт.
const MD_MAX: u64 = 1024 * 1024;
/// Маркер начала блока ZeroAIMemory в CLAUDE.md/AGENTS.md.
const BEGIN: &str = "<!-- zeroaimemory:begin";

pub fn run(env: &Env, inp: &Input, s: &Settings) -> Output {
    cache::gc_cache(env);
    let session = inp.session_id.as_deref();
    let source = inp.str("source");
    if let Some(id) = session {
        cache::write_current_session(env, id);
        if matches!(source, Some("compact" | "clear")) {
            let mut sh = cache::read_shown(env, id);
            if !sh.shown.is_empty() {
                sh.shown.clear();
                cache::write_shown(env, id, &sh);
            }
        }
    }
    let Some(client) = env.client() else {
        return Output::Empty;
    };
    let solution = env.solution.as_ref().map(|x| x.id.clone());
    let (reg, recall, trig) = std::thread::scope(|sc| {
        let reg = session.map(|id| {
            let body = json!({"id": id, "agent": env.agent.name(), "solution": solution});
            let c = &client;
            sc.spawn(move || c.post_json("/sessions", &body).map(|_| ()))
        });
        let recall = {
            let mut body = json!({
                "event": "session_start",
                "session_id": session,
                "solution": solution,
                "source": source,
            });
            if let Some(v) = block_version(&env.cwd, &env.root, env.agent) {
                body["agent_block_version"] = json!(v);
            }
            let c = &client;
            sc.spawn(move || c.post_json("/recall", &body).and_then(read_json))
        };
        let trig = sc.spawn(|| cache::refresh_triggers(env, &client, false));
        (reg.map(|h| h.join()), recall.join(), trig.join())
    });
    match reg {
        Some(Ok(Err(e))) => env.log(&format!("регистрация сессии: {e}")),
        Some(Err(_)) => env.log("регистрация сессии: паника"),
        _ => {}
    }
    match trig {
        Ok(Ok((_, warnings))) => {
            for w in warnings {
                env.log(&format!("триггеры: предупреждение сервера: {w}"));
            }
        }
        Ok(Err(e)) => env.log(&format!("кэш триггеров не обновлён: {e}")),
        Err(_) => env.log("кэш триггеров: паника"),
    }
    let v: Value = match recall {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            env.log(&format!("recall(session_start): {e}"));
            return Output::Empty;
        }
        Err(_) => {
            env.log("recall(session_start): паника");
            return Output::Empty;
        }
    };
    for w in cache::strings(&v["warnings"]) {
        env.log(&format!(
            "recall(session_start): предупреждение сервера: {w}"
        ));
    }
    let mut server = s.server;
    if let Ok(h) = serde_json::from_value::<HookFlags>(v["hooks"].clone())
        && v["hooks"].is_object()
    {
        cache::write_server_hooks(env, client.base(), h);
        server = h;
    }
    let now = Settings {
        client: s.client,
        server,
    };
    if !now.on(Kind::SessionStart) {
        return Output::Empty;
    }
    Output::context(&inp.event, v["text"].as_str().unwrap_or_default())
}

/// Версия блока инструкции (R2): маркер `<!-- zeroaimemory:begin vN -->` ближайшего
/// CLAUDE.md/AGENTS.md вверх от `cwd` до корня проекта включительно (Codex — сначала
/// AGENTS.md). Нет маркера или версии — `None`.
pub fn block_version(cwd: &Path, root: &Path, agent: super::Agent) -> Option<u32> {
    let names: [&str; 2] = match agent {
        super::Agent::Claude => ["CLAUDE.md", "AGENTS.md"],
        super::Agent::Codex => ["AGENTS.md", "CLAUDE.md"],
    };
    let stop = if cwd.starts_with(root) {
        Some(root)
    } else {
        None
    };
    for dir in cwd.ancestors() {
        for n in names {
            if let Some(v) = marker_version(&dir.join(n)) {
                return Some(v);
            }
        }
        if stop == Some(dir) {
            break;
        }
    }
    None
}

/// Версия из маркера файла: `<!-- zeroaimemory:begin v2 -->` → 2.
fn marker_version(path: &Path) -> Option<u32> {
    let m = std::fs::metadata(path).ok()?;
    if !m.is_file() || m.len() > MD_MAX {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().find_map(|l| {
        let rest = l.trim().strip_prefix(BEGIN)?;
        let rest = rest.trim_start().strip_prefix('v')?;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp/zam-cli-hook")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn block_version_nearest_file() {
        let root = tmp("block-version");
        let sub = root.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            root.join("CLAUDE.md"),
            "# x\n<!-- zeroaimemory:begin v2 -->\nZeroAIMemoryUrl=http://h\n<!-- zeroaimemory:end -->\n",
        )
        .unwrap();
        assert_eq!(
            block_version(&sub, &root, super::super::Agent::Claude),
            Some(2)
        );
        std::fs::write(root.join("a/AGENTS.md"), "<!-- zeroaimemory:begin v1 -->\n").unwrap();
        assert_eq!(
            block_version(&sub, &root, super::super::Agent::Codex),
            Some(1)
        );
        assert_eq!(
            block_version(&sub, &root, super::super::Agent::Claude),
            Some(1)
        );
        // выше корня проекта не ищем
        assert_eq!(
            block_version(&sub, &root.join("a/b"), super::super::Agent::Claude),
            None
        );
        std::fs::write(root.join("a/b/CLAUDE.md"), "<!-- zeroaimemory:begin -->\n").unwrap();
        assert_eq!(
            block_version(&sub, &root.join("a/b"), super::super::Agent::Claude),
            None
        );
    }
}
