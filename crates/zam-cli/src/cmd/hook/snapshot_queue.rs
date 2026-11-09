//! Снимок сессии в хуках (спецификация M6 §8а, задача 17b): когда снимать, локальное состояние,
//! очередь и отправка `PUT /sessions/{id}/snapshot`.
//!
//! - **Когда** ([`job_for_checkpoint`], [`job_for_session_start`]; ключ клиента
//!   `hooks.session_snapshot`, выключен — журнал агента не открывается и не проверяется):
//!   - `PostCompact` — конспект из входа хука (`compact_summary`), запросы, файлы и команды — из
//!     журнала агента **[решение исполнителя]**: `PUT` заменяет снимок сессии целиком, и
//!     снимок из одного конспекта стёр бы запросы и файлы `PreCompact`;
//!   - `PreCompact`, `SessionEnd` — всегда;
//!   - `SessionStart source=compact` — если `PostCompact` не пришёл (в `snapst-*` последний
//!     снимок не `post_compact` или старше [`POST_COMPACT_FRESH`]); снимок и `PUT` — **до**
//!     `/recall` (его делает поток `/recall` хука);
//!   - `Stop` — с прошлого снимка журнал вырос на ≥ [`STOP_GROWTH`] байт или в журнале сессии
//!     есть `Ev::Prompt` новее снимка, и не чаще раза в [`STOP_INTERVAL`] ([`stop_due`]); без
//!     ключа `session_journal` — только по росту;
//!   - `SessionStart source=startup` — догоняющий снимок ([`abandoned`]): у последней по `at`
//!     другой сессии проекта нет признака конца (`ended`), а её журнал вырос после снимка.
//! - **Сборка** ([`run`]) — [`super::snapshot::take`] со сроком `hooks.snapshot_ms`: опоздание —
//!   ничего (поток брошен). Готовый снимок — событие `Ev::Snapshot` журнала сессии (только
//!   счётчики), состояние `snapst-<сессия>.json`, очередь `snapq-<сессия>.json` (права 0600:
//!   в ней текст снимка) и сразу отправка очереди, если остался бюджет. Пустой снимок (в
//!   хвосте нет ни конспекта, ни запросов, ни файлов, ни команд) только обновляет `snapst-*`:
//!   прежний снимок сессии на сервере не затирается **[решение исполнителя]**.
//! - **Тело** ([`PutBody`]) — сериализованный [`Snapshot`] и `taken_at`: размер тела меряется
//!   ([`body_bytes`]), запас [`super::snapshot::ENVELOPE_RESERVE`] покрывает `taken_at`.
//! - **Отправка** ([`flush`]): `X-Zam-Session` — id сессии снимка (явная сессия §8а). 2xx —
//!   очередь удаляется; 404/405 (старый сервер, маршрута нет) — счётчик попыток очереди, после
//!   [`OLD_SERVER_ATTEMPTS`] снимок остаётся в очереди без новых попыток в этой сессии; 400, 413,
//!   422 — снимок отброшен; прочее (сеть, 5xx, 401/403) — остаётся до следующего хука.
//! - **`hooks.log`** — повод, размер, полнота и итог отправки; текста снимка там нет. Упор
//!   сканера секретов — `snapshot: secrets_scan_depth` без текста.
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zam_core::snapshot::SnapTrigger;

use super::cache::{self, read_json_file, session_file_part, write_atomic};
use super::input::Input;
use super::journal::{self, Ev, Timestamp};
use super::snapshot::{self, AgentKind, Snapshot, TakeOpts};
use super::{Agent, Env, Flag, Settings, encode_segment};
use crate::client::CliError;

/// `Stop` снимает, если журнал агента вырос хотя бы на столько байт.
pub const STOP_GROWTH: u64 = 16 * 1024;
/// `Stop` снимает не чаще.
pub const STOP_INTERVAL: Duration = Duration::from_secs(60);
/// Попыток `PUT` старому серверу (404/405) за сессию.
pub const OLD_SERVER_ATTEMPTS: u32 = 3;
/// Предел записи, байт (`snapshot.max_bytes` сервера по умолчанию, §14): клиент настроек
/// сервера не знает; сервер с меньшим пределом ответит 413 — снимок отбрасывается.
pub const MAX_BYTES: usize = 16 * 1024;
/// Снимок `PostCompact` моложе — `SessionStart source=compact` журнал не читает.
pub const POST_COMPACT_FRESH: Duration = Duration::from_secs(5 * 60);
/// Очередей других сессий, которые `session-start` отправляет за раз (новые первыми).
pub const FLUSH_OTHERS_MAX: usize = 3;

