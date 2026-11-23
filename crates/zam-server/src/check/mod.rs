//! Ночная проверка памяти (M7.3 §6.4–§6.5): пробы, прогоны тем же путём, что у агента, вердикт.
//!
//! - **Ночь** ([`night`]) — первым шагом ночи ночного исполнителя ([`crate::works::NightRunner`]),
//!   для **всех** пользователей до работ: пользователи по одному, база с `works.check = off`
//!   пропускается, база, у которой за эту ночь уже есть ночной прогон (кроме `failed`, в том числе
//!   прерванных), — тоже; идёт копия — ждать (опрос раз в [`BACKUP_POLL`]) до конца окна; перед
//!   прогоном — пробы из кэша выжимок (до [`CACHE_PROBES_PER_NIGHT`] за ночь). База без проб с целью
//!   ночью не проверяется **[решение исполнителя]**: посторонние вопросы без своих записей ничего не
//!   меряют.
//! - **Ручной прогон** ([`start_manual`], `POST /check/run`, `zam check run`) — учтённым запуском
//!   раннера (`st.night.spawn_tracked`): окном не ограничен, `check = off` и `works.check = off` его
//!   не выключают, при `[works] enabled = false` — отказ (409 `/problems/works-disabled`, решение
//!   76); второй прогон пользователя, пока идёт первый (ночной или ручной), — отказ (409).
//! - **Старт** ([`on_start`]) — один раз в фоне раннера: прогоны `running`, оборванные остановкой
//!   сервера, — `failed` с `error = "interrupted"`; такой прогон ночным прогоном окна не считается,
//!   перезапуск в окне начинает новый.
//! - **Прогон** — [`run`]; **вердикт** — [`verdict`]; **посторонние вопросы** — [`negatives`].
//! - **Без следов** ([`Accounting::Off`]): поиск и подсказки проверки не пишут `usage`,
//!   `usage_opened`, `session_seen`, `session_tokens`, `tokens_unattributed` и не трогают `hits` и
//!   `last_used_at` выжимок кэша.
//!
//! Правила ночного кода (задача 1 плана): остановка — только `ctx.stopping()`/`ctx.stopped()`; у
//! `ctx.st().night` (отвязанный раннер) не звать `start`/`stop`/`spawn_tracked`; клон
//! `NightCtx`/`AppState` не уносить в `tokio::spawn` — фоновое только через учтённый запуск
//! раннера обработчика.

pub mod negatives;
pub mod run;
pub mod verdict;

use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp, civil};
use zam_core::Id;
use zam_store::StoreError;
use zam_store::check::Trigger;
use zam_store::probes::{
    InsertOutcome, NewProbe, ProbeAuthor, ProbeKind, cache_candidates, insert_probe, kind_since,
};

use crate::app::AppState;
use crate::works::NightCtx;

pub use verdict::{
    CHECK_DIGEST_LINE_AFTER_UPDATE_EN, CHECK_DIGEST_LINE_AFTER_UPDATE_RU, CHECK_DIGEST_LINE_EN,
    CHECK_DIGEST_LINE_RU,
};

/// Версия правил проверки (метрики, вердикт, набор посторонних вопросов) — часть отпечатка
/// `fp_hash`: её смена делает числа несопоставимыми (`changed`).
pub const CHECK_VERSION: u32 = 1;
/// Пауза между пробами прогона.
pub const PROBE_PAUSE: Duration = Duration::from_millis(25);
/// Опрос идущей копии перед ночным прогоном.
pub const BACKUP_POLL: Duration = Duration::from_secs(60);
/// Проб из кэша выжимок за ночь на пользователя (§6.4 п. 3).
pub const CACHE_PROBES_PER_NIGHT: u32 = 40;

/// Учёт выдачи поиска и подсказок (M7.3 §6.5 п. 2): `Normal` — как прежде (обработчики REST и
/// MCP), `Off` — прогон ночной проверки: ни `usage`, ни `usage_opened`, ни `session_seen`, ни
/// `session_tokens`, ни `tokens_unattributed`, у выжимок кэша — ни `hits`, ни `last_used_at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Accounting {
    Normal,
    Off,
}

impl Accounting {
    /// Выдача учитывается.
    pub fn records(self) -> bool {
        self == Accounting::Normal
    }
}

/// Прогоны, идущие в процессе: пользователь и прогон (один на пользователя). Пометка прерванных
/// при старте их не трогает.
static ACTIVE: Mutex<Vec<(Id, Id)>> = Mutex::new(Vec::new());

