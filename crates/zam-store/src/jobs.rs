//! Outbox векторной индексации (§8.1 ТЗ, таблица `index_jobs`) и данные для воркера.
//!
//! Задания ставятся записью узла в той же транзакции (см. `nodes`): на узел —
//! не больше одного активного (`queued` | `running`) задания, новая запись
//! заменяет активное. Обработка узла сериализована:
//! - заменяемое `queued` получает новые `rev`/`op` на месте;
//! - заменяемое `running` остаётся `running` (второй раз воркеру не выдаётся),
//!   получает новые `rev`/`op` и `dirty = 1`;
//! - каждая выдача ([`claim_jobs`]) увеличивает `claim_gen`; [`complete_job`] и
//!   [`fail_job`] принимаются только от текущей выдачи (`running` и тот же
//!   `claim_gen`), иначе — [`JobOutcome::Stale`];
//! - завершение «грязного» задания возвращает его в `queued` (`attempts = 0`,
//!   `not_before = now`): новая ревизия обрабатывается следующей выдачей;
//! - чистое завершение удаляет строку: выполненные задания не копятся.
//!
//! Backoff: после `n`-й неудачи `not_before = now + min(2^n с, 1 ч)`;
//! после [`MAX_ATTEMPTS`]-й — `failed` (узел — `index_state = 'failed'`).

use jiff::{SignedDuration, Timestamp};
use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use zam_core::{Id, TargetKind, TargetRef};
use zam_markdown::ChunkLevel;

use crate::paths::path_ancestors;
use crate::time::{fmt_ts, parse_ts, ts_at, ts_opt};
use crate::user_db::SpaceKind;
use crate::{Db, StoreError};

/// Неудач до перевода задания в `failed`.
pub const MAX_ATTEMPTS: i64 = 5;
/// Предел задержки повтора, секунд.
pub const MAX_BACKOFF_SECS: i64 = 3600;
/// Приоритет заданий от записи узла: идут раньше массового reindex.
pub(crate) const PRIORITY_WRITE: i64 = 10;
/// Приоритет заданий reindex и разовой перечанковки при миграции.
pub(crate) const PRIORITY_REINDEX: i64 = 0;
/// Предел длины `last_error`, символов.
const LAST_ERROR_MAX_CHARS: usize = 1000;

/// Операция задания.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobOp {
    /// Пересобрать точки узла в Qdrant по текущим чанкам.
    Upsert,
    /// Удалить точки узла из Qdrant.
    Delete,
}

impl JobOp {
    pub fn as_str(self) -> &'static str {
        match self {
            JobOp::Upsert => "upsert",
            JobOp::Delete => "delete",
        }
    }
}

impl ToSql for JobOp {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl FromSql for JobOp {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        match v.as_str()? {
            "upsert" => Ok(JobOp::Upsert),
            "delete" => Ok(JobOp::Delete),
            other => Err(rusqlite::types::FromSqlError::Other(
                format!("неизвестная операция задания: {other}").into(),
            )),
        }
    }
}

/// Задание, выданное воркеру [`claim_jobs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: i64,
    /// Вид цели: узел (`target_kind = 'node'`) или запись журнала (`'entry'`, M3).
    pub target_kind: TargetKind,
    /// Узел или запись.
    pub target_id: Id,
    /// Ревизия цели на момент постановки.
    pub rev: i64,
    pub op: JobOp,
    /// Неудачных попыток до этой выдачи.
    pub attempts: i64,
    /// Номер выдачи: передаётся в [`complete_job`] и [`fail_job`].
    pub claim_gen: i64,
}

impl Job {
    /// Цель задания.
    pub fn target(&self) -> TargetRef {
        TargetRef {
            kind: self.target_kind,
            id: self.target_id,
        }
    }
}

/// `index_jobs.target_kind` цели.
pub(crate) fn target_kind_sql(k: TargetKind) -> &'static str {
    match k {
        TargetKind::Node => "node",
        TargetKind::Entry => "entry",
    }
}

/// Вид цели из `index_jobs.target_kind`.
fn parse_target_kind(s: &str) -> rusqlite::Result<TargetKind> {
    match s {
        "node" => Ok(TargetKind::Node),
        "entry" => Ok(TargetKind::Entry),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("неизвестный вид цели задания: {other}").into(),
        )),
    }
}

/// Чем закончилось [`complete_job`] или [`fail_job`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobOutcome {
    /// Задание выполнено, строка удалена.
    Done,
    /// Во время обработки узел переписан: задание снова в очереди с новой
    /// ревизией и сброшенными попытками; результат воркера для узла не окончательный
    /// (узел остаётся `pending`).
    Requeued,
    /// Неудача: повтор после backoff.
    Retry,
    /// Неудача [`MAX_ATTEMPTS`]-я: задание `failed`.
    Failed,
    /// Выдача устарела (задание перевыдано, сброшено при старте или его нет):
    /// ничего не изменено.
    Stale,
}

