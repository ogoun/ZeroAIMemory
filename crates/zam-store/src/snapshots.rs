//! Снимок сессии (`session_snapshots`, спецификация M6 §8а, миграция 0025).
//!
//! - Один снимок на сессию базы пользователя: [`put_snapshot`] заменяет прежний целиком. Сессия
//!   должна существовать (внешний ключ на `sessions(id)`): её регистрирует явная сессия запроса.
//! - `data` — JSON нейтрального снимка [`SnapshotData`] после маскировки клиентом; размер `bytes`
//!   и полноту `status` присылает клиент, предел размера проверяет сервер.
//! - Удаление: вместе с сессией ([`crate::gc_sessions`], каскада нет) и по сроку
//!   `snapshot.keep_days` ([`gc_snapshots`], воркер обслуживания).
//! - Показ «последнего известного состояния» (§8а, задача 17c): последний снимок других сессий
//!   решения ([`digest_snapshot`]), запись в память другой сессией после снимка
//!   ([`writes_after`]), задачи сессии снимка ([`snapshot_tasks`]); моменты снимков списка
//!   сессий — [`snapshot_times`].
use std::collections::HashMap;

use jiff::Timestamp;
use rusqlite::{OptionalExtension, params, params_from_iter};
use serde::Serialize;
use zam_core::Id;
use zam_core::snapshot::{SnapStatus, SnapTrigger, SnapshotData};

use crate::time::{conv_err, fmt_ts, ser_ts, ts_at, ts_opt};
use crate::{Db, StoreError};

/// Строка `session_snapshots`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotRow {
    pub session_id: String,
    #[serde(serialize_with = "ser_ts")]
    pub taken_at: Timestamp,
    pub trigger: SnapTrigger,
    pub agent: Option<String>,
    pub status: SnapStatus,
    /// Пропущено строк журнала (не разобраны или вне предела).
    pub skipped: u32,
    /// Размер снимка после маскировки, байт.
    pub bytes: u32,
    pub data: SnapshotData,
}

/// Ошибка разбора столбца `i`.
fn corrupt(i: usize, what: String) -> rusqlite::Error {
    conv_err(i, StoreError::Corrupt(what))
}

fn snapshot_row(r: &rusqlite::Row) -> rusqlite::Result<SnapshotRow> {
    let trigger: String = r.get(2)?;
    let status: String = r.get(4)?;
    let data: String = r.get(7)?;
    Ok(SnapshotRow {
        session_id: r.get(0)?,
        taken_at: ts_at(r, 1)?,
        trigger: SnapTrigger::parse(&trigger)
            .ok_or_else(|| corrupt(2, format!("повод снимка «{trigger}»")))?,
        agent: r.get(3)?,
        status: SnapStatus::parse(&status)
            .ok_or_else(|| corrupt(4, format!("полнота снимка «{status}»")))?,
        skipped: r.get(5)?,
        bytes: r.get(6)?,
        data: serde_json::from_str(&data).map_err(|e| corrupt(7, format!("данные снимка: {e}")))?,
    })
}

/// Записывает снимок сессии, заменяя прежний, если тот не новее (`taken_at`): опоздавший старый
/// снимок (очередь клиента) новый не затирает — тогда `false`. Сессии нет —
/// [`StoreError::NotFound`].
pub async fn put_snapshot(db: &Db, row: &SnapshotRow) -> Result<bool, StoreError> {
    let data = serde_json::to_string(&row.data)
        .map_err(|e| StoreError::BadInput(format!("данные снимка: {e}")))?;
    let row = row.clone();
    db.write(move |t| {
        let exists = t
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                [&row.session_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(StoreError::NotFound);
        }
        let n = t.execute(
            "INSERT INTO session_snapshots (session_id, taken_at, trigger_kind, agent, status,
                                            skipped, bytes, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (session_id) DO UPDATE SET
               taken_at = excluded.taken_at, trigger_kind = excluded.trigger_kind,
               agent = excluded.agent, status = excluded.status, skipped = excluded.skipped,
               bytes = excluded.bytes, data = excluded.data
             WHERE excluded.taken_at >= session_snapshots.taken_at",
            params![
                row.session_id,
                fmt_ts(row.taken_at),
                row.trigger.as_str(),
                row.agent,
                row.status.as_str(),
                row.skipped,
                row.bytes,
                data
            ],
        )?;
        Ok(n > 0)
    })
    .await
}

/// Снимок сессии; нет — `None`.
pub async fn get_snapshot(db: &Db, session: &str) -> Result<Option<SnapshotRow>, StoreError> {
    let session = session.to_string();
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT session_id, taken_at, trigger_kind, agent, status, skipped, bytes, data
             FROM session_snapshots WHERE session_id = ?1",
            [session],
            snapshot_row,
        )
        .optional()?)
    })
    .await
}

/// Удаляет снимки, снятые раньше `cutoff` (срок `snapshot.keep_days`); число удалённых.
pub async fn gc_snapshots(db: &Db, cutoff: Timestamp) -> Result<u64, StoreError> {
    let cutoff = fmt_ts(cutoff);
    db.write(move |t| {
        let n = t.execute(
            "DELETE FROM session_snapshots WHERE taken_at < ?1",
            [cutoff],
        )?;
        Ok(n as u64)
    })
    .await
}

