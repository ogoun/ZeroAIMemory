//! Точки остановки для тестов (фича `test-hooks`). Фичу включает только dev-зависимость
//! тестов `zam-server`; в сборке сервера и в релизе этого кода нет, а место вызова
//! компилируется в пустоту.
//!
//! [`hold_rechunk`] задерживает фоновую перенарезку при открытии базы: уведомление уже
//! создано (WARN, `rechunk_progress`), а перенарезка не началась — тест видит ход
//! «0 из N» на любой машине, а не только если перенарезка медленнее запроса.
//!
//! [`crate::time::freeze_now`] замораживает часы хранилища и сервера (`time::now()`, M6 §4.4).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::thread::ThreadId;

use jiff::Timestamp;

use tokio::sync::{OwnedRwLockWriteGuard, RwLock};

static GATE: LazyLock<Arc<RwLock<()>>> = LazyLock::new(Arc::default);

/// Запись [`GATE`] и поток, который её взял: повторный захват тем же потоком — паника
/// ([`write_gate`]), а не вечное ожидание самого себя.
pub struct GateGuard {
    _gate: OwnedRwLockWriteGuard<()>,
}

impl Drop for GateGuard {
    fn drop(&mut self) {
        *holder() = None;
    }
}

/// Поток — держатель записи [`GATE`].
static HOLDER: Mutex<Option<ThreadId>> = Mutex::new(None);

fn holder() -> MutexGuard<'static, Option<ThreadId>> {
    HOLDER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Запись [`GATE`] сразу или `Err(())`; держит её этот же поток — паника с `what`.
fn try_write_gate(what: &str) -> Result<GateGuard, ()> {
    match GATE.clone().try_write_owned() {
        Ok(g) => {
            *holder() = Some(std::thread::current().id());
            Ok(GateGuard { _gate: g })
        }
        Err(_) if *holder() == Some(std::thread::current().id()) => panic!(
            "{what}: блокировка тестовых точек уже у этого потока (живой FreezeGuard или              hold_rechunk) — снимите прежний guard"
        ),
        Err(_) => Err(()),
    }
}

/// Пока жив результат, фоновые перенарезки при открытии баз процесса ждут перед началом.
/// Повторный вызов из того же потока при живом результате (или [`crate::time::freeze_now`]) —
/// паника.
pub async fn hold_rechunk() -> GateGuard {
    if let Ok(g) = try_write_gate("hold_rechunk") {
        return g;
    }
    let g = GATE.clone().write_owned().await;
    *holder() = Some(std::thread::current().id());
    GateGuard { _gate: g }
}

/// Ждёт, пока перенарезки не задержаны ([`hold_rechunk`]).
pub(crate) async fn rechunk_gate() {
    drop(GATE.read().await);
}

/// Замороженный момент [`crate::time::now`], микросекунды; [`NOT_FROZEN`] — часы идут.
static FROZEN_US: AtomicI64 = AtomicI64::new(NOT_FROZEN);
const NOT_FROZEN: i64 = i64::MIN;

/// Замороженные часы [`crate::time::freeze_now`]: пока жив, `time::now()` отдаёт заданный момент;
/// держит запись [`GATE`].
pub struct FreezeGuard {
    _gate: GateGuard,
}

impl Drop for FreezeGuard {
    fn drop(&mut self) {
        // часы снимаются раньше блокировки: следующий держатель GATE их не видит
        FROZEN_US.store(NOT_FROZEN, Ordering::SeqCst);
    }
}

/// Момент заморозки или `None`.
pub(crate) fn frozen() -> Option<Timestamp> {
    match FROZEN_US.load(Ordering::SeqCst) {
        NOT_FROZEN => None,
        us => Timestamp::from_microsecond(us).ok(),
    }
}

pub(crate) fn freeze(t: Timestamp) -> FreezeGuard {
    let gate = match try_write_gate("freeze_now") {
        Ok(g) => g,
        Err(()) => {
            let g = block_on(GATE.clone().write_owned());
            *holder() = Some(std::thread::current().id());
            GateGuard { _gate: g }
        }
    };
    FROZEN_US.store(t.as_microsecond(), Ordering::SeqCst);
    FreezeGuard { _gate: gate }
}