/// Состояние снимка сессии `snapst-<сессия>.json`: журнал агента, его размер и момент снимка.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// Id сессии (имя файла — с хэшем, догоняющему снимку нужен сам id).
    pub session: String,
    pub transcript_path: PathBuf,
    /// Размер журнала агента при снимке, байт.
    pub size: u64,
    /// Момент снимка, мс Unix.
    pub at: u64,
    /// Формат журнала (`claude` | `codex`).
    pub agent: String,
    /// Повод последнего снимка.
    pub trigger: String,
    /// Пришёл `SessionEnd`: сессия закончилась, догоняющий снимок ей не нужен.
    #[serde(default)]
    pub ended: bool,
    /// Последний запрос человека (`Ev::Prompt` журнала сессии), учтённый снимком, мс Unix; 0 —
    /// не было. «Новый запрос» у `Stop` — новее его, а не новее `at`.
    #[serde(default)]
    pub prompt_at: u64,
}

/// Очередь `snapq-<сессия>.json`: последний неотправленный снимок (новый заменяет старый) и
/// ответы старого сервера.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Queued {
    session: String,
    #[serde(default)]
    old_server: u32,
    body: Value,
}

/// Тело `PUT /sessions/{id}/snapshot` (§8а, задача 17c): `taken_at` (UTC, RFC 3339) и поля
/// снимка `trigger, agent, status, skipped, data`.
#[derive(Serialize)]
pub struct PutBody<'a> {
    pub taken_at: &'a str,
    #[serde(flatten)]
    pub snap: &'a Snapshot,
}

/// Байты тела `PUT` — то, что уходит серверу и меряется пределом.
pub fn body_bytes(snap: &Snapshot, taken_at: &str) -> Vec<u8> {
    serde_json::to_vec(&PutBody { taken_at, snap }).unwrap_or_default()
}

/// Что снять: сессия, журнал агента, формат, повод и конспект входа хука.
#[derive(Debug, Clone)]
pub struct Job {
    pub session: String,
    pub transcript: PathBuf,
    pub agent: AgentKind,
    pub trigger: SnapTrigger,
    pub summary: Option<String>,
}

pub fn state_path(env: &Env, session: &str) -> PathBuf {
    env.cache_dir()
        .join(format!("snapst-{}.json", session_file_part(session)))
}

pub fn queue_path(env: &Env, session: &str) -> PathBuf {
    env.cache_dir()
        .join(format!("snapq-{}.json", session_file_part(session)))
}

fn read_state(path: &Path) -> Option<State> {
    read_json_file::<State>(path).ok().flatten()
}

fn now_ms() -> u64 {
    Timestamp::now().0
}

/// Формат журнала агента хука.
pub fn agent_kind(a: Agent) -> AgentKind {
    match a {
        Agent::Claude => AgentKind::Claude,
        Agent::Codex => AgentKind::Codex,
    }
}

