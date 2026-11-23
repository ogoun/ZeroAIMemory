//! Ночной исполнитель `NightRunner` (M7.3 §6.1 п. 5, решение координатора 56): отдельная задача
//! сервера, не воркер обслуживания и не зависит от `[backup]`.
//!
//! - **Тик** раз в [`WORKS_TICK`]: «пора ли» — [`super::schedule::tick`] на окне ночи `[works]
//!   at`/`tz`/`window_min` и часах `zam_store::time::now()`; на каждую новую ночь — сначала
//!   ночная проверка всех пользователей ([`crate::check::night`], при `[works] check =
//!   "nightly"`), затем работы ([`super::queue::night`]).
//! - **Контекст ночи** [`NightCtx`] — клон `AppState` (поиск и `/recall` тем же путём, что у
//!   агента, Vault, базы, конфигурация, копии) и признак остановки. REST-обработчики ручных
//!   запусков строят тот же контекст: `NightCtx::new(st.clone())`.
//! - **Создаётся до сборки `AppState`** ([`NightRunner::idle`]), **запускается после неё**
//!   ([`NightRunner::start`], при `[works] enabled`). Клон состояния ночи держит сам раннер, а
//!   задача — только `Weak` на него (между ночами) и сильную ссылку на время ночи; поле `night` у
//!   клона — отвязанный раннер без задачи. Цикла `Arc` нет: когда уходит последний внешний клон
//!   `AppState` (без [`NightRunner::stop`] — тесты, сборки в процессе), раннер уничтожается и
//!   отпускает клон сразу — лок каталога данных и соединения баз освобождаются, как до M7.3.
//! - **Останавливается первой** в `AppState::shutdown` ([`NightRunner::stop`]): признак
//!   остановки, ожидание конца задачи не дольше [`STOP_WAIT`] (функции ночи видят признак —
//!   [`NightCtx::stopping`] — и заканчивают шаг), затем задача снимается, клон отпускается — до
//!   остановки обслуживания и закрытия баз.
//! - **Узкая ручка**: блокирующее замыкание (`spawn_blocking`, вызов модели) получает только
//!   данные запроса — ни `Db`, ни `AppState`: снятая задача бросает висящий вызов, и он не держит
//!   соединений баз.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use jiff::{Timestamp, civil};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use zam_store::StoreError;

use crate::app::AppState;
use crate::config::CheckMode;

/// Период тика ночного исполнителя.
pub const WORKS_TICK: Duration = Duration::from_secs(60);
/// Сколько [`NightRunner::stop`] ждёт конца задачи, прежде чем снять её.
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
        let stop = st.night.stop.clone();
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

/// Номер раннера процесса (журнал тестовой точки остановки).
static SERIAL: AtomicU64 = AtomicU64::new(1);

