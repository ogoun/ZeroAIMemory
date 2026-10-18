//! Версионные миграции на `PRAGMA user_version` с копией файла перед обновлением.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::StoreError;

/// Одна миграция: целевая версия схемы и SQL (может содержать несколько операторов).
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: u32,
    pub sql: &'static str,
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

/// Применяет недостающие миграции, возвращает итоговую версию.
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
    if current == latest {
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
        let tx = conn.transaction()?;
        tx.execute_batch(m.sql)?;
        tx.pragma_update(None, "user_version", m.version)?;
        tx.commit()?;
    }
    Ok(latest)
}