fn active() -> MutexGuard<'static, Vec<(Id, Id)>> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Пользователь занят прогоном, пока жив страж.
#[derive(Debug)]
pub struct Busy {
    user: Id,
}

impl Drop for Busy {
    fn drop(&mut self) {
        active().retain(|(u, _)| *u != self.user);
    }
}

/// Занимает пользователя прогоном `run`; уже занят — `Err(идущий прогон)`.
fn claim(user: Id, run: Id) -> Result<Busy, Id> {
    let mut a = active();
    if let Some((_, r)) = a.iter().find(|(u, _)| *u == user) {
        return Err(*r);
    }
    a.push((user, run));
    Ok(Busy { user })
}

/// Идущий прогон пользователя.
pub fn running(user: Id) -> Option<Id> {
    active().iter().find(|(u, _)| *u == user).map(|(_, r)| *r)
}

fn active_runs() -> Vec<Id> {
    active().iter().map(|(_, r)| *r).collect()
}

/// Ночной прогон проверки ночи `night` до конца окна `until` — для **всех** пользователей, до
/// работ ночи (§6.5 п. 1); `[works] check = "nightly"` проверяет вызывающий, `works.check`
/// пользователя — эта функция.
pub async fn night(ctx: &NightCtx, night: civil::Date, until: Timestamp) -> Result<(), StoreError> {
    let users = ctx.st().system.list_users().await?;
    for u in users {
        if ctx.stopping() || zam_store::time::now() >= until {
            break;
        }
        if u.disabled_at.is_some() {
            continue;
        }
        if let Err(e) = nightly_for(ctx, u.id, night, until).await {
            tracing::warn!(user = %u.id, %night, error = %e, "ночная проверка памяти пользователя не удалась");
        }
    }
    Ok(())
}

/// Ночной прогон пользователя `user` за ночь `night` без тика (тестовая точка, фича
/// `test-hooks`): окно — по `[works] at`/`tz`/`window_min`. `None` — прогона не было (выключен у
/// пользователя, уже был, копия или другой прогон не кончились до конца окна, нет проб).
#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub async fn run_night(
    ctx: &NightCtx,
    user: Id,
    night: civil::Date,
) -> Result<Option<Id>, StoreError> {
    let w = &ctx.st().cfg.works;
    let start = crate::schedule::slot(night, w.at_hm(), &w.time_zone());
    let until = start + SignedDuration::from_mins(i64::from(w.window_min));
    nightly_for(ctx, user, night, until).await
}

async fn nightly_for(
    ctx: &NightCtx,
    user: Id,
    night: civil::Date,
    until: Timestamp,
) -> Result<Option<Id>, StoreError> {
    let st = ctx.st();
    if !crate::works::settings::load(&st.system, user).await?.check {
        return Ok(None);
    }
    let db = st.users.get(user).await?;
    ensure_marked(st, user, &db).await?;
    if zam_store::check::has_nightly(&db, night).await? {
        return Ok(None);
    }
    // копия идёт — ждать до конца окна (§6.5 п. 1)
    if !wait_for(ctx, until, || st.backups.state().running.is_none()).await {
        return Ok(None);
    }
    let id = Id::new();
    // ручной прогон пользователя идёт — ждать его конца
    let mut busy = None;
    if !wait_for(ctx, until, || {
        busy = claim(user, id).ok();
        busy.is_some()
    })
    .await
    {
        return Ok(None);
    }
    let window = SignedDuration::from_mins(i64::from(st.cfg.works.window_min));
    harvest_cache(&db, until - window).await?;
    let probes = zam_store::probes::probe_counts(&db).await?;
    let with_target = probes
        .by_kind
        .iter()
        .filter(|(k, _)| k != ProbeKind::Negative.as_str())
        .map(|(_, n)| n)
        .sum::<u32>();
    if with_target == 0 {
        return Ok(None);
    }
    let r = run::execute(ctx, user, Trigger::Nightly, Some(night), id).await;
    drop(busy);
    r.map(Some)
}

/// Ждёт `ready()` с опросом [`BACKUP_POLL`] до конца окна `until`; `false` — окно кончилось или
/// сервер останавливается.
async fn wait_for(ctx: &NightCtx, until: Timestamp, mut ready: impl FnMut() -> bool) -> bool {
    let poll = backup_poll();
    loop {
        if ready() {
            return true;
        }
        if ctx.stopping() || zam_store::time::now() >= until {
            return false;
        }
        tokio::select! {
            () = ctx.stopped() => return false,
            () = tokio::time::sleep(poll) => {}
        }
    }
}