/// Ночной исполнитель (см. модуль).
pub struct NightRunner {
    serial: u64,
    stop: CancellationToken,
    /// Клон состояния ночи; задача держит на него `Weak`.
    ctx: Mutex<Option<Arc<NightCtx>>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for NightRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NightRunner")
            .field("serial", &self.serial)
            .field("started", &lock(&self.task).is_some())
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl NightRunner {
    /// Раннер без задачи — до сборки `AppState` (и при `[works] enabled = false`).
    pub fn idle() -> Arc<NightRunner> {
        Arc::new(NightRunner::with_token(CancellationToken::new()))
    }

    fn with_token(stop: CancellationToken) -> NightRunner {
        NightRunner {
            serial: SERIAL.fetch_add(1, Ordering::Relaxed),
            stop,
            ctx: Mutex::new(None),
            task: Mutex::new(None),
        }
    }

    /// Номер раннера в процессе (тестовые точки).
    #[doc(hidden)]
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// Запускает задачу ночи в текущем рантайме tokio — при `[works] enabled`; повторный запуск
    /// и запуск после [`Self::stop`] ничего не делают.
    pub fn start(&self, ctx: NightCtx) {
        if !ctx.st.cfg.works.enabled {
            tracing::debug!("ночные работы выключены: [works] enabled = false");
            return;
        }
        let mut task = lock(&self.task);
        if task.is_some() || self.stop.is_cancelled() {
            return;
        }
        let NightCtx { mut st, .. } = ctx;
        // у клона ночи — отвязанный раннер с тем же признаком остановки: клон в поле раннера не
        // держит сам раннер (нет цикла `Arc`), а `NightCtx::new` от него видит ту же остановку
        st.night = Arc::new(NightRunner::with_token(self.stop.clone()));
        let ctx = Arc::new(NightCtx {
            st,
            stop: self.stop.clone(),
        });
        let weak = Arc::downgrade(&ctx);
        *lock(&self.ctx) = Some(ctx);
        #[cfg(feature = "test-hooks")]
        let period = test_hooks::tick().unwrap_or(WORKS_TICK);
        #[cfg(not(feature = "test-hooks"))]
        let period = WORKS_TICK;
        *task = Some(tokio::spawn(run(weak, self.stop.clone(), period)));
    }

    /// Останавливает задачу: признак остановки, ожидание не дольше [`STOP_WAIT`], затем снятие
    /// (незавершённый вызов брошен); клон состояния ночи отпускается.
    pub async fn stop(&self) {
        self.stop.cancel();
        let handle = lock(&self.task).take();
        if let Some(mut h) = handle
            && tokio::time::timeout(STOP_WAIT, &mut h).await.is_err()
        {
            tracing::warn!(
                "ночные работы не остановились за {} с: шаг ночи снят",
                STOP_WAIT.as_secs()
            );
            h.abort();
            // снятие задачи — на ближайшем ожидании; висящее вне ожидания не держит остановку
            let _ = tokio::time::timeout(Duration::from_secs(1), h).await;
        }
        drop(lock(&self.ctx).take());
        #[cfg(feature = "test-hooks")]
        test_hooks::log_stop(self.serial, "night");
    }
}

impl Drop for NightRunner {
    fn drop(&mut self) {
        let task = self.task.get_mut().unwrap_or_else(|e| e.into_inner());
        if let Some(h) = task.take() {
            self.stop.cancel();
            h.abort();
        }
    }
}

/// Цикл задачи: тик, «пора ли», ночь.
async fn run(weak: Weak<NightCtx>, stop: CancellationToken, period: Duration) {
    let mut last: Option<civil::Date> = None;
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
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

/// Тестовые точки ночи (фича `test-hooks`): период тика и подмена каждой из двух функций ночи по
/// отдельности — для раннера сервера, собранного `app::build` (интеграционные тесты); журнал
/// остановки (`NightRunner::stop` и обслуживание в `AppState::shutdown`). Настройки общие для
/// процесса: тесты, которые их ставят, идут по одному.
#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub mod test_hooks {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use jiff::{Timestamp, civil};

    use super::{JobFuture, NightCtx, lock};

    /// Подменённая функция ночи: `(контекст, ночь, конец окна)`.
    pub type JobFn = Arc<dyn Fn(NightCtx, civil::Date, Timestamp) -> JobFuture + Send + Sync>;

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
    static STOPS: Mutex<Vec<(u64, &'static str)>> = Mutex::new(Vec::new());

    /// Период тика раннеров, запущенных после вызова.
    pub fn set_tick(d: Duration) {
        *lock(&TICK) = Some(d);
    }

    /// Подмены функций ночи (читаются на каждой ночи).
    pub fn set_jobs(j: Jobs) {
        *lock(&JOBS) = j;
    }

    /// Снимает подмены тика и функций ночи.
    pub fn reset() {
        *lock(&TICK) = None;
        *lock(&JOBS) = Jobs::default();
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

    pub(crate) fn log_stop(serial: u64, what: &'static str) {
        lock(&STOPS).push((serial, what));
    }
}
