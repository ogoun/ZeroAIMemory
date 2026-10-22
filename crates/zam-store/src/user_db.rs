//! Базы пользователей: `users/<id>.db`, ленивое открытие, фиксированные пространства (§6.3 ТЗ).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use jiff::Timestamp;
use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, watch};
use zam_core::{Id, SolutionGuid};

use crate::jobs::{JobOp, PRIORITY_REINDEX};
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
    Migration {
        version: 6,
        sql: include_str!("../migrations/user/0006_pending_index.sql"),
    },
    Migration {
        version: 7,
        sql: include_str!("../migrations/user/0007_attribution.sql"),
    },
    Migration {
        version: 8,
        sql: include_str!("../migrations/user/0008_entries.sql"),
    },
    Migration {
        version: 9,
        sql: include_str!("../migrations/user/0009_node_certainty.sql"),
    },
    Migration {
        version: 10,
        sql: include_str!("../migrations/user/0010_edges.sql"),
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
    /// GUID решения; в JSON API — `agent_solution_id` (§11.6).
    #[serde(rename = "agent_solution_id")]
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
    /// Ячейка на пользователя: глобальный замок держится только на поиск ячейки, открытие
    /// (миграции, копия, дозаполнение ключей) идёт под замком своей ячейки и не задерживает
    /// другие базы.
    open: Arc<Mutex<HashMap<Id, Slot>>>,
}

type Slot = Arc<Mutex<Option<OpenDb>>>;

/// Открытая база и признак завершения разовых фоновых задач открытия (перечанковка R1,
/// дозаполнение рёбер графа).
#[derive(Debug)]
struct OpenDb {
    db: Db,
    /// `true` — фоновые задачи не нужны или уже завершились (успешно или с ошибкой).
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
    /// Так же, следом за ней, — дозаполнение рёбер графа базы, обновлённой до v10
    /// (`meta.edges_backfill`, задача 10 M3). Дождаться обеих — [`UserDbs::wait_rechunk`].
    pub async fn get(&self, user: Id) -> Result<Db, StoreError> {
        let slot = self.open.lock().await.entry(user).or_default().clone();
        // Открытие под замком ячейки: два одновременных вызова не создадут два Db на один
        // файл; другие базы при этом открываются параллельно.
        let mut cell = slot.lock().await;
        if let Some(e) = cell.as_ref() {
            return Ok(e.db.clone());
        }
        let db = Db::open(&self.dir.join(format!("{user}.db")), USER_MIGRATIONS)?;
        ensure_fixed_spaces(&db).await?;
        crate::nodes::backfill_embed_keys(&db).await?;
        // статус заметок из frontmatter (v9) — до первой записи: права certainty читают столбец
        crate::nodes::backfill_certainty(&db).await?;
        let pending = match rechunk_pending(&db).await? {
            Some(why) if !clear_rechunk_if_empty(&db).await? => Some(why),
            _ => None,
        };
        let edges = crate::graph::backfill_pending(&db).await?;
        let secrets = secrets_scan_pending(&db).await?;
        let (done, rechunked) = watch::channel(pending.is_none() && !edges && !secrets);
        *cell = Some(OpenDb {
            db: db.clone(),
            rechunked,
        });
        drop(cell);
        if pending.is_some() || edges || secrets {
            let bg = db.clone();
            tokio::spawn(async move {
                if let Some(why) = pending
                    && let Err(e) = rechunk_all(&bg, why).await
                {
                    // повторится при следующем открытии базы
                    tracing::warn!(error = %e, reason = why.as_str(), "перечанковка заметок прервана");
                }
                if edges && let Err(e) = crate::graph::backfill_edges(&bg).await {
                    tracing::warn!(error = %e, "дозаполнение рёбер графа прервано");
                }
                if secrets && let Err(e) = rescan_secrets(&bg).await {
                    // повторится при следующем открытии базы
                    tracing::warn!(error = %e, "перенарезка заметок и записей с секретами прервана");
                }
                let _ = done.send(true);
            });
        }
        Ok(db)
    }

    /// Ждёт завершения разовых фоновых задач открытия базы `user` (перечанковка R1,
    /// дозаполнение рёбер графа), если она открыта и они идут; иначе возвращается сразу.
    pub async fn wait_rechunk(&self, user: Id) {
        let slot = self.open.lock().await.get(&user).cloned();
        let rx = match slot {
            Some(s) => s.lock().await.as_ref().map(|e| e.rechunked.clone()),
            None => None,
        };
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

/// Ключ `meta`: разовая перенарезка заметок и записей со значениями секретов (сканер §7.6,
/// задача 11 M3) выполнена. Отсутствует у баз, открытых до сканера; у новой пустой базы
/// ставится сразу (все её записи нарезаются уже замаскированными).
const SECRETS_SCANNED: &str = "secrets_scanned_v2";

/// Нужна ли перенарезка секретов: ключа нет и в базе есть живые заметки или записи.
/// В пустой базе ключ ставится сразу.
async fn secrets_scan_pending(db: &Db) -> Result<bool, StoreError> {
    db.write(|tx| {
        if has_meta(tx, SECRETS_SCANNED)? {
            return Ok(false);
        }
        let any: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM nodes WHERE kind = 'note' AND deleted_at IS NULL)
                 OR EXISTS (SELECT 1 FROM entries WHERE deleted_at IS NULL)",
            [],
            |r| r.get(0),
        )?;
        if !any {
            tx.execute(
                "INSERT OR IGNORE INTO meta (key, value) VALUES (?1, '1')",
                [SECRETS_SCANNED],
            )?;
        }
        Ok(any)
    })
    .await
}

