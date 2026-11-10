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
//!     **удавшийся** снимок не `post_compact` или старше [`POST_COMPACT_FRESH`]: опоздавшая
//!     попытка `PostCompact` не считается); снимок и `PUT` — **до**
//!     `/recall` (его делает поток `/recall` хука);
//!   - `Stop` — не чаще раза в [`STOP_INTERVAL`], затем: сменился `transcript_path`, в журнале
//!     сессии есть `Ev::Prompt` новее учтённого снимком (`prompt_at`) или журнал вырос на ≥
//!     [`STOP_GROWTH`] байт ([`stop_due`]); без ключа `session_journal` — без запросов;
//!   - `SessionStart source=startup` — догоняющий снимок ([`abandoned`]): у самой новой по
//!     времени файла `snapst-*` другой сессии проекта нет признака конца (`ended`), а её журнал
//!     вырос после снимка. Давность журнала не проверяется (как в §8а): живую параллельную
//!     сессию снять безвредно — живость для показа решает сервер по `last_seen_at`.
//! - **Ввод-вывод журнала агента** — только в потоке снимка со сроком
//!   ([`super::snapshot::take_ext`]: `metadata`, рост, чтение); до него — только файлы
//!   кэша проекта (`snapst-*`, журнал сессии).
//! - **Сборка** ([`run`]) — со сроком `hooks.snapshot_ms`: опоздание — снимка нет (поток брошен),
//!   в `snapst-*` — момент попытки; нового снимка нет — уходит прежняя очередь сессии. Готовый снимок — событие `Ev::Snapshot` журнала сессии (только
//!   счётчики), состояние `snapst-<сессия>.json`, очередь `snapq-<сессия>.json` (права 0600:
//!   в ней текст снимка) и сразу отправка очереди, если остался бюджет. Пустой снимок (в
//!   хвосте нет ни конспекта, ни запросов, ни файлов, ни команд) только обновляет `snapst-*`:
//!   прежний снимок сессии на сервере не затирается **[решение исполнителя]**.
//! - **Тело** ([`PutBody`]) — сериализованный [`Snapshot`] и `taken_at`: размер тела меряется
//!   ([`body_bytes`]), запас [`super::snapshot::ENVELOPE_RESERVE`] покрывает `taken_at`.
//! - **Отправка** ([`flush`]): свой срок [`SNAPSHOT_PUT_MS`] внутри бюджета хука,
//!   `X-Zam-Session` — id сессии снимка (явная сессия §8а), правка очереди — под замком
//!   `.snapq-….json.lock`. 2xx —
//!   очередь удаляется; 404/405 (старый сервер, маршрута нет) — счётчик попыток очереди, после
//!   [`OLD_SERVER_ATTEMPTS`] снимок остаётся в очереди без новых попыток в этой сессии; 404 с
//!   `type` `…/session-not-found` (сервер с маршрутом, сессии у него нет) старым сервером не
//!   считается — счётчик не растёт, снимок уйдёт, когда сессия появится; 400, 413,
//!   422 — снимок отброшен; прочее (сеть, 5xx, 401/403) — остаётся до следующего хука.
//! - **`hooks.log`** — повод, размер, полнота и итог отправки (ответ сервера — только `HTTP <код>`
//!   и `type`, без `detail`); текста снимка там нет. Упор
//!   сканера секретов — `snapshot: secrets_scan_depth` без текста. У `checkpoint` поток снимка не
//!   scoped и срок `PUT` истекает у самого конца бюджета: итог отправки поток и хук делят через
//!   [`SendNote`] — не успел поток, строку пишет сам хук перед выходом ([`wait_snapshot`]).
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zam_core::snapshot::SnapTrigger;

use super::cache::{self, read_json_file, session_file_part, write_atomic};
use super::input::Input;
use super::journal::{self, Ev, Timestamp};
use super::snapshot::{self, AgentKind, Snapshot, TakeOpts, Taken};
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
/// Срок `PUT` снимка внутри бюджета хука, мс (как `IMPACT_MS` у `checkpoint`): медленный
/// сервер не съедает бюджет `/recall` у `session-start`; не успел — снимок в очереди.
pub const SNAPSHOT_PUT_MS: u64 = 60;

/// Состояние снимка сессии `snapst-<сессия>.json`: журнал агента, его размер и момент снимка.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// Id сессии (имя файла — с хэшем, догоняющему снимку нужен сам id).
    pub session: String,
    pub transcript_path: PathBuf,
    /// Размер журнала агента при снимке, байт.
    pub size: u64,
    /// Момент последней попытки снимка (удавшейся или опоздавшей), мс Unix: правило 60 с у
    /// `Stop`.
    pub at: u64,
    /// Момент последнего **удавшегося** снимка, мс Unix; 0 — снимков не было (только попытки).
    #[serde(default)]
    pub ok_at: u64,
    /// Формат журнала (`claude` | `codex`).
    pub agent: String,
    /// Повод последнего удавшегося снимка (опоздавшая попытка его не меняет); пустая строка —
    /// удавшихся снимков не было (только опоздавшие попытки, `ok_at == 0`).
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