/// Пробы из кэша выжимок (§6.4 п. 3): действующие выжимки без пробы — вопрос `query`, цели —
/// источники (до 5), вид `cache`, автор `system`; до [`CACHE_PROBES_PER_NIGHT`] за ночь (с
/// `since` — начала окна ночи). Повтор вопроса пропускается; предел активных проб — конец.
async fn harvest_cache(db: &zam_store::Db, since: Timestamp) -> Result<u32, StoreError> {
    let done = kind_since(db, ProbeKind::Cache, since).await?;
    let room = CACHE_PROBES_PER_NIGHT.saturating_sub(done);
    if room == 0 {
        return Ok(0);
    }
    let mut added = 0;
    for c in cache_candidates(db, room as usize).await? {
        if added == room {
            break;
        }
        let split = zam_search::eval::probe_split(&c.targets[0].target);
        let p = NewProbe {
            space: Some(c.space),
            question: c.query,
            kind: ProbeKind::Cache,
            targets: c.targets,
            split,
            overlap: None,
            author_kind: ProbeAuthor::System,
            author: None,
            work_id: None,
        };
        match insert_probe(db, p).await? {
            InsertOutcome::Inserted(_) => added += 1,
            InsertOutcome::Duplicate => {}
            InsertOutcome::Full => break,
        }
    }
    Ok(added)
}

/// Отказ ручного прогона.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartError {
    /// `[works] enabled = false` (409 `/problems/works-disabled`).
    Disabled,
    /// У пользователя уже идёт прогон (409).
    Running(Id),
    /// Сервер останавливается (503).
    Stopping,
}

/// Ручной прогон проверки пользователя `user`: занимает пользователя и запускает прогон
/// учтённой задачей раннера (`stop` ждёт её и снимает; прерванный прогон — `interrupted` при
/// следующем старте); id прогона.
pub fn start_manual(st: &AppState, user: Id) -> Result<Id, StartError> {
    if !st.cfg.works.enabled {
        return Err(StartError::Disabled);
    }
    let id = Id::new();
    let busy = claim(user, id).map_err(StartError::Running)?;
    st.night
        .spawn_tracked(move |ctx: NightCtx| async move {
            let _busy = busy;
            if let Err(e) = run::execute(&ctx, user, Trigger::Manual, None, id).await {
                tracing::warn!(user = %user, run = %id, error = %e, "ручной прогон проверки памяти не удался");
            }
        })
        .map_err(|_| StartError::Stopping)?;
    Ok(id)
}

/// Прогон проверки пользователя сейчас — в текущей задаче (тесты, вызовы изнутри процесса);
/// пользователь занят — `Conflict`.
pub async fn run_now(ctx: &NightCtx, user: Id, trigger: Trigger) -> Result<Id, StoreError> {
    let id = Id::new();
    let _busy = claim(user, id).map_err(|r| {
        StoreError::Conflict(format!("у пользователя уже идёт прогон проверки {r}"))
    })?;
    run::execute(ctx, user, trigger, None, id).await
}

/// Базы, в которых этот экземпляр сервера уже пометил прерванные прогоны: экземпляр — момент
/// сборки состояния (`AppState::started_at`), пользователь.
static MARKED: Mutex<Vec<(std::time::Instant, Id)>> = Mutex::new(Vec::new());

/// Прогоны `running` базы `db` пользователя `user`, оборванные остановкой прежнего экземпляра
/// сервера, — `failed` с `error = "interrupted"` (§6.5 п. 1) — один раз на экземпляр сервера и
/// базу, до первого чтения и записи прогонов этой базы (статус, список, ночной и ручной прогон);
/// прогоны, идущие в этом процессе, не трогаются.
pub(crate) async fn ensure_marked(
    st: &AppState,
    user: Id,
    db: &zam_store::Db,
) -> Result<(), StoreError> {
    let key = (st.started_at, user);
    let lock = || MARKED.lock().unwrap_or_else(|e| e.into_inner());
    if lock().contains(&key) {
        return Ok(());
    }
    let n = zam_store::check::mark_interrupted(db, active_runs()).await?;
    if n > 0 {
        tracing::info!(user = %user, runs = n, "проверка памяти: прерванные прогоны помечены");
    }
    lock().push(key);
    Ok(())
}

