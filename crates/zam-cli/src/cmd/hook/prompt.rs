//! `zam hook prompt` (`UserPromptSubmit`, бюджет 250 мс; §10.3): `/recall(prompt)` — 0–3
//! подсказки или тишина. Серверу уходят первые [`PROMPT_SEND_MAX`] байт промпта (сервер ищет
//! по первым 512 символам). Каждый промпт освежает `session.json` (текущая сессия для CLI).
use serde_json::json;

use super::cache;
use super::input::Input;
use super::output::Output;
use super::{Env, Kind, Settings, head_bytes};
use crate::client::read_json;

/// Предел промпта в запросе, байт (512 символов — не больше 2 КиБ UTF-8).
pub const PROMPT_SEND_MAX: usize = 2048;

pub fn run(env: &Env, inp: &Input, s: &Settings) -> Output {
    if let Some(id) = inp.session_id.as_deref() {
        cache::write_current_session(env, id);
    }
    if !s.on(Kind::Prompt) {
        return Output::Empty;
    }
    let Some(prompt) = inp.str("prompt").map(str::trim).filter(|p| !p.is_empty()) else {
        return Output::Empty;
    };
    let Some(client) = env.client() else {
        return Output::Empty;
    };
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