/// Что снять: сессия, журнал агента, формат, повод, конспект входа хука и условия на журнал
/// (проверяются в потоке со сроком).
#[derive(Debug, Clone)]
pub struct Job {
    pub session: String,
    /// Журнал агента; пусто — нет (`PostCompact` с одним конспектом).
    pub transcript: PathBuf,
    pub agent: AgentKind,
    pub trigger: SnapTrigger,
    pub summary: Option<String>,
    /// Журнал короче — не снимать (`Stop` без нового запроса: рост ≥ [`STOP_GROWTH`];
    /// догоняющий снимок: журнал не вырос).
    pub min_len: Option<u64>,
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
/// `None` (выключено, нет сессии или журнала, `Stop` ещё рано). `PostCompact` без журнала, но с
/// `compact_summary` — снимок из одного конспекта. Журнал агента здесь не трогается.
pub fn job_for_checkpoint(env: &Env, inp: &Input, s: &Settings) -> Option<Job> {
    // точка подмены — в самом решении: вызов на главном пути хука в любой форме тест поймает
    if let Some(t) = transcript_of(inp) {
        test_point(&t, "decide-stall-ms");
    }
    if !s.session_snapshot() {
        return None;
    }
    let session = inp.session_id.clone()?;
    let transcript = transcript_of(inp);
    let mut min_len = None;
    let (trigger, summary) = match inp.event.as_str() {
        "Stop" => {
            match stop_due(
                env,
                &session,
                transcript.as_deref()?,
                s.flag_on(Flag::SessionJournal),
            ) {
                StopDue::No => return None,
                StopDue::Now => {}
                StopDue::IfGrown(n) => min_len = Some(n),
            }
            (SnapTrigger::Stop, None)
        }
        "PreCompact" => (SnapTrigger::PreCompact, None),
        "PostCompact" => {
            let summary = inp
                .str("compact_summary")
                .filter(|t| !t.trim().is_empty())
                .map(str::to_string);
            (SnapTrigger::PostCompact, summary)
        }
        "SessionEnd" => (SnapTrigger::SessionEnd, None),
        _ => return None,
    };
    if transcript.is_none() && summary.is_none() {
        return None;
    }
    Some(Job {
        session,
        transcript: transcript.unwrap_or_default(),
        agent: agent_kind(env.agent),
        trigger,
        summary,
        min_len,
    })
}

/// Решение `Stop` до чтения журнала агента ([`stop_due`]).
#[derive(Debug, PartialEq, Eq)]
pub enum StopDue {
    /// Рано (прошло меньше [`STOP_INTERVAL`]).
    No,
    /// Снимать: снимка ещё не было, сменился журнал или был новый запрос человека.
    Now,
    /// Снимать, если журнал не короче (рост ≥ [`STOP_GROWTH`]; проверка — в потоке со сроком).
    IfGrown(u64),
}

/// Пора ли снимать на `Stop` — только по `snapst-*` и журналу сессии (кэш проекта), без
/// ввода-вывода журнала агента: снимка не было — сразу; не чаще [`STOP_INTERVAL`]; затем — смена
/// `transcript_path`, запрос человека новее учтённого снимком (`Ev::Prompt` журнала сессии,
/// пишет хук `prompt`; ключ `session_journal`) или рост журнала ≥ [`STOP_GROWTH`].
pub fn stop_due(env: &Env, session: &str, transcript: &Path, journal_on: bool) -> StopDue {
    let Some(st) = read_state(&state_path(env, session)) else {
        return StopDue::Now;
    };
    if now_ms().saturating_sub(st.at) < STOP_INTERVAL.as_millis() as u64 {
        return StopDue::No;
    }
    // удавшегося снимка ещё не было (только опоздавшие попытки, `size` в них — 0) — сразу
    if st.ok_at == 0 || st.transcript_path != transcript {
        return StopDue::Now;
    }
    let prompted = journal_on
        && journal::read(&env.root, session)
            .iter()
            .any(|(t, e)| matches!(e, Ev::Prompt { .. }) && t.0 > st.prompt_at);
    if prompted {
        return StopDue::Now;
    }
    StopDue::IfGrown(st.size.saturating_add(STOP_GROWTH))
}

/// Снимок для `session-start`: `compact` без свежего `PostCompact` — снимок этой сессии;
/// `startup` — догоняющий снимок оборванной сессии ([`abandoned`]); иначе `None`. На `startup`,
/// `resume` и `clear` признак конца текущей сессии в её `snapst-*` снимается (сессия снова идёт).
pub fn job_for_session_start(env: &Env, inp: &Input, s: &Settings) -> Option<Job> {
    if !s.session_snapshot() {
        return None;
    }
    let source = inp.str("source")?;
    if matches!(source, "startup" | "resume" | "clear")
        && let Some(id) = inp.session_id.as_deref()
    {
        reopen(env, id);
    }
    match source {
        "compact" => {
            let session = inp.session_id.clone()?;
            let fresh = read_state(&state_path(env, &session)).is_some_and(|st| {
                st.trigger == SnapTrigger::PostCompact.as_str()
                    && st.ok_at > 0
                    && now_ms().saturating_sub(st.ok_at) < POST_COMPACT_FRESH.as_millis() as u64
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
                min_len: None,
            })
        }
        "startup" => {
            let st = abandoned(env, inp.session_id.as_deref())?;
            Some(Job {
                agent: AgentKind::parse(&st.agent)?,
                session: st.session,
                min_len: Some(st.size.saturating_add(1)),
                transcript: st.transcript_path,
                trigger: SnapTrigger::CatchUp,
                summary: None,
            })
        }
        _ => None,
    }
}

/// Снимает `ended` в `snapst-*` сессии (`resume` закрытой сессии).
fn reopen(env: &Env, session: &str) {
    let p = state_path(env, session);
    if let Some(mut st) = read_state(&p).filter(|st| st.ended) {
        st.ended = false;
        write_state(env, &p, &st);
    }
}

/// Кандидат на догоняющий снимок: самый новый по времени файла `snapst-*` другой сессии
/// (разбирается он один, файлы — по `read_dir`, без чтения прочих). У него нет `ended` и есть
/// журнал. Вырос ли журнал, проверяет поток снимка в сроке.
///
/// Давность журнала не требуется (§8а; отмена Minor 7 ревью 17b, повторное ревью N1): при
/// быстром перезапуске (новая сессия раньше срока давности) оборванная сессия была бы пропущена,
/// а после первого снимка новой сессии её `snapst-*` перестал бы быть самым новым — оборванная
/// не рассматривалась бы никогда. Снять живую параллельную сессию безвредно: живость для показа
/// решает сервер по `last_seen_at`.
pub fn abandoned(env: &Env, current: Option<&str>) -> Option<State> {
    let rd = std::fs::read_dir(env.cache_dir()).ok()?;
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("snapst-") && n.ends_with(".json"))
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    let current_file = current.map(|id| state_path(env, id));
    let (_, path) = files
        .into_iter()
        .find(|(_, p)| Some(p) != current_file.as_ref())?;
    let st = read_state(&path)?;
    (!st.ended
        && Some(st.session.as_str()) != current
        && !st.transcript_path.as_os_str().is_empty())
    .then_some(st)
}

/// Снимок по `job` в срок `hooks.snapshot_ms` (весь ввод-вывод журнала агента — в потоке), затем
/// журнал сессии, состояние, очередь и отправка (см. модуль); `send` — пробовать отправку (без
/// сервера — только очередь). Нового снимка нет — отправляется прежняя очередь сессии; опоздал —
/// в `snapst-*` записывается попытка (`at`: правило 60 с у `Stop`); у `SessionEnd` — признак
/// конца.
pub fn run(env: &Env, s: &Settings, job: Job, send: bool) {
    run_noted(env, s, job, send, None);
}

