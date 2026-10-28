//! Чанки записи журнала, FTS и задания индексации (§8.1–8.2 ТЗ).
//!
//! Документ записи — `# <title>\n\n<body>`, его режет тот же `chunk_note`, что и заметки
//! (листы `chunk`, L1 секций `section`, L1 записи `note`); в FTS попадают `chunk` и `note`
//! (условие триггеров миграции 0004). Контекстный префикс — `[<пространство>/<kind>] <title>
//! | <теги>`. Чанки записи пишутся с `entry_id` (у них нет `node_id`).
//!
//! После пересборки чанков в той же транзакции ставится задание `target_kind = 'entry'`:
//! индексируемая запись → `upsert` (и при смене статуса или `certainty`: меняется payload);
//! запись Human — `excluded`, без задания. Удаление — `delete`.

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use zam_core::{Id, TargetKind, TargetRef};
use zam_markdown::{parse, scan_secrets};

use super::{EntryKind, data_kind};
use crate::embeddings::embed_key_of;
use crate::jobs::{
    EntryPayload, IndexChunk, IndexState, JobOp, NodePayload, PRIORITY_WRITE, enqueue_target,
    has_active_target, read_index_chunks,
};
use crate::nodes::{
    ChunkInsert, chunk_id, context_prefix, note_chunks, replace_chunks, token_counter,
};
use crate::time::{fmt_ts, ts_at};
use crate::user_db::SpaceKind;
use crate::{Db, StoreError};

/// `index_state` новой или изменённой записи: Human никогда не векторизуется.
pub(crate) fn entry_index_state(kind: SpaceKind) -> IndexState {
    if kind == SpaceKind::Human {
        IndexState::Excluded
    } else {
        IndexState::Pending
    }
}

/// Документ записи для нарезки.
fn entry_doc(title: &str, body: &str) -> String {
    if body.trim().is_empty() {
        format!("# {title}\n")
    } else {
        format!("# {title}\n\n{body}")
    }
}

