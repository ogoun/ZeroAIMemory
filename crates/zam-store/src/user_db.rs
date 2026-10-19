//! Базы пользователей: `users/<id>.db`, ленивое открытие, фиксированные пространства (§6.3 ТЗ).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use jiff::Timestamp;
use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, watch};
use zam_core::Id;

use crate::jobs::PRIORITY_REINDEX;
use crate::time::fmt_ts;
use crate::{Db, Migration, StoreError};

/// Миграции пользовательской БД.
pub static USER_MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("../migrations/user/0001_init.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("../migrations/user/0002_notes.sql"),
    },
    Migration {
        version: 3,
        sql: include_str!("../migrations/user/0003_revisions_append_only.sql"),
    },
    Migration {
        version: 4,
        sql: include_str!("../migrations/user/0004_index.sql"),
    },
    Migration {
        version: 5,
        sql: include_str!("../migrations/user/0005_embed_key.sql"),
    },
];

/// Вид пространства.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpaceKind {
    Rules,
    Human,
    Knowledge,
    Solution,
}

impl SpaceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SpaceKind::Rules => "rules",
            SpaceKind::Human => "human",
            SpaceKind::Knowledge => "knowledge",
            SpaceKind::Solution => "solution",
        }
    }
}

impl ToSql for SpaceKind {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl FromSql for SpaceKind {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        match v.as_str()? {
            "rules" => Ok(SpaceKind::Rules),
            "human" => Ok(SpaceKind::Human),
            "knowledge" => Ok(SpaceKind::Knowledge),
            "solution" => Ok(SpaceKind::Solution),
            other => Err(rusqlite::types::FromSqlError::Other(
                format!("неизвестный вид пространства: {other}").into(),
            )),
        }
    }
}

/// Пространство пользователя.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Space {
    pub id: Id,
    pub kind: SpaceKind,
    pub solution_guid: Option<String>,
    pub name: String,
}

/// Фиксированные пространства, создаваемые в каждой пользовательской БД.
const FIXED: [(SpaceKind, &str); 3] = [
    (SpaceKind::Rules, "Rules"),
    (SpaceKind::Human, "Human"),
    (SpaceKind::Knowledge, "Knowledge"),
];

/// Кэш открытых пользовательских баз; дёшево клонируется.
#[derive(Clone, Debug)]
pub struct UserDbs {
    dir: PathBuf,
    open: Arc<Mutex<HashMap<Id, OpenDb>>>,
}

/// Открытая база и признак завершения разовой перечанковки (R1).
#[derive(Debug)]
struct OpenDb {
    db: Db,
    /// `true` — перечанковка не нужна или уже завершилась (успешно или с ошибкой).
    rechunked: watch::Receiver<bool>,
}

impl UserDbs {
    /// `users_dir` — каталог `users/` из `DataDir::users_dir()`.
    pub fn new(users_dir: PathBuf) -> UserDbs {
        UserDbs {
            dir: users_dir,
            open: Arc::default(),
        }
    }

    /// База пользователя: открывается лениво (с миграцией и копией) и кэшируется.
    ///
    /// Чанки без `embed_key` (созданные до v5) получают ключ сразу, до возврата базы.
    ///
    /// Если база только что обновлена до v4, разовая перечанковка (R1) запускается
    /// фоновой задачей tokio один раз на открытие: `get` её не ждёт, записи во время
    /// неё безопасны (пакеты идемпотентны и читают текущую ревизию в своей транзакции).
    /// Дождаться её — [`UserDbs::wait_rechunk`].
    pub async fn get(&self, user: Id) -> Result<Db, StoreError> {
        // Открытие под замком: два одновременных вызова не создадут два Db на один файл.
        let mut open = self.open.lock().await;
        if let Some(e) = open.get(&user) {
            return Ok(e.db.clone());
        }
        let db = Db::open(&self.dir.join(format!("{user}.db")), USER_MIGRATIONS)?;
        ensure_fixed_spaces(&db).await?;
        crate::nodes::backfill_embed_keys(&db).await?;
        let pending = rechunk_pending(&db).await? && !clear_rechunk_if_empty(&db).await?;
        let (done, rechunked) = watch::channel(!pending);
        open.insert(
            user,
            OpenDb {
                db: db.clone(),
                rechunked,
            },
        );
        drop(open);
        if pending {
            let bg = db.clone();
            tokio::spawn(async move {
                if let Err(e) = rechunk_after_v4(&bg).await {
                    // повторится при следующем открытии базы
                    tracing::warn!(error = %e, "перечанковка после миграции индекса прервана");
                }
                let _ = done.send(true);
            });
        }
        Ok(db)
    }

    /// Ждёт завершения разовой перечанковки (R1) базы `user`, если она открыта и
    /// перечанковка идёт; иначе возвращается сразу.
    pub async fn wait_rechunk(&self, user: Id) {
        let rx = self
            .open
            .lock()
            .await
            .get(&user)
            .map(|e| e.rechunked.clone());
        if let Some(mut rx) = rx {
            // ошибка — отправитель закрыт без `true` (задача прервана): ждать нечего
            let _ = rx.wait_for(|done| *done).await;
        }
    }
}

/// Идемпотентно добавляет недостающие фиксированные пространства.
async fn ensure_fixed_spaces(db: &Db) -> Result<(), StoreError> {
    db.write(|tx| {
        let now = fmt_ts(Timestamp::now());
        for (kind, name) in FIXED {
            tx.execute(
                "INSERT INTO spaces (id, kind, name, created_at)
                 SELECT ?1, ?2, ?3, ?4
                 WHERE NOT EXISTS (SELECT 1 FROM spaces WHERE kind = ?2)",
                params![Id::new(), kind, name, now],
            )?;
        }
        Ok(())
    })
    .await
}

