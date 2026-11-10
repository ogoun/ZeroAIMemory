//! `zam hook checkpoint` (`Stop`, `PreCompact`, `PostCompact`, `SessionEnd`; бюджет 100 мс; §10.3,
//! R1; M6 §8а): одно напоминание
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
//! M6 §7.2: на `PreCompact` — новая эпоха `session_seen` отдельным `POST /sessions {id,
//! new_epoch: true}` параллельно с очередями, в том же бюджете ([`new_epoch`]): выданное до
//! сжатия контекста больше не считается выданным. Старый сервер отвечает 400 (неизвестное
//! поле) — хук молча продолжает без эпохи.
//! M6 §12: `hooks.checkpoint_impact = true` (клиент, по умолчанию выключен) — к напоминанию
//! `Stop` до [`IMPACT_LINES_MAX`] строк «правка затрагивает `<путь>[#<символ>]` → i:<id> «…»»
//! **внутри** того же единственного напоминания ([`impact_lines`]). Вход — пути правок сессии
//! из журнала (`Ev::Edit`, новые первыми) и из очереди изменённых файлов (с хэшами символов, если
//! они есть), до [`IMPACT_FILES_MAX`] файлов ([`impact_files`]); оба читает поток влияния, очередь —
//! до её отправки (`flush_changed` ждёт этого до [`IMPACT_READ_WAIT`]). `POST /code/impact` идёт
//! параллельно с `GET /sessions/{id}` со своим сроком [`IMPACT_MS`] внутри бюджета хука; не
//! ответил — без строк (причина — в `hooks.log`). Сами строки напоминания не вызывают
//! (`decision: block` — по прежним условиям); сессия контрольной группы A/B (`holdout` в
//! `GET /sessions/{id}`) их не получает.
//! M5.6 §5: были правки в сессии — после запросов хука запуск фоновой
//! `zam code sync --background`, если пора ([`crate::cmd::code::auto::maybe_spawn_sync`]).
//! M6 §8а: снимок сессии из журнала агента ([`snapshot_queue`]) — в своём потоке параллельно
//! остальным запросам хука, со сроком `hooks.snapshot_ms` на сборку и `PUT` в остатке бюджета:
//! `decision: block` у `Stop` его не ждёт дольше этого. `PostCompact` и `SessionEnd` — без вывода;
//! `SessionEnd` ставит сессии `ended_at` (`PATCH /sessions/{id} {ended: true}`) и при выключенном
//! снимке.
use std::time::Duration;

use serde_json::{Value, json};

use super::cache;
use super::input::Input;
use super::journal::{self, Ev};
use super::output::Output;
use super::snapshot_queue;
use super::{Env, Flag, Kind, Settings, encode_segment};
use crate::client::{Client, EXIT_NOT_FOUND, read_json};

/// Срок `POST /code/impact` внутри бюджета `checkpoint` (100 мс), мс (M6 §12).
pub const IMPACT_MS: u64 = 60;
/// Файлов во входе влияния.
pub const IMPACT_FILES_MAX: usize = 20;
/// Строк влияния в напоминании.
pub const IMPACT_LINES_MAX: usize = 3;
/// Предел тела запроса влияния, байт: больше — у самых старых файлов снимаются символы.
const IMPACT_BODY_MAX: usize = 64 * 1024;
/// Предел заголовка записи в строке влияния, символов.
const IMPACT_TITLE_MAX: usize = 60;
/// Ожидание отправки очереди изменённых, пока поток влияния её прочтёт (обычно — микросекунды).
const IMPACT_READ_WAIT: Duration = Duration::from_millis(50);

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
    let out = checkpoint(env, inp, s);
    // карта кода (M5.6 §5): после правок сессии — фоновая синхронизация, если пора
    if inp
        .session_id
        .as_deref()
        .is_some_and(|id| cache::had_edits(env, id))
    {
        crate::cmd::code::auto::maybe_spawn_sync(env, s);
    }
    out
}