/// Состояние очереди для `/index/status`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JobStats {
    pub queued: i64,
    pub running: i64,
    pub failed: i64,
    /// Время постановки самого старого активного задания.
    pub oldest: Option<Timestamp>,
}

/// Состояние векторной индексации узла или записи (`index_state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexState {
    Pending,
    Indexed,
    Failed,
    Excluded,
}

impl IndexState {
    pub fn as_str(self) -> &'static str {
        match self {
            IndexState::Pending => "pending",
            IndexState::Indexed => "indexed",
            IndexState::Failed => "failed",
            IndexState::Excluded => "excluded",
        }
    }
}

impl FromSql for IndexState {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        match v.as_str()? {
            "pending" => Ok(IndexState::Pending),
            "indexed" => Ok(IndexState::Indexed),
            "failed" => Ok(IndexState::Failed),
            "excluded" => Ok(IndexState::Excluded),
            other => Err(rusqlite::types::FromSqlError::Other(
                format!("неизвестное состояние индексации: {other}").into(),
            )),
        }
    }
}

/// Поля узла для payload точек Qdrant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodePayload {
    pub node_id: Id,
    pub space_id: Id,
    pub space_kind: SpaceKind,
    pub solution_guid: Option<String>,
    pub path: String,
    /// Все префиксы пути по сегментам, включая сам путь: `a`, `a/b`, `a/b/c`.
    pub path_ancestors: Vec<String>,
    /// Теги из frontmatter (у записи журнала — её теги), значения секретов замаскированы.
    pub tags: Vec<String>,
    /// Область (`nodes.area` / `entries.area`).
    pub area: Option<String>,
    pub updated_at: Timestamp,
    /// `certainty` заметки (`nodes.certainty`, §7.5); у записи журнала — в [`EntryPayload`].
    pub certainty: Option<String>,
}

/// Поля записи журнала для payload точек Qdrant (M3). Чанк записи несёт их вместо узла:
/// точка получает `entry_id` и эти поля ([`crate::entry_chunks_for_index`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryPayload {
    pub entry_id: Id,
    /// `insight` / `changelog` / `release`.
    pub kind: String,
    pub status: Option<String>,
    /// Вид инсайта (`data.kind`).
    pub insight_kind: Option<String>,
    pub certainty: Option<String>,
    pub area: Option<String>,
}

/// Чанк для построения точки Qdrant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexChunk {
    /// Детерминированный id чанка (hex 16 байт).
    pub id: String,
    pub level: ChunkLevel,
    pub heading_path: String,
    pub ordinal: i64,
    pub text: String,
    pub context_prefix: String,
    /// blake3 hex текста (payload точки, `chunk_id`).
    pub text_hash: String,
    /// Ключ кэша `embeddings` (`chunks.embed_key`, [`crate::embed_key_of`]).
    pub embed_key: String,
    pub token_count: i64,
    /// Ревизия, по которой построен чанк.
    pub rev: i64,
    /// Поля цели для payload. У записи журнала `node_id` — id записи, `path` пуст.
    pub node: NodePayload,
    /// Поля записи журнала; `None` — чанк заметки.
    pub entry: Option<EntryPayload>,
}

/// Ставит задание `op` для узла, заменяя его активное задание (если есть):
/// `queued` — на месте, `running` — на месте с `dirty = 1` (см. документацию модуля).
/// Вызывается внутри транзакции писателя.
pub(crate) fn enqueue(
    c: &Connection,
    node: Id,
    rev: i64,
    op: JobOp,
    priority: i64,
    now: &str,
) -> Result<(), StoreError> {
    enqueue_target(c, TargetKind::Node, node, rev, op, priority, now)
}