/// Перенарезка живых заметок и записей, в которых сканер находит секреты: значения из версий
/// до сканера остаются в `chunks`/FTS и Qdrant, пока текст не перенарезан (нарушение инварианта
/// «значения не в FTS/Qdrant»). Флаг ставится после успеха; прерванная работа повторится.
async fn rescan_secrets(db: &Db) -> Result<(), StoreError> {
    let notes = crate::nodes::rescan_secrets(db, PRIORITY_REINDEX).await?;
    let entries = crate::entries::rescan_entry_secrets(db, PRIORITY_REINDEX).await?;
    // векторы старых (незамаскированных) чанков больше не нужны: чистим кэш эмбеддингов
    let mut pruned = 0u64;
    if notes + entries > 0 {
        loop {
            let n = crate::embeddings::gc_embeddings(db, 1000).await?;
            pruned += n;
            if n < 1000 {
                break;
            }
        }
    }
    db.write(|tx| {
        tx.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES (?1, '1')",
            [SECRETS_SCANNED],
        )?;
        Ok(())
    })
    .await?;
    tracing::info!(
        notes,
        entries,
        pruned,
        "перенарезка секретов базы завершена"
    );
    Ok(())
}

/// Ключ `meta`: разовая перечанковка после миграции 0004 ещё не выполнена.
const RECHUNK_V4: &str = "rechunk_v4";

/// Ключ `meta`: часть чанков базы нарезана приближённым счётчиком токенов (свежая установка
/// без моделей, `ApproxCounter`). Ставится в транзакции нарезки, снимается после перенарезки
/// всех живых заметок точным счётчиком.
pub(crate) const CHUNKED_APPROX: &str = "chunked_approx";

/// Почему база перечанковывается при открытии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rechunk {
    /// Ruling R1: база M1 обновлена до v4.
    AfterV4,
    /// Чанки по оценке, а счётчик теперь точный (модель установлена после записи заметок).
    ExactCounter,
}

impl Rechunk {
    fn as_str(self) -> &'static str {
        match self {
            Rechunk::AfterV4 => "after_v4",
            Rechunk::ExactCounter => "exact_counter",
        }
    }
}

fn has_meta(c: &Connection, key: &str) -> rusqlite::Result<bool> {
    Ok(
        c.query_row("SELECT 1 FROM meta WHERE key = ?1", [key], |_| Ok(()))
            .optional()?
            .is_some(),
    )
}

/// Нужна ли перечанковка при открытии: флаг R1 или чанки по оценке при точном счётчике.
async fn rechunk_pending(db: &Db) -> Result<Option<Rechunk>, StoreError> {
    let exact = crate::nodes::token_counter_exact();
    db.read(move |c| {
        Ok(if has_meta(c, RECHUNK_V4)? {
            Some(Rechunk::AfterV4)
        } else if exact && has_meta(c, CHUNKED_APPROX)? {
            Some(Rechunk::ExactCounter)
        } else {
            None
        })
    })
    .await
}

/// База без живых заметок (новая или пустая) — флаги перечанковки снимаются сразу, без
/// фоновой задачи. `true` — сняты.
async fn clear_rechunk_if_empty(db: &Db) -> Result<bool, StoreError> {
    db.write(|tx| {
        let any: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM nodes WHERE kind = 'note' AND deleted_at IS NULL)",
            [],
            |r| r.get(0),
        )?;
        if !any {
            tx.execute(
                "DELETE FROM meta WHERE key IN (?1, ?2)",
                [RECHUNK_V4, CHUNKED_APPROX],
            )?;
        }
        Ok(!any)
    })
    .await
}

/// Снимает отметку «чанки по оценке», если счётчик точный: вызывается после перечанковки
/// всех живых заметок базы. Заметка, записанная во время перечанковки, нарезана тем же
/// точным счётчиком; счётчик, откатившийся на оценку посреди работы, отметку не снимает.
pub(crate) async fn clear_approx_if_exact(db: &Db) -> Result<(), StoreError> {
    if !crate::nodes::token_counter_exact() {
        return Ok(());
    }
    db.write(|tx| {
        tx.execute("DELETE FROM meta WHERE key = ?1", [CHUNKED_APPROX])?;
        Ok(())
    })
    .await
}