/// Журнал, очереди и напоминание (см. модуль).
fn checkpoint(env: &Env, inp: &Input, s: &Settings) -> Output {
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
    // влияние диффа (M6 §12): только к напоминанию и не в контрольной группе (её вывод пуст)
    let impact_in = match (remind.as_ref(), env.solution.as_ref()) {
        (Some((id, _)), Some(sol)) if s.checkpoint_impact() && !cache::is_holdout(env, id) => {
            Some((sol.id.clone(), *id))
        }
        _ => None,
    };
    // новая эпоха `session_seen` на сжатии контекста (M6 §7.2)
    let epoch = session.filter(|_| inp.event == "PreCompact");
    // снимок сессии и конец сессии (M6 §8а). Решение о снимке (`snapst-*`, журнал сессии), сам
    // снимок и отправка его очереди — целиком в отдельном потоке, **не** на пути сетевой части:
    // очередь изменённых файлов, напоминание и эпоха не ждут ни одного файла снимка
    let end = session.filter(|_| inp.event == "SessionEnd");
    let snap = session
        .filter(|_| s.session_snapshot())
        .map(|_| snapshot_queue::spawn_checkpoint(env, inp, s));
    if snap.is_none() && end.is_none() {
        return checkpoint_net(env, inp, s, remind, impact_in, queued, tokens, epoch);
    }
    let out = std::thread::scope(|sc| {
        let ended = end.map(|id| sc.spawn(move || snapshot_queue::end_session(env, id)));
        let out = checkpoint_net(env, inp, s, remind, impact_in, queued, tokens, epoch);
        if let Some(h) = ended
            && h.join().is_err()
        {
            env.log("sessions(ended): паника");
        }
        out
    });
    if let Some(done) = snap {
        snapshot_queue::wait_snapshot(env, s, &done);
    }
    out
}