/// Ключ `meta`: разовая перечанковка после миграции 0004 ещё не выполнена.
const RECHUNK_V4: &str = "rechunk_v4";

/// Флаг разовой перечанковки после миграции 0004 стоит.
async fn rechunk_pending(db: &Db) -> Result<bool, StoreError> {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT 1 FROM meta WHERE key = ?1",
            [RECHUNK_V4],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
    })
    .await
}

/// База без живых заметок (новая или пустая) — флаг R1 снимается сразу, без фоновой
/// задачи. `true` — снят.
async fn clear_rechunk_if_empty(db: &Db) -> Result<bool, StoreError> {
    db.write(|tx| {
        let any: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM nodes WHERE kind = 'note' AND deleted_at IS NULL)",
            [],
            |r| r.get(0),
        )?;
        if !any {
            tx.execute("DELETE FROM meta WHERE key = ?1", [RECHUNK_V4])?;
        }
        Ok(!any)
    })
    .await
}

/// Ruling R1: чанки M1 построены по секциям (`section`) и без векторов, поэтому при
/// первом открытии базы v4 все живые заметки перечанковываются текущим счётчиком
/// и получают `upsert`. Флаг снимается только после успеха: прерванная перечанковка
/// повторится при следующем открытии (она идемпотентна). На большой базе — долго;
/// в лог идут только счётчики.
async fn rechunk_after_v4(db: &Db) -> Result<(), StoreError> {
    tracing::info!("перечанковка заметок после миграции индекса (v4)");
    let started = std::time::Instant::now();
    let (notes, upserts) = crate::nodes::rechunk_notes(db, None, PRIORITY_REINDEX).await?;
    db.write(|tx| {
        tx.execute("DELETE FROM meta WHERE key = ?1", [RECHUNK_V4])?;
        Ok(())
    })
    .await?;
    tracing::info!(
        notes,
        upserts,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "перечанковка после миграции индекса завершена"
    );
    Ok(())
}

/// Пространства: rules, human, knowledge, затем решения по имени.
pub async fn list_spaces(db: &Db) -> Result<Vec<Space>, StoreError> {
    db.read(|c| {
        let mut st = c.prepare(&format!(
            "SELECT {SPACE_COLS} FROM spaces
             ORDER BY CASE kind WHEN 'rules' THEN 0 WHEN 'human' THEN 1
                                WHEN 'knowledge' THEN 2 ELSE 3 END, name, id"
        ))?;
        let rows = st.query_map([], space_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}

const SPACE_COLS: &str = "id, kind, solution_guid, name";

fn space_row(r: &Row) -> rusqlite::Result<Space> {
    Ok(Space {
        id: r.get(0)?,
        kind: r.get(1)?,
        solution_guid: r.get(2)?,
        name: r.get(3)?,
    })
}

fn solution_by_guid(c: &Connection, guid: &str) -> Result<Option<Space>, StoreError> {
    Ok(c.query_row(
        &format!("SELECT {SPACE_COLS} FROM spaces WHERE solution_guid = ?1"),
        [guid],
        space_row,
    )
    .optional()?)
}

/// Пространство по id; нет — `None`.
pub async fn get_space(db: &Db, id: Id) -> Result<Option<Space>, StoreError> {
    db.read(move |c| {
        Ok(c.query_row(
            &format!("SELECT {SPACE_COLS} FROM spaces WHERE id = ?1"),
            [id],
            space_row,
        )
        .optional()?)
    })
    .await
}

/// Фиксированное пространство вида `kind` (`Rules`, `Human`, `Knowledge`).
/// Для `Solution` и при отсутствии — `NotFound`.
pub async fn space_of_kind(db: &Db, kind: SpaceKind) -> Result<Space, StoreError> {
    if kind == SpaceKind::Solution {
        return Err(StoreError::NotFound);
    }
    db.read(move |c| {
        c.query_row(
            &format!("SELECT {SPACE_COLS} FROM spaces WHERE kind = ?1"),
            [kind],
            space_row,
        )
        .optional()?
        .ok_or(StoreError::NotFound)
    })
    .await
}

/// Пространство решения по GUID (как сохранён: нижний регистр с дефисами).
pub async fn get_solution(db: &Db, guid: &str) -> Result<Option<Space>, StoreError> {
    let guid = guid.to_string();
    db.read(move |c| solution_by_guid(c, &guid)).await
}

/// Идемпотентно регистрирует решение: создаёт пространство `solution` с `guid`
/// или возвращает существующее (имя существующего не меняется).
/// Второе значение — `true`, если пространство создано сейчас.
/// GUID и имя проверяет вызывающий; GUID хранится как передан.
pub async fn create_solution(db: &Db, guid: &str, name: &str) -> Result<(Space, bool), StoreError> {
    let (guid, name) = (guid.to_string(), name.to_string());
    db.write(move |tx| {
        if let Some(s) = solution_by_guid(tx, &guid)? {
            return Ok((s, false));
        }
        let id = Id::new();
        tx.execute(
            "INSERT INTO spaces (id, kind, solution_guid, name, created_at)
             VALUES (?1, 'solution', ?2, ?3, ?4)",
            params![id, guid, name, fmt_ts(Timestamp::now())],
        )?;
        let s = solution_by_guid(tx, &guid)?.ok_or(StoreError::NotFound)?;
        Ok((s, true))
    })
    .await
}