/// `transcript_path` входа хука: непустая строка.
fn transcript_of(inp: &Input) -> Option<PathBuf> {
    inp.str("transcript_path")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

/// Снимок для события `checkpoint` (`Stop`, `PreCompact`, `PostCompact`, `SessionEnd`) или
/// `None` (выключено, нет сессии или журнала, `Stop` ещё рано).
pub fn job_for_checkpoint(env: &Env, inp: &Input, s: &Settings) -> Option<Job> {
    if !s.session_snapshot() {
        return None;
    }
    let session = inp.session_id.clone()?;
    let transcript = transcript_of(inp)?;
    let (trigger, summary) = match inp.event.as_str() {
        "Stop" => {
            if !stop_due(env, &session, &transcript, s.flag_on(Flag::SessionJournal)) {
                return None;
            }
            (SnapTrigger::Stop, None)
        }
        "PreCompact" => (SnapTrigger::PreCompact, None),
        "PostCompact" => (
            SnapTrigger::PostCompact,
            inp.str("compact_summary").map(str::to_string),
        ),
        "SessionEnd" => (SnapTrigger::SessionEnd, None),
        _ => return None,
    };
    Some(Job {
        session,
        transcript,
        agent: agent_kind(env.agent),
        trigger,
        summary,
    })
}

/// Пора ли снимать на `Stop`: снимка ещё не было; или прошло не меньше [`STOP_INTERVAL`] и
/// журнал вырос на [`STOP_GROWTH`] или (ключ `session_journal`) после снимка был запрос
/// человека (`Ev::Prompt` журнала сессии новее учтённого снимком `prompt_at`; событие пишет хук
/// `prompt`).
pub fn stop_due(env: &Env, session: &str, transcript: &Path, journal_on: bool) -> bool {
    let Some(st) = read_state(&state_path(env, session)) else {
        return true;
    };
    if now_ms().saturating_sub(st.at) < STOP_INTERVAL.as_millis() as u64 {
        return false;
    }
    let size = std::fs::metadata(transcript).map_or(0, |m| m.len());
    if size >= st.size.saturating_add(STOP_GROWTH) {
        return true;
    }
    journal_on
        && journal::read(&env.root, session)
            .iter()
            .any(|(t, e)| matches!(e, Ev::Prompt { .. }) && t.0 > st.prompt_at)
}

/// Снимок для `session-start`: `compact` без свежего `PostCompact` — снимок этой сессии;
/// `startup` — догоняющий снимок оборванной сессии ([`abandoned`]); иначе `None`.
pub fn job_for_session_start(env: &Env, inp: &Input, s: &Settings) -> Option<Job> {
    if !s.session_snapshot() {
        return None;
    }
    match inp.str("source")? {
        "compact" => {
            let session = inp.session_id.clone()?;
            let fresh = read_state(&state_path(env, &session)).is_some_and(|st| {
                st.trigger == SnapTrigger::PostCompact.as_str()
                    && now_ms().saturating_sub(st.at) < POST_COMPACT_FRESH.as_millis() as u64
            });
            if fresh {
                return None;
            }
            Some(Job {
                session,
                transcript: transcript_of(inp)?,
                agent: agent_kind(env.agent),
                trigger: SnapTrigger::SessionStartCompact,
                summary: None,
            })
        }
        "startup" => {
            let st = abandoned(env, inp.session_id.as_deref())?;
            Some(Job {
                agent: AgentKind::parse(&st.agent)?,
                session: st.session,
                transcript: st.transcript_path,
                trigger: SnapTrigger::CatchUp,
                summary: None,
            })
        }
        _ => None,
    }
}

/// Оборванная сессия проекта: последняя по `at` из `snapst-*` (кроме `current`) без `ended`,
/// чей журнал вырос после её снимка.
pub fn abandoned(env: &Env, current: Option<&str>) -> Option<State> {
    let rd = std::fs::read_dir(env.cache_dir()).ok()?;
    let last = rd
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("snapst-") && n.ends_with(".json"))
        })
        .filter_map(|e| read_state(&e.path()))
        .filter(|st| Some(st.session.as_str()) != current)
        .max_by_key(|st| st.at)?;
    if last.ended {
        return None;
    }
    let size = std::fs::metadata(&last.transcript_path).ok()?.len();
    (size > last.size).then_some(last)
}