/// Сетевая часть `checkpoint`: очереди, эпоха, напоминание (см. модуль).
#[allow(clippy::too_many_arguments)]
fn checkpoint_net(
    env: &Env,
    inp: &Input,
    s: &Settings,
    remind: Option<(&str, cache::Shown)>,
    impact_in: Option<(String, &str)>,
    queued: bool,
    tokens: bool,
    epoch: Option<&str>,
) -> Output {
    let session = inp.session_id.as_deref();
    if remind.is_none() && !queued && !tokens && epoch.is_none() {
        return Output::Empty;
    }
    let Some(client) = env.client() else {
        return Output::Empty;
    };
    std::thread::scope(|sc| {
        // влияние диффа — в своём потоке параллельно с `GET /sessions/{id}`, со своим сроком;
        // очередь изменённых он читает до её отправки (отправка забирает файл очереди)
        let (read_tx, read_rx) = std::sync::mpsc::channel::<()>();
        let impact = impact_in.map(|(sol, id)| {
            let journal_on = s.flag_on(Flag::SessionJournal);
            sc.spawn(move || {
                let queued = cache::peek_changed(env, id);
                let _ = read_tx.send(());
                let events = if journal_on {
                    journal::read(&env.root, id)
                } else {
                    Vec::new()
                };
                let files = impact_files(&events, &queued);
                if files.is_empty() {
                    return Vec::new();
                }
                impact(env, &sol, files)
            })
        });
        let wait_read = impact.is_some();
        let c = &client;
        let flush = queued.then(|| {
            sc.spawn(move || {
                if !wait_read {
                    return cache::flush_changed(env, c, session);
                }
                let _ = read_rx.recv_timeout(IMPACT_READ_WAIT);
                // после ожидания — новым клиентом по остатку бюджета (повторное ревью, m4)
                if let Some(fresh) = env.client() {
                    cache::flush_changed(env, &fresh, session);
                }
            })
        });
        // очередь учёта локального вывода (M4.5 §9.5)
        let flush_tokens = session
            .filter(|_| tokens)
            .map(|id| sc.spawn(|| cache::flush_tokens(env, &client, id)));
        let bump = epoch.map(|id| sc.spawn(|| new_epoch(env, &client, id)));
        let out = match remind {
            Some((id, sh)) => {
                let fix = s
                    .flag_on(Flag::SessionJournal)
                    .then(|| fix_line(&journal::read(&env.root, id)))
                    .flatten();
                let lines = || match impact {
                    Some(h) => h.join().unwrap_or_else(|_| {
                        env.log("code/impact: паника");
                        Vec::new()
                    }),
                    None => Vec::new(),
                };
                reminder(env, &client, id, sh, fix, lines)
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
        if let Some(h) = bump
            && h.join().is_err()
        {
            env.log("sessions(new_epoch): паника");
        }
        out
    })
}

/// Новая эпоха `session_seen` сессии (M6 §7.2; `PreCompact`, а также `session-start` при
/// выключенном в клиенте `recovery`): отдельный запрос, чтобы старый сервер
/// (`deny_unknown_fields` тела) не потерял ничего другого. Молча — только 400 старого сервера
/// про неизвестное поле `new_epoch` ([`old_server_epoch`]); прочее — в `hooks.log` с причиной:
/// `sessions(new_epoch): HTTP <код>: …` (сервер ответил — эпохи нет) или `sessions(new_epoch):
/// сеть: …` (сеть, срок запроса — сервер мог успеть её начать).
pub fn new_epoch(env: &Env, client: &Client, session: &str) {
    let body = serde_json::json!({"id": session, "new_epoch": true});
    match client.post_json("/sessions", &body) {
        Ok(_) => {}
        Err(e) if old_server_epoch(&e) => {}
        Err(e) => match e.http_status() {
            Some(code) => env.log(&format!("sessions(new_epoch): HTTP {code}: {e}")),
            None => env.log(&format!("sessions(new_epoch): сеть: {e}")),
        },
    }
}

/// 400 старого сервера (v0.0.10-m5.7) на неизвестное поле `new_epoch` тела `POST /sessions`.
fn old_server_epoch(e: &crate::client::CliError) -> bool {
    e.http_status() == Some(400)
        && e.problem
            .as_ref()
            .and_then(|p| p["detail"].as_str())
            .is_some_and(|d| d.contains("new_epoch"))
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

/// `GET /sessions/{id}` и решение о напоминании (R1, M4.5 §3, §9.3); `impact` — строки влияния
/// диффа (M6 §12, ожидание параллельного запроса): идут только внутрь напоминания, которое и
/// без них состоялось, и не в контрольной группе (`holdout`).
fn reminder(
    env: &Env,
    client: &Client,
    session: &str,
    mut sh: cache::Shown,
    fix: Option<String>,
    impact: impl FnOnce() -> Vec<String>,
) -> Output {
    let got = client
        .get(&format!("/sessions/{}", encode_segment(session)), &[])
        .and_then(read_json);
    // запрос влияния — дождаться всегда (свой срок; строки нужны не всегда)
    let impact = impact();
    let (writes, tasks, holdout) = match got {
        Ok(v) => {
            // контрольная группа A/B (M6 §12): маркер сессии — тишина всех хуков
            cache::note_holdout(env, Some(session), &v);
            (
                v["memory_writes"].as_i64(),
                task_line(&v),
                v["holdout"].as_bool() == Some(true),
            )
        }
        Err(e) if e.code == EXIT_NOT_FOUND => (Some(0), None, false),
        Err(e) => {
            env.log(&format!("сессия: {e}"));
            return Output::Empty;
        }
    };
    let mut lines: Vec<String> = (writes == Some(0))
        .then(|| REMINDER.to_string())
        .into_iter()
        .chain(tasks)
        .chain(fix)
        .collect();
    if lines.is_empty() {
        return Output::Empty;
    }
    if !holdout {
        lines.extend(impact);
    }
    let reason = lines.join("\n");
    sh.reminded = true;
    cache::write_shown(env, session, &sh);
    Output::Block { reason }
}

/// Вход `POST /code/impact` (M6 §12): пути правок сессии — из журнала (`Ev::Edit`, новые
/// первыми), затем из очереди изменённых файлов (новые первыми), без повторов, до
/// [`IMPACT_FILES_MAX`]; хэши символов — из последней строки очереди пути (нет — путь без
/// символов: сервер отдаёт все знания о файле). Тело больше [`IMPACT_BODY_MAX`] — у самых старых
/// путей символы снимаются.
pub fn impact_files(events: &[(journal::Timestamp, Ev)], queued: &[cache::Changed]) -> Vec<Value> {
    let mut paths: Vec<&str> = Vec::new();
    let edits = events.iter().rev().filter_map(|(_, e)| match e {
        Ev::Edit { path } => Some(path.as_str()),
        _ => None,
    });
    for p in edits.chain(queued.iter().rev().map(|c| c.path.as_str())) {
        if paths.len() >= IMPACT_FILES_MAX {
            break;
        }
        if !paths.iter().any(|x| crate::cmd::refs::same_path(x, p)) {
            paths.push(p);
        }
    }
    let mut files: Vec<Value> = paths
        .iter()
        .map(|p| {
            let symbols = queued
                .iter()
                .rev()
                .find(|c| crate::cmd::refs::same_path(&c.path, p))
                .and_then(|c| c.symbols.as_ref());
            match symbols {
                Some(s) => json!({"path": p, "symbols": s}),
                None => json!({"path": p}),
            }
        })
        .collect();
    let size = |f: &[Value]| serde_json::to_vec(f).map_or(0, |v| v.len());
    for i in (0..files.len()).rev() {
        if size(&files) <= IMPACT_BODY_MAX {
            break;
        }
        if let Some(o) = files[i].as_object_mut() {
            o.remove("symbols");
        }
    }
    files
}

/// Срок запроса влияния, мс: [`IMPACT_MS`]; отладочная сборка или фича `test-hooks` —
/// `ZAM_TEST_HOOK_IMPACT_MS` (тесты содержимого под нагрузкой).
fn impact_ms() -> u64 {
    #[cfg(any(feature = "test-hooks", debug_assertions))]
    if let Some(ms) = std::env::var("ZAM_TEST_HOOK_IMPACT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        return ms;
    }
    IMPACT_MS
}

/// `POST /code/impact {solution, files}` со сроком [`impact_ms`] и строки влияния; сбой и срок —
/// без строк (`code/impact: …` в `hooks.log`).
fn impact(env: &Env, solution: &str, files: Vec<Value>) -> Vec<String> {
    let Some(client) = env.client_within(Duration::from_millis(impact_ms())) else {
        return Vec::new();
    };
    let body = json!({"solution": solution, "files": files});
    match client.post_json("/code/impact", &body).and_then(read_json) {
        Ok(v) => impact_lines(&v),
        Err(e) => {
            env.log(&format!("code/impact: {e}"));
            Vec::new()
        }
    }
}

/// Строки влияния по ответу `POST /code/impact` (порядок сервера: знания о символах первыми):
/// «ZeroAIMemory: правка затрагивает `<путь>[#<символ>]` → <ссылка> «<заголовок>»», без
/// повторов, до [`IMPACT_LINES_MAX`].
pub fn impact_lines(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for k in v["knowledge"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        if out.len() >= IMPACT_LINES_MAX {
            break;
        }
        let (Some(path), Some(id)) = (k["path"].as_str(), k["id"].as_str()) else {
            continue;
        };
        let target = match k["symbol"].as_str() {
            Some(sym) => format!("{path}#{sym}"),
            None => path.to_string(),
        };
        let title: String = k["title"]
            .as_str()
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(IMPACT_TITLE_MAX)
            .collect();
        let line = format!("ZeroAIMemory: правка затрагивает `{target}` → {id} «{title}»");
        if !out.contains(&line) {
            out.push(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(p: &str) -> (journal::Timestamp, Ev) {
        (journal::Timestamp(0), Ev::Edit { path: p.into() })
    }

    /// Вход влияния: журнал (новые первыми), затем очередь; повтор пути — один раз; до 20
    /// файлов; символы — из последней строки очереди пути.
    #[test]
    fn impact_files_from_journal_and_queue() {
        let sym = |q: &str| zam_code::SymbolHash {
            qual: q.into(),
            body_hash: "h".repeat(16),
        };
        let q = |p: &str, s: Option<Vec<zam_code::SymbolHash>>| cache::Changed {
            path: p.into(),
            hash: "x".into(),
            symbols: s,
        };
        let events = [edit("a.rs"), edit("b.rs"), edit("a.rs")];
        let queued = [
            q("b.rs", Some(vec![sym("old")])),
            q("b.rs", Some(vec![sym("B::f")])),
            q("c.rs", None),
        ];
        let f = impact_files(&events, &queued);
        assert_eq!(
            f,
            [
                json!({"path": "a.rs"}),
                json!({"path": "b.rs", "symbols": [{"qual": "B::f", "body_hash": "h".repeat(16)}]}),
                json!({"path": "c.rs"}),
            ]
        );
        let many: Vec<_> = (0..30).map(|i| edit(&format!("f{i}.rs"))).collect();
        let f = impact_files(&many, &[]);
        assert_eq!(f.len(), IMPACT_FILES_MAX);
        assert_eq!(f[0]["path"], "f29.rs");
        assert!(impact_files(&[], &[]).is_empty());
    }

    /// Строки влияния: символ — `путь#символ`, заголовок — до 60 символов в одну строку, не
    /// больше трёх, без повторов.
    #[test]
    fn impact_lines_format_and_cap() {
        let k = |p: &str, s: Option<&str>, id: &str, t: &str| {
            let mut v = json!({"path": p, "id": id, "kind": "insight", "title": t, "state": "ok"});
            if let Some(s) = s {
                v["symbol"] = json!(s);
            }
            v
        };
        let v = json!({"knowledge": [
            k("src/a.rs", Some("Pool::get"), "i:1", "Пул\nтечёт"),
            k("src/a.rs", Some("Pool::get"), "i:1", "Пул\nтечёт"),
            k("src/a.rs", None, "n:2", &"д".repeat(80)),
            k("src/b.rs", None, "c:3", "Итог"),
            k("src/c.rs", None, "i:4", "Лишняя"),
        ]});
        let l = impact_lines(&v);
        assert_eq!(l.len(), 3, "{l:?}");
        assert_eq!(
            l[0],
            "ZeroAIMemory: правка затрагивает `src/a.rs#Pool::get` → i:1 «Пул течёт»"
        );
        assert_eq!(
            l[1],
            format!(
                "ZeroAIMemory: правка затрагивает `src/a.rs` → n:2 «{}»",
                "д".repeat(60)
            )
        );
        assert!(l[2].contains("`src/b.rs` → c:3 «Итог»"));
        assert!(impact_lines(&json!({"knowledge": []})).is_empty());
        assert!(impact_lines(&json!({})).is_empty());
    }
}
