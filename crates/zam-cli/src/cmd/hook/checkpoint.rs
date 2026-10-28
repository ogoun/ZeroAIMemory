//! `zam hook checkpoint` (`Stop`, `PreCompact`; бюджет 100 мс; §10.3, R1): одно напоминание
//! за сессию «запиши итоги в changelog/insights» — только на `Stop`, если в сессии были правки
//! (маркер `edits-<сессия>`, пишет `pre-tool`) и `GET /sessions/{id}` даёт `memory_writes == 0`.
//! Сервер недоступен (сеть, тайм-аут) — не напоминать (R1). Сессии на сервере нет (404), а
//! маркер правок есть — напоминать: ложное напоминание раз за сессию дешевле пропущенного (fix
//! round 2; отменяет m-4 round 1).
//! Задачи журнала (M4.5 §3): `GET /sessions/{id}` несёт `claimed_tasks`; есть задача `in_progress`,
//! захваченная сессией, и маркер правок — единственное напоминание содержит «обнови next_step
//! задачи k:<id>» и приходит, даже если в память уже писали (сам захват — запись в память).
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

/// Строка про next_step захваченных задач `in_progress` (до 3); пусто — таких нет.
pub fn task_line(v: &serde_json::Value) -> Option<String> {
    let items: Vec<String> = v["claimed_tasks"]
        .as_array()?
        .iter()
        .filter(|t| t["status"] == "in_progress")
        .filter_map(|t| {
            let id = t["id"].as_str()?;
            let title = t["title"].as_str().unwrap_or("");
            Some(format!(
                "k:{id} («{}»)",
                title.chars().take(60).collect::<String>()
            ))
        })
        .take(3)
        .collect();
    (!items.is_empty()).then(|| {
        format!(
            "ZeroAIMemory: обнови next_step задачи {} — `zam task status <k:id> in_progress --next \"…\"` (MCP: memory_update_entry action=status); закончена — status done.",
            items.join(", ")
        )
    })
}

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
    let (writes, tasks) = match client.get(&format!("/sessions/{}", encode_segment(session)), &[]) {
        Ok(r) => match read_json(r) {
            Ok(v) => (v["memory_writes"].as_i64(), task_line(&v)),
            Err(e) => {
                env.log(&format!("сессия: {e}"));
                return Output::Empty;
            }
        },
        Err(e) if e.code == EXIT_NOT_FOUND => (Some(0), None),
        Err(e) => {
            env.log(&format!("сессия: {e}"));
            return Output::Empty;
        }
    };
    let reason = match (writes == Some(0), tasks) {
        (true, None) => REMINDER.to_string(),
        (true, Some(t)) => format!(
            "{REMINDER}
{t}"
        ),
        (false, Some(t)) => t,
        (false, None) => return Output::Empty,
    };
    sh.reminded = true;
    cache::write_shown(env, session, &sh);
    Output::Block { reason }
}
