//! `Db`: один поток-писатель (транзакции IMMEDIATE) и пул read-only соединений.
//!
//! **Checkpoint WAL (ТЗ §17).** Автоматический checkpoint писателя выключен
//! (`wal_autocheckpoint=0`): иначе fsync базы при переносе кадров попадал бы в
//! задержку записи. Кадры переносит фоновый поток со своим соединением —
//! `wal_checkpoint(PASSIVE)` раз в [`DbOptions::checkpoint_interval`], а следом
//! `RESTART`, если в WAL больше [`DbOptions::wal_restart_bytes`].
//! Страховка на случай, если фоновый поток не успевает или умер: писатель после
//! задания делает `wal_checkpoint(RESTART)`, когда WAL больше
//! [`DbOptions::wal_limit_bytes`]. При закрытии — `wal_checkpoint(TRUNCATE)`,
//! затем закрывается последнее соединение и SQLite удаляет `-wal`/`-shm`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};

use crate::StoreError;
use crate::migrate::{self, Migration};

const READERS: usize = 4;

type WriterJob = Box<dyn FnOnce(&mut Connection) + Send>;

struct ReaderPool {
    path: PathBuf,
    conns: Mutex<Vec<Connection>>,
    sem: Arc<Semaphore>,
    // Число живых соединений (в пуле и выданных).
    live: AtomicUsize,
}

impl ReaderPool {
    /// Одно соединение потеряно безвозвратно. Если живых не осталось, закрываем
    /// семафор: иначе `read()` навсегда завис бы на `acquire_owned`; теперь он вернёт `Closed`.
    fn shrink(&self) {
        if self.live.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.sem.close();
        }
    }
}

/// Соединение из пула; возвращается обратно при drop, в том числе при панике.
struct PooledConn {
    conn: Option<Connection>,
    pool: Arc<ReaderPool>,
    permit: Option<OwnedSemaphorePermit>,
}

fn open_reader(path: &Path) -> Result<Connection, StoreError> {
    let r = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    set_common_pragmas(&r)?;
    Ok(r)
}

impl Drop for PooledConn {
    fn drop(&mut self) {
        let Some(mut c) = self.conn.take() else {
            return;
        };
        // Незакрытая транзакция (в т.ч. при панике) не должна утечь в пул.
        if !c.is_autocommit() && c.execute_batch("ROLLBACK").is_err() {
            match open_reader(&self.pool.path) {
                Ok(fresh) => c = fresh,
                Err(_) => {
                    // Заменить нечем: пул сжимается, пермит гасим, чтобы
                    // инвариант «пермит ⇒ есть соединение» сохранился.
                    if let Some(p) = self.permit.take() {
                        p.forget();
                    }
                    self.pool.shrink();
                    return;
                }
            }
        }
        self.pool
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(c);
    }
}

struct Inner {
    // Option: в Drop отправитель закрывается раньше join потока-писателя.
    writer: Option<mpsc::Sender<WriterJob>>,
    thread: Option<JoinHandle<()>>,
    // Фоновый checkpoint: закрытие канала — сигнал остановки.
    checkpointer_stop: Option<mpsc::Sender<()>>,
    checkpointer: Option<JoinHandle<()>>,
    readers: Arc<ReaderPool>,
}

/// Если последний `Db` уничтожается внутри самого потока — join невозможен.
fn join_unless_current(h: JoinHandle<()>) {
    if h.thread().id() != std::thread::current().id() {
        let _ = h.join();
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Сначала закрываем читателей и фоновый checkpoint: писатель должен
        // закрыться последним, тогда SQLite удаляет -wal/-shm.
        self.readers
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        drop(self.checkpointer_stop.take());
        if let Some(h) = self.checkpointer.take() {
            join_unless_current(h);
        }
        drop(self.writer.take());
        if let Some(h) = self.thread.take() {
            join_unless_current(h);
        }
    }
}

/// Параметры открытия [`Db`]. Умолчания — для работы; тесты меняют пороги.
#[derive(Debug, Clone)]
pub struct DbOptions {
    /// Запускать ли фоновый поток checkpoint.
    pub background_checkpoint: bool,
    /// Период фонового `wal_checkpoint(PASSIVE)`.
    pub checkpoint_interval: Duration,
    /// Размер WAL, после которого писатель сам делает `wal_checkpoint(RESTART)`;
    /// он же — `journal_size_limit` (до него WAL усекается после сброса).
    pub wal_limit_bytes: u64,
    /// Кадров в WAL больше чем на столько байт — фоновый поток после
    /// PASSIVE делает `RESTART`, чтобы WAL начался сначала и при непрерывной записи.
    pub wal_restart_bytes: u64,
}

