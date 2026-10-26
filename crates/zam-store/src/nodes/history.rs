//! История ревизий заметки и унифицированный дифф между текстами.

use jiff::Timestamp;
use rusqlite::OptionalExtension;
use serde::Serialize;
use similar::TextDiff;
use zam_core::Id;

use crate::time::ts_at;
use crate::{Db, StoreError};

/// Строк контекста вокруг изменения в [`unified_diff`].
const DIFF_CONTEXT: usize = 3;

/// Ревизия без текста — для списка истории.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RevisionInfo {
    pub rev: i64,
    pub content_hash: String,
    /// Размер текста в байтах.
    pub size: i64,
    /// `human` | `agent` | `system`.
    pub author_kind: String,
    pub token_id: Option<Id>,
    pub session_id: Option<String>,
    pub machine: Option<String>,
    pub message: Option<String>,
    /// Пользователь-автор (пользователь токена записи); `None` — запись без токена
    /// (администратор, система) и ревизии до M3.
    pub user_id: Option<Id>,
    #[serde(serialize_with = "crate::time::ser_ts")]
    pub ts: Timestamp,
}

/// Ревизии узла, новые сверху. История удалённого узла тоже доступна;
/// нет узла → `NotFound`, у папки — пустой список.
pub async fn list_revisions(db: &Db, id: Id) -> Result<Vec<RevisionInfo>, StoreError> {
    db.read(move |c| {
        c.query_row("SELECT 1 FROM nodes WHERE id = ?1", [id], |_| Ok(()))
            .optional()?
            .ok_or(StoreError::NotFound)?;
        let mut st = c.prepare(
            "SELECT rev, content_hash, size, author_kind, token_id, session_id, machine,
                    message, ts, user_id
             FROM revisions WHERE node_id = ?1 ORDER BY rev DESC",
        )?;
        let rows = st.query_map([id], |r| {
            Ok(RevisionInfo {
                rev: r.get(0)?,
                content_hash: r.get(1)?,
                size: r.get(2)?,
                author_kind: r.get(3)?,
                token_id: r.get(4)?,
                session_id: r.get(5)?,
                machine: r.get(6)?,
                message: r.get(7)?,
                ts: ts_at(r, 8)?,
                user_id: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}

/// Унифицированный дифф по строкам (контекст 3) с заголовками
/// `--- old_label` / `+++ new_label`. Тексты равны → пустая строка.
pub fn unified_diff(old: &str, new: &str, old_label: &str, new_label: &str) -> String {
    if old == new {
        return String::new();
    }
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(DIFF_CONTEXT)
        .header(old_label, new_label)
        .to_string()
}