/// Снимок по `job` в срок `hooks.snapshot_ms`, затем журнал сессии, состояние, очередь и
/// отправка (см. модуль). Не успел, журнала нет — ничего (у `SessionEnd` — только признак
/// конца в `snapst-*`).
pub fn run(env: &Env, s: &Settings, job: Job) {
    let opts = TakeOpts {
        root: env.root.clone(),
        agent: job.agent,
        session: job.session.clone(),
        trigger: job.trigger,
        tail_kb: s.snapshot_tail_kb(),
        deadline: s.snapshot_deadline(),
        max_bytes: MAX_BYTES,
        summary: job.summary.clone(),
    };
    let snap = snapshot::take(&job.transcript, &opts);
    let ended = job.trigger == SnapTrigger::SessionEnd;
    let st_path = state_path(env, &job.session);
    let Some(snap) = snap else {
        if ended && let Some(mut st) = read_state(&st_path) {
            st.ended = true;
            write_state(env, &st_path, &st);
        }
        return;
    };
    if snap.scan_depth_capped {
        env.log("snapshot: secrets_scan_depth");
    }
    let taken_at = cache::now_rfc3339();
    let body = body_bytes(&snap, &taken_at);
    let d = &snap.data;
    let trigger = job.trigger.as_str();
    let prev = read_state(&st_path);
    let prompt_at = if s.flag_on(Flag::SessionJournal) {
        last_prompt(env, &job.session)
    } else {
        prev.as_ref().map_or(0, |p| p.prompt_at)
    };
    let st = State {
        session: job.session.clone(),
        transcript_path: job.transcript.clone(),
        size: std::fs::metadata(&job.transcript).map_or(0, |m| m.len()),
        at: now_ms(),
        agent: job.agent.as_str().to_string(),
        trigger: trigger.to_string(),
        ended: ended || prev.is_some_and(|p| p.ended),
        prompt_at,
    };
    write_state(env, &st_path, &st);
    // пустой снимок (в хвосте нет ни конспекта, ни запросов, ни файлов, ни команд) — не нужен
    // серверу: прежний снимок сессии остаётся
    if d.compact_summary.is_none()
        && d.prompts.is_empty()
        && d.files.is_empty()
        && d.commands.is_empty()
    {
        return;
    }
    if s.flag_on(Flag::SessionJournal) {
        journal::note(
            env,
            &job.session,
            &Ev::Snapshot {
                trigger: trigger.to_string(),
                status: snap.status.as_str().to_string(),
                bytes: body.len() as u64,
                prompts: d.prompts.len() as u32,
                files: d.files.len() as u32,
                cmds: d.commands.len() as u32,
            },
        );
    }
    let what = format!(
        "снимок {trigger}: {} байт, {}",
        body.len(),
        snap.status.as_str()
    );
    if body.len() > MAX_BYTES {
        // `mask_and_limit` укладывает снимок с запасом под `taken_at`: сюда не попадаем
        env.log(&format!("{what}: больше {MAX_BYTES} байт, отброшен"));
        return;
    }
    let Ok(body) = serde_json::from_slice::<Value>(&body) else {
        return;
    };
    if !enqueue(env, &job.session, body) {
        return;
    }
    match flush(env, &job.session) {
        Some(Sent::Ok) => env.log(&format!("{what}, отправлен")),
        Some(Sent::Queued(why)) => env.log(&format!("{what}, в очереди: {why}")),
        Some(Sent::Dropped(why)) => env.log(&format!("{what}, отброшен: {why}")),
        None => env.log(&format!("{what}, в очереди")),
    }
}

/// Момент последнего `Ev::Prompt` журнала сессии, мс Unix (0 — запросов не было).
fn last_prompt(env: &Env, session: &str) -> u64 {
    journal::read(&env.root, session)
        .iter()
        .filter(|(_, e)| matches!(e, Ev::Prompt { .. }))
        .map(|(t, _)| t.0)
        .max()
        .unwrap_or(0)
}

fn write_state(env: &Env, path: &Path, st: &State) {
    let r = serde_json::to_vec(st)
        .map_err(std::io::Error::other)
        .and_then(|b| write_atomic(path, &b));
    if let Err(e) = r {
        env.log(&format!("snapst сессии: {e}"));
    }
}

/// Кладёт снимок в очередь сессии (новый заменяет старый; счётчик попыток старому серверу
/// сохраняется). Права файла — 0600 на unix ([`write_atomic`]).
fn enqueue(env: &Env, session: &str, body: Value) -> bool {
    let q = queue_path(env, session);
    let old_server = read_json_file::<Queued>(&q)
        .ok()
        .flatten()
        .map_or(0, |x| x.old_server);
    let item = Queued {
        session: session.to_string(),
        old_server,
        body,
    };
    let r = serde_json::to_vec(&item)
        .map_err(std::io::Error::other)
        .and_then(|b| write_atomic(&q, &b));
    match r {
        Ok(()) => true,
        Err(e) => {
            env.log(&format!("очередь снимка: {e}"));
            false
        }
    }
}

/// Итог отправки очереди.
#[derive(Debug, PartialEq, Eq)]
pub enum Sent {
    Ok,
    /// Остался в очереди (причина без текста снимка).
    Queued(String),
    /// Отброшен: повтор не поможет (400, 413, 422).
    Dropped(String),
}