/// Синхронное ожидание будущего парковкой потока: блокировки `tokio::sync` не требуют рантайма,
/// а `blocking_*` tokio паникуют в контексте рантайма (`#[tokio::test]`).
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park();
    }
}

// ---------- тексты SQL по соединениям (размер кэша запросов, M7.1 задача 9) ----------

/// Ячеек учёта SQL: у каждого соединения базы (писатель и читатели, [`crate::Db`]) — своя, по
/// кругу; соединений в процессе больше — ячейки делятся (оценка только растёт).
const SQL_SLOTS: usize = 32;

/// Соединение — ячейка учёта: путь базы и разные тексты SQL, выполненные с начала учёта, с
/// признаком «подготовленный запрос выполнялся повторно» ([`record_sql`]).
type SqlSlot = Mutex<(PathBuf, HashMap<String, bool>)>;

static SQL_SLOT_DATA: LazyLock<Vec<SqlSlot>> = LazyLock::new(|| {
    (0..SQL_SLOTS)
        .map(|_| Mutex::new(Default::default()))
        .collect()
});
static SQL_NEXT_SLOT: AtomicUsize = AtomicUsize::new(0);
static SQL_RECORDING: AtomicBool = AtomicBool::new(false);
/// Учёт SQL включён ([`sql_trace_on`]): без него `trace_v2` на соединения не ставится — встроенные
/// серверы замеров (`m4`, `m56`, `m57`, гейт `m6`) идут без трассировки (ревью задачи 9, круг 2).
static SQL_TRACE: AtomicBool = AtomicBool::new(false);
/// Ёмкость кэша `prepare_cached` под учётом SQL ([`sql_trace_on`]); 0 — без переопределения.
static SQL_CACHE: AtomicUsize = AtomicUsize::new(0);

fn sql_slot(i: usize) -> MutexGuard<'static, (PathBuf, HashMap<String, bool>)> {
    SQL_SLOT_DATA[i].lock().unwrap_or_else(|e| e.into_inner())
}

/// Колбэк `trace_v2` ячейки `N`: при идущем учёте — текст выполняемого запроса (без значений
/// параметров, `sqlite3_sql`) и признак повторного запуска того же подготовленного запроса.
/// `SQLITE_STMTSTATUS_RUN` растёт в `OP_Init` после события `SQLITE_TRACE_STMT`: в событии он —
/// число прежних запусков. Запрос из кэша `prepare_cached` запускается снова (> 0), разовый
/// (`prepare`, `query_row`, `execute`, `execute_batch`) готовится заново (0).
fn record_sql<const N: usize>(e: rusqlite::trace::TraceEvent<'_>) {
    if !SQL_RECORDING.load(Ordering::Relaxed) {
        return;
    }
    if let rusqlite::trace::TraceEvent::Stmt(s, _) = e {
        let reused = s.get_status(rusqlite::StatementStatus::Run) > 0;
        *sql_slot(N).1.entry(s.sql().into_owned()).or_default() |= reused;
    }
}

