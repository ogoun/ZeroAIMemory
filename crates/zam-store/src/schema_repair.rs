//! Досборка схемы 0025 у баз пользователя, открытых промежуточной сборкой M6 (ревью задачи 19,
//! I-1).
//!
//! Мигратор ([`crate::migrate`]) смотрит только на `user_version`. Миграция 0025 дописывалась
//! до выпуска M6: `imported_from` у `nodes` и `entries` (задача 18b2) и таблица
//! `node_idempotency` (задача 19). База, получившая версию 25 раньше этих правок, их не получит —
//! запросы с `Idempotency-Key` и импорт давали бы 500. Поэтому после миграций база пользователя
//! версии 25 дособирается: недостающие столбцы — `ALTER TABLE … ADD COLUMN` тем же текстом, что
//! в 0025, недостающая таблица — тем же DDL ([`NODE_IDEMPOTENCY_DDL`]; тест сверяет его с
//! файлом миграции). Досборка идемпотентна и идёт одной транзакцией; что дособрано — строка WARN.
//!
//! 0025 заморожена после M6: новые изменения схемы — только новой миграцией.
use rusqlite::{Connection, OptionalExtension};

use crate::StoreError;

/// Версия схемы, к которой относится досборка.
const REPAIR_VERSION: u32 = 25;

/// DDL таблицы ключей идемпотентности заметок — дословно как в `0025_session_epochs.sql` (без
/// `;`): SQLite хранит текст `CREATE` как есть, и дособранная схема совпадает с миграцией.
pub(crate) const NODE_IDEMPOTENCY_DDL: &str = "CREATE TABLE node_idempotency (
  key TEXT PRIMARY KEY,
  op TEXT NOT NULL CHECK (op IN ('create','replace','patch')),
  node_id TEXT NOT NULL REFERENCES nodes(id),
  fingerprint TEXT NOT NULL,
  created_at TEXT NOT NULL) WITHOUT ROWID";

/// Столбцы, дописанные в 0025 после её первого применения: (таблица, определение столбца).
const LATE_COLUMNS: [(&str, &str, &str); 2] = [
    ("nodes", "imported_from", "imported_from TEXT"),
    ("entries", "imported_from", "imported_from TEXT"),
];

/// Дособирает схему базы пользователя версии 25; возвращает, что дособрано (пусто — ничего).
/// Другая версия — ничего не делает.
pub(crate) fn repair_user_schema(
    conn: &mut Connection,
    version: u32,
) -> Result<Vec<&'static str>, StoreError> {
    if version != REPAIR_VERSION {
        return Ok(Vec::new());
    }
    let tx = conn.transaction()?;
    let mut done = Vec::new();
    for (table, col, def) in LATE_COLUMNS {
        let has: bool = tx.query_row(
            "SELECT count(*) > 0 FROM pragma_table_info(?1) WHERE name = ?2",
            [table, col],
            |r| r.get(0),
        )?;
        if !has {
            tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {def}"))?;
            done.push(if table == "nodes" {
                "nodes.imported_from"
            } else {
                "entries.imported_from"
            });
        }
    }
    let table: Option<String> = tx
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'node_idempotency'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if table.is_none() {
        tx.execute_batch(NODE_IDEMPOTENCY_DDL)?;
        done.push("node_idempotency");
    }
    tx.commit()?;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DDL досборки — дословно из миграции 0025 (один источник текста схемы).
    #[test]
    fn node_idempotency_ddl_matches_0025() {
        let sql = crate::user_db::USER_MIGRATIONS[24].sql;
        assert!(
            sql.contains(&format!("{NODE_IDEMPOTENCY_DDL};")),
            "DDL node_idempotency разошёлся с 0025_session_epochs.sql"
        );
        for (table, _, def) in LATE_COLUMNS {
            assert!(
                sql.contains(&format!("ALTER TABLE {table} ADD COLUMN {def};")),
                "{table}: {def}"
            );
        }
    }
}
