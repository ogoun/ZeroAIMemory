//! Снимок сессии (`session_snapshots`, спецификация M6 §8а, миграция 0025).
//!
//! - Один снимок на сессию базы пользователя: [`put_snapshot`] заменяет прежний целиком. Сессия
//!   должна существовать (внешний ключ на `sessions(id)`): её регистрирует явная сессия запроса.
//! - `data` — JSON нейтрального снимка [`SnapshotData`] после маскировки клиентом; размер `bytes`
//!   и полноту `status` присылает клиент, предел размера проверяет сервер.
//! - Удаление: вместе с сессией ([`crate::gc_sessions`], каскада нет) и по сроку
//!   `snapshot.keep_days` ([`gc_snapshots`], воркер обслуживания).
use jiff::Timestamp;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use zam_core::snapshot::{SnapStatus, SnapTrigger, SnapshotData};

use crate::time::{conv_err, fmt_ts, ser_ts, ts_at};
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

/// Записывает снимок сессии, заменяя прежний. Сессии нет — [`StoreError::NotFound`].
pub async fn put_snapshot(db: &Db, row: &SnapshotRow) -> Result<(), StoreError> {
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
        t.execute(
            "INSERT INTO session_snapshots (session_id, taken_at, trigger_kind, agent, status,
                                            skipped, bytes, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (session_id) DO UPDATE SET
               taken_at = excluded.taken_at, trigger_kind = excluded.trigger_kind,
               agent = excluded.agent, status = excluded.status, skipped = excluded.skipped,
               bytes = excluded.bytes, data = excluded.data",
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
        Ok(())
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