/// Последний снимок сессий решения `solution` кроме `except`, снятый не раньше `since`
/// (по `taken_at`, затем по id сессии — порядок однозначен); нет — `None`.
pub async fn digest_snapshot(
    db: &Db,
    solution: &str,
    except: Option<&str>,
    since: Timestamp,
    show_ended: bool,
) -> Result<Option<SnapshotRow>, StoreError> {
    let (solution, except, since) = (
        solution.to_string(),
        except.unwrap_or_default().to_string(),
        fmt_ts(since),
    );
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT p.session_id, p.taken_at, p.trigger_kind, p.agent, p.status, p.skipped,
                    p.bytes, p.data
             FROM session_snapshots p JOIN sessions s ON s.id = p.session_id
             WHERE s.solution_guid = ?1 AND p.session_id <> ?2 AND p.taken_at >= ?3
               AND (?4 OR s.ended_at IS NULL)
             ORDER BY p.taken_at DESC, p.session_id DESC LIMIT 1",
            params![solution, except, since, show_ended],
            snapshot_row,
        )
        .optional()?)
    })
    .await
}

/// Писала ли в память после `after` другая сессия решения `solution` — не `session` и не её
/// субагент (`parent_id`): `memory_writes > 0` и `last_seen_at > after` (§8а, признак (а)).
pub async fn writes_after(
    db: &Db,
    solution: &str,
    session: &str,
    after: Timestamp,
) -> Result<bool, StoreError> {
    let (solution, session, after) = (solution.to_string(), session.to_string(), fmt_ts(after));
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT 1 FROM sessions
             WHERE solution_guid = ?1 AND id <> ?2 AND coalesce(parent_id, '') <> ?2
               AND memory_writes > 0 AND last_seen_at > ?3
             LIMIT 1",
            params![solution, session, after],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
    })
    .await
}

/// Задача сессии снимка (§8а): захваченная сессией (любой срок захвата) или с её ревизией, в
/// любом статусе — закрытая после снимка задача тоже признак того, что работа продолжилась.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapTask {
    pub id: Id,
    /// Статус задачи (`open`, `in_progress`, `blocked`, `done`, …).
    pub status: String,
    /// Последняя ревизия задачи — любой сессии и без сессии (WebUI): ревизия другой сессии
    /// после снимка (§8а, признак (б)) — частный случай «задача обновлялась после снимка».
    pub last_rev_at: Option<Timestamp>,
    /// Действующий захват (`until > now`) другой сессией.
    pub claimed_by_other: bool,
}

/// Задачи сессии `session` пространств `spaces` ([`SnapTask`]), недавно менявшиеся первыми, до
/// `limit`; `now` — для действующего захвата.
pub async fn snapshot_tasks(
    db: &Db,
    spaces: &[Id],
    session: &str,
    now: Timestamp,
    limit: usize,
) -> Result<Vec<SnapTask>, StoreError> {
    if spaces.is_empty() {
        return Ok(Vec::new());
    }
    let n = spaces.len();
    let mut args: Vec<String> = spaces.iter().map(ToString::to_string).collect();
    args.push(session.to_string());
    args.push(fmt_ts(now));
    args.push(limit.to_string());
    let sql = format!(
        "SELECT e.id, e.status,
                (SELECT max(ts) FROM entry_revisions r WHERE r.entry_id = e.id),
                coalesce(json_extract(e.data, '$.claimed_by.session'), '') <> ?{s}
                  AND json_extract(e.data, '$.claimed_by.until') > ?{now}
         FROM entries e INDEXED BY entries_feed
         WHERE e.space_id IN ({list}) AND e.kind = 'task' AND e.deleted_at IS NULL
           AND (json_extract(e.data, '$.claimed_by.session') = ?{s}
                OR EXISTS (SELECT 1 FROM entry_revisions r
                           WHERE r.entry_id = e.id AND r.session_id = ?{s}))
         ORDER BY e.updated_at DESC, e.id DESC
         LIMIT ?{lim}",
        list = crate::questions::in_list(n),
        s = n + 1,
        now = n + 2,
        lim = n + 3,
    );
    db.read(move |c| {
        let mut st = c.prepare(&sql)?;
        let rows = st.query_map(params_from_iter(args), |r| {
            Ok(SnapTask {
                id: r.get(0)?,
                status: r.get(1)?,
                last_rev_at: ts_opt(r, 2)?,
                claimed_by_other: r.get::<_, Option<bool>>(3)?.unwrap_or(false),
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .await
}

/// Моменты снимков сессий `ids` (у кого снимок есть).
pub async fn snapshot_times(
    db: &Db,
    ids: &[String],
) -> Result<HashMap<String, Timestamp>, StoreError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let sql = format!(
        "SELECT session_id, taken_at FROM session_snapshots WHERE session_id IN ({})",
        crate::questions::in_list(ids.len())
    );
    let ids = ids.to_vec();
    db.read(move |c| {
        let mut st = c.prepare(&sql)?;
        let rows = st.query_map(params_from_iter(ids), |r| Ok((r.get(0)?, ts_at(r, 1)?)))?;
        Ok(rows.collect::<Result<HashMap<_, _>, _>>()?)
    })
    .await
}
