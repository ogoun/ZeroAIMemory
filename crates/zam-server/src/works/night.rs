//! Ночной исполнитель `NightRunner` (M7.3 §6.1 п. 5, решение координатора 56): отдельная задача
//! сервера, не воркер обслуживания и не зависит от `[backup]`.
//!
//! - **Тик** раз в [`WORKS_TICK`]: «пора ли» — [`super::schedule::tick`] на окне ночи `[works]
//!   at`/`tz`/`window_min` и часах `zam_store::time::now()`; на каждую новую ночь — сначала
//!   ночная проверка всех пользователей ([`crate::check::night`], при `[works] check =
//!   "nightly"`), затем работы ([`super::queue::night`]).
//! - **Шаги старта** — возврат работ модели в очередь ([`super::queue::on_start`]) и пометка
//!   прерванных прогонов проверки ([`crate::check::on_start`]) — один раз, при любом `[works]
//!   enabled`, **в фоне** задачи раннера: сборка состояния и роутера их не ждёт, базы
//!   пользователей на старте синхронно не открываются. Учтённые запуски
//!   ([`NightRunner::spawn_tracked`]) начинаются после них.
//! - **Контекст ночи** [`NightCtx`] — клон `AppState` (поиск и `/recall` тем же путём, что у
//!   агента, Vault, базы, конфигурация, копии) и признак остановки.
//! - **Ручные запуски** (`POST /works/run`, `POST /check/run`) идут через раннер:
//!   `st.night.spawn_tracked(fut)` с `NightCtx::new(st.clone())` внутри; [`NightRunner::stop`]
//!   ждёт их тем же сроком и снимает.
//! - **Кто держит клон состояния.** Создаётся раннер до сборки `AppState`
//!   ([`NightRunner::idle`]), запускается после неё ([`NightRunner::start`]). Клон состояния ночи
//!   держит **поле раннера**, задача — только `Weak` на него (между ночами) и сильную ссылку на
//!   время шага (старт, ночь); поле `night` у этого клона — **отвязанный раннер**: та же остановка
//!   и тот же учёт запусков, но без задачи — у него `start` и `stop` ничего не делают. Цикла `Arc`
//!   нет: когда уходит последний внешний клон `AppState` без [`NightRunner::stop`] (тесты, сборки
//!   в процессе), раннер уничтожается и отпускает клон сразу — лок каталога данных и соединения
//!   баз освобождаются, как до M7.3. Шаг, идущий в этот момент (старт, ночь, учтённый запуск),
//!   держит свой клон, пока рантайм его не снимет.
//! - **Останавливается первой** в `AppState::shutdown` ([`NightRunner::stop`]): признак
//!   остановки, ожидание задачи и учтённых запусков не дольше [`STOP_WAIT`] (функции видят
//!   признак — [`NightCtx::stopping`], [`NightCtx::stopped`] — и заканчивают шаг), затем снятие,
//!   клон отпускается — до остановки обслуживания и закрытия баз.
//! - **Правила для функций ночи и ручных запусков** (задачи 2, 3, 5 плана): признак остановки —
//!   только `ctx.stopping()` / `ctx.stopped()`; у `ctx.st().night` (отвязанный раннер) не звать
//!   `start`/`stop`; клон `NightCtx`/`AppState` не уносить в `tokio::spawn`, переживающий будущее
//!   шага (иначе снятие в `stop` его не отпустит) — фоновое — только `spawn_tracked`.
//! - **Узкая ручка**: блокирующее замыкание (`spawn_blocking`, вызов модели) получает только
//!   данные запроса — ни `Db`, ни `AppState`: снятая задача бросает висящий вызов, и он не держит
//!   соединений баз.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use jiff::{Timestamp, civil};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use zam_store::StoreError;

use crate::app::AppState;
use crate::config::CheckMode;

/// Период тика ночного исполнителя.
pub const WORKS_TICK: Duration = Duration::from_secs(60);
/// Сколько [`NightRunner::stop`] ждёт конца задачи и учтённых запусков, прежде чем снять их.
pub const STOP_WAIT: Duration = Duration::from_secs(5);

/// Контекст ночи и ручных запусков: клон состояния сервера и признак остановки ночной части.
#[derive(Clone)]
pub struct NightCtx {
    st: AppState,
    stop: CancellationToken,
}

impl NightCtx {
    /// Контекст из клона состояния (признак остановки — ночного исполнителя этого состояния).
    pub fn new(st: AppState) -> NightCtx {
        let stop = st.night.shared.stop.clone();
        NightCtx { st, stop }
    }

    /// Состояние сервера: конфигурация, базы, поиск и `/recall` изнутри процесса, Vault, копии.
    pub fn st(&self) -> &AppState {
        &self.st
    }