/// [`run`] с общим итогом отправки `note` (поток снимка `checkpoint`, [`SendNote`]).
fn run_noted(env: &Env, s: &Settings, job: Job, send: bool, note: Option<&SendNote>) {
    let opts = TakeOpts {
        root: env.root.clone(),
        agent: job.agent,
        session: job.session.clone(),
        trigger: job.trigger,
        tail_kb: s.snapshot_tail_kb(),
        deadline: s.snapshot_deadline(),
        max_bytes: MAX_BYTES,
        summary: job.summary.clone(),
        min_len: job.min_len,
    };
    let taken = snapshot::take_ext(&job.transcript, &opts);
    let ended = job.trigger == SnapTrigger::SessionEnd;
    let trigger = job.trigger.as_str();
    let st_path = state_path(env, &job.session);
    let prev = read_state(&st_path);
    let flush_old = || {
        if send {
            flush_noted(env, &job.session, note);
        }
    };
    let (snap, len) = match taken {
        Taken::Snap(snap, len) => (snap, len),
        Taken::Late => {
            // попытка — в `snapst-*` (`at`): правило 60 с у `Stop` действует и для опоздавших.
            // Повод, `ok_at`, размер и `prompt_at` — прежнего удавшегося снимка: опоздавший
            // `PostCompact` не выдаёт себя за пришедший (`fresh` у `session-start`). Снимка ещё
            // не было — размер 0 и `ok_at` 0: размер журнала узнать можно только `stat` вне срока,
            // а `stop_due` при `ok_at == 0` снимает сразу (после 60 с), рост не нужен.
            let st = State {
                session: job.session.clone(),
                transcript_path: job.transcript.clone(),
                at: now_ms(),
                ended: ended || prev.as_ref().is_some_and(|p| p.ended),
                agent: job.agent.as_str().to_string(),
                ..prev.clone().unwrap_or_default()
            };
            write_state(env, &st_path, &st);
            flush_old();
            return;
        }
        Taken::Missing | Taken::Skipped => {
            if ended && let Some(mut st) = prev.filter(|p| !p.ended) {
                st.ended = true;
                write_state(env, &st_path, &st);
            }
            flush_old();
            return;
        }
    };
    if snap.scan_depth_capped {
        env.log("snapshot: secrets_scan_depth");
    }
    let taken_at = cache::now_rfc3339();
    let body = body_bytes(&snap, &taken_at);
    let d = &snap.data;
    let prompt_at = if s.flag_on(Flag::SessionJournal) {
        last_prompt(env, &job.session)
    } else {
        prev.as_ref().map_or(0, |p| p.prompt_at)
    };
    let st = State {
        session: job.session.clone(),
        transcript_path: job.transcript.clone(),
        size: len,
        at: now_ms(),
        ok_at: now_ms(),
        agent: job.agent.as_str().to_string(),
        trigger: trigger.to_string(),
        ended: ended || prev.is_some_and(|p| p.ended),
        prompt_at,
    };
    // пустой снимок (в хвосте нет ни конспекта, ни запросов, ни файлов, ни команд) — не нужен
    // серверу: прежний снимок сессии остаётся, отметка о снимке — в `snapst-*`
    if d.compact_summary.is_none()
        && d.prompts.is_empty()
        && d.files.is_empty()
        && d.commands.is_empty()
    {
        write_state(env, &st_path, &st);
        flush_old();
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
    // сначала очередь, потом `snapst-*` (отметка «снимок записан»): обрыв между ними оставляет
    // снимок в очереди без отметки (следующий хук снимет заново), а не отметку без снимка
    if !enqueue(env, &job.session, body) {
        return;
    }
    test_point(&job.transcript, "write-stall-ms");
    write_state(env, &st_path, &st);
    let r = if send {
        flush(env, &job.session, &what, note)
    } else {
        None
    };
    log_sent(env, &what, r, true, note);
}

/// Итог отправки — строкой в `hooks.log` (`always` — и когда отправки не было). С `note` (поток
/// снимка `checkpoint`) строка пишется, только если её ещё не записал за поток сам хук
/// ([`SendNote::write`]).
fn log_sent(env: &Env, what: &str, r: Option<Sent>, always: bool, note: Option<&SendNote>) {
    let line = match r {
        Some(Sent::Ok) => format!("{what}, отправлен"),
        Some(Sent::OkKept) => format!("{what}, отправлен, очередь оставлена (замок занят)"),
        Some(Sent::Queued(why)) => format!("{what}, в очереди: {why}"),
        Some(Sent::Dropped(why)) => format!("{what}, отброшен: {why}"),
        None if always => format!("{what}, в очереди"),
        None => return,
    };
    match note {
        Some(n) => n.write(env, &line),
        None => env.log(&line),
    }
}

/// Итог отправки снимка, общий для потока снимка `checkpoint` и хука. Поток не scoped, хук ждёт
/// его только до конца своего бюджета ([`wait_snapshot`]), а срок `PUT` при пределе по бюджету
/// истекает за [`super::HTTP_RESERVE_MS`] до этого конца: на занятой машине поток не успевал
/// записать итог в `hooks.log` до выхода процесса, и причина («срок PUT N мс») терялась (CI
/// ubuntu, `77063ec`). Поток отмечает здесь начало и конец `PUT`, строку итога пишет под замком;
/// хук, не дождавшись потока, пишет за него строку по этапу, на котором поток застал предел
/// ([`SendNote::late`]). Строка итога — ровно одна.
#[derive(Default)]
pub struct SendNote {
    stage: std::sync::Mutex<Stage>,
}

/// Этап потока снимка `checkpoint` ([`SendNote`]).
#[derive(Default)]
enum Stage {
    /// До `PUT`: решение, снимок, очередь и `snapst-*`.
    #[default]
    Before,
    /// `PUT` идёт: что отправляется, фактический срок запроса и момент его начала.
    Put {
        what: String,
        limit: Duration,
        t0: std::time::Instant,
    },
    /// `PUT` вернулся, идёт работа с очередью (замок, сверка, удаление или счётчик) и запись
    /// итога: `outcome` — чем кончился запрос (ответ сервера, срок, сеть).
    PutDone { what: String, outcome: String },
    /// Строка итога записана (потоком или хуком за него).
    Logged,
}

impl SendNote {
    fn lock(&self) -> std::sync::MutexGuard<'_, Stage> {
        self.stage.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Этап потока, если строка итога ещё не записана.
    fn advance(&self, next: Stage) {
        let mut g = self.lock();
        if !matches!(*g, Stage::Logged) {
            *g = next;
        }
    }

    /// Поток: `PUT` начат в `t0` (`what` — что отправляется, `limit` — фактический срок).
    fn sending(&self, what: &str, limit: Duration, t0: std::time::Instant) {
        self.advance(Stage::Put {
            what: what.to_string(),
            limit,
            t0,
        });
    }

    /// Поток: `PUT` вернулся (`outcome` — чем кончился запрос), дальше — работа с очередью.
    fn put_done(&self, what: &str, outcome: String) {
        self.advance(Stage::PutDone {
            what: what.to_string(),
            outcome,
        });
    }

    /// Поток: строка итога, если хук ещё не записал её сам. Запись — под замком: хук, заставший
    /// её, дожидается конца ([`SendNote::late`]).
    fn write(&self, env: &Env, line: &str) {
        let mut g = self.lock();
        if !matches!(*g, Stage::Logged) {
            env.log(line);
            *g = Stage::Logged;
        }
    }

    /// Хук: поток не закончил к пределу ожидания ([`wait_snapshot`]) — строку пишет хук, по этапу
    /// потока:
    /// - до `PUT` поток не дошёл — «не готов» с событием `event` и временем от старта процесса;
    /// - `PUT` идёт: его срок вышел (предел ожидания по бюджету хука — срок раньше него на
    ///   [`super::HTTP_RESERVE_MS`] − [`EXIT_LOG_RESERVE`]) — «срок PUT N мс»; срок ещё не вышел
    ///   (предел ожидания — свой, запуск потока + сроки + [`WAIT_SLACK`], а поток долго шёл к
    ///   `PUT`) — «PUT не завершён (M из N мс)»: сервер в этом случае ни при чём. Срок считается
    ///   вышедшим с запасом [`PUT_LATE_SLACK`]: запрос, которому до срока осталось меньше шага
    ///   таймера ОС, ответа уже не дождётся;
    /// - `PUT` вернулся, очередь не обработана — чем кончился запрос (ответ сервера, срок,
    ///   сеть): медленная локальная доработка не выдаётся за медленный сервер.
    ///
    /// Снимок при этом в очереди, пока поток её не удалил: уйдёт или повторится со следующим
    /// хуком, `PUT` заменяет снимок целиком. Замок занят — поток пишет строку прямо сейчас: хук
    /// ждёт конца потока и пробует замок ещё раз; снова занят (запись в `hooks.log` дольше
    /// хвоста) — хук выходит без строки, а строку допишет поток, если успеет до конца процесса.
    /// После своей строки хук отдаёт потоку остаток хвоста (уже без замка): опоздавший успевает
    /// доработать очередь. Хвост — всё ожидание внутри `late` — не дольше [`EXIT_LOG_RESERVE`] и
    /// не дольше остатка бюджета: предел ожидания потока уже вышел ([`wait_snapshot`]), и когда
    /// он свой, а не по бюджету, хук не досиживает до конца бюджета (так было в `76458e3`:
    /// `snapshot_queue_written_before_state` — хук шёл весь бюджет вместо своего предела).
    fn late(&self, env: &Env, done: &std::sync::mpsc::Receiver<()>, event: &str) {
        let until = std::time::Instant::now() + EXIT_LOG_RESERVE;
        self.late_with(env, event, &mut || {
            let tail = until.saturating_duration_since(std::time::Instant::now());
            done.recv_timeout(tail.min(env.budget_left())).is_ok()
        });
    }

    /// [`SendNote::late`] с ожиданием конца потока `wait_done` (`true` — поток закончил):
    /// в работе — сигнал потока не дольше хвоста, в тестах — подмена без времени.
    fn late_with(&self, env: &Env, event: &str, wait_done: &mut dyn FnMut() -> bool) {
        use std::sync::TryLockError;
        let mut g = match self.stage.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(e)) => e.into_inner(),
            Err(TryLockError::WouldBlock) => {
                if wait_done() {
                    return;
                }
                match self.stage.try_lock() {
                    Ok(g) => g,
                    Err(TryLockError::Poisoned(e)) => e.into_inner(),
                    Err(TryLockError::WouldBlock) => return,
                }
            }
        };
        let line = match std::mem::replace(&mut *g, Stage::Logged) {
            Stage::Before => Some(format!(
                "snapshot: не готов до выхода хука ({event}, прошло {} мс)",
                env.started.elapsed().as_millis()
            )),
            Stage::Put { what, limit, t0 } => {
                let late = t0.elapsed() + PUT_LATE_SLACK >= limit;
                let (spent, limit) = (t0.elapsed().as_millis(), limit.as_millis());
                Some(if late {
                    format!("{what}: срок PUT {limit} мс, итог отправки не записан до выхода хука")
                } else {
                    format!("{what}: PUT не завершён ({spent} из {limit} мс) до выхода хука")
                })
            }
            Stage::PutDone { what, outcome } => Some(format!(
                "{what}: {outcome}, очередь не обработана до выхода хука"
            )),
            Stage::Logged => None,
        };
        if let Some(line) = line {
            env.log(&line);
        }
        drop(g);
        wait_done();
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

/// Ожидание замка очереди снимка `.snapq-….json.lock` (правка очереди — микросекунды).
const QUEUE_LOCK_WAIT: Duration = Duration::from_millis(20);

/// Очередь сессии с пределом размера ([`cache::CACHE_MAX`]): сырые байты и разбор.
fn read_queue(q: &Path) -> Option<(Vec<u8>, Queued)> {
    let m = std::fs::metadata(q).ok()?;
    if !m.is_file() || m.len() > cache::CACHE_MAX {
        return None;
    }
    let raw = std::fs::read(q).ok()?;
    let item = serde_json::from_slice(&raw).ok()?;
    Some((raw, item))
}

/// Кладёт снимок в очередь сессии под замком (новый заменяет старый; счётчик попыток старому
/// серверу сохраняется). Права файла — 0600 на unix ([`write_atomic`]). Замок не взят — запись
/// без него (fail-open: счётчик мог бы потеряться, снимок — нет).
fn enqueue(env: &Env, session: &str, body: Value) -> bool {
    let q = queue_path(env, session);
    let _lock = cache::file_lock(&q, QUEUE_LOCK_WAIT);
    let old_server = read_queue(&q).map_or(0, |(_, x)| x.old_server);
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

/// Ответ сервера с маршрутом снимка «сессии нет» (404 с `type` `…/session-not-found`, задача
/// 17c) — в отличие от 404 старого сервера без маршрута (`about:blank` или без тела).
fn session_not_found(e: &CliError) -> bool {
    e.http_status() == Some(404)
        && e.problem
            .as_ref()
            .and_then(|p| p["type"].as_str())
            .is_some_and(|t| t.ends_with("/session-not-found"))
}

/// Итог отправки очереди.
#[derive(Debug, PartialEq, Eq)]
pub enum Sent {
    Ok,
    /// Отправлен, но очередь не удалена: замок очереди занят (снимок уйдёт ещё раз).
    OkKept,
    /// Остался в очереди (причина без текста снимка).
    Queued(String),
    /// Отброшен: повтор не поможет (400, 413, 422).
    Dropped(String),
}

/// Срок `PUT` снимка, мс ([`SNAPSHOT_PUT_MS`]); отладочная сборка или фича `test-hooks` —
/// `ZAM_TEST_SNAPSHOT_PUT_MS` (тесты содержимого под нагрузкой).
fn put_ms() -> u64 {
    #[cfg(any(feature = "test-hooks", debug_assertions))]
    if let Some(ms) = std::env::var("ZAM_TEST_SNAPSHOT_PUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        return ms;
    }
    SNAPSHOT_PUT_MS
}

/// Отправка очереди сессии `session` со своим сроком [`SNAPSHOT_PUT_MS`] внутри бюджета хука (не
/// успел — снимок остаётся в очереди); `None` — нечего отправлять, старый сервер уже ответил
/// [`OLD_SERVER_ATTEMPTS`] раз, нет клиента или бюджета. Правка очереди по ответу — под замком и
/// только если очередь не сменилась за время запроса (новый снимок не трогается). `note` (поток
/// снимка `checkpoint`) получает начало и конец запроса; `what` — что отправляется.
fn flush(env: &Env, session: &str, what: &str, note: Option<&SendNote>) -> Option<Sent> {
    let q = queue_path(env, session);
    let (raw, mut item) = read_queue(&q)?;
    if item.old_server >= OLD_SERVER_ATTEMPTS {
        return None;
    }
    env.http_left().filter(|d| *d >= cache::FLUSH_MIN_LEFT)?;
    let put = Duration::from_millis(put_ms());
    let mut client = env.client_within(put)?;
    // фактический срок клиента — меньшее из своего срока `PUT` и остатка бюджета хука (у
    // `checkpoint` остаток бывает меньше 60 мс): с ним и сравнивается опоздание. Остаток
    // меряется **после** создания клиента (тот читает настройки подключения с диска и свой срок
    // считает уже после них): `limit` не больше срока клиента, сетевой отказ по сроку всегда
    // распознаётся как «срок PUT», сколько бы ни читались настройки. Бюджет вышел за время
    // чтения настроек — запроса нет (срок 0 мс был бы ложной причиной «срок PUT 0 мс»)
    let Some(left) = env.http_left() else {
        return Some(Sent::Queued(format!(
            "{} (прошло {} мс)",
            zam_core::HOOK_BUDGET_SPENT,
            env.started.elapsed().as_millis()
        )));
    };
    let limit = left.min(put);
    client.set_session(Some(item.session.clone()));
    let path = format!("/sessions/{}/snapshot", encode_segment(&item.session));
    let body = serde_json::to_vec(&item.body).ok()?;
    let t0 = std::time::Instant::now();
    if let Some(n) = note {
        n.sending(what, limit, t0);
    }
    let res = client.put_json_body(&path, body);
    // сеть: ответа нет за свой срок — отдельная причина (вечная очередь видна в журнале). Срок —
    // по типу ошибки клиента ([`CliError::is_timeout`]); правила «запрос шёл почти весь срок»
    // больше нет: оно ошибалось в обе стороны — ожидание сокета возвращается раньше срока на
    // шаг таймера ОС (срок не распознавался), а обрыв соединения у самого срока сроком не
    // является (распознавался бы)
    let timed_out = |e: &CliError| e.is_timeout();
    let late_why = format!("срок PUT {} мс", limit.as_millis());
    // чем кончился запрос — до работы с очередью: хук, не дождавшийся потока, пишет это, а не
    // «срок PUT» ([`SendNote::late`])
    if let Some(n) = note {
        let outcome = match &res {
            Ok(r) => format!("ответ получен (HTTP {})", r.status().as_u16()),
            Err(e) if timed_out(e) => late_why.clone(),
            Err(e) if e.http_status().is_some() => format!("ответ получен ({})", short(e)),
            Err(e) => format!("сеть: {}", short(e)),
        };
        n.put_done(what, outcome);
    }
    test_point(&env.cache_dir().join("snapshot"), "put-stall-ms");
    let lock = cache::file_lock(&q, QUEUE_LOCK_WAIT);
    let same = || lock.is_some() && read_queue(&q).is_some_and(|(now, _)| now == raw);
    Some(match res {
        Ok(_) => {
            if same() {
                let _ = std::fs::remove_file(&q);
                Sent::Ok
            } else if lock.is_none() {
                Sent::OkKept
            } else {
                // очередь сменилась за время запроса: в ней уже новый снимок
                Sent::Ok
            }
        }
        Err(e) if matches!(e.http_status(), Some(400 | 413 | 422)) => {
            if same() {
                let _ = std::fs::remove_file(&q);
            }
            Sent::Dropped(short(&e))
        }
        // сессии нет на сервере с маршрутом: не старый сервер — счётчик попыток не растёт, снимок
        // ждёт в очереди и уходит следующим хуком, когда сессия появится
        Err(e) if session_not_found(&e) => Sent::Queued(short(&e)),
        Err(e) if matches!(e.http_status(), Some(404 | 405)) => {
            item.old_server += 1;
            let n = item.old_server;
            let target = if same() {
                Some(item)
            } else if lock.is_some() {
                read_queue(&q).map(|(_, mut newer)| {
                    newer.old_server = newer.old_server.max(n);
                    newer
                })
            } else {
                None
            };
            if let Some(b) = target.and_then(|t| serde_json::to_vec(&t).ok()) {
                let _ = write_atomic(&q, &b);
            }
            Sent::Queued(format!(
                "старый сервер ({}), попытка {n} из {OLD_SERVER_ATTEMPTS}",
                short(&e)
            ))
        }
        Err(e) if timed_out(&e) => Sent::Queued(late_why),
        Err(e) => Sent::Queued(short(&e)),
    })
}

/// Запас, с которым хук считает срок идущего `PUT` вышедшим ([`SendNote::late`]): не меньше шага
/// таймера ОС (на Windows ~15,6 мс) — ожидание сокета возвращается с такой точностью. Итог
/// законченного запроса по времени не определяется: там — тип ошибки клиента ([`flush`]).
const PUT_LATE_SLACK: Duration = Duration::from_millis(20);

/// Причина ошибки для `hooks.log`: ответ сервера — только `HTTP <код>` и `type` problem+json
/// (`detail` мог бы нести кусок снимка); сеть — текст клиента (без тела запроса).
fn short(e: &CliError) -> String {
    match e.http_status() {
        Some(code) => {
            let ty = e
                .problem
                .as_ref()
                .and_then(|p| p["type"].as_str())
                .filter(|t| !t.is_empty() && t.chars().count() <= 80);
            match ty {
                Some(t) => format!("HTTP {code} {t}"),
                None => format!("HTTP {code}"),
            }
        }
        None => e.to_string().chars().take(160).collect(),
    }
}

/// Очереди других сессий проекта (кроме `except`), новые первыми, до [`FLUSH_OTHERS_MAX`]:
/// снимок `SessionEnd` при недоступном сервере уходит со следующим стартом. Идёт параллельно с
/// `/recall` и в его срок не входит.
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
            let (_, q) = read_queue(&e.path())?;
            (q.old_server < OLD_SERVER_ATTEMPTS && !except.contains(&q.session.as_str()))
                .then_some((t, q.session))
        })
        .collect();
    items.sort_by_key(|x| std::cmp::Reverse(x.0));
    for (_, id) in items.into_iter().take(FLUSH_OTHERS_MAX) {
        flush_logged(env, &id);
    }
}