/// Пересобирает чанки записи `id` по её текущим полям: при отличии от записанных старые
/// удаляются (триггеры убирают их из FTS), новые вставляются; совпадающая нарезка не
/// переписывается. Вызывается в транзакции писателя. `true` — чанки переписаны.
pub(crate) fn rebuild_entry_chunks(c: &Connection, id: Id) -> Result<bool, StoreError> {
    let (space, kind, title, body, tags, rev): (String, EntryKind, String, String, String, i64) = c
        .prepare_cached(
            "SELECT s.name, e.kind, e.title, e.body, e.tags, e.current_rev
             FROM entries e JOIN spaces s ON s.id = e.space_id WHERE e.id = ?1",
        )?
        .query_row([id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?;
    let tags: Vec<String> = serde_json::from_str(&tags)
        .map_err(|e| StoreError::Corrupt(format!("теги записи {id}: {e}")))?;
    let src = entry_doc(&title, &body);
    let doc = parse(&src);
    let tc = token_counter();
    let chunks = note_chunks(&src, &doc, &title, tc.as_ref());
    let prefix = context_prefix(&space, kind.as_str(), &title, "", &tags);
    let rows: Vec<ChunkInsert> = chunks
        .into_iter()
        .enumerate()
        .map(|(ordinal, ch)| {
            let ordinal = ordinal as i64;
            ChunkInsert {
                id: chunk_id(id, ordinal, &ch.text_hash),
                rev,
                level: ch.level.as_str(),
                ordinal,
                embed_key: embed_key_of(&prefix, &ch.text),
                context_prefix: prefix.clone(),
                heading_path: ch.heading_path,
                text: ch.text,
                token_count: ch.token_count,
                text_hash: ch.text_hash,
            }
        })
        .collect();
    let changed = replace_chunks(c, "entry_id", id, &rows)?;
    if !tc.is_exact() {
        c.prepare_cached("INSERT OR IGNORE INTO meta (key, value) VALUES (?1, '1')")?
            .execute([crate::user_db::CHUNKED_APPROX])?;
    }
    Ok(changed)
}

/// Пересборка чанков и задание после записи (создание, правка, смена статуса,
/// восстановление). `entries.index_state` к этому моменту уже записан; `prev` — прежний
/// (`None` у новой записи). Возвращает поставленную операцию.
pub(crate) fn index_entry(
    c: &Connection,
    id: Id,
    prev: Option<IndexState>,
    priority: i64,
    now: &str,
) -> Result<Option<JobOp>, StoreError> {
    rebuild_entry_chunks(c, id)?;
    enqueue_entry(c, id, prev, priority, now)
}

/// Задание записи по уже записанным чанкам и `entries.index_state`: индексируемая →
/// `upsert`, ставшая исключённой → `delete`. Возвращает поставленную операцию.
fn enqueue_entry(
    c: &Connection,
    id: Id,
    prev: Option<IndexState>,
    priority: i64,
    now: &str,
) -> Result<Option<JobOp>, StoreError> {
    let (state, rev): (IndexState, i64) = c
        .prepare_cached("SELECT index_state, current_rev FROM entries WHERE id = ?1")?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let op = if state != IndexState::Excluded {
        Some(JobOp::Upsert)
    } else if prev.is_some_and(|p| p != IndexState::Excluded) {
        Some(JobOp::Delete)
    } else {
        None
    };
    if let Some(op) = op {
        enqueue_target(c, TargetKind::Entry, id, rev, op, priority, now)?;
    }
    Ok(op)
}

/// [`index_entry`] с приоритетом записи.
pub(crate) fn index_entry_now(
    c: &Connection,
    id: Id,
    prev: Option<IndexState>,
    now: &str,
) -> Result<(), StoreError> {
    index_entry(c, id, prev, PRIORITY_WRITE, now)?;
    Ok(())
}

/// Мягкое удаление: чанки (и FTS) убираются, индексируемой записи ставится `delete`.
pub(crate) fn unindex_entry(
    c: &Connection,
    id: Id,
    rev: i64,
    state: IndexState,
    now: &str,
) -> Result<(), StoreError> {
    c.prepare_cached("DELETE FROM chunks WHERE entry_id = ?1")?
        .execute([id])?;
    if state != IndexState::Excluded {
        enqueue_target(
            c,
            TargetKind::Entry,
            id,
            rev,
            JobOp::Delete,
            PRIORITY_WRITE,
            now,
        )?;
    }
    Ok(())
}

/// `entries.index_state` записи, если `rev` — её текущая ревизия и она жива.
/// Исключённая запись меняется только на `excluded`.
pub(crate) fn set_entry_state(
    c: &Connection,
    id: Id,
    rev: i64,
    state: IndexState,
) -> Result<(), StoreError> {
    c.prepare_cached(
        "UPDATE entries SET index_state = ?3
         WHERE id = ?1 AND current_rev = ?2 AND deleted_at IS NULL
           AND (index_state <> 'excluded' OR ?3 = 'excluded')",
    )?
    .execute(params![id, rev, state.as_str()])?;
    Ok(())
}

/// Чанки записи для векторного индекса с полями payload. Запись удалена, исключена или
/// лежит в Human — пустой список. Нет записи → `NotFound`.
pub async fn entry_chunks_for_index(db: &Db, id: Id) -> Result<Vec<IndexChunk>, StoreError> {
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        entry_index_chunks(&tx, id)
    })
    .await
}

/// [`entry_chunks_for_index`] внутри открытой транзакции чтения.
fn entry_index_chunks(tx: &Connection, id: Id) -> Result<Vec<IndexChunk>, StoreError> {
    type Head = (NodePayload, EntryPayload, IndexState, bool);
    let row: Option<Head> = tx
        .prepare_cached(
            "SELECT e.space_id, s.kind, s.solution_guid, e.tags, e.area, e.updated_at,
                    e.kind, e.status, e.data, e.certainty, e.index_state,
                    e.deleted_at IS NOT NULL
             FROM entries e JOIN spaces s ON s.id = e.space_id
             WHERE e.id = ?1",
        )?
        .query_row([id], |r| {
            // теги — в payload точек: значения секретов маскируются (§7.6)
            let tags = crate::nodes::mask_tags(super::json_col(r, 3)?);
            let data: serde_json::Value = super::json_col(r, 8)?;
            let area: Option<String> = r.get(4)?;
            let kind: EntryKind = r.get(6)?;
            Ok((
                NodePayload {
                    node_id: id,
                    space_id: r.get(0)?,
                    space_kind: r.get(1)?,
                    solution_guid: r.get(2)?,
                    path: String::new(),
                    path_ancestors: Vec::new(),
                    tags,
                    area: area.clone(),
                    updated_at: ts_at(r, 5)?,
                    // статус записи — в `EntryPayload`
                    certainty: None,
                },
                EntryPayload {
                    entry_id: id,
                    kind: kind.as_str().to_string(),
                    status: r.get(7)?,
                    insight_kind: data_kind(&data),
                    certainty: r.get(9)?,
                    area,
                },
                r.get(10)?,
                r.get(11)?,
            ))
        })
        .optional()?;
    let Some((payload, entry, state, deleted)) = row else {
        return Err(StoreError::NotFound);
    };
    if deleted || state == IndexState::Excluded || payload.space_kind == SpaceKind::Human {
        return Ok(Vec::new());
    }
    read_index_chunks(
        tx,
        "SELECT id, level, heading_path, ordinal, text, context_prefix, text_hash,
                token_count, rev, embed_key
         FROM chunks WHERE entry_id = ?1 ORDER BY ordinal",
        id,
        &payload,
        Some(&entry),
    )
}

