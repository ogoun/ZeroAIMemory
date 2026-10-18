//! `Db`: один поток-писатель (транзакции IMMEDIATE) и пул read-only соединений.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

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
    readers: Arc<ReaderPool>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Сначала закрываем читателей: писатель должен закрыться последним,
        // тогда SQLite делает checkpoint и удаляет -wal/-shm.
        self.readers
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        drop(self.writer.take());
        if let Some(h) = self.thread.take() {
            // Если последний Db уничтожается внутри самого писателя — join невозможен.
            if h.thread().id() != std::thread::current().id() {
                let _ = h.join();
            }
        }
    }
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
    /// Открывает (создаёт) БД, применяет PRAGMA и миграции, запускает писателя и читателей.
    pub fn open(path: &Path, migrations: &'static [Migration]) -> Result<Db, StoreError> {
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

        // Читатели открываются после того, как писатель создал файл и применил миграции.
        let mut readers = Vec::with_capacity(READERS);
        for _ in 0..READERS {
            readers.push(open_reader(path)?);
        }

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
                }
            })?;

        Ok(Db {
            inner: Arc::new(Inner {
                writer: Some(tx),
                thread: Some(thread),
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