/// [`enqueue`] для цели любого вида (узел или запись журнала).
pub(crate) fn enqueue_target(
    c: &Connection,
    kind: TargetKind,
    id: Id,
    rev: i64,
    op: JobOp,
    priority: i64,
    now: &str,
) -> Result<(), StoreError> {
    let kind = target_kind_sql(kind);
    let queued = c
        .prepare_cached(
            "UPDATE index_jobs
             SET rev = ?2, op = ?3, attempts = 0, last_error = NULL,
                 priority = max(priority, ?4), not_before = ?5, updated_at = ?5
             WHERE target_kind = ?6 AND target_id = ?1 AND state = 'queued'",
        )?
        .execute(params![id, rev, op, priority, now, kind])?;
    if queued > 0 {
        return Ok(());
    }
    let running = c
        .prepare_cached(
            "UPDATE index_jobs
             SET rev = ?2, op = ?3, dirty = 1, priority = max(priority, ?4), updated_at = ?5
             WHERE target_kind = ?6 AND target_id = ?1 AND state = 'running'",
        )?
        .execute(params![id, rev, op, priority, now, kind])?;
    if running > 0 {
        return Ok(());
    }
    c.prepare_cached(
        "INSERT INTO index_jobs (target_kind, target_id, rev, op, state, attempts,
                                 priority, not_before, enqueued_at, updated_at)
         VALUES (?6, ?1, ?2, ?3, 'queued', 0, ?4, ?5, ?5, ?5)",
    )?
    .execute(params![id, rev, op, priority, now, kind])?;
    Ok(())
}

/// Выдаёт до `limit` заданий `queued` с `not_before ≤ now` (приоритет по убыванию,
/// затем id) и атомарно переводит их в `running` с новым `claim_gen`. Узел, задание
/// которого уже `running`, не выдаётся: активное задание у узла одно.
pub async fn claim_jobs(db: &Db, limit: usize, now: Timestamp) -> Result<Vec<Job>, StoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    db.write(move |tx| {
        let now = fmt_ts(now);
        let mut jobs: Vec<Job> = {
            let mut st = tx.prepare_cached(
                "SELECT id, target_id, rev, op, attempts, target_kind FROM index_jobs
                 WHERE state = 'queued' AND not_before <= ?1
                 ORDER BY priority DESC, id
                 LIMIT ?2",
            )?;
            st.query_map(params![now, limit], |r| {
                Ok(Job {
                    id: r.get(0)?,
                    target_kind: parse_target_kind(&r.get::<_, String>(5)?)?,
                    target_id: r.get(1)?,
                    rev: r.get(2)?,
                    op: r.get(3)?,
                    attempts: r.get(4)?,
                    claim_gen: 0,
                })
            })?
            .collect::<Result<_, _>>()?
        };
        let mut st = tx.prepare_cached(
            "UPDATE index_jobs SET state = 'running', claim_gen = claim_gen + 1, updated_at = ?2
             WHERE id = ?1
             RETURNING claim_gen",
        )?;
        for j in &mut jobs {
            j.claim_gen = st.query_row(params![j.id, now], |r| r.get(0))?;
        }
        Ok(jobs)
    })
    .await
}

/// Строка задания текущей выдачи: `(цель, rev, op, attempts, dirty)`.
type ClaimedRow = (TargetRef, i64, JobOp, i64, bool);

/// Строка задания текущей выдачи; `None` — выдача устарела.
fn claimed(c: &Connection, id: i64, claim_gen: i64) -> Result<Option<ClaimedRow>, StoreError> {
    Ok(c.prepare_cached(
        "SELECT target_id, rev, op, attempts, dirty, target_kind FROM index_jobs
         WHERE id = ?1 AND claim_gen = ?2 AND state = 'running'",
    )?
    .query_row(params![id, claim_gen], |r| {
        let target = TargetRef {
            kind: parse_target_kind(&r.get::<_, String>(5)?)?,
            id: r.get(0)?,
        };
        Ok((target, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })
    .optional()?)
}

/// «Грязное» задание (узел переписан во время обработки) — снова в очередь сразу.
fn requeue_dirty(c: &Connection, id: i64, now: &str) -> Result<JobOutcome, StoreError> {
    c.prepare_cached(
        "UPDATE index_jobs SET state = 'queued', attempts = 0, dirty = 0, last_error = NULL,
                               not_before = ?2, updated_at = ?2
         WHERE id = ?1",
    )?
    .execute(params![id, now])?;
    Ok(JobOutcome::Requeued)
}

/// Завершает выдачу `claim_gen` задания `id`: чистое задание удаляется
/// ([`JobOutcome::Done`]), «грязное» возвращается в очередь ([`JobOutcome::Requeued`]),
/// устаревшая выдача ничего не меняет ([`JobOutcome::Stale`]).
///
/// Выполненный `upsert` в той же транзакции отмечает узел `indexed` (ревизия задания
/// актуальна, узел жив и не исключён, активного задания у узла нет — как
/// [`finish_reindexed`]): между закрытием задания и отметкой не вклинится перенос или
/// восстановление, поставившее новое задание.
pub async fn complete_job(db: &Db, id: i64, claim_gen: i64) -> Result<JobOutcome, StoreError> {
    db.write(move |tx| {
        let Some((node, rev, op, _, dirty)) = claimed(tx, id, claim_gen)? else {
            return Ok(JobOutcome::Stale);
        };
        if dirty {
            return requeue_dirty(tx, id, &fmt_ts(Timestamp::now()));
        }
        tx.execute("DELETE FROM index_jobs WHERE id = ?1", [id])?;
        if op == JobOp::Upsert && !has_active_target(tx, node)? {
            set_target_state(tx, node, rev, IndexState::Indexed)?;
        }
        Ok(JobOutcome::Done)
    })
    .await
}

/// У узла есть задание `queued` или `running`.
fn has_active_job(c: &Connection, node: Id) -> Result<bool, StoreError> {
    has_active_target(
        c,
        TargetRef {
            kind: TargetKind::Node,
            id: node,
        },
    )
}

/// У цели есть задание `queued` или `running`.
pub(crate) fn has_active_target(c: &Connection, t: TargetRef) -> Result<bool, StoreError> {
    Ok(c.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM index_jobs
                        WHERE target_kind = ?2 AND target_id = ?1
                          AND state IN ('queued', 'running'))",
    )?
    .query_row(params![t.id, target_kind_sql(t.kind)], |r| r.get(0))?)
}