/// Отправка очереди сессии `session`; `None` — нечего отправлять, старый сервер уже ответил
/// [`OLD_SERVER_ATTEMPTS`] раз, нет клиента или бюджета.
pub fn flush(env: &Env, session: &str) -> Option<Sent> {
    let q = queue_path(env, session);
    let raw = std::fs::read(&q).ok()?;
    let mut item: Queued = serde_json::from_slice(&raw).ok()?;
    if item.old_server >= OLD_SERVER_ATTEMPTS {
        return None;
    }
    if env.http_left().is_none_or(|d| d < cache::FLUSH_MIN_LEFT) {
        return None;
    }
    let mut client = env.client()?;
    client.set_session(Some(item.session.clone()));
    let path = format!("/sessions/{}/snapshot", encode_segment(&item.session));
    let body = serde_json::to_vec(&item.body).ok()?;
    let res = client.put_json_body(&path, body);
    // очередь сменилась за время запроса (новый снимок) — новую не трогать
    let same = || std::fs::read(&q).is_ok_and(|now| now == raw);
    Some(match res {
        Ok(_) => {
            if same() {
                let _ = std::fs::remove_file(&q);
            }
            Sent::Ok
        }
        Err(e) if matches!(e.http_status(), Some(400 | 413 | 422)) => {
            if same() {
                let _ = std::fs::remove_file(&q);
            }
            Sent::Dropped(short(&e))
        }
        Err(e) if matches!(e.http_status(), Some(404 | 405)) => {
            item.old_server += 1;
            let n = item.old_server;
            if same() {
                if let Ok(b) = serde_json::to_vec(&item) {
                    let _ = write_atomic(&q, &b);
                }
            } else if let Ok(Some(mut newer)) = read_json_file::<Queued>(&q) {
                newer.old_server = newer.old_server.max(n);
                if let Ok(b) = serde_json::to_vec(&newer) {
                    let _ = write_atomic(&q, &b);
                }
            }
            Sent::Queued(format!(
                "старый сервер (HTTP {}), попытка {n} из {OLD_SERVER_ATTEMPTS}",
                e.http_status().unwrap_or(0)
            ))
        }
        Err(e) => Sent::Queued(short(&e)),
    })
}

/// Причина ошибки для `hooks.log`: статус и начало текста сервера.
fn short(e: &CliError) -> String {
    e.to_string().chars().take(160).collect()
}

/// Очереди других сессий проекта (кроме `except`), новые первыми, до [`FLUSH_OTHERS_MAX`]:
/// снимок `SessionEnd` при недоступном сервере уходит со следующим стартом.
pub fn flush_others(env: &Env, except: &[&str]) {
    let Ok(rd) = std::fs::read_dir(env.cache_dir()) else {
        return;
    };
    let mut items: Vec<(std::time::SystemTime, String)> = rd
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("snapq-") && n.ends_with(".json"))
        })
        .filter_map(|e| {
            let t = e.metadata().and_then(|m| m.modified()).ok()?;
            let q = read_json_file::<Queued>(&e.path()).ok()??;
            (q.old_server < OLD_SERVER_ATTEMPTS && !except.contains(&q.session.as_str()))
                .then_some((t, q.session))
        })
        .collect();
    items.sort_by_key(|x| std::cmp::Reverse(x.0));
    for (_, id) in items.into_iter().take(FLUSH_OTHERS_MAX) {
        match flush(env, &id) {
            Some(Sent::Ok) => env.log("снимок из очереди: отправлен"),
            Some(Sent::Queued(why)) => env.log(&format!("снимок из очереди: {why}")),
            Some(Sent::Dropped(why)) => env.log(&format!("снимок из очереди: отброшен: {why}")),
            None => {}
        }
    }
}

/// `SessionEnd`: `PATCH /sessions/{id} {ended: true}` (§8а; ставится и при выключенном снимке).
/// Сессии нет на сервере (404) — молча.
pub fn end_session(env: &Env, session: &str) {
    let Some(client) = env.client() else {
        return;
    };
    let path = format!("/sessions/{}", encode_segment(session));
    match client.patch_json(&path, &serde_json::json!({"ended": true})) {
        Ok(_) => {}
        Err(e) if e.http_status() == Some(404) => {}
        Err(e) => env.log(&format!("sessions(ended): {}", short(&e))),
    }
}