impl Default for DbOptions {
    fn default() -> Self {
        Self {
            background_checkpoint: true,
            checkpoint_interval: Duration::from_secs(1),
            wal_limit_bytes: 64 * 1024 * 1024,
            wal_restart_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Путь `<база>-wal` (суффикс к имени файла, как у SQLite).
fn wal_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push("-wal");
    PathBuf::from(s)
}
/// Размер WAL в байтах; 0 — файла нет. `fs::metadata` на Windows читает размер
/// через дескриптор файла, поэтому он актуален и при открытых соединениях.
fn wal_len(wal: &Path) -> u64 {
    std::fs::metadata(wal).map(|m| m.len()).unwrap_or(0)
}

/// `PRAGMA wal_checkpoint(<mode>)`; результат (busy, кадров, перенесено) не нужен.
fn checkpoint(conn: &Connection, mode: &str) -> Result<(), rusqlite::Error> {
    conn.query_row(&format!("PRAGMA wal_checkpoint({mode})"), [], |_| Ok(()))
}

/// Сколько миллисекунд фоновый RESTART ждёт писателя и читателей, прежде чем
/// отступить до следующего тика (пока он держит блокировку записи, писатель ждёт).
const RESTART_BUSY_MS: u64 = 50;

/// Фоновый поток: `wal_checkpoint(PASSIVE)` раз в `interval`. Если новых кадров
/// нет, PASSIVE лишь читает wal-index и ничего не пишет и не синхронизирует, —
/// отдельная проверка «WAL вырос» не нужна (размер и mtime файла на Windows
/// ненадёжны, `data_version` без транзакции чтения запаздывает). PASSIVE не ждёт
/// читателей и писателя; ошибки не фатальны — сработает страховка писателя.
///
/// При непрерывной записи WAL сам не начинается сначала: писатель перезапускает
/// его, только если к началу транзакции все кадры перенесены. Поэтому, если в WAL
/// больше кадров, чем на `restart_bytes`, поток делает `RESTART` сразу после
/// PASSIVE, перенёсшего всё: переносить нечего, блокировка записи держится мгновение.
/// Если же WAL вчетверо больше порога, а полного переноса не случилось, `RESTART`
/// делается и с переносом остатка — писатель ждёт его fsync, но WAL не дорастает
/// до страховочного предела писателя.
fn spawn_checkpointer(
    path: &Path,
    interval: Duration,
    restart_bytes: u64,
) -> Result<(mpsc::Sender<()>, JoinHandle<()>), StoreError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    set_common_pragmas(&conn)?;
    conn.pragma_update(None, "wal_autocheckpoint", 0)?;
    conn.busy_timeout(Duration::from_millis(RESTART_BUSY_MS))?;
    let page: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    let restart_frames = i64::try_from(restart_bytes / page.max(512) as u64).unwrap_or(i64::MAX);
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let handle = std::thread::Builder::new()
        .name("zam-db-checkpoint".into())
        .spawn(move || {
            while let Err(mpsc::RecvTimeoutError::Timeout) = stop_rx.recv_timeout(interval) {
                // (busy, кадров в WAL, перенесено)
                let Ok((busy, log, done)) =
                    conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, i64>(2)?,
                        ))
                    })
                else {
                    continue;
                };
                // всё перенесено — RESTART почти бесплатен; WAL вчетверо больше порога —
                // RESTART и с переносом остатка (писатель ждёт его fsync)
                if busy == 0 && log > restart_frames && (log == done || log > 4 * restart_frames) {
                    let _ = checkpoint(&conn, "RESTART");
                }
            }
            drop(conn);
        })?;
    Ok((stop_tx, handle))
}

/// Дескриптор базы; дёшево клонируется.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db").finish_non_exhaustive()
    }
}

