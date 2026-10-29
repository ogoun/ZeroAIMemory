//! `zam hook checkpoint` (`Stop`, `PreCompact`; бюджет 100 мс; §10.3, R1): одно напоминание
//! за сессию «запиши итоги в changelog/insights» — только на `Stop`, если в сессии были правки
//! (маркер `edits-<сессия>`, пишет `pre-tool`) и `GET /sessions/{id}` даёт `memory_writes == 0`.
//! Сервер недоступен (сеть, тайм-аут) — не напоминать (R1). Сессии на сервере нет (404), а
//! маркер правок есть — напоминать: ложное напоминание раз за сессию дешевле пропущенного (fix
//! round 2; отменяет m-4 round 1).
//! Задачи журнала (M4.5 §3): `GET /sessions/{id}` несёт `claimed_tasks`; есть задача `in_progress`,
//! захваченная сессией, и маркер правок — единственное напоминание содержит «обнови next_step
//! задачи k:<id>» и приходит, даже если в память уже писали (сам захват — запись в память).
//! Пары «ошибка → исправление» (M4.5 §9.3, ключ `session_journal`): в журнале сессии есть пара
//! ([`journal::fixes_at`]), а после последней не было записи insight — то же напоминание
//! содержит «запиши gotcha/failure с error_regex: <ошибка> (правки: <файлы>)» ([`fix_line`]) и
//! приходит, даже если в память уже писали другое.
//! `stop_hook_active == true` — ничего. Вывод — `{"decision": "block", "reason": …}`.
//! На `PreCompact` — пусто (fix round 1, m-3: ⚠️ поддержку контекста на `PreCompact` у агентов
//! проверить на машине владельца).
//! M4.5: `PreCompact` пишет в журнал сессии `Ev::Compact {trigger}` (ключ `session_journal`);
//! на `Stop` и `PreCompact` очередь изменённых файлов сессии уходит `POST /files/changed`
//! параллельно с `GET /sessions/{id}` в том же бюджете ([`cache::flush_changed`]).
use super::cache;
use super::input::Input;
use super::journal::{self, Ev};
use super::output::Output;
use super::{Env, Flag, Kind, Settings, encode_segment};
use crate::client::{Client, EXIT_NOT_FOUND, read_json};

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
    let session = inp.session_id.as_deref();
    if inp.event == "PreCompact"
        && let Some(id) = session
        && s.flag_on(Flag::SessionJournal)
    {
        let trigger = inp
            .str("trigger")
            .unwrap_or("auto")
            .chars()
            .take(32)
            .collect();
        journal::note(env, id, &Ev::Compact { trigger });
    }
    let queued =
        env.solution.is_some() && session.is_some_and(|id| cache::changed_path(env, id).is_file());
    let tokens = cache::tokens_queued(env, session);
    let remind = needs_reminder(env, inp, s);
    if remind.is_none() && !queued && !tokens {
        return Output::Empty;
    }
    let Some(client) = env.client() else {
        return Output::Empty;
    };
    std::thread::scope(|sc| {
        let flush = queued.then(|| sc.spawn(|| cache::flush_changed(env, &client, session)));
        // очередь учёта локального вывода (M4.5 §9.5)
        let flush_tokens = session
            .filter(|_| tokens)
            .map(|id| sc.spawn(|| cache::flush_tokens(env, &client, id)));
        let out = match remind {
            Some((id, sh)) => {
                let fix = s
                    .flag_on(Flag::SessionJournal)
                    .then(|| fix_line(&journal::read(&env.root, id)))
                    .flatten();
                reminder(env, &client, id, sh, fix)
            }
            None => Output::Empty,
        };
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
        out
    })
}

/// Напоминание возможно (`Stop`, включено, правки были, ещё не напоминали): сессия и
/// `shown`-файл; иначе `None`.
fn needs_reminder<'a>(env: &Env, inp: &'a Input, s: &Settings) -> Option<(&'a str, cache::Shown)> {
    if inp.event != "Stop"
        || !s.on(Kind::Checkpoint)
        || inp.v["stop_hook_active"].as_bool() == Some(true)
    {
        return None;
    }
    let session = inp.session_id.as_deref()?;
    if !cache::had_edits(env, session) {
        return None;
    }
    let sh = cache::read_shown(env, session);
    (!sh.reminded).then_some((session, sh))
}

/// Строка о последней паре «ошибка → исправление» (§9.3), если после неё в сессии не было
/// записи insight (`Ev::MemoryWrite {kind: insight}`); пусто — пар нет или уже записано.
pub fn fix_line(events: &[(journal::Timestamp, Ev)]) -> Option<String> {
    let last_write = events
        .iter()
        .filter(|(_, e)| matches!(e, Ev::MemoryWrite { kind } if kind == "insight"))
        .map(|(t, _)| *t)
        .max();
    let (at, fix) = journal::fixes_at(events).pop()?;
    if last_write.is_some_and(|w| w >= at) {
        return None;
    }
    Some(format!(
        "ZeroAIMemory: запиши gotcha/failure с error_regex: {} (правки: {}) — `zam log insight` \
         (MCP: memory_log).",
        journal::fix_error(&fix),
        fix.files.join(", ")
    ))
}

/// `GET /sessions/{id}` и решение о напоминании (R1, M4.5 §3, §9.3).
fn reminder(
    env: &Env,
    client: &Client,
    session: &str,
    mut sh: cache::Shown,
    fix: Option<String>,
) -> Output {
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
    let lines: Vec<String> = (writes == Some(0))
        .then(|| REMINDER.to_string())
        .into_iter()
        .chain(tasks)
        .chain(fix)
        .collect();
    if lines.is_empty() {
        return Output::Empty;
    }
    let reason = lines.join("\n");
    sh.reminded = true;
    cache::write_shown(env, session, &sh);
    Output::Block { reason }
}