/// Шаг старта раннера (при любом `[works] enabled`, в фоне): пометка прерванных прогонов (§6.5
/// п. 1) **отложена до первого обращения к базе пользователя** ([`ensure_marked`])
/// **[решение исполнителя]**: шаг старта держит клон состояния, и открытие всех баз пользователей
/// при старте держало бы лок каталога данных после ухода последнего внешнего клона без
/// `shutdown` (тесты `boot::first_run_creates_admin_and_token_once` и
/// `revoked_admin_tokens_do_not_issue_new_initial_token` пересобирают сервер на том же каталоге
/// сразу). Наблюдаемо то же: прогон, оборванный остановкой, любой читатель видит `failed`
/// (`interrupted`), ночным прогоном окна он не считается.
pub async fn on_start(ctx: &NightCtx) -> Result<(), StoreError> {
    tracing::debug!(
        instance = ?ctx.st().started_at,
        "проверка памяти: прерванные прогоны помечаются при первом обращении к базе"
    );
    Ok(())
}

/// Строка дайджеста при регрессии (M7.3 §6.5 п. 7): последний законченный ночной прогон
/// пользователя `user` — `regression`, норма не принята. Сбой чтения — без строки (WARN).
pub async fn digest_line(st: &AppState, user: Id) -> Option<String> {
    let r = async {
        let db = st.users.get(user).await?;
        zam_store::check::nightly_done(&db, 1).await
    };
    let last = match r.await {
        Ok(mut v) => v.pop()?,
        Err(e) => {
            tracing::warn!(error = %e, "recall: последний прогон проверки памяти не прочитан");
            return None;
        }
    };
    if last.accepted_at.is_some() {
        return None;
    }
    let v = verdict::from_json(last.verdict.as_ref()?)?;
    verdict::digest_line(&v, verdict::LineLang::Ru)
}

fn backup_poll() -> Duration {
    #[cfg(feature = "test-hooks")]
    if let Some(d) = *test_hooks::lock(&test_hooks::BACKUP_POLL) {
        return d;
    }
    BACKUP_POLL
}

/// Тестовые точки проверки (фича `test-hooks`): переопределения в миллисекундах — пауза между
/// пробами, предел прогона (`check_max_min`), опрос идущей копии; признаки «поиск без векторов» и
/// «идёт переиндексация»; версия сервера в контексте прогона. Общие для процесса: тесты, которые
/// их ставят, идут по одному.
#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub mod test_hooks {
    use std::sync::{Mutex, MutexGuard};
    use std::time::Duration;

    pub(crate) static PAUSE: Mutex<Option<Duration>> = Mutex::new(None);
    pub(crate) static MAX_RUN: Mutex<Option<Duration>> = Mutex::new(None);
    pub(crate) static BACKUP_POLL: Mutex<Option<Duration>> = Mutex::new(None);
    pub(crate) static DEGRADED: Mutex<Option<bool>> = Mutex::new(None);
    pub(crate) static REINDEXING: Mutex<Option<bool>> = Mutex::new(None);
    pub(crate) static VERSION: Mutex<Option<String>> = Mutex::new(None);

    pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Пауза между пробами вместо 25 мс.
    pub fn set_pause(d: Duration) {
        *lock(&PAUSE) = Some(d);
    }

    /// Предел прогона вместо `check_max_min`.
    pub fn set_max_run(d: Duration) {
        *lock(&MAX_RUN) = Some(d);
    }

    /// Опрос идущей копии вместо 60 с.
    pub fn set_backup_poll(d: Duration) {
        *lock(&BACKUP_POLL) = Some(d);
    }

    /// «Поиск шёл без векторов» вместо признака ответа поиска (`Some(false)` — серверу без
    /// модели в тестах — «с векторами»).
    pub fn set_degraded(v: Option<bool>) {
        *lock(&DEGRADED) = v;
    }

    /// «Идёт переиндексация» вместо `IndexRuntime::reindexing`.
    pub fn set_reindexing(v: Option<bool>) {
        *lock(&REINDEXING) = v;
    }

    /// Версия сервера в контексте прогона.
    pub fn set_version(v: Option<&str>) {
        *lock(&VERSION) = v.map(str::to_string);
    }

    /// Снимает все переопределения.
    pub fn reset() {
        *lock(&PAUSE) = None;
        *lock(&MAX_RUN) = None;
        *lock(&BACKUP_POLL) = None;
        *lock(&DEGRADED) = None;
        *lock(&REINDEXING) = None;
        *lock(&VERSION) = None;
    }
}