/// Снимок `checkpoint` в отдельном потоке (повторное ревью 17b, `prompt_flushes_changed_queue`):
/// решение ([`job_for_checkpoint`]: `snapst-*`, журнал сессии), снимок ([`run`]) или отправка
/// прежней очереди ([`flush_noted`]) — всё вне пути сетевой части хука. Поток не scoped: хук ждёт
/// его не дольше [`wait_snapshot`], опоздавший обрывается с процессом (записи файлов — атомарные,
/// итог отправки за него пишет хук — [`SendNote`]).
pub fn spawn_checkpoint(env: &Env, inp: &Input, s: &Settings) -> SnapWait {
    let (tx, rx) = std::sync::mpsc::channel();
    let started = std::time::Instant::now();
    let note = std::sync::Arc::new(SendNote::default());
    let thread_note = note.clone();
    let event = inp.event.clone();
    let log_env = env.clone();
    let (env, inp, s) = (env.clone(), inp.clone(), *s);
    let spawned = std::thread::Builder::new()
        .name("zam-snapshot-job".into())
        .spawn(move || {
            // паника потока — строкой в `hooks.log`, хук не ждёт до своего предела
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Some(t) = transcript_of(&inp) {
                    test_panic(&t);
                }
                let note = Some(&*thread_note);
                match job_for_checkpoint(&env, &inp, &s) {
                    Some(job) => run_noted(&env, &s, job, true, note),
                    None => {
                        if let Some(id) = inp.session_id.as_deref().filter(|id| queued(&env, id)) {
                            flush_noted(&env, id, note);
                        }
                    }
                }
            }));
            if r.is_err() {
                env.log("snapshot: паника");
            }
            let _ = tx.send(());
        });
    if let Err(e) = spawned {
        log_env.log(&format!("snapshot: поток не запущен: {e}"));
    }
    SnapWait {
        rx,
        started,
        note,
        event,
    }
}