/// Задержка повтора после `attempts`-й неудачи, секунд.
fn backoff_secs(attempts: i64) -> i64 {
    if attempts >= 12 {
        // 2^12 > предела; заодно без переполнения сдвига
        MAX_BACKOFF_SECS
    } else {
        (1i64 << attempts.max(0)).min(MAX_BACKOFF_SECS)
    }
}

/// Неудача выдачи `claim_gen` задания `id`: `attempts + 1`, повтор через
/// `min(2^attempts с, 1 ч)` от `now` ([`JobOutcome::Retry`]); после
/// [`MAX_ATTEMPTS`]-й неудачи — `failed` ([`JobOutcome::Failed`]), а узел (если
/// ревизия задания актуальна) — `index_state = 'failed'`. «Грязное» задание
/// возвращается в очередь с `attempts = 0` ([`JobOutcome::Requeued`]): новая
/// ревизия ещё не пробовалась. Устаревшая выдача — [`JobOutcome::Stale`].
/// `err` не должен содержать текста заметок; длиннее 1000 символов обрезается.
pub async fn fail_job(
    db: &Db,
    id: i64,
    claim_gen: i64,
    err: &str,
    now: Timestamp,
) -> Result<JobOutcome, StoreError> {
    let err: String = err.chars().take(LAST_ERROR_MAX_CHARS).collect();
    db.write(move |tx| {
        let Some((node, rev, op, attempts, dirty)) = claimed(tx, id, claim_gen)? else {
            return Ok(JobOutcome::Stale);
        };
        let stamp = fmt_ts(now);
        if dirty {
            return requeue_dirty(tx, id, &stamp);
        }
        let attempts = attempts + 1;
        if attempts >= MAX_ATTEMPTS {
            tx.execute(
                "UPDATE index_jobs SET state = 'failed', attempts = ?2, last_error = ?3,
                                       updated_at = ?4
                 WHERE id = ?1",
                params![id, attempts, err, stamp],
            )?;
            if op == JobOp::Upsert {
                set_target_state(tx, node, rev, IndexState::Failed)?;
            }
            return Ok(JobOutcome::Failed);
        }
        let next = fmt_ts(now + SignedDuration::from_secs(backoff_secs(attempts)));
        tx.execute(
            "UPDATE index_jobs SET state = 'queued', attempts = ?2, last_error = ?3,
                                   not_before = ?4, updated_at = ?5
             WHERE id = ?1",
            params![id, attempts, err, next, stamp],
        )?;
        Ok(JobOutcome::Retry)
    })
    .await
}

/// Возвращает выдачу `claim_gen` задания `id` в очередь без неудачи: `attempts` и
/// `not_before` не меняются, `dirty` снимается (следующая выдача и так возьмёт актуальную
/// ревизию). Для заданий, которые воркер не смог выполнить не по своей вине (Qdrant
/// недоступен, коллекция пересоздаётся): попытки не сгорают, пока индекс лежит.
/// Результат — [`JobOutcome::Requeued`] или [`JobOutcome::Stale`].
pub async fn release_job(db: &Db, id: i64, claim_gen: i64) -> Result<JobOutcome, StoreError> {
    db.write(move |tx| {
        if claimed(tx, id, claim_gen)?.is_none() {
            return Ok(JobOutcome::Stale);
        }
        tx.execute(
            "UPDATE index_jobs SET state = 'queued', dirty = 0, updated_at = ?2 WHERE id = ?1",
            params![id, fmt_ts(Timestamp::now())],
        )?;
        Ok(JobOutcome::Requeued)
    })
    .await
}