    /// Сервер останавливается: шаг ночи заканчивается, долгий вызов бросается.
    pub fn stopping(&self) -> bool {
        self.stop.is_cancelled()
    }

    /// Ждёт остановки сервера (для `select!` с долгим вызовом).
    pub async fn stopped(&self) {
        self.stop.cancelled().await;
    }
}

/// Учтённый запуск не принят: сервер останавливается.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("сервер останавливается: новые внутренние работы не запускаются")]
pub struct Stopping;

/// Шаги старта раннера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Startup {
    /// Раннер не запускался: учтённым запускам ждать нечего.
    NotStarted,
    /// Шаги старта идут в фоне.
    Running,
    /// Шаги старта закончены.
    Done,
}

/// Общее у раннера и его отвязанного двойника в клоне состояния ночи.
struct Shared {
    stop: CancellationToken,
    /// Учтённые запуски; `None` — после [`NightRunner::stop`]: новых нет.
    tracked: Mutex<Option<JoinSet<()>>>,
    startup: watch::Sender<Startup>,
}

/// Номер раннера процесса (журнал тестовой точки остановки).
static SERIAL: AtomicU64 = AtomicU64::new(1);

/// Ночной исполнитель (см. модуль).
pub struct NightRunner {
    serial: u64,
    shared: Arc<Shared>,
    /// Отвязанный двойник (поле `night` клона состояния ночи): `start` и `stop` — без действия.
    detached: bool,
    /// Клон состояния ночи; задача держит на него `Weak`.
    ctx: Mutex<Option<Arc<NightCtx>>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for NightRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NightRunner")
            .field("serial", &self.serial)
            .field("detached", &self.detached)
            .field("started", &lock(&self.task).is_some())
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl NightRunner {
    /// Раннер без задачи — до сборки `AppState`.
    pub fn idle() -> Arc<NightRunner> {
        let shared = Arc::new(Shared {
            stop: CancellationToken::new(),
            tracked: Mutex::new(Some(JoinSet::new())),
            startup: watch::Sender::new(Startup::NotStarted),
        });
        Arc::new(NightRunner::with_shared(shared, false))
    }

    fn with_shared(shared: Arc<Shared>, detached: bool) -> NightRunner {
        NightRunner {
            serial: SERIAL.fetch_add(1, Ordering::Relaxed),
            shared,
            detached,
            ctx: Mutex::new(None),
            task: Mutex::new(None),
        }
    }

    /// Номер раннера в процессе (тестовые точки).
    #[doc(hidden)]
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// Запускает задачу в текущем рантайме tokio: шаги старта в фоне (при любом `[works]
    /// enabled`), затем — при `enabled` — ночной цикл. Повторный запуск, запуск после
    /// [`Self::stop`] и запуск у отвязанного двойника ничего не делают.
    pub fn start(&self, ctx: NightCtx) {
        if self.detached {
            tracing::warn!("ночной исполнитель: start у отвязанного раннера клона ночи пропущен");
            return;
        }
        let mut task = lock(&self.task);
        if task.is_some() || self.shared.stop.is_cancelled() {
            return;
        }
        let enabled = ctx.st.cfg.works.enabled;
        if !enabled {
            tracing::debug!(
                "ночные работы выключены ([works] enabled = false): только шаги старта"
            );
        }
        let NightCtx { mut st, .. } = ctx;
        // у клона ночи — отвязанный двойник с той же остановкой и учётом запусков: клон в поле
        // раннера не держит сам раннер (нет цикла `Arc`)
        st.night = Arc::new(NightRunner::with_shared(self.shared.clone(), true));
        let ctx = Arc::new(NightCtx {
            st,
            stop: self.shared.stop.clone(),
        });
        let weak = Arc::downgrade(&ctx);
        *lock(&self.ctx) = Some(ctx);
        #[cfg(feature = "test-hooks")]
        let period = test_hooks::tick().unwrap_or(WORKS_TICK);
        #[cfg(not(feature = "test-hooks"))]
        let period = WORKS_TICK;
        self.shared.startup.send_replace(Startup::Running);
        *task = Some(tokio::spawn(run(
            weak,
            self.shared.clone(),
            period,
            enabled,
        )));
    }

    /// Учтённый запуск (ручные работы и прогоны проверки, задачи 3 и 5 плана): `fut` идёт в фоне
    /// после шагов старта, [`Self::stop`] ждёт его тем же сроком [`STOP_WAIT`] и снимает. Сервер
    /// останавливается — [`Stopping`]. Клон состояния — внутри `fut` (`NightCtx::new`).
    pub fn spawn_tracked<F>(&self, fut: F) -> Result<(), Stopping>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if self.shared.stop.is_cancelled() {
            return Err(Stopping);
        }
        let mut tracked = lock(&self.shared.tracked);
        let Some(set) = tracked.as_mut() else {
            return Err(Stopping);
        };
        // законченные — из учёта
        while set.try_join_next().is_some() {}
        let mut startup = self.shared.startup.subscribe();
        let stop = self.shared.stop.clone();
        set.spawn(async move {
            // после шагов старта: ручной запуск не обгоняет возврат работ в очередь и пометку
            // прерванных прогонов
            tokio::select! {
                () = stop.cancelled() => return,
                () = async {
                    let _ = startup.wait_for(|s| *s != Startup::Running).await;
                } => {}
            }
            fut.await;
        });
        Ok(())
    }

