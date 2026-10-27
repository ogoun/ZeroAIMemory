//! `zam hook checkpoint` (`Stop`, `PreCompact`; бюджет 100 мс; §10.3, R1): одно напоминание
//! за сессию «запиши итоги в changelog/insights» — только на `Stop`, если в сессии были правки
//! (маркер `edits-<сессия>`, пишет `pre-tool`) и `GET /sessions/{id}` даёт `memory_writes == 0`.
//! Сервер недоступен или сессии на сервере нет (404) — не напоминать (fix round 1, m-4).
//! `stop_hook_active == true` — ничего. Вывод — `{"decision": "block", "reason": …}`.
//! На `PreCompact` — пусто (fix round 1, m-3: ⚠️ поддержку контекста на `PreCompact` у агентов
//! проверить на машине владельца).
use super::cache;
use super::input::Input;
use super::output::Output;
use super::{Env, Kind, Settings, encode_segment};
use crate::client::{EXIT_NOT_FOUND, read_json};

/// Текст напоминания.
pub const REMINDER: &str = "ZeroAIMemory: в этой сессии были правки файлов, а в память ничего не \
записано. Если сделано что-то значимое, запиши итоги: изменение — `zam log changelog` \
(MCP: memory_log), открытие, ловушку или провал — `zam log insight`. Если записывать нечего — \
просто заверши работу.";

pub fn run(env: &Env, inp: &Input, s: &Settings) -> Output {
    if inp.event != "Stop"
        || !s.on(Kind::Checkpoint)
        || inp.v["stop_hook_active"].as_bool() == Some(true)
    {
        return Output::Empty;
    }
    let Some(session) = inp.session_id.as_deref() else {
        return Output::Empty;
    };
    if !cache::had_edits(env, session) {
        return Output::Empty;
    }
    let mut sh = cache::read_shown(env, session);
    if sh.reminded {
        return Output::Empty;
    }
    let Some(client) = env.client() else {
        return Output::Empty;
    };
    let writes = match client.get(&format!("/sessions/{}", encode_segment(session)), &[]) {
        Ok(r) => match read_json(r) {
            Ok(v) => v["memory_writes"].as_i64(),
            Err(e) => {
                env.log(&format!("сессия: {e}"));
                return Output::Empty;
            }
        },
        Err(e) if e.code == EXIT_NOT_FOUND => {
            env.log("сессии нет на сервере — без напоминания");
            return Output::Empty;
        }
        Err(e) => {
            env.log(&format!("сессия: {e}"));
            return Output::Empty;
        }
    };
    if writes != Some(0) {
        return Output::Empty;
    }
    sh.reminded = true;
    cache::write_shown(env, session, &sh);
    Output::Block {
        reason: REMINDER.to_string(),
    }
}