fn set_common_pragmas(conn: &Connection) -> Result<(), StoreError> {
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

impl Db {
    /// Открывает (создаёт) БД, применяет PRAGMA и миграции, запускает писателя,
    /// читателей и фоновый checkpoint с параметрами по умолчанию.
    pub fn open(path: &Path, migrations: &'static [Migration]) -> Result<Db, StoreError> {
        Self::open_with(path, migrations, DbOptions::default())
    }

    /// То же, что [`Db::open`], с явными [`DbOptions`].
    pub fn open_with(
        path: &Path,
        migrations: &'static [Migration],
        opts: DbOptions,
    ) -> Result<Db, StoreError> {
        let mut conn = Connection::open(path)?;
        set_common_pragmas(&conn)?;
        let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError::Unsupported(format!(
                "не удалось включить journal_mode=WAL (получено: {mode})"
            )));
        }
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate::apply(&mut conn, path, migrations)?;
        // checkpoint — не в потоке писателя (см. документацию модуля)
        conn.pragma_update(None, "wal_autocheckpoint", 0)?;
        let size_limit = i64::try_from(opts.wal_limit_bytes).unwrap_or(i64::MAX);
        conn.pragma_update(None, "journal_size_limit", size_limit)?;

        // Читатели открываются после того, как писатель создал файл и применил миграции.
        let mut readers = Vec::with_capacity(READERS);
        for _ in 0..READERS {
            readers.push(open_reader(path)?);
        }

        let (checkpointer_stop, checkpointer) = if opts.background_checkpoint {
            let (stop, handle) =
                spawn_checkpointer(path, opts.checkpoint_interval, opts.wal_restart_bytes)?;
            (Some(stop), Some(handle))
        } else {
            (None, None)
        };

        let wal = wal_path(path);
        let limit = opts.wal_limit_bytes;
        let (tx, rx) = mpsc::channel::<WriterJob>();
        let thread = std::thread::Builder::new()
            .name("zam-db-writer".into())
            .spawn(move || {
                let mut conn = conn;
                while let Ok(job) = rx.recv() {
                    job(&mut conn);
                    // Страховка: после любого задания соединение не в транзакции.
                    if !conn.is_autocommit() {
                        let _ = conn.execute_batch("ROLLBACK");
                    }
                    // Страховка от неограниченного WAL, если фоновый checkpoint отстал или умер.
                    if wal_len(&wal) > limit {
                        let _ = checkpoint(&conn, "RESTART");
                    }
                }
                // Закрытие: читатели и фоновый поток уже закрыты (Inner::drop).
                let _ = checkpoint(&conn, "TRUNCATE");
            })?;

        Ok(Db {
            inner: Arc::new(Inner {
                writer: Some(tx),
                thread: Some(thread),
                checkpointer_stop,
                checkpointer,
                readers: Arc::new(ReaderPool {
                    path: path.to_path_buf(),
                    conns: Mutex::new(readers),
                    sem: Arc::new(Semaphore::new(READERS)),
                    live: AtomicUsize::new(READERS),
                }),
            }),
        })
    }

    /// Выполняет `f` в потоке-писателе внутри транзакции IMMEDIATE; при `Err` — откат.
    /// Паника в `f` откатывает транзакцию, возвращает `Closed`, писатель продолжает работать.
    pub async fn write<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Transaction) -> Result<R, StoreError> + Send + 'static,
    ) -> Result<R, StoreError> {
        let (tx, rx) = oneshot::channel();
        let job: WriterJob = Box::new(move |conn| {
            let res = catch_unwind(AssertUnwindSafe(|| {
                let t = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let r = f(&t)?;
                t.commit()?;
                Ok(r)
            }))
            .unwrap_or(Err(StoreError::Closed));
            let _ = tx.send(res);
        });
        self.inner
            .writer
            .as_ref()
            .ok_or(StoreError::Closed)?
            .send(job)
            .map_err(|_| StoreError::Closed)?;
        rx.await.map_err(|_| StoreError::Closed)?
    }

    /// Выполняет `f` на read-only соединении из пула (через `spawn_blocking`).
    pub async fn read<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> Result<R, StoreError> + Send + 'static,
    ) -> Result<R, StoreError> {
        let pool = self.inner.readers.clone();
        let permit = pool
            .sem
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| StoreError::Closed)?;
        tokio::task::spawn_blocking(move || {
            let conn = pool
                .conns
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop()
                .ok_or(StoreError::Closed)?;
            let guard = PooledConn {
                conn: Some(conn),
                pool: pool.clone(),
                permit: Some(permit),
            };
            f(guard.conn.as_ref().expect("соединение только что выдано"))
        })
        .await
        .map_err(|_| StoreError::Closed)?
    }
}

// Проверка на этапе компиляции: Db должен быть Send + Sync.
const _: fn() = || {
    fn check<T: Send + Sync>() {}
    check::<Db>();
};

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_pool_closes_semaphore() {
        let pool = ReaderPool {
            path: PathBuf::new(),
            conns: Mutex::new(Vec::new()),
            sem: Arc::new(Semaphore::new(2)),
            live: AtomicUsize::new(2),
        };
        pool.shrink();
        assert!(pool.sem.clone().acquire_owned().await.is_ok());
        pool.shrink();
        assert!(pool.sem.clone().acquire_owned().await.is_err());
    }
}
