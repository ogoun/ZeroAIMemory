//! `zam hook session-start` (`SessionStart`, бюджет 300 мс; §10.3).
//!
//! Параллельно (бюджет общий): `POST /sessions {id, agent, solution}`, `/recall(session_start)`
//! с `source` из stdin и `agent_block_version` (R2), `GET /triggers` с `If-None-Match` (кэш
//! триггеров). Вывод — дайджест `text`; `hooks` ответа запоминаются (R5).
//!
//! Выключенный в клиенте `session_start` гасит только вывод: `/recall` вызывается всё равно —
//! ради `hooks` сервера (fix round 1, m-5) и с `record: false` (без `session_seen` и `usage`,
//! fix round 2), текст отбрасывается; регистрация сессии и кэш
//! триггеров нужны `checkpoint`, MCP (R13) и `pre-tool`. Выключенный на сервере — дайджест,
//! полученный в том же ответе, не выводится.
//! На `source` `compact` и `clear` (контекст агента потерян) показанное `pre-tool` в сессии
//! сбрасывается, факт правок остаётся.
//! M4.5: в том же параллельном наборе — `GET /file-refs` с `If-None-Match` (кэш знания о файле
//! `file-refs.json`; ключ `file_context` выключен — не запрашивается) и отправка очередей
//! изменённых файлов всех сессий проекта (`POST /files/changed`, [`cache::flush_changed`]).
//! Очереди уходят **до** `GET /file-refs` (задача 9, замечание ревью задачи 8): индекс берётся
//! уже с устарелостью, которую выставил сервер по отправленным хэшам.
//! M4.5 §9.2: на `source` `compact` и `resume` сервер строит пакет восстановления, а хук
//! дописывает после его текста сводку журнала сессии ([`journal::render_summary`], ключи
//! `recovery` и `session_journal`); общий предел — прежний ([`output::with_tail`]). Сервер
//! недоступен — только сводка; журнала нет — только текст сервера.
use std::path::Path;

use serde_json::{Value, json};

use super::cache;
use super::input::Input;
use super::journal;
use super::output::{self, Output};
use super::{Env, Flag, HookFlags, Kind, Settings};
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
        // просроченные запуски субагентов (§9.4) не ждут следующего `subagent-start`
        cache::prune_spawns(env, id);
        if matches!(source, Some("compact" | "clear")) {
            let mut sh = cache::read_shown(env, id);
            if !sh.shown.is_empty() {
                sh.shown.clear();
                cache::write_shown(env, id, &sh);
            }
        }
    }
    // сводка журнала до сжатия (§9.2): только после `compact`/`resume`
    let summary = |st: &Settings| -> String {
        match session {
            Some(id)
                if matches!(source, Some("compact" | "resume"))
                    && st.flag_on(Flag::Recovery)
                    && st.flag_on(Flag::SessionJournal) =>
            {
                journal::render_summary(&journal::summarize(&journal::read(&env.root, id)))
            }
            _ => String::new(),
        }
    };
    // сервер недоступен — только сводка
    let local = |st: &Settings| -> Output {
        if !st.on(Kind::SessionStart) {
            return Output::Empty;
        }
        Output::context(&inp.event, &summary(st))
    };
    let Some(client) = env.client() else {
        return local(s);
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
            // `recovery` выключен в клиенте — сервер не строит пакет (по `source` он его и
            // выбирает; иначе `source` только в журнал DEBUG)
            if s.client.get(Flag::Recovery) == Some(false) {
                body["source"] = Value::Null;
            }
            if !s.client_on(Kind::SessionStart) {
                body["record"] = json!(false);
            }
            if let Some(v) = block_version(&env.cwd, &env.root, env.agent) {
                body["agent_block_version"] = json!(v);
            }
            let c = &client;
            sc.spawn(move || c.post_json("/recall", &body).and_then(read_json))
        };
        let trig = sc.spawn(|| cache::refresh_triggers(env, &client, false));
        // очередь учёта локального вывода прошлой сессии хуков (M4.5 §9.5)
        let tokens = session
            .filter(|_| cache::tokens_queued(env, session))
            .map(|id| sc.spawn(|| cache::flush_tokens(env, &client, id)));
        // сначала очереди изменённых файлов, затем индекс ссылок в остатке бюджета: иначе
        // индекс может прийти без устарелости, которую выставит эта же отправка
        let want_refs = s.flag_on(Flag::FileContext) && env.solution.is_some();
        let c = &client;
        let refs = sc.spawn(move || {
            cache::flush_changed(env, c, None);
            want_refs.then(|| cache::refresh_file_refs(env, c))
        });
        match refs.join() {
            Ok(Some(Err(e))) => env.log(&format!("кэш file-refs не обновлён: {e}")),
            Err(_) => env.log("files/changed, file-refs: паника"),
            _ => {}
        }
        if let Some(h) = tokens
            && h.join().is_err()
        {
            env.log("sessions(tokens): паника");
        }
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
            return local(s);
        }
        Err(_) => {
            env.log("recall(session_start): паника");
            return local(s);
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
    let text = output::with_tail(
        v["text"].as_str().unwrap_or_default(),
        &summary(&now),
        env.agent,
    );
    Output::context(&inp.event, &text)
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
    // BOM в начале файла (fix round 1, I-1)
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
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