/// Перечанковка всех живых заметок текущим счётчиком с заданиями `upsert`.
///
/// Ruling R1: чанки M1 построены по секциям (`section`) и без векторов, поэтому при первом
/// открытии базы v4 все живые заметки перечанковываются. То же — при первом открытии с точным
/// счётчиком базы, чанки которой нарезаны оценкой: иначе листы могут не уложиться в окно
/// модели (усечение при инференсе) и разойтись с порогами §8.2. Флаги снимаются только после
/// успеха: прерванная перечанковка повторится при следующем открытии (она идемпотентна).
/// На большой базе — долго (и полный повторный инференс); в лог идут только счётчики.
async fn rechunk_all(db: &Db, why: Rechunk) -> Result<(), StoreError> {
    tracing::info!(reason = why.as_str(), "перечанковка заметок базы");
    let started = std::time::Instant::now();
    let (notes, upserts) = crate::nodes::rechunk_notes(db, None, PRIORITY_REINDEX).await?;
    db.write(|tx| {
        tx.execute("DELETE FROM meta WHERE key = ?1", [RECHUNK_V4])?;
        Ok(())
    })
    .await?;
    clear_approx_if_exact(db).await?;
    tracing::info!(
        reason = why.as_str(),
        notes,
        upserts,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "перечанковка заметок базы завершена"
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

/// Создаёт пространство `solution` с заданным id (`space_id` из реестра `system.db`).
/// Идемпотентно: то же пространство с этим GUID уже есть — возвращается оно (`false`, имя
/// не меняется). GUID у другого пространства или id занят — `Conflict`.
pub async fn create_solution_with_id(
    db: &Db,
    space_id: Id,
    guid: &SolutionGuid,
    name: &str,
) -> Result<(Space, bool), StoreError> {
    let (guid, name) = (guid.clone(), name.to_string());
    db.write(move |tx| {
        if let Some(s) = solution_by_guid(tx, guid.as_str())? {
            if s.id == space_id {
                return Ok((s, false));
            }
            return Err(StoreError::Conflict(format!(
                "решение {guid} уже у другого пространства"
            )));
        }
        let taken = tx
            .query_row("SELECT 1 FROM spaces WHERE id = ?1", [space_id], |_| Ok(()))
            .optional()?
            .is_some();
        if taken {
            return Err(StoreError::Conflict(format!(
                "пространство {space_id} уже существует"
            )));
        }
        tx.execute(
            "INSERT INTO spaces (id, kind, solution_guid, name, created_at)
             VALUES (?1, 'solution', ?2, ?3, ?4)",
            params![space_id, guid, name, fmt_ts(Timestamp::now())],
        )?;
        let s = solution_by_guid(tx, guid.as_str())?.ok_or(StoreError::NotFound)?;
        Ok((s, true))
    })
    .await
}

/// Меняет GUID пространства решения `space_id` (конфликт GUID при регистрации решений
/// M1–M2). Узлы, ревизии и чанки остаются на месте; живые индексируемые заметки получают
/// `upsert`, чтобы точки Qdrant получили новый `solution_guid` в payload. Нет такого
/// пространства решения — `NotFound`, GUID занят — `Conflict`.
pub async fn rename_solution_guid(
    db: &Db,
    space_id: Id,
    new: &SolutionGuid,
) -> Result<(), StoreError> {
    let new = new.clone();
    db.write(move |tx| {
        let cur: Option<Option<String>> = tx
            .query_row(
                "SELECT solution_guid FROM spaces WHERE id = ?1 AND kind = 'solution'",
                [space_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(cur) = cur else {
            return Err(StoreError::NotFound);
        };
        if cur.as_deref() == Some(new.as_str()) {
            return Ok(());
        }
        tx.execute(
            "UPDATE spaces SET solution_guid = ?2 WHERE id = ?1",
            params![space_id, new],
        )
        .map_err(|e| {
            if crate::system::is_unique_violation(&e) {
                StoreError::Conflict(format!("решение {new} уже есть в базе"))
            } else {
                e.into()
            }
        })?;
        let now = fmt_ts(Timestamp::now());
        let notes: Vec<(Id, i64)> = {
            let mut st = tx.prepare(
                "UPDATE nodes SET index_state = 'pending'
                 WHERE space_id = ?1 AND kind = 'note' AND deleted_at IS NULL
                   AND index_state <> 'excluded'
                 RETURNING id, current_rev",
            )?;
            st.query_map([space_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?
        };
        for (node, rev) in notes {
            crate::jobs::enqueue(tx, node, rev, JobOp::Upsert, PRIORITY_REINDEX, &now)?;
        }
        Ok(())
    })
    .await
}