    /// Учтённых запусков в полёте (тестовые точки и диагностика).
    #[doc(hidden)]
    pub fn tracked_running(&self) -> usize {
        lock(&self.shared.tracked).as_mut().map_or(0, |set| {
            while set.try_join_next().is_some() {}
            set.len()
        })
    }

    /// Останавливает задачу и учтённые запуски: признак остановки, ожидание не дольше
    /// [`STOP_WAIT`], затем снятие (незавершённый вызов брошен); клон состояния ночи отпускается.
    /// У отвязанного двойника — ничего.
    pub async fn stop(&self) {
        if self.detached {
            tracing::warn!("ночной исполнитель: stop у отвязанного раннера клона ночи пропущен");
            return;
        }
        self.shared.stop.cancel();
        let mut handle = lock(&self.task).take();
        let mut set = lock(&self.shared.tracked).take().unwrap_or_default();
        let mut main_done = handle.is_none();
        let waited = tokio::time::timeout(STOP_WAIT, async {
            if let Some(h) = handle.as_mut() {
                let _ = h.await;
                main_done = true;
            }
            while set.join_next().await.is_some() {}
        })
        .await;
        if waited.is_err() {
            tracing::warn!(
                "ночные работы не остановились за {} с: шаги сняты",
                STOP_WAIT.as_secs()
            );
            if let Some(h) = handle.as_ref().filter(|_| !main_done) {
                h.abort();
            }
            set.abort_all();
            // снятие — на ближайшем ожидании; висящее вне ожидания не держит остановку
            let _ = tokio::time::timeout(Duration::from_secs(1), async {
                if let Some(h) = handle.as_mut().filter(|_| !main_done) {
                    let _ = h.await;
                }
                while set.join_next().await.is_some() {}
            })
            .await;
        }
        drop(lock(&self.ctx).take());
        #[cfg(feature = "test-hooks")]
        test_hooks::log_stop(self.serial, "night");
    }
}

impl Drop for NightRunner {
    fn drop(&mut self) {
        if self.detached {
            return;
        }
        let task = self.task.get_mut().unwrap_or_else(|e| e.into_inner());
        if let Some(h) = task.take() {
            self.shared.stop.cancel();
            h.abort();
        }
    }
}

/// Задача раннера: шаги старта, затем (при `enabled`) тик, «пора ли», ночь.
async fn run(weak: Weak<NightCtx>, shared: Arc<Shared>, period: Duration, enabled: bool) {
    if let Some(ctx) = weak.upgrade() {
        startup(&ctx).await;
    }
    shared.startup.send_replace(Startup::Done);
    if !enabled {
        return;
    }
    let mut last: Option<civil::Date> = None;
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = shared.stop.cancelled() => return,
            _ = tick.tick() => {}
        }
        let Some(ctx) = weak.upgrade() else {
            return;
        };
        let works = &ctx.st.cfg.works;
        let tz = works.time_zone();
        let Some((night, until)) = super::schedule::tick(zam_store::time::now(), last, works, &tz)
        else {
            continue;
        };
        last = Some(night);
        run_night(&ctx, night, until).await;
    }
}

/// Шаги старта: возврат работ модели в очередь, пометка прерванных прогонов проверки. Сбой —
/// WARN, сервер работает.
async fn startup(ctx: &NightCtx) {
    #[cfg(feature = "test-hooks")]
    if let Some(f) = test_hooks::on_start() {
        if let Err(e) = f(ctx.clone()).await {
            tracing::warn!(error = %e, "шаги старта (тестовая точка) не удались");
        }
        return;
    }
    if let Err(e) = super::queue::on_start(ctx).await {
        tracing::warn!(error = %e, "внутренние работы: возврат работ в очередь при старте не удался");
    }
    if let Err(e) = crate::check::on_start(ctx).await {
        tracing::warn!(error = %e, "проверка памяти: пометка прерванных прогонов не удалась");
    }
}