/// Запись для пересборки индекса из кэша: текущая ревизия и её чанки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReindexEntry {
    pub entry_id: Id,
    pub rev: i64,
    pub chunks: Vec<IndexChunk>,
}

/// Условие «живая индексируемая запись» (алиасы: `e` — entries, `s` — spaces).
const INDEXABLE: &str =
    "e.deleted_at IS NULL AND e.index_state <> 'excluded' AND s.kind <> 'human'";

/// Страница живых индексируемых записей пространства `space` (`None` — всех) с чанками:
/// по возрастанию id после `after`, не больше `limit`, одним снимком чтения.
pub async fn reindex_entries_page(
    db: &Db,
    space: Option<Id>,
    after: Option<Id>,
    limit: usize,
) -> Result<Vec<ReindexEntry>, StoreError> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        let ids: Vec<(Id, i64)> = {
            let mut st = tx.prepare_cached(&format!(
                "SELECT e.id, e.current_rev FROM entries e JOIN spaces s ON s.id = e.space_id
                 WHERE {INDEXABLE}
                   AND (?1 IS NULL OR e.space_id = ?1) AND (?2 IS NULL OR e.id > ?2)
                 ORDER BY e.id LIMIT ?3"
            ))?;
            st.query_map(params![space, after, limit], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?
        };
        let mut out = Vec::with_capacity(ids.len());
        for (entry_id, rev) in ids {
            out.push(ReindexEntry {
                entry_id,
                rev,
                chunks: entry_index_chunks(&tx, entry_id)?,
            });
        }
        Ok(out)
    })
    .await
}

/// Ставит `upsert` (приоритет reindex) живым индексируемым записям `entries` и переводит их
/// в `pending`. Возвращает число поставленных заданий.
pub async fn enqueue_entry_upserts(db: &Db, entries: Vec<Id>) -> Result<u64, StoreError> {
    if entries.is_empty() {
        return Ok(0);
    }
    db.write(move |tx| {
        let now = fmt_ts(Timestamp::now());
        let mut pick = tx.prepare_cached(&format!(
            "UPDATE entries SET index_state = 'pending'
             WHERE id = ?1 AND EXISTS (SELECT 1 FROM entries e JOIN spaces s ON s.id = e.space_id
                                       WHERE e.id = ?1 AND {INDEXABLE})
             RETURNING current_rev"
        ))?;
        let mut jobs = 0u64;
        for id in entries {
            let rev: Option<i64> = pick.query_row([id], |r| r.get(0)).optional()?;
            if let Some(rev) = rev {
                enqueue_target(
                    tx,
                    TargetKind::Entry,
                    id,
                    rev,
                    JobOp::Upsert,
                    crate::jobs::PRIORITY_REINDEX,
                    &now,
                )?;
                jobs += 1;
            }
        }
        Ok(jobs)
    })
    .await
}

/// Точки записей `(запись, ревизия)` восстановлены из кэша и подтверждены Qdrant:
/// неудавшиеся задания снимаются, запись без активного задания — `indexed`.
pub async fn finish_entries_reindexed(db: &Db, items: Vec<(Id, i64)>) -> Result<(), StoreError> {
    if items.is_empty() {
        return Ok(());
    }
    db.write(move |tx| {
        let mut drop_failed = tx.prepare_cached(
            "DELETE FROM index_jobs
             WHERE target_kind = 'entry' AND target_id = ?1 AND state = 'failed'",
        )?;
        for (id, rev) in items {
            drop_failed.execute([id])?;
            let t = TargetRef {
                kind: TargetKind::Entry,
                id,
            };
            if !has_active_target(tx, t)? {
                set_entry_state(tx, id, rev, IndexState::Indexed)?;
            }
        }
        Ok(())
    })
    .await
}

