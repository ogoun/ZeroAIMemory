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
//!   [`OLD_SERVER_ATTEMPTS`] снимок остаётся в очереди без новых попыток в этой сессии; 400, 413,
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
        flush(env, &job.session, &mut |limit| {
            if let Some(n) = note {
                n.sending(&what, limit);
            }
        })
    } else {
        None
    };
    test_point(&job.transcript, "log-stall-ms");
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
/// его только до конца своего бюджета ([`wait_snapshot`]), а срок `PUT` истекает за
/// [`super::HTTP_RESERVE_MS`] до этого конца: на занятой машине поток не успевал записать итог в
/// `hooks.log` до выхода процесса, и причина («срок PUT N мс») терялась (CI ubuntu, `77063ec`).
/// Поток отмечает здесь начало `PUT`, строку итога пишет под замком; хук, не дождавшись потока,
/// пишет строку за него. Строка итога — ровно одна.
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
    /// `PUT` начат: что отправляется и фактический срок запроса.
    Put { what: String, limit: Duration },
    /// Строка итога записана (потоком или хуком за него).
    Logged,
}

impl SendNote {
    fn lock(&self) -> std::sync::MutexGuard<'_, Stage> {
        self.stage.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Поток: `PUT` начат (`what` — что отправляется, `limit` — фактический срок).
    fn sending(&self, what: &str, limit: Duration) {
        let mut g = self.lock();
        if !matches!(*g, Stage::Logged) {
            *g = Stage::Put {
                what: what.to_string(),
                limit,
            };
        }
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

    /// Хук: поток не закончил к пределу ожидания. `PUT` начат, итога нет — срок `PUT` к этому
    /// моменту вышел (он раньше предела на [`super::HTTP_RESERVE_MS`] − [`EXIT_LOG_RESERVE`]),
    /// причину пишет хук; чем закончился запрос, поток записать не успел, поэтому в строке —
    /// только срок (снимок в очереди, пока поток её не удалил: уйдёт или повторится со следующим
    /// хуком, `PUT` заменяет снимок целиком). До `PUT` поток не дошёл — строка об этом. Поток
    /// пишет строку прямо сейчас (замок занят) — хук ждёт её не дольше остатка бюджета.
    fn late(&self, env: &Env, done: &std::sync::mpsc::Receiver<()>) {
        let mut g = match self.stage.try_lock() {
            Ok(g) => g,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                let _ = done.recv_timeout(env.budget_left());
                return;
            }
        };
        match std::mem::replace(&mut *g, Stage::Logged) {
            Stage::Put { what, limit } => env.log(&format!(
                "{what}: срок PUT {} мс, итог отправки не записан до выхода хука",
                limit.as_millis()
            )),
            Stage::Before => env.log("snapshot: не готов до выхода хука"),
            Stage::Logged => {}
        }
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
/// только если очередь не сменилась за время запроса (новый снимок не трогается). `on_put` —
/// перед самым запросом, с его фактическим сроком.
fn flush(env: &Env, session: &str, on_put: &mut dyn FnMut(Duration)) -> Option<Sent> {
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
    // распознаётся как «срок PUT», сколько бы ни читались настройки
    let limit = env.http_left().unwrap_or_default().min(put);
    client.set_session(Some(item.session.clone()));
    let path = format!("/sessions/{}/snapshot", encode_segment(&item.session));
    let body = serde_json::to_vec(&item.body).ok()?;
    on_put(limit);
    let t0 = std::time::Instant::now();
    let res = client.put_json_body(&path, body);
    let spent = t0.elapsed();
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
        // сеть: ответа нет за свой срок — отдельная причина (вечная очередь видна в журнале)
        Err(e) if e.http_status().is_none() && spent + PUT_LATE_SLACK >= limit => {
            Sent::Queued(format!("срок PUT {} мс", limit.as_millis()))
        }
        Err(e) => Sent::Queued(short(&e)),
    })
}

/// Запас при распознавании «срок PUT вышел» (запрос шёл почти весь срок).
const PUT_LATE_SLACK: Duration = Duration::from_millis(5);

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
    SnapWait { rx, started, note }
}

/// Поток снимка `checkpoint`: сигнал окончания, момент запуска и общий итог отправки.
pub struct SnapWait {
    rx: std::sync::mpsc::Receiver<()>,
    started: std::time::Instant,
    note: std::sync::Arc<SendNote>,
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
        w.note.late(env, &w.rx);
    }
}

/// Запас ожидания потока снимка сверх его сроков: решение, журнал сессии, `snapst-*`, очередь.
const WAIT_SLACK: Duration = Duration::from_millis(50);

/// Хвост бюджета хука, который ожидание потока снимка оставляет хуку на строку итога отправки
/// ([`SendNote::late`]): запись строки не выводит хук за бюджет. Срок `PUT` истекает раньше
/// предела ожидания на [`super::HTTP_RESERVE_MS`] − этот запас (20 мс): обычно поток успевает
/// записать итог сам.
const EXIT_LOG_RESERVE: Duration = Duration::from_millis(10);

/// Точки подмены для тестов (только отладочная сборка и фича `test-hooks`): файл
/// `<журнал>.<suffix>` с числом миллисекунд — поток засыпает. `decide-stall-ms` — в начале
/// решения ([`job_for_checkpoint`]: сетевая часть хука решения не ждёт), `write-stall-ms` — между
/// очередью и `snapst-*` в [`run`] (порядок записей при обрыве), `log-stall-ms` — между концом
/// `PUT` и строкой итога в [`run`] (поток не успел записать итог до выхода хука).
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
    let r = flush(env, session, &mut |limit| {
        if let Some(n) = note {
            n.sending(what, limit);
        }
    });
    log_sent(env, what, r, false, note);
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
