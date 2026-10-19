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
use zam_core::Id;
use zam_markdown::ChunkLevel;

use crate::time::{fmt_ts, parse_ts, ts_at};
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
    /// Узел (`target_kind = 'node'`).
    pub target_id: Id,
    /// Ревизия узла на момент постановки.
    pub rev: i64,
    pub op: JobOp,
    /// Неудачных попыток до этой выдачи.
    pub attempts: i64,
    /// Номер выдачи: передаётся в [`complete_job`] и [`fail_job`].
    pub claim_gen: i64,
}

/// Чем закончилось [`complete_job`] или [`fail_job`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobOutcome {
    /// Задание выполнено, строка удалена.
    Done,
    /// Во время обработки узел переписан: задание снова в очереди с новой
    /// ревизией и сброшенными попытками; результат воркера для узла не окончательный
    /// (`set_index_state(Indexed)` не вызывать).
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

/// Состояние векторной индексации узла (`nodes.index_state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// Теги из frontmatter.
    pub tags: Vec<String>,
    pub updated_at: Timestamp,
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
    /// blake3 hex текста — ключ кэша `embeddings`.
    pub text_hash: String,
    pub token_count: i64,
    /// Ревизия, по которой построен чанк.
    pub rev: i64,
    pub node: NodePayload,
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
    let queued = c
        .prepare_cached(
            "UPDATE index_jobs
             SET rev = ?2, op = ?3, attempts = 0, last_error = NULL,
                 priority = max(priority, ?4), not_before = ?5, updated_at = ?5
             WHERE target_kind = 'node' AND target_id = ?1 AND state = 'queued'",
        )?
        .execute(params![node, rev, op, priority, now])?;
    if queued > 0 {
        return Ok(());
    }
    let running = c
        .prepare_cached(
            "UPDATE index_jobs
             SET rev = ?2, op = ?3, dirty = 1, priority = max(priority, ?4), updated_at = ?5
             WHERE target_kind = 'node' AND target_id = ?1 AND state = 'running'",
        )?
        .execute(params![node, rev, op, priority, now])?;
    if running > 0 {
        return Ok(());
    }
    c.prepare_cached(
        "INSERT INTO index_jobs (target_kind, target_id, rev, op, state, attempts,
                                 priority, not_before, enqueued_at, updated_at)
         VALUES ('node', ?1, ?2, ?3, 'queued', 0, ?4, ?5, ?5, ?5)",
    )?
    .execute(params![node, rev, op, priority, now])?;
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
                "SELECT id, target_id, rev, op, attempts FROM index_jobs
                 WHERE state = 'queued' AND not_before <= ?1
                 ORDER BY priority DESC, id
                 LIMIT ?2",
            )?;
            st.query_map(params![now, limit], |r| {
                Ok(Job {
                    id: r.get(0)?,
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

/// Строка задания текущей выдачи: `(target_id, rev, op, attempts, dirty)`.
type ClaimedRow = (Id, i64, JobOp, i64, bool);

/// Строка задания текущей выдачи; `None` — выдача устарела.
fn claimed(c: &Connection, id: i64, claim_gen: i64) -> Result<Option<ClaimedRow>, StoreError> {
    Ok(c.prepare_cached(
        "SELECT target_id, rev, op, attempts, dirty FROM index_jobs
         WHERE id = ?1 AND claim_gen = ?2 AND state = 'running'",
    )?
    .query_row(params![id, claim_gen], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
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
pub async fn complete_job(db: &Db, id: i64, claim_gen: i64) -> Result<JobOutcome, StoreError> {
    db.write(move |tx| {
        let Some((.., dirty)) = claimed(tx, id, claim_gen)? else {
            return Ok(JobOutcome::Stale);
        };
        if dirty {
            return requeue_dirty(tx, id, &fmt_ts(Timestamp::now()));
        }
        tx.execute("DELETE FROM index_jobs WHERE id = ?1", [id])?;
        Ok(JobOutcome::Done)
    })
    .await
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
                set_state(tx, node, rev, IndexState::Failed)?;
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

/// Перечанковывает живые заметки пространства `space` (`None` — всех) текущим
/// счётчиком токенов и ставит `upsert` индексируемым (не Human, не `index: false`)
/// с низким приоритетом. Узел с активным заданием получает одно задание, не два.
/// Возвращает число поставленных `upsert`. Идёт пакетами по отдельным транзакциям.
pub async fn enqueue_reindex(db: &Db, space: Option<Id>) -> Result<u64, StoreError> {
    let (_, jobs) = crate::nodes::rechunk_notes(db, space, PRIORITY_REINDEX).await?;
    Ok(jobs)
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

/// Все префиксы пути по сегментам, включая сам путь.
fn path_ancestors(path: &str) -> Vec<String> {
    path.match_indices('/')
        .map(|(i, _)| path[..i].to_string())
        .chain(std::iter::once(path.to_string()))
        .collect()
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
        let row = tx
            .query_row(
                "SELECT n.space_id, s.kind, s.solution_guid, n.path, n.frontmatter,
                        n.updated_at, n.kind, n.index_state, n.deleted_at IS NOT NULL
                 FROM nodes n JOIN spaces s ON s.id = n.space_id
                 WHERE n.id = ?1",
                [node],
                |r| {
                    Ok((
                        NodePayload {
                            node_id: node,
                            space_id: r.get(0)?,
                            space_kind: r.get(1)?,
                            solution_guid: r.get(2)?,
                            path: r.get(3)?,
                            path_ancestors: Vec::new(),
                            tags: Vec::new(),
                            updated_at: ts_at(r, 5)?,
                        },
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, bool>(8)?,
                    ))
                },
            )
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
            Some(s) => crate::nodes::frontmatter_tags(s)
                .map_err(|e| StoreError::Corrupt(format!("frontmatter узла {node}: {e}")))?,
            None => Vec::new(),
        };
        let mut st = tx.prepare_cached(
            "SELECT id, level, heading_path, ordinal, text, context_prefix, text_hash,
                    token_count, rev
             FROM chunks WHERE node_id = ?1 ORDER BY ordinal",
        )?;
        let rows = st.query_map([node], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, level, heading_path, ordinal, text, context_prefix, text_hash, tokens, rev) =
                row?;
            // папки и вложения (этап M3) векторизуются не здесь
            let Some(level) = parse_level(&level) else {
                continue;
            };
            out.push(IndexChunk {
                id,
                level,
                heading_path,
                ordinal,
                text,
                context_prefix,
                text_hash,
                token_count: tokens,
                rev,
                node: payload.clone(),
            });
        }
        Ok(out)
    })
    .await
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

    #[test]
    fn ancestors() {
        assert_eq!(path_ancestors("a"), vec!["a"]);
        assert_eq!(path_ancestors("a/b/c"), vec!["a", "a/b", "a/b/c"]);
    }
}