/// Число живых заметок и записей журналов с `index_state = 'indexed'`: есть ли что
/// терять, если коллекция Qdrant создана заново (тогда нужен reindex из `embeddings`).
pub async fn indexed_count(db: &Db) -> Result<i64, StoreError> {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT (SELECT count(*) FROM nodes
                     WHERE kind = 'note' AND index_state = 'indexed' AND deleted_at IS NULL)
                  + (SELECT count(*) FROM entries
                     WHERE index_state = 'indexed' AND deleted_at IS NULL)",
            [],
            |r| r.get(0),
        )?)
    })
    .await
}

/// При старте: задания, оставшиеся `running` после аварийного завершения, — снова
/// `queued` (с `dirty = 0`: следующая выдача и так возьмёт актуальную ревизию).
/// Возвращает их число.
pub async fn requeue_running(db: &Db) -> Result<u64, StoreError> {
    db.write(|tx| {
        let n = tx.execute(
            "UPDATE index_jobs SET state = 'queued', dirty = 0, updated_at = ?1
             WHERE state = 'running'",
            [fmt_ts(Timestamp::now())],
        )?;
        Ok(n as u64)
    })
    .await
}

/// Перечанковывает живые заметки и записи журналов пространства `space` (`None` — всех)
/// текущим счётчиком токенов и ставит `upsert` индексируемым (не Human, не `index: false`)
/// с низким приоритетом. Цель с активным заданием получает одно задание, не два.
/// Возвращает число поставленных `upsert`. Идёт пакетами по отдельным транзакциям.
/// Перечанковка всей базы точным счётчиком снимает отметку «чанки по оценке» и ставит
/// текущий отпечаток нарезки.
pub async fn enqueue_reindex(db: &Db, space: Option<Id>) -> Result<u64, StoreError> {
    let (_, mut jobs) = crate::nodes::rechunk_notes(db, space, PRIORITY_REINDEX, false).await?;
    jobs += crate::entries::rechunk_entries(db, space, PRIORITY_REINDEX, false).await?;
    if space.is_none() {
        crate::user_db::clear_approx_if_exact(db).await?;
        crate::user_db::mark_chunker_current(db).await?;
    }
    Ok(jobs)
}

/// Ключ `meta`: незавершённая пересборка индекса базы из кэша (reindex).
pub(crate) const REINDEX_KEY: &str = "reindex_pending";

/// Отмечает (`Some(spec)`) или снимает (`None`) незавершённую пересборку индекса базы:
/// отметка ставится до удаления точек области и снимается после её восстановления, так что
/// прерванный reindex (остановка, авария) повторяется при следующем запуске воркера.
/// `spec` — описание запроса вызывающего (JSON).
pub async fn mark_reindex(db: &Db, spec: Option<String>) -> Result<(), StoreError> {
    db.write(move |tx| {
        match spec {
            Some(s) => tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                params![REINDEX_KEY, s],
            )?,
            None => tx.execute("DELETE FROM meta WHERE key = ?1", [REINDEX_KEY])?,
        };
        Ok(())
    })
    .await
}

/// Незавершённая пересборка индекса базы ([`mark_reindex`]), если есть.
pub async fn pending_reindex(db: &Db) -> Result<Option<String>, StoreError> {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [REINDEX_KEY],
            |r| r.get(0),
        )
        .optional()?)
    })
    .await
}

/// Счётчики очереди.
pub async fn job_stats(db: &Db) -> Result<JobStats, StoreError> {
    db.read(|c| {
        let (queued, running, failed, oldest) = c.query_row(
            "SELECT count(*) FILTER (WHERE state = 'queued'),
                    count(*) FILTER (WHERE state = 'running'),
                    count(*) FILTER (WHERE state = 'failed'),
                    min(enqueued_at) FILTER (WHERE state IN ('queued', 'running'))
             FROM index_jobs WHERE state <> 'done'",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )?;
        Ok(JobStats {
            queued,
            running,
            failed,
            oldest: oldest.as_deref().map(parse_ts).transpose()?,
        })
    })
    .await
}

fn parse_level(s: &str) -> Option<ChunkLevel> {
    match s {
        "chunk" => Some(ChunkLevel::Chunk),
        "section" => Some(ChunkLevel::Section),
        "note" => Some(ChunkLevel::Note),
        _ => None,
    }
}

/// Чанки узла для векторного индекса с полями payload. Узел удалён, исключён
/// (`index_state = 'excluded'`: Human, `index: false`) или это папка — пустой
/// список: такие узлы в Qdrant не попадают. Нет узла → `NotFound`.
pub async fn chunks_for_index(db: &Db, node: Id) -> Result<Vec<IndexChunk>, StoreError> {
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        index_chunks(&tx, node)
    })
    .await
}

