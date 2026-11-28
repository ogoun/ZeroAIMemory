//! Версионные миграции на `PRAGMA user_version` с копией файла перед обновлением.
//!
//! **Контрольные суммы (M7.4 §7.1).** Служебная таблица мигратора [`TABLE`] (`version`,
//! `sha256`, `recorded_at`, `origin`) — в каждой базе; её DDL — здесь, а не номерной миграцией
//! (решение 98). Сумма — SHA-256 текста SQL после замены `\r\n` на `\n` ([`checksum`]).
//! Применённая миграция пишет строку `applied` в той же транзакции, что её SQL и `user_version`;
//! версии `≤ user_version` без строки (база до 0.0.17) получают строки `adopted` текущих файлов
//! одной транзакцией после применения (решение 99). Записанные суммы сверяются с программой при
//! каждом открытии — **до** применения новых миграций и до копии `bak-v<N>` **[решение
//! исполнителя, задача 1]**: база с изменённой выпущенной миграцией не получает ни копии, ни
//! следующих миграций, открытие — [`StoreError::MigrationChanged`] (решение 100). Сверка схемы с
//! эталоном и открытие без изменений — [`crate::inspect`].

use std::path::{Path, PathBuf};

use rusqlite::{Connection, TransactionBehavior};

use crate::StoreError;

/// Служебная таблица мигратора: суммы применённых и принятых миграций.
pub const TABLE: &str = "zam_migrations";

/// DDL таблицы сумм (`IF NOT EXISTS`: выполняется в каждой транзакции записи сумм).
const TABLE_DDL: &str = "CREATE TABLE IF NOT EXISTS zam_migrations (
  version INTEGER PRIMARY KEY,
  sha256 TEXT NOT NULL,
  recorded_at TEXT NOT NULL,
  origin TEXT NOT NULL CHECK (origin IN ('applied', 'adopted')))";

