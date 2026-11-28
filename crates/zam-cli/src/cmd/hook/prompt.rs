//! `zam hook prompt` (`UserPromptSubmit`, бюджет 250 мс; §10.3): `/recall(prompt)` — 0–3
//! подсказки или тишина. Серверу уходят первые [`PROMPT_SEND_MAX`] байт промпта (сервер ищет
//! по первым 512 символам). Каждый промпт освежает `session.json` (текущая сессия для CLI).
//!
//! M4.5: в журнал сессии (ключ `session_journal`) — `Ev::Prompt {len}` (только длина, §9.1);
//! очередь изменённых файлов сессии уходит `POST /files/changed` параллельно с `/recall` в
//! том же бюджете ([`cache::flush_changed`]), очередь учёта выданных токенов — так же
//! ([`cache::flush_tokens`], §9.5).
//!
//! M6.6 §3.3 (замечание 22): пересланное сообщение — отчёт субагента, сообщение агента,
//! уведомление фоновой задачи ([`zam_core::text::is_relayed_message`]) — не слова человека: без
//! `/recall` и без `Ev::Prompt` в журнале сессии; очереди файлов и токенов уходят как прежде
//! **[решение координатора; журнал — решение исполнителя, задача 2]**. Сервер отвечает на такие
//! сообщения тишиной и сам — для клиентов 0.0.12.
//!
//! M7.5 §8.9 п. 3: кэш триггеров `triggers.json` старше [`cache::TRIGGERS_FRESH`] (5 мин; или
//! помеченный устаревшим записью insight через `zam`, другого сервера или решения) хук сверяет с
//! сервером условным `GET /triggers` (`If-None-Match`; обычно 304) — параллельно `/recall` и
//! очередям, в том же сроке хука; не успел или сбой — кэш прежний, строка в журнал. Только при
//! включённом `pre_tool` (кэш нужен ему); сверка идёт и у пересланного сообщения, как очереди.
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
    // отчёт субагента и уведомление фоновой задачи — не промпт человека (M6.6 §3.3)
    let relayed = zam_core::text::is_relayed_message(raw);
    if let Some(id) = session {
        cache::write_current_session(env, id);
        if s.flag_on(Flag::SessionJournal) && !relayed {
            let len = raw.chars().count();
            journal::note(env, id, &Ev::Prompt { len });
        }
    }
    let prompt = Some(raw.trim()).filter(|p| !p.is_empty() && !relayed && s.on(Kind::Prompt));
    let queued =
        env.solution.is_some() && session.is_some_and(|id| cache::changed_path(env, id).is_file());
    let tokens = cache::tokens_queued(env, session);
    // кэш триггеров нужен только `pre-tool` (M7.5 §8.9 п. 3)
    let pre_on = s.on(Kind::PreTool);
    if prompt.is_none() && !queued && !tokens && !(pre_on && cache::triggers_stale(env, None)) {
        return Output::Empty;
    }
    let Some(client) = env.client() else {
        return Output::Empty;
    };
    let refresh = pre_on && cache::triggers_stale(env, Some(client.base()));
    std::thread::scope(|sc| {
        // сверка кэша триггеров — параллельно `/recall`, в общем сроке хука
        let trig = refresh.then(|| sc.spawn(|| cache::refresh_triggers(env, &client, false)));
        let flush = queued.then(|| sc.spawn(|| cache::flush_changed(env, &client, session)));
        // очередь учёта локального вывода (M4.5 §9.5) — тем же сетевым вызовом хука
        let flush_tokens = session
            .filter(|_| tokens)
            .map(|id| sc.spawn(|| cache::flush_tokens(env, &client, id)));
        let out = prompt.map_or(Output::Empty, |p| recall(env, inp, &client, p));
        if let Some(h) = flush
            && h.join().is_err()
        {
            env.log("files/changed: паника");
        }
        if let Some(h) = flush_tokens
            && h.join().is_err()
        {
            env.log("sessions(tokens): паника");
        }
        match trig.map(|h| h.join()) {
            None => {}
            Some(Ok(Ok((_, warnings)))) => {
                for w in warnings {
                    env.log(&format!("триггеры: предупреждение сервера: {w}"));
                }
            }
            Some(Ok(Err(e))) => env.log(&format!("кэш триггеров не обновлён ({})", e.reason())),
            Some(Err(_)) => env.log("кэш триггеров: паника"),
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
        // предел канала агента (ревью 8a M6, C1)
        "output_limit": super::output::output_limit(env.agent, ""),
    });
    match client.post_json("/recall", &body).and_then(read_json) {
        Ok(v) => {
            for w in cache::strings(&v["warnings"]) {
                env.log(&format!("recall(prompt): предупреждение сервера: {w}"));
            }
            // контрольная группа A/B (M6 §12): маркер сессии
            cache::note_holdout(env, inp.session_id.as_deref(), &v);
            Output::context(&inp.event, v["text"].as_str().unwrap_or_default())
        }
        Err(e) => {
            env.log(&format!("recall(prompt): {e}"));
            Output::Empty
        }
    }
}