/// Строка чанка: `(id, level, heading_path, ordinal)` и
/// `(text, context_prefix, text_hash, token_count, rev, embed_key)`.
type ChunkRowRead = (
    (String, String, String, i64),
    (String, String, String, i64, i64, Option<String>),
);

/// [`chunks_for_index`] внутри открытой транзакции чтения.
fn index_chunks(tx: &Connection, node: Id) -> Result<Vec<IndexChunk>, StoreError> {
    let row = tx
        .prepare_cached(
            "SELECT n.space_id, s.kind, s.solution_guid, n.path, n.frontmatter,
                    n.updated_at, n.kind, n.index_state, n.deleted_at IS NOT NULL, n.area,
                    n.certainty
             FROM nodes n JOIN spaces s ON s.id = n.space_id
             WHERE n.id = ?1",
        )?
        .query_row([node], |r| {
            Ok((
                NodePayload {
                    node_id: node,
                    space_id: r.get(0)?,
                    space_kind: r.get(1)?,
                    solution_guid: r.get(2)?,
                    path: r.get(3)?,
                    path_ancestors: Vec::new(),
                    tags: Vec::new(),
                    area: r.get(9)?,
                    updated_at: ts_at(r, 5)?,
                    certainty: r.get(10)?,
                },
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, bool>(8)?,
            ))
        })
        .optional()?;
    let Some((mut payload, fm, kind, state, deleted)) = row else {
        return Err(StoreError::NotFound);
    };
    if deleted
        || kind != "note"
        || state == IndexState::Excluded.as_str()
        || payload.space_kind == SpaceKind::Human
    {
        return Ok(Vec::new());
    }
    payload.path_ancestors = path_ancestors(&payload.path);
    payload.tags = match fm.as_deref() {
        Some(s) => crate::nodes::mask_tags(
            crate::nodes::frontmatter_tags(s)
                .map_err(|e| StoreError::Corrupt(format!("frontmatter узла {node}: {e}")))?,
        ),
        None => Vec::new(),
    };
    read_index_chunks(
        tx,
        "SELECT id, level, heading_path, ordinal, text, context_prefix, text_hash,
                token_count, rev, embed_key
         FROM chunks WHERE node_id = ?1 ORDER BY ordinal",
        node,
        &payload,
        None,
    )
}

/// Чанки цели `id` запросом `sql` (колонки — как в [`index_chunks`]) с полями payload.
pub(crate) fn read_index_chunks(
    tx: &Connection,
    sql: &str,
    id: Id,
    payload: &NodePayload,
    entry: Option<&EntryPayload>,
) -> Result<Vec<IndexChunk>, StoreError> {
    let mut st = tx.prepare_cached(sql)?;
    let rows = st.query_map([id], |r| -> rusqlite::Result<ChunkRowRead> {
        Ok((
            (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?),
            (
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
            ),
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (
            (id, level, heading_path, ordinal),
            (text, context_prefix, text_hash, tokens, rev, key),
        ) = row?;
        // папки и вложения (этап M3) векторизуются не здесь
        let Some(level) = parse_level(&level) else {
            continue;
        };
        // строка без ключа (вставлена в обход записи чанков) — ключ по той же формуле
        let embed_key = key
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| crate::embed_key_of(&context_prefix, &text));
        out.push(IndexChunk {
            id,
            level,
            heading_path,
            ordinal,
            text,
            context_prefix,
            text_hash,
            embed_key,
            token_count: tokens,
            rev,
            node: payload.clone(),
            entry: entry.cloned(),
        });
    }
    Ok(out)
}

/// Условие «живая индексируемая заметка» (алиасы: `n` — nodes, `s` — spaces): то, что
/// должно лежать в векторном индексе.
const INDEXABLE: &str = "n.kind = 'note' AND n.deleted_at IS NULL
     AND n.index_state <> 'excluded' AND s.kind <> 'human'";

/// Заметка для пересборки индекса из кэша: текущая ревизия и её чанки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReindexNote {
    pub node_id: Id,
    pub rev: i64,
    pub chunks: Vec<IndexChunk>,
}

/// Страница живых индексируемых заметок пространства `space` (`None` — всех) с чанками:
/// по возрастанию id после `after`, не больше `limit` заметок, одним снимком чтения.
pub async fn reindex_page(
    db: &Db,
    space: Option<Id>,
    after: Option<Id>,
    limit: usize,
) -> Result<Vec<ReindexNote>, StoreError> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        let ids: Vec<(Id, i64)> = {
            let mut st = tx.prepare_cached(&format!(
                "SELECT n.id, n.current_rev FROM nodes n JOIN spaces s ON s.id = n.space_id
                 WHERE {INDEXABLE}
                   AND (?1 IS NULL OR n.space_id = ?1) AND (?2 IS NULL OR n.id > ?2)
                 ORDER BY n.id LIMIT ?3"
            ))?;
            st.query_map(params![space, after, limit], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?
        };
        let mut out = Vec::with_capacity(ids.len());
        for (node_id, rev) in ids {
            out.push(ReindexNote {
                node_id,
                rev,
                chunks: index_chunks(&tx, node_id)?,
            });
        }
        Ok(out)
    })
    .await
}

