//! Досборка схемы 0025 у баз пользователя, открытых промежуточной сборкой M6 (ревью задачи 19,
//! I-1, N-1…N-3).
//!
//! Мигратор ([`crate::migrate`]) смотрит только на `user_version`, а 0025 правилась после первого
//! применения. Промежуточные версии 0025 (`git log -- …/0025_session_epochs.sql`) и их отличия от
//! итоговой схемы:
//! - `61c5c14` (задача 2) — столбец `session_snapshots.trigger` (итог — `trigger_kind`), нет
//!   `imported_from` у `nodes`/`entries`, нет `node_idempotency`;
//! - `d00f46c` (ревью задачи 2) — нет `imported_from`, нет `node_idempotency`;
//! - `966cb85` (задача 18b2) — нет `node_idempotency`;
//! - `52ea190` (задача 19) и далее — итоговая схема (дальше менялись только комментарии вне
//!   `CREATE`).
//!
//! Досборка покрывает все три отличия: `RENAME COLUMN trigger TO trigger_kind`, `ALTER TABLE …
//! ADD COLUMN` тем же текстом, что в 0025, и таблица тем же DDL ([`NODE_IDEMPOTENCY_DDL`]; тест
//! сверяет его с файлом миграции). Идёт **до** следующих миграций, если база открыта на версии 25
//! (`Db::open_with` читает версию до `migrate::apply`): миграция 0026+ увидит итоговую схему 0025.
//! Одна транзакция `IMMEDIATE`, идемпотентна; что дособрано — строка WARN.
//!
//! 0025 заморожена после M6: новые изменения схемы — только новой миграцией.
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use crate::{Migration, StoreError};

/// Версия схемы, к которой относится досборка.
pub(crate) const REPAIR_VERSION: u32 = 25;

/// DDL таблицы ключей идемпотентности заметок — дословно как в `0025_session_epochs.sql` (без
/// `;`): SQLite хранит текст `CREATE` как есть, и дособранная схема совпадает с миграцией.
pub(crate) const NODE_IDEMPOTENCY_DDL: &str = "CREATE TABLE node_idempotency (
  key TEXT PRIMARY KEY,
  op TEXT NOT NULL CHECK (op IN ('create','replace','patch')),
  node_id TEXT NOT NULL REFERENCES nodes(id),
  fingerprint TEXT NOT NULL,
  created_at TEXT NOT NULL) WITHOUT ROWID";

/// Столбцы, дописанные в 0025 после её первого применения: (таблица, столбец, определение,
/// метка в WARN).
const LATE_COLUMNS: [(&str, &str, &str, &str); 2] = [
    (
        "nodes",
        "imported_from",
        "imported_from TEXT",
        "nodes.imported_from",
    ),
    (
        "entries",
        "imported_from",
        "imported_from TEXT",
        "entries.imported_from",
    ),
];

/// Список миграций — миграции базы пользователя: его 0025 — наша (по тексту SQL). Так досборка
/// узнаёт базу пользователя и в тестовых списках с фиктивными следующими миграциями.
pub(crate) fn is_user_list(migrations: &[Migration]) -> bool {
    let ours = &crate::user_db::USER_MIGRATIONS[REPAIR_VERSION as usize - 1];
    migrations
        .get(REPAIR_VERSION as usize - 1)
        .is_some_and(|m| m.version == ours.version && m.sql == ours.sql)
}

/// Есть ли столбец `col` у таблицы `table`.
fn has_column(c: &Connection, table: &str, col: &str) -> Result<bool, StoreError> {
    Ok(c.query_row(
        "SELECT count(*) > 0 FROM pragma_table_info(?1) WHERE name = ?2",
        [table, col],
        |r| r.get(0),
    )?)
}

/// Дособирает схему базы пользователя версии 25 (`version` — `user_version` до миграций);
/// возвращает, что дособрано (пусто — ничего). Другая версия — ничего не делает.
pub(crate) fn repair_user_schema(
    conn: &mut Connection,
    version: u32,
) -> Result<Vec<&'static str>, StoreError> {
    if version != REPAIR_VERSION {
        return Ok(Vec::new());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut done = Vec::new();
    // `61c5c14`: повод снимка назывался `trigger` (ключевое слово SQL — в кавычках)
    if has_column(&tx, "session_snapshots", "trigger")?
        && !has_column(&tx, "session_snapshots", "trigger_kind")?
    {
        tx.execute_batch(
            "ALTER TABLE session_snapshots RENAME COLUMN \"trigger\" TO trigger_kind",
        )?;
        done.push("session_snapshots.trigger_kind");
    }
    for (table, col, def, label) in LATE_COLUMNS {
        if !has_column(&tx, table, col)? {
            tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {def}"))?;
            done.push(label);
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
        for (table, _, def, _) in LATE_COLUMNS {
            assert!(
                sql.contains(&format!("ALTER TABLE {table} ADD COLUMN {def};")),
                "{table}: {def}"
            );
        }
        assert!(sql.contains("  trigger_kind TEXT NOT NULL,"));
        assert!(is_user_list(crate::user_db::USER_MIGRATIONS));
        assert!(!is_user_list(&crate::user_db::USER_MIGRATIONS[..24]));
    }
}