/// SHA-256 текста миграции (нижний регистр hex) после замены `\r\n` на `\n`: файл, полученный
/// с CRLF (Windows без `.gitattributes`), даёт ту же сумму.
pub fn checksum(sql: &str) -> String {
    use sha2::Digest;
    let norm = sql.replace("\r\n", "\n");
    sha2::Sha256::digest(norm.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Есть ли таблица сумм в базе.
pub(crate) fn has_table(conn: &Connection) -> Result<bool, StoreError> {
    Ok(conn.query_row(
        "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [TABLE],
        |r| r.get(0),
    )?)
}

/// Записанные суммы `(версия, сумма)` по возрастанию версии; таблицы нет — пусто.
pub(crate) fn recorded(conn: &Connection) -> Result<Vec<(u32, String)>, StoreError> {
    if !has_table(conn)? {
        return Ok(Vec::new());
    }
    let mut st = conn.prepare("SELECT version, sha256 FROM zam_migrations ORDER BY version")?;
    let rows = st.query_map([], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, String>(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Версии `≤ applied`, у которых записанная сумма расходится с суммой миграции программы:
/// `(версия, записанная, программы)` по возрастанию. Версий вне списка программы не сверяет.
pub(crate) fn changed(
    rows: &[(u32, String)],
    migrations: &[Migration],
    applied: u32,
) -> Vec<(u32, String, String)> {
    rows.iter()
        .filter(|(v, _)| *v <= applied)
        .filter_map(|(v, sha)| {
            let m = migrations.iter().find(|m| m.version == *v)?;
            let compiled = checksum(m.sql);
            (compiled != *sha).then(|| (*v, sha.clone(), compiled))
        })
        .collect()
}

/// Сверка записанных сумм применённых версий (`≤ applied`, обычно `user_version`) с
/// программой: первое расхождение — [`StoreError::MigrationChanged`]. Только чтение.
pub(crate) fn verify(
    conn: &Connection,
    migrations: &[Migration],
    applied: u32,
) -> Result<(), StoreError> {
    let rows = recorded(conn)?;
    match changed(&rows, migrations, applied).into_iter().next() {
        Some((version, recorded, compiled)) => Err(StoreError::MigrationChanged {
            version,
            recorded,
            compiled,
        }),
        None => Ok(()),
    }
}

/// Строка `applied` миграции `m` в транзакции её SQL (таблица создаётся при необходимости).
fn record_applied(tx: &Connection, m: &Migration) -> Result<(), StoreError> {
    tx.execute_batch(TABLE_DDL)?;
    // строка той же версии без применения (аномалия: `user_version` меньше записанного) —
    // заменяется: сумма теперь у только что применённого SQL
    tx.execute(
        "INSERT OR REPLACE INTO zam_migrations (version, sha256, recorded_at, origin)
         VALUES (?1, ?2, ?3, 'applied')",
        rusqlite::params![
            m.version,
            checksum(m.sql),
            crate::time::fmt_ts(crate::time::now())
        ],
    )?;
    Ok(())
}

/// Строки `adopted` для версий `1..=upto` без строки — одной транзакцией; ничего не нужно —
/// без записи.
fn adopt_missing(
    conn: &mut Connection,
    migrations: &[Migration],
    upto: u32,
) -> Result<(), StoreError> {
    if upto == 0 {
        return Ok(());
    }
    if has_table(conn)? {
        let have: u32 = conn.query_row(
            "SELECT count(*) FROM zam_migrations WHERE version BETWEEN 1 AND ?1",
            [upto],
            |r| r.get(0),
        )?;
        if have >= upto {
            return Ok(());
        }
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(TABLE_DDL)?;
    let now = crate::time::fmt_ts(crate::time::now());
    {
        let mut st = tx.prepare(
            "INSERT OR IGNORE INTO zam_migrations (version, sha256, recorded_at, origin)
             VALUES (?1, ?2, ?3, 'adopted')",
        )?;
        for m in migrations.iter().take(upto as usize) {
            st.execute(rusqlite::params![m.version, checksum(m.sql), now])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Одна миграция: целевая версия схемы и SQL (может содержать несколько операторов).
///
/// `rebuild` — миграция пересобирает таблицы (новая таблица, перенос строк, `DROP`, `RENAME`):
/// SQLite не меняет CHECK через `ALTER`. Для неё загрузчик выполняет процедуру документации
/// SQLite «Making Other Kinds Of Table Schema Changes»: `PRAGMA foreign_keys=OFF` вне
/// транзакции, SQL миграции, `PRAGMA foreign_key_check` (любая строка — откат и
/// [`StoreError::InvalidMigrations`]), `user_version`, `COMMIT`, прежнее значение
/// `foreign_keys` (спецификация M4.5 §1.1). Внутри транзакции `PRAGMA foreign_keys` не
/// действует, поэтому признак задаётся на миграции, а не в её SQL.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: u32,
    pub sql: &'static str,
    pub rebuild: bool,
}

/// Путь копии перед миграцией: `<file>.bak-v<old>` рядом с файлом.
fn backup_path(path: &Path, old: u32) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".bak-v{old}"));
    path.with_file_name(name)
}

fn user_version(conn: &Connection) -> Result<u32, StoreError> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Применяет недостающие миграции, возвращает итоговую версию. Записанные суммы сверяются до
/// любых изменений; применённые миграции пишут строки `applied`, версии без строки получают
/// `adopted` (см. документацию модуля).
pub fn apply(
    conn: &mut Connection,
    path: &Path,
    migrations: &[Migration],
) -> Result<u32, StoreError> {
    // Валидация до любых обращений к файлу и транзакций.
    for (i, m) in migrations.iter().enumerate() {
        let expected = i as u32 + 1;
        if m.version != expected {
            return Err(StoreError::InvalidMigrations(format!(
                "позиция {}: ожидалась версия {expected}, указана {} (нужны версии 1..N по порядку, без дубликатов и пропусков)",
                i + 1,
                m.version
            )));
        }
    }
    let latest = migrations.iter().map(|m| m.version).max().unwrap_or(0);
    let current = user_version(conn)?;
    if current > latest {
        return Err(StoreError::SchemaTooNew {
            found: current,
            supported: latest,
        });
    }
    // база с изменённой выпущенной миграцией не получает ни копии, ни следующих миграций
    verify(conn, migrations, current)?;
    if current == latest {
        adopt_missing(conn, migrations, latest)?;
        return Ok(current);
    }

    // Копия нужна, только если в файле уже есть данные/схема.
    let has_tables: i64 = conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
    if current > 0 || has_tables > 0 {
        let bak = backup_path(path, current);
        match std::fs::remove_file(&bak) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        // Путь передаётся параметром, а не вставляется в SQL (кавычки в путях).
        conn.execute("VACUUM INTO ?1", [bak.to_string_lossy().as_ref()])?;
    }

    let mut pending: Vec<&Migration> = migrations.iter().filter(|m| m.version > current).collect();
    pending.sort_by_key(|m| m.version);
    for m in pending {
        if m.rebuild {
            apply_rebuild(conn, m)?;
        } else {
            let tx = conn.transaction()?;
            tx.execute_batch(m.sql)?;
            record_applied(&tx, m)?;
            tx.pragma_update(None, "user_version", m.version)?;
            tx.commit()?;
        }
    }
    // версии до `current` у базы до 0.0.17 — без строк: суммы текущих файлов принимаются
    adopt_missing(conn, migrations, latest)?;
    Ok(latest)
}

/// Миграция с пересборкой таблиц: проверки внешних ключей выключены на время транзакции,
/// целостность проверяется `foreign_key_check` перед `COMMIT`. Прежнее значение
/// `foreign_keys` восстанавливается и при ошибке.
fn apply_rebuild(conn: &mut Connection, m: &Migration) -> Result<(), StoreError> {
    let fk_was_on: bool = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    conn.pragma_update(None, "foreign_keys", false)?;
    // внутри открытой транзакции PRAGMA молча не действует: без проверки пересборка шла бы
    // с включёнными внешними ключами и падала бы на DROP или теряла бы дочерние строки
    let still_on: bool = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if still_on {
        return Err(StoreError::InvalidMigrations(format!(
            "миграция {}: нельзя выключить foreign_keys: открыта транзакция",
            m.version
        )));
    }
    let res = rebuild_tx(conn, m);
    let restore = conn.pragma_update(None, "foreign_keys", fk_was_on);
    res?;
    restore?;
    Ok(())
}

fn rebuild_tx(conn: &mut Connection, m: &Migration) -> Result<(), StoreError> {
    let tx = conn.transaction()?;
    tx.execute_batch(m.sql)?;
    // Нарушения: (таблица, rowid, родитель, номер FK). Первые строки — в текст ошибки.
    let violations: Vec<String> = {
        let mut st = tx.prepare("PRAGMA foreign_key_check")?;
        let rows = st.query_map([], |r| {
            Ok(format!(
                "{} rowid={} → {}",
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?
                    .map_or_else(|| "?".to_owned(), |v| v.to_string()),
                r.get::<_, String>(2)?
            ))
        })?;
        rows.take(5).collect::<Result<_, _>>()?
    };
    if !violations.is_empty() {
        // Транзакция откатывается при drop.
        return Err(StoreError::InvalidMigrations(format!(
            "миграция {}: нарушены внешние ключи после пересборки: {}",
            m.version,
            violations.join("; ")
        )));
    }
    record_applied(&tx, m)?;
    tx.pragma_update(None, "user_version", m.version)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Пересборка при открытой транзакции отклоняется до выполнения SQL миграции.
    #[test]
    fn rebuild_rejected_inside_transaction() {
        let mut c = Connection::open_in_memory().unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        c.execute_batch("BEGIN").unwrap();
        let m = Migration {
            version: 1,
            sql: "CREATE TABLE t (id INTEGER)",
            rebuild: true,
        };
        match apply_rebuild(&mut c, &m) {
            Err(StoreError::InvalidMigrations(msg)) => {
                assert!(
                    msg.contains("нельзя выключить foreign_keys: открыта транзакция"),
                    "{msg}"
                );
            }
            other => panic!("ожидалась InvalidMigrations: {other:?}"),
        }
        // SQL миграции не выполнялся
        let n: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 't'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }
}