/// Одна ночь: проверка всех пользователей (при `[works] check = "nightly"`), затем работы.
async fn run_night(ctx: &NightCtx, night: civil::Date, until: Timestamp) {
    tracing::info!(%night, until = %until, "ночная часть: начало ночи");
    if ctx.st.cfg.works.check == CheckMode::Nightly
        && let Err(e) = check_job(ctx, night, until).await
    {
        tracing::warn!(%night, error = %e, "ночная проверка памяти не завершилась");
    }
    if ctx.stopping() {
        return;
    }
    if let Err(e) = queue_job(ctx, night, until).await {
        tracing::warn!(%night, error = %e, "ночные работы не завершились");
    }
}

async fn check_job(ctx: &NightCtx, night: civil::Date, until: Timestamp) -> Result<(), StoreError> {
    #[cfg(feature = "test-hooks")]
    if let Some(f) = test_hooks::jobs().check {
        return f(ctx.clone(), night, until).await;
    }
    crate::check::night(ctx, night, until).await
}

async fn queue_job(ctx: &NightCtx, night: civil::Date, until: Timestamp) -> Result<(), StoreError> {
    #[cfg(feature = "test-hooks")]
    if let Some(f) = test_hooks::jobs().queue {
        return f(ctx.clone(), night, until).await;
    }
    super::queue::night(ctx, night, until).await
}

/// Будущее работы ночи тестовой точки.
pub type JobFuture = std::pin::Pin<Box<dyn Future<Output = Result<(), StoreError>> + Send>>;

/// Тестовые точки ночи (фича `test-hooks`): период тика, подмена каждой из двух функций ночи по
/// отдельности и шагов старта — для раннера сервера, собранного `app::build` (интеграционные
/// тесты); журнал остановки (`NightRunner::stop` и обслуживание в `AppState::shutdown`).
/// Настройки общие для процесса: тесты, которые их ставят, идут по одному.
#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub mod test_hooks {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use jiff::{Timestamp, civil};

    use super::{JobFuture, NightCtx, lock};

    /// Подменённая функция ночи: `(контекст, ночь, конец окна)`.
    pub type JobFn = Arc<dyn Fn(NightCtx, civil::Date, Timestamp) -> JobFuture + Send + Sync>;

    /// Подменённые шаги старта (вместо `queue::on_start` и `check::on_start` вместе).
    pub type StartFn = Arc<dyn Fn(NightCtx) -> JobFuture + Send + Sync>;

    /// Подмены функций ночи: `check` — вместо `check::night`, `queue` — вместо `queue::night`.
    #[derive(Clone, Default)]
    pub struct Jobs {
        pub check: Option<JobFn>,
        pub queue: Option<JobFn>,
    }

    static TICK: Mutex<Option<Duration>> = Mutex::new(None);
    static JOBS: Mutex<Jobs> = Mutex::new(Jobs {
        check: None,
        queue: None,
    });
    static ON_START: Mutex<Option<StartFn>> = Mutex::new(None);
    static STOPS: Mutex<Vec<(u64, &'static str)>> = Mutex::new(Vec::new());

    /// Период тика раннеров, запущенных после вызова.
    pub fn set_tick(d: Duration) {
        *lock(&TICK) = Some(d);
    }

    /// Подмены функций ночи (читаются на каждой ночи).
    pub fn set_jobs(j: Jobs) {
        *lock(&JOBS) = j;
    }

    /// Подмена шагов старта (читается при старте раннера).
    pub fn set_on_start(f: StartFn) {
        *lock(&ON_START) = Some(f);
    }

    /// Снимает подмены тика, функций ночи и шагов старта.
    pub fn reset() {
        *lock(&TICK) = None;
        *lock(&JOBS) = Jobs::default();
        *lock(&ON_START) = None;
    }

    /// Журнал остановки раннера `serial`: `night` — `NightRunner::stop` закончен, `maintenance`
    /// — остановлено обслуживание (`AppState::shutdown`).
    pub fn stop_log(serial: u64) -> Vec<&'static str> {
        lock(&STOPS)
            .iter()
            .filter(|(s, _)| *s == serial)
            .map(|(_, w)| *w)
            .collect()
    }

    pub(crate) fn tick() -> Option<Duration> {
        *lock(&TICK)
    }

    pub(crate) fn jobs() -> Jobs {
        lock(&JOBS).clone()
    }

    pub(crate) fn on_start() -> Option<StartFn> {
        lock(&ON_START).clone()
    }

    pub(crate) fn log_stop(serial: u64, what: &'static str) {
        lock(&STOPS).push((serial, what));
    }
}