/// Поток снимка `checkpoint`: сигнал окончания, момент запуска, общий итог отправки и событие
/// хука (для строки «не готов»).
pub struct SnapWait {
    rx: std::sync::mpsc::Receiver<()>,
    started: std::time::Instant,
    note: std::sync::Arc<SendNote>,
    event: String,
}

/// Ожидание потока снимка `checkpoint` после сетевой части. Предел абсолютный: меньшее из конца
/// бюджета хука (от старта процесса) без [`EXIT_LOG_RESERVE`] и запуска потока + срок снимка +
/// срок `PUT` + [`WAIT_SLACK`] — медленное решение или сеть не уводят выход хука (и `decision:
/// block` у `Stop`) за бюджет; обычный снимок успевает целиком (очередь и `snapst-*` записаны).
/// Поток не успел — хук выходит без него, но итог отправки в `hooks.log` не теряется: строку за
/// поток пишет хук ([`SendNote::late`]).
pub fn wait_snapshot(env: &Env, s: &Settings, w: &SnapWait) {
    use std::sync::mpsc::RecvTimeoutError;
    let own = s.snapshot_deadline() + Duration::from_millis(put_ms()) + WAIT_SLACK;
    let own_left = own.saturating_sub(w.started.elapsed());
    let to_exit = env.budget_left().saturating_sub(EXIT_LOG_RESERVE);
    // `Disconnected` — поток не запущен (строка об этом уже в журнале)
    if w.rx.recv_timeout(own_left.min(to_exit)) == Err(RecvTimeoutError::Timeout) {
        w.note.late(env, &w.rx, &w.event);
    }
}

