//! `zam hook prompt` (`UserPromptSubmit`, бюджет 250 мс; §10.3): `/recall(prompt)` — 0–3
//! подсказки или тишина. Серверу уходят первые [`PROMPT_SEND_MAX`] байт промпта (сервер ищет
//! по первым 512 символам). Каждый промпт освежает `session.json` (текущая сессия для CLI).
//!
//! M4.5: в журнал сессии (ключ `session_journal`) — `Ev::Prompt {len}` (только длина, §9.1);
//! очередь изменённых файлов сессии уходит `POST /files/changed` параллельно с `/recall` в
//! том же бюджете ([`cache::flush_changed`]).
use serde_json::json;

use super::cache;
use super::input::Input;
use super::journal::{self, Ev};
use super::output::Output;
use super::{Env, Flag, Kind, Settings, head_bytes};
use crate::client::read_json;

/// Предел промпта в запросе, байт (512 символов — не больше 2 КиБ UTF-8).
pub const PROMPT_SEND_MAX: usize = 2048;

pub fn run(env: &Env, inp: &Input, s: &Settings) -> Output {
    let session = inp.session_id.as_deref();
    let raw = inp.str("prompt").unwrap_or_default();
    if let Some(id) = session {
        cache::write_current_session(env, id);
        if s.flag_on(Flag::SessionJournal) {
            let len = raw.chars().count();
            journal::note(env, id, &Ev::Prompt { len });
        }
    }
    let prompt = Some(raw.trim()).filter(|p| !p.is_empty() && s.on(Kind::Prompt));
    let queued =
        env.solution.is_some() && session.is_some_and(|id| cache::changed_path(env, id).is_file());
    if prompt.is_none() && !queued {
        return Output::Empty;
    }
    let Some(client) = env.client() else {
        return Output::Empty;
    };
    std::thread::scope(|sc| {
        let flush = queued.then(|| sc.spawn(|| cache::flush_changed(env, &client, session)));
        let out = prompt.map_or(Output::Empty, |p| recall(env, inp, &client, p));
        if let Some(h) = flush
            && h.join().is_err()
        {
            env.log("files/changed: паника");
        }
        out
    })
}

fn recall(env: &Env, inp: &Input, client: &crate::client::Client, prompt: &str) -> Output {
    let body = json!({
        "event": "prompt",
        "session_id": inp.session_id,
        "solution": env.solution.as_ref().map(|x| x.id.clone()),
        "prompt": head_bytes(prompt, PROMPT_SEND_MAX),
    });
    match client.post_json("/recall", &body).and_then(read_json) {
        Ok(v) => {
            for w in cache::strings(&v["warnings"]) {
                env.log(&format!("recall(prompt): предупреждение сервера: {w}"));
            }
            Output::context(&inp.event, v["text"].as_str().unwrap_or_default())
        }
        Err(e) => {
            env.log(&format!("recall(prompt): {e}"));
            Output::Empty
        }
    }
}