/// Число живых индексируемых заметок и записей журналов пространства `space`
/// (`None` — всех).
pub async fn indexable_count(db: &Db, space: Option<Id>) -> Result<i64, StoreError> {
    db.read(move |c| {
        Ok(c.query_row(
            &format!(
                "SELECT (SELECT count(*) FROM nodes n JOIN spaces s ON s.id = n.space_id
                         WHERE {INDEXABLE} AND (?1 IS NULL OR n.space_id = ?1))
                      + (SELECT count(*) FROM entries e
                         WHERE e.deleted_at IS NULL AND e.index_state <> 'excluded'
                           AND (?1 IS NULL OR e.space_id = ?1))"
            ),
            [space],
            |r| r.get(0),
        )?)
    })
    .await
}

/// Ставит `upsert` (приоритет reindex) живым индексируемым заметкам из `nodes` по их
/// текущей ревизии и переводит их в `pending`: эмбеддингов в кэше нет, нужен инференс.
/// Возвращает число поставленных заданий.
pub async fn enqueue_upserts(db: &Db, nodes: Vec<Id>) -> Result<u64, StoreError> {
    if nodes.is_empty() {
        return Ok(0);
    }
    db.write(move |tx| {
        let now = fmt_ts(Timestamp::now());
        let mut pick = tx.prepare_cached(&format!(
            "UPDATE nodes SET index_state = 'pending'
             WHERE id = ?1 AND EXISTS (SELECT 1 FROM nodes n JOIN spaces s ON s.id = n.space_id
                                       WHERE n.id = ?1 AND {INDEXABLE})
             RETURNING current_rev"
        ))?;
        let mut jobs = 0u64;
        for node in nodes {
            let rev: Option<i64> = pick.query_row([node], |r| r.get(0)).optional()?;
            if let Some(rev) = rev {
                enqueue(tx, node, rev, JobOp::Upsert, PRIORITY_REINDEX, &now)?;
                jobs += 1;
            }
        }
        Ok(jobs)
    })
    .await
}

/// Точки заметок `(узел, ревизия)` восстановлены из кэша и подтверждены Qdrant:
/// неудавшиеся (`failed`) задания этих узлов снимаются, узел без активного задания —
/// `indexed` по этой ревизии. Переписанный с тех пор узел не меняется (его задание впереди).
pub async fn finish_reindexed(db: &Db, items: Vec<(Id, i64)>) -> Result<(), StoreError> {
    if items.is_empty() {
        return Ok(());
    }
    db.write(move |tx| {
        let mut drop_failed = tx.prepare_cached(
            "DELETE FROM index_jobs
             WHERE target_kind = 'node' AND target_id = ?1 AND state = 'failed'",
        )?;
        for (node, rev) in items {
            drop_failed.execute([node])?;
            if !has_active_job(tx, node)? {
                set_state(tx, node, rev, IndexState::Indexed)?;
            }
        }
        Ok(())
    })
    .await
}

/// Задание, ушедшее в `failed` (для `/index/status`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedJob {
    /// Id задания (`index_jobs.id`): у одного узла неудавшихся заданий может быть несколько.
    pub id: i64,
    /// Вид цели: узел или запись журнала.
    pub target_kind: TargetKind,
    /// Id узла или записи.
    pub node_id: Id,
    /// Пространство и путь узла; `None` — цель удалена насовсем (у записи пути нет).
    pub space_id: Option<Id>,
    pub path: Option<String>,
    /// Цель удалена мягко; `None` — жива или удалена насовсем.
    pub deleted_at: Option<Timestamp>,
    pub op: JobOp,
    pub attempts: i64,
    /// Последняя ошибка (без текста заметок).
    pub error: Option<String>,
    pub updated_at: Timestamp,
}

