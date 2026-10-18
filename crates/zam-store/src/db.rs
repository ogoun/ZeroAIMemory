//! `Db`: один поток-писатель (транзакции IMMEDIATE) и пул read-only соединений.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};

use crate::StoreError;
use crate::migrate::{self, Migration};

const READERS: usize = 4;

type WriterJob = Box<dyn FnOnce(&mut Connection) + Send>;

struct ReaderPool {
    conns: Mutex<Vec<Connection>>,
    sem: Arc<Semaphore>,
}

/// Соединение из пула; возвращается обратно при drop, в том числе при панике.
struct PooledConn {
    conn: Option<Connection>,
    pool: Arc<ReaderPool>,
    _permit: OwnedSemaphorePermit,
}

impl Drop for PooledConn {
    fn drop(&mut self) {
        if let Some(c) = self.conn.take() {
            self.pool
                .conns
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(c);
        }
    }
}

struct Inner {
    // Закрытие отправителя (drop последнего Db) завершает поток-писатель.
    writer: mpsc::Sender<WriterJob>,
    readers: Arc<ReaderPool>,
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
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate::apply(&mut conn, path, migrations)?;

        // Читатели открываются после того, как писатель создал файл и применил миграции.
        let mut readers = Vec::with_capacity(READERS);
        for _ in 0..READERS {
            let r = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            set_common_pragmas(&r)?;
            readers.push(r);
        }

        let (tx, rx) = mpsc::channel::<WriterJob>();
        std::thread::Builder::new()
            .name("zam-db-writer".into())
            .spawn(move || {
                let mut conn = conn;
                while let Ok(job) = rx.recv() {
                    job(&mut conn);
                }
            })?;

        Ok(Db {
            inner: Arc::new(Inner {
                writer: tx,
                readers: Arc::new(ReaderPool {
                    conns: Mutex::new(readers),
                    sem: Arc::new(Semaphore::new(READERS)),
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
                _permit: permit,
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