/// Запас ожидания потока снимка сверх его сроков: решение, журнал сессии, `snapst-*`, очередь.
const WAIT_SLACK: Duration = Duration::from_millis(50);

/// Хвост бюджета хука, который ожидание потока снимка оставляет хуку на строку итога отправки
/// ([`SendNote::late`]): запись строки не выводит хук за бюджет, остаток хвоста после неё снова
/// отдаётся потоку; он же — предел всего ожидания внутри `late`. Когда предел ожидания — бюджет хука, срок `PUT` истекает раньше него на
/// [`super::HTTP_RESERVE_MS`] − этот запас (20 мс): обычно поток успевает записать итог сам.
/// Когда предел — свой (запуск потока + сроки + [`WAIT_SLACK`]), срок `PUT` к нему мог и не
/// выйти — [`SendNote::late`] различает это по времени запроса.
const EXIT_LOG_RESERVE: Duration = Duration::from_millis(10);

/// Точки подмены для тестов (только отладочная сборка и фича `test-hooks`): файл
/// `<журнал>.<suffix>` с числом миллисекунд — поток засыпает. `decide-stall-ms` — в начале
/// решения ([`job_for_checkpoint`]: сетевая часть хука решения не ждёт), `write-stall-ms` — между
/// очередью и `snapst-*` в [`run`] (порядок записей при обрыве). Не от журнала, а от кэша
/// проекта — `<кэш>/snapshot.put-stall-ms`: между концом `PUT` и работой с очередью в [`flush`]
/// (поток не успел обработать очередь и записать итог до выхода хука).
fn test_point(transcript: &Path, suffix: &str) {
    #[cfg(any(feature = "test-hooks", debug_assertions))]
    if let Some(ms) = std::fs::read_to_string(format!("{}.{suffix}", transcript.display()))
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        std::thread::sleep(Duration::from_millis(ms));
    }
    #[cfg(not(any(feature = "test-hooks", debug_assertions)))]
    let _ = (transcript, suffix);
}