/// До `limit` заданий `failed`, новые первыми.
pub async fn failed_jobs(db: &Db, limit: usize) -> Result<Vec<FailedJob>, StoreError> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT j.id, j.target_id, j.op, j.attempts, j.last_error, j.updated_at,
                    coalesce(n.space_id, e.space_id), n.path,
                    coalesce(n.deleted_at, e.deleted_at), j.target_kind
             FROM index_jobs j
             LEFT JOIN nodes n ON j.target_kind = 'node' AND n.id = j.target_id
             LEFT JOIN entries e ON j.target_kind = 'entry' AND e.id = j.target_id
             WHERE j.state = 'failed' ORDER BY j.updated_at DESC, j.id DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit], |r| {
            Ok(FailedJob {
                id: r.get(0)?,
                target_kind: parse_target_kind(&r.get::<_, String>(9)?)?,
                node_id: r.get(1)?,
                op: r.get(2)?,
                attempts: r.get(3)?,
                error: r.get(4)?,
                updated_at: ts_at(r, 5)?,
                space_id: r.get(6)?,
                path: r.get(7)?,
                deleted_at: ts_opt(r, 8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}

/// Строка входа эмбеддинга L0 папки до маскировки: `[<пространство>/<путь>] <имя>\n<L0>`.
pub(crate) fn folder_embed_raw(space: &str, path: &str, name: &str, l0: &str) -> String {
    format!("[{space}/{path}] {name}\n{l0}")
}

/// Вход эмбеддинга L0 папки (точка уровня `folder`, §8.3): имя пространства, путь и имя
/// папки задаёт автор, L0 — свёртка имён и L0 детей, поэтому строка маскируется сканером
/// секретов целиком (как `context_prefix` чанков, §7.6). Без секретов — строка как есть
/// (ключ кэша чистых папок не меняется).
pub fn folder_embed_input(space: &str, path: &str, name: &str, l0: &str) -> String {
    zam_markdown::mask_text(&folder_embed_raw(space, path, name, l0))
}

/// Папка с L0 для точки уровня `folder` (пересборка индекса из кэша).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexFolder {
    pub node_id: Id,
    pub space_id: Id,
    pub space_kind: SpaceKind,
    pub space_name: String,
    pub solution_guid: Option<String>,
    pub path: String,
    pub name: String,
    pub l0: String,
    pub updated_at: Timestamp,
}

/// Живые папки с L0 пространства `space` (`None` — всех), кроме Human.
pub async fn folders_for_index(db: &Db, space: Option<Id>) -> Result<Vec<IndexFolder>, StoreError> {
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT n.id, n.space_id, s.kind, s.name, s.solution_guid, n.path, n.name, n.l0,
                    n.updated_at
             FROM nodes n JOIN spaces s ON s.id = n.space_id
             WHERE n.kind = 'folder' AND n.deleted_at IS NULL AND n.l0 IS NOT NULL
               AND s.kind <> 'human' AND (?1 IS NULL OR n.space_id = ?1)
             ORDER BY n.id",
        )?;
        let rows = st.query_map([space], |r| {
            Ok(IndexFolder {
                node_id: r.get(0)?,
                space_id: r.get(1)?,
                space_kind: r.get(2)?,
                space_name: r.get(3)?,
                solution_guid: r.get(4)?,
                path: r.get(5)?,
                name: r.get(6)?,
                l0: r.get(7)?,
                updated_at: ts_at(r, 8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}

/// `index_state` цели (узла или записи), если `rev` — её текущая ревизия и она жива.
fn set_target_state(
    c: &Connection,
    t: TargetRef,
    rev: i64,
    state: IndexState,
) -> Result<(), StoreError> {
    match t.kind {
        TargetKind::Node => set_state(c, t.id, rev, state),
        TargetKind::Entry => crate::entries::set_entry_state(c, t.id, rev, state),
    }
}

/// `nodes.index_state` узла, если `rev` — его текущая ревизия и узел жив.
/// Исключённый узел (`excluded`) меняется только на `excluded`.
fn set_state(c: &Connection, node: Id, rev: i64, state: IndexState) -> Result<(), StoreError> {
    c.prepare_cached(
        "UPDATE nodes SET index_state = ?3
         WHERE id = ?1 AND current_rev = ?2 AND deleted_at IS NULL
           AND (index_state <> 'excluded' OR ?3 = 'excluded')",
    )?
    .execute(params![node, rev, state.as_str()])?;
    Ok(())
}

/// Отмечает результат индексации узла по ревизии `rev`. Устаревшая ревизия
/// (узел уже переписан), удалённый или исключённый узел — без изменений.
pub async fn set_index_state(
    db: &Db,
    node: Id,
    rev: i64,
    state: IndexState,
) -> Result<(), StoreError> {
    db.write(move |tx| set_state(tx, node, rev, state)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_secs(1), 2);
        assert_eq!(backoff_secs(4), 16);
        assert_eq!(backoff_secs(11), 2048);
        assert_eq!(backoff_secs(12), 3600);
        assert_eq!(backoff_secs(i64::MAX), 3600);
    }
}