/// Записей в одной транзакции перечанковки.
const RECHUNK_BATCH: i64 = 200;

/// Перечанковывает живые записи (`space = None` — всех пространств) текущим счётчиком
/// токенов и ставит задания с `priority`. `only_changed` — как у
/// [`crate::nodes::rechunk_notes`]: проиндексированная запись с неизменной нарезкой задания
/// не получает. Пакетами в отдельных транзакциях. Возвращает число поставленных `upsert`.
/// `progress` — счётчик обработанных записей (ход перенарезки), растёт после каждого пакета.
pub(crate) async fn rechunk_entries(
    db: &Db,
    space: Option<Id>,
    priority: i64,
    only_changed: bool,
    progress: Option<&std::sync::atomic::AtomicU64>,
) -> Result<u64, StoreError> {
    let mut upserts = 0u64;
    let mut after: Option<Id> = None;
    loop {
        let cursor = after;
        let (n, u, last) = db
            .write(move |tx| {
                let now = fmt_ts(Timestamp::now());
                let batch: Vec<(Id, IndexState)> = {
                    let mut st = tx.prepare_cached(
                        "SELECT id, index_state FROM entries
                         WHERE deleted_at IS NULL
                           AND (?1 IS NULL OR space_id = ?1) AND (?2 IS NULL OR id > ?2)
                         ORDER BY id LIMIT ?3",
                    )?;
                    st.query_map(params![space, cursor, RECHUNK_BATCH], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?
                    .collect::<Result<_, _>>()?
                };
                let mut u = 0u64;
                for &(id, prev) in &batch {
                    let changed = rebuild_entry_chunks(tx, id)?;
                    if only_changed && !changed && prev == IndexState::Indexed {
                        continue;
                    }
                    if prev != IndexState::Excluded {
                        tx.prepare_cached(
                            "UPDATE entries SET index_state = 'pending' WHERE id = ?1",
                        )?
                        .execute([id])?;
                    }
                    let op = enqueue_entry(tx, id, Some(prev), priority, &now)?;
                    u += u64::from(op == Some(JobOp::Upsert));
                }
                Ok((batch.len() as i64, u, batch.last().map(|b| b.0)))
            })
            .await?;
        upserts += u;
        if let Some(p) = progress {
            p.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
        }
        match last {
            Some(l) if n == RECHUNK_BATCH => after = Some(l),
            _ => break,
        }
    }
    Ok(upserts)
}

/// Разовая перенарезка записей с секретами (миграция на сканер §7.6): живые записи, чей
/// документ `# <title>\n\n<body>` содержит секреты, пересобирают чанки из замаскированного
/// текста и получают `upsert` с приоритетом `priority`. Записи без секретов не трогаются.
/// Пакетами по [`RECHUNK_BATCH`]; идемпотентно. Возвращает число перенарезанных записей.
pub(crate) async fn rescan_entry_secrets(db: &Db, priority: i64) -> Result<u64, StoreError> {
    let (mut total, mut after) = (0u64, None::<Id>);
    loop {
        let cursor = after;
        let (n, found, last) = db
            .write(move |tx| {
                let now = fmt_ts(Timestamp::now());
                let batch: Vec<(Id, IndexState, String, String)> = {
                    let mut st = tx.prepare_cached(
                        "SELECT id, index_state, title, body FROM entries
                         WHERE deleted_at IS NULL AND (?1 IS NULL OR id > ?1)
                         ORDER BY id LIMIT ?2",
                    )?;
                    st.query_map(params![cursor, RECHUNK_BATCH], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })?
                    .collect::<Result<_, _>>()?
                };
                let mut found = 0u64;
                for (id, prev, title, body) in &batch {
                    if scan_secrets(&entry_doc(title, body)).is_empty()
                        && !crate::nodes::stored_prefix_hit(tx, "entry_id", *id)?
                    {
                        continue;
                    }
                    found += 1;
                    index_entry(tx, *id, Some(*prev), priority, &now)?;
                }
                Ok((batch.len() as i64, found, batch.last().map(|b| b.0)))
            })
            .await?;
        total += found;
        match last {
            Some(l) if n == RECHUNK_BATCH => after = Some(l),
            _ => break,
        }
    }
    if total > 0 {
        tracing::info!(
            entries = total,
            "секреты записей: чанки перенарезаны без значений"
        );
    }
    Ok(total)
}