/// Точка паники для тестов: файл `<журнал>.panic` — поток снимка `checkpoint` паникует (только
/// отладочная сборка и фича `test-hooks`).
fn test_panic(transcript: &Path) {
    #[cfg(any(feature = "test-hooks", debug_assertions))]
    if std::path::PathBuf::from(format!("{}.panic", transcript.display())).is_file() {
        panic!("тестовая паника потока снимка");
    }
    #[cfg(not(any(feature = "test-hooks", debug_assertions)))]
    let _ = transcript;
}

/// Отправка очереди сессии без нового снимка, итог — в `hooks.log`.
pub fn flush_logged(env: &Env, session: &str) {
    flush_noted(env, session, None);
}

/// [`flush_logged`] с общим итогом отправки `note` (поток снимка `checkpoint`, [`SendNote`]).
fn flush_noted(env: &Env, session: &str, note: Option<&SendNote>) {
    let what = "снимок из очереди";
    log_sent(env, what, flush(env, session, what, note), false, note);
}

/// Неотправленная очередь снимка сессии есть (один `stat`).
pub fn queued(env: &Env, session: &str) -> bool {
    queue_path(env, session).is_file()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Instant;

    /// Окружение хука в пустом каталоге проекта (`target/tmp`), журнал хуков пуст; `age` —
    /// сколько «прошло» от старта процесса (бюджет `session-start` — 300 мс).
    fn env_at(name: &str, age: Duration) -> Env {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp")
            .join(format!("zam-cli-send-note-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".zeroaimemory")).unwrap();
        Env::new(
            super::super::Kind::SessionStart,
            Agent::Claude,
            Instant::now().checked_sub(age).unwrap(),
            dir.clone(),
            dir.join("home"),
        )
    }

    /// Окружение с целым бюджетом.
    fn fresh(name: &str) -> Env {
        env_at(name, Duration::ZERO)
    }

    /// Строки журнала хуков без времени и источника.
    fn lines(env: &Env) -> Vec<String> {
        std::fs::read_to_string(env.log_path())
            .unwrap_or_default()
            .lines()
            .map(|l| l.splitn(3, ' ').nth(2).unwrap_or("").to_string())
            .collect()
    }

    /// Сигнал конца потока, который уже не придёт (поток «оборван»): `late` не ждёт.
    fn gone() -> std::sync::mpsc::Receiver<()> {
        channel().1
    }

    /// Момент `ms` миллисекунд назад.
    fn ago(ms: u64) -> Instant {
        Instant::now()
            .checked_sub(Duration::from_millis(ms))
            .unwrap()
    }

    /// `PUT` начат и срок вышел — хук пишет «срок PUT»; опоздавшая строка потока не пишется.
    #[test]
    fn late_after_put_deadline_logs_limit_once() {
        let env = fresh("put");
        let n = SendNote::default();
        n.sending("снимок stop", Duration::from_millis(100), ago(500));
        n.late(&env, &gone(), "Stop");
        n.write(&env, "снимок stop, в очереди: срок PUT 100 мс");
        assert_eq!(
            lines(&env),
            ["снимок stop: срок PUT 100 мс, итог отправки не записан до выхода хука"]
        );
    }

    /// Ревью 76458e3, M4: до срока идущего `PUT` осталось меньше запаса — он считается вышедшим.
    #[test]
    fn late_within_slack_of_put_deadline_is_a_timeout() {
        let env = fresh("put-slack");
        let n = SendNote::default();
        let limit = Duration::from_millis(1000);
        n.sending("снимок stop", limit, ago(1000 - 10));
        n.late(&env, &gone(), "Stop");
        assert_eq!(
            lines(&env),
            ["снимок stop: срок PUT 1000 мс, итог отправки не записан до выхода хука"]
        );
    }

    /// `PUT` начат, срок не вышел (хук вышел по своему пределу) — не «срок PUT».
    #[test]
    fn late_before_put_deadline_is_not_a_timeout() {
        let env = fresh("put-early");
        let n = SendNote::default();
        n.sending("снимок stop", Duration::from_secs(600), Instant::now());
        n.late(&env, &gone(), "Stop");
        let got = lines(&env);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(
            got[0].starts_with("снимок stop: PUT не завершён ("),
            "{got:?}"
        );
        assert!(got[0].ends_with(" из 600000 мс) до выхода хука"), "{got:?}");
        assert!(!got[0].contains("срок PUT"), "{got:?}");
    }

    /// `PUT` вернулся, очередь не обработана — в строке ответ сервера, а не срок.
    #[test]
    fn late_after_response_logs_outcome() {
        let env = fresh("done");
        let n = SendNote::default();
        n.sending("снимок stop", Duration::ZERO, Instant::now());
        n.put_done("снимок stop", "ответ получен (HTTP 204)".to_string());
        n.late(&env, &gone(), "Stop");
        n.write(&env, "снимок stop, отправлен");
        assert_eq!(
            lines(&env),
            ["снимок stop: ответ получен (HTTP 204), очередь не обработана до выхода хука"]
        );
    }

    /// Поток записал итог сам — хук строку не добавляет.
    #[test]
    fn write_then_late_logs_once() {
        let env = fresh("written");
        let n = SendNote::default();
        n.sending("снимок stop", Duration::ZERO, Instant::now());
        n.write(&env, "снимок stop, отправлен");
        n.late(&env, &gone(), "Stop");
        assert_eq!(lines(&env), ["снимок stop, отправлен"]);
    }

    /// До `PUT` поток не дошёл — «не готов» с событием и временем; опоздавшие отметки и строка
    /// потока, повторный `late` второй строки не дают.
    #[test]
    fn late_before_put_logs_not_ready_once() {
        let env = fresh("before");
        let n = SendNote::default();
        n.late(&env, &gone(), "PreCompact");
        n.sending("снимок pre_compact", Duration::ZERO, Instant::now());
        n.late(&env, &gone(), "PreCompact");
        n.put_done("снимок pre_compact", "ответ получен (HTTP 204)".to_string());
        n.late(&env, &gone(), "PreCompact");
        n.write(&env, "снимок pre_compact, отправлен");
        let got = lines(&env);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(
            got[0].starts_with("snapshot: не готов до выхода хука (PreCompact, прошло "),
            "{got:?}"
        );
        assert!(got[0].ends_with(" мс)"), "{got:?}");
    }

    /// Отравленный замок (паника потока под ним) не мешает ни хуку, ни потоку.
    #[test]
    fn poisoned_lock_still_logs() {
        let env = fresh("poison");
        let n = std::sync::Arc::new(SendNote::default());
        n.sending("снимок stop", Duration::ZERO, Instant::now());
        let n2 = n.clone();
        let _ = std::thread::spawn(move || {
            let _g = n2.stage.lock().unwrap();
            panic!("отравление замка");
        })
        .join();
        assert!(n.stage.is_poisoned());
        n.put_done("снимок stop", "ответ получен (HTTP 500)".to_string());
        n.late(&env, &gone(), "Stop");
        n.write(&env, "снимок stop, в очереди: HTTP 500");
        assert_eq!(
            lines(&env),
            ["снимок stop: ответ получен (HTTP 500), очередь не обработана до выхода хука"]
        );
    }

    /// Поток, который держит замок до сигнала `release`, затем выполняет `then` (под замком)
    /// и отпускает; возвращается, когда замок уже взят.
    fn hold_lock(
        n: &std::sync::Arc<SendNote>,
        then: impl FnOnce(&mut Stage) + Send + 'static,
    ) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let (held_tx, held_rx) = channel();
        let (release_tx, release_rx) = channel::<()>();
        let n2 = n.clone();
        let h = std::thread::spawn(move || {
            let mut g = n2.lock();
            held_tx.send(()).unwrap();
            let _ = release_rx.recv();
            then(&mut g);
        });
        held_rx.recv().unwrap();
        (release_tx, h)
    }

    /// Замок занят потоком, пишущим строку: `late` ждёт конца потока и, дождавшись, своей строки
    /// не добавляет и замок больше не трогает. Ожидание подменено: поток дописывает строку
    /// ровно в нём.
    #[test]
    fn late_returns_after_writer_holding_lock_finished() {
        let env = fresh("busy");
        let n = std::sync::Arc::new(SendNote::default());
        n.sending("снимок stop", Duration::from_millis(100), ago(500));
        let env2 = env.clone();
        let (release, h) = hold_lock(&n, move |g| {
            env2.log("снимок stop, отправлен");
            *g = Stage::Logged;
        });
        let mut holder = Some((release, h));
        let mut waits = 0;
        n.late_with(&env, "Stop", &mut || {
            waits += 1;
            let (release, h) = holder.take().expect("ожидание — одно");
            release.send(()).unwrap();
            h.join().unwrap();
            true
        });
        assert_eq!(waits, 1);
        assert_eq!(lines(&env), ["снимок stop, отправлен"]);
    }

    /// Третье ревью 599c933: ожидание внутри `late` — хвост [`EXIT_LOG_RESERVE`], а не весь
    /// остаток бюджета (здесь — 300 мс, старт «в будущем»: остаток не тает): хук, вышедший по
    /// своему пределу, не досиживает до конца бюджета.
    #[test]
    fn late_tail_wait_is_not_whole_budget() {
        let mut env = fresh("tail");
        env.started = Instant::now() + Duration::from_secs(60);
        let n = SendNote::default();
        n.sending("снимок stop", Duration::from_millis(100), ago(500));
        // сигнала не будет, но канал жив: ожидание идёт до своего предела
        let (tx, rx) = channel::<()>();
        let t0 = Instant::now();
        n.late(&env, &rx, "Stop");
        let spent = t0.elapsed();
        assert!(spent < Duration::from_millis(150), "{spent:?}");
        assert_eq!(lines(&env).len(), 1);
        drop(tx);
    }

    /// Ревью 2c36319, M2: замок был занят, поток за время ожидания не закончил, но замок
    /// отпустил — `late` пробует замок ещё раз и пишет строку по этапу. Ожидание подменено:
    /// замок отпускается ровно между двумя попытками, без расчёта на время.
    #[test]
    fn late_retries_lock_after_failed_wait() {
        let env = fresh("retry");
        let n = std::sync::Arc::new(SendNote::default());
        n.sending("снимок stop", Duration::from_millis(100), ago(500));
        let (release, h) = hold_lock(&n, |_| {});
        let mut holder = Some((release, h));
        let mut waits = 0;
        n.late_with(&env, "Stop", &mut || {
            waits += 1;
            if let Some((release, h)) = holder.take() {
                release.send(()).unwrap();
                h.join().unwrap();
            }
            false
        });
        // ожидание до повтора замка и ещё одно — после строки хука (остаток бюджета потоку)
        assert_eq!(waits, 2);
        assert_eq!(
            lines(&env),
            ["снимок stop: срок PUT 100 мс, итог отправки не записан до выхода хука"]
        );
    }

    /// Ревью 2c36319, M2: замок занят и после ожидания (запись в `hooks.log` дольше остатка
    /// бюджета) — `late` выходит без строки, замок не ждёт и второй раз потока не ждёт.
    #[test]
    fn late_gives_up_when_lock_stays_busy() {
        let env = fresh("stuck");
        let n = std::sync::Arc::new(SendNote::default());
        n.sending("снимок stop", Duration::from_millis(100), ago(500));
        let (release, h) = hold_lock(&n, |_| {});
        let mut waits = 0;
        n.late_with(&env, "Stop", &mut || {
            waits += 1;
            false
        });
        assert_eq!(waits, 1);
        assert!(lines(&env).is_empty(), "{:?}", lines(&env));
        // замок отпущен только после возврата `late`
        release.send(()).unwrap();
        h.join().unwrap();
    }
}