macro_rules! sql_slot_fns {
    ($($n:literal)*) => { [$(record_sql::<$n> as fn(rusqlite::trace::TraceEvent<'_>)),*] };
}

static SQL_SLOT_FNS: [fn(rusqlite::trace::TraceEvent<'_>); SQL_SLOTS] = sql_slot_fns!(
    0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31
);

/// Включить учёт SQL процесса — до открытия баз, которые надо считать (трассировка ставится при
/// открытии соединения): `trace_v2` на новые соединения и кэш `prepare_cached` ёмкостью `cache`
/// вместо `crate::db::STATEMENT_CACHE` (счёт не должен зависеть от размера кэша — у вытесненного
/// запроса нет повторных запусков). Только для теста, один в своём двоичном файле.
pub fn sql_trace_on(cache: usize) {
    SQL_CACHE.store(cache, Ordering::SeqCst);
    SQL_TRACE.store(true, Ordering::SeqCst);
}

/// Ёмкость кэша `prepare_cached` новых соединений, заданная [`sql_trace_on`].
pub(crate) fn statement_cache_override() -> Option<usize> {
    SQL_TRACE
        .load(Ordering::Relaxed)
        .then(|| SQL_CACHE.load(Ordering::Relaxed))
        .filter(|&c| c > 0)
}

/// Учёт текстов SQL соединения `conn` базы `path` (зовёт [`crate::Db`] при открытии соединения),
/// если включён [`sql_trace_on`].
pub(crate) fn trace_statements(conn: &rusqlite::Connection, path: &Path) {
    if !SQL_TRACE.load(Ordering::Relaxed) {
        return;
    }
    let i = SQL_NEXT_SLOT.fetch_add(1, Ordering::Relaxed) % SQL_SLOTS;
    sql_slot(i).0 = path.to_path_buf();
    conn.trace_v2(
        rusqlite::trace::TraceEventCodes::SQLITE_TRACE_STMT,
        Some(SQL_SLOT_FNS[i]),
    );
}

/// Начать учёт разных текстов SQL по соединениям (прежний учёт сбрасывается). Учёт общий для
/// процесса: тест, который его читает, — один в своём двоичном файле.
pub fn sql_texts_start() {
    for i in 0..SQL_SLOTS {
        sql_slot(i).1.clear();
    }
    SQL_RECORDING.store(true, Ordering::SeqCst);
}

/// Текст SQL соединения за время учёта ([`sql_texts_stop`]).
#[derive(Debug, Clone)]
pub struct SqlText {
    /// Текст запроса без значений параметров.
    pub sql: String,
    /// Тот же подготовленный запрос запускался повторно — запрос живёт в кэше: свой
    /// `prepare_cached` или внутренний запрос SQLite и FTS5 (их тексты — с `'main'.`).
    pub reused: bool,
}

/// Закончить учёт: по соединению — путь базы и разные тексты SQL с [`sql_texts_start`]
/// (соединения без запросов не входят), по тексту.
pub fn sql_texts_stop() -> Vec<(PathBuf, Vec<SqlText>)> {
    SQL_RECORDING.store(false, Ordering::SeqCst);
    (0..SQL_SLOTS)
        .filter_map(|i| {
            let s = sql_slot(i);
            (!s.1.is_empty()).then(|| {
                let mut v: Vec<SqlText> =
                    s.1.iter()
                        .map(|(sql, reused)| SqlText {
                            sql: sql.clone(),
                            reused: *reused,
                        })
                        .collect();
                v.sort_by(|a, b| a.sql.cmp(&b.sql));
                (s.0.clone(), v)
            })
        })
        .collect()
}

// ---------- проходы каналов FTS (проверка пропуска пустых, M7.1 задача 9) ----------

static FTS_RUNS: LazyLock<Mutex<HashMap<PathBuf, u64>>> = LazyLock::new(Default::default);
/// Счёт проходов включён ([`count_fts_channels`]): без него `fts_channels` не берёт блокировку и
/// не копирует путь базы.
static FTS_COUNT: AtomicBool = AtomicBool::new(false);

/// Включить счёт проходов каналов FTS процесса ([`fts_channel_runs`]).
pub fn count_fts_channels() {
    FTS_COUNT.store(true, Ordering::SeqCst);
}

/// Счёт проходов включён ([`count_fts_channels`]).
pub(crate) fn fts_counting() -> bool {
    FTS_COUNT.load(Ordering::Relaxed)
}

/// Каналы FTS базы `path` выполнили `MATCH` (`fts_channels` не пропустил их проверкой
/// `any_live_entry`); зовётся только при [`fts_counting`].
pub(crate) fn fts_channels_ran(path: &Path) {
    *FTS_RUNS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(path.to_path_buf())
        .or_default() += 1;
}

/// Сколько раз каналы FTS базы `path` выполняли `MATCH` с [`count_fts_channels`]
/// (`search_fts_channels`, `search_fts_channels_light`; пропущенные проверкой живых записей — не
/// в счёт).
pub fn fts_channel_runs(path: &Path) -> u64 {
    FTS_RUNS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
        .copied()
        .unwrap_or(0)
}
