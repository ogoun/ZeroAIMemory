//! L0 без ревизии (§8.3 ТЗ): значения вычисляет воркер индексации.
//!
//! - заметка: экстрактивная сводка по эмбеддингам заменяет первый абзац, только если
//!   L0 автоматический (`l0_source = 'auto'`) и ревизия не изменилась;
//! - папка: свёртка L0 детей.
//!
//! Ревизии, `updated_at` и задания индексации не меняются: L0 — производное значение.

use rusqlite::params;
use zam_core::Id;

use crate::{Db, StoreError};

/// Заменяет автоматический L0 заметки `node` ревизии `rev`. `false` — L0 ручной или
/// агентский, ревизия устарела, узел удалён или значение то же.
pub async fn set_auto_l0(db: &Db, node: Id, rev: i64, l0: &str) -> Result<bool, StoreError> {
    let l0 = l0.to_string();
    db.write(move |tx| {
        let n = tx.execute(
            "UPDATE nodes SET l0 = ?3
             WHERE id = ?1 AND kind = 'note' AND current_rev = ?2 AND deleted_at IS NULL
               AND l0_source = 'auto' AND l0 IS NOT ?3",
            params![node, rev, l0],
        )?;
        Ok(n > 0)
    })
    .await
}

/// L0 живой папки `folder` (`None` — очистить), `l0_source = 'auto'`.
/// `false` — не папка, удалена или значение то же.
pub async fn set_folder_l0(db: &Db, folder: Id, l0: Option<&str>) -> Result<bool, StoreError> {
    let l0 = l0.map(str::to_string);
    db.write(move |tx| {
        let n = tx.execute(
            "UPDATE nodes SET l0 = ?2, l0_source = CASE WHEN ?2 IS NULL THEN NULL ELSE 'auto' END
             WHERE id = ?1 AND kind = 'folder' AND deleted_at IS NULL AND l0 IS NOT ?2",
            params![folder, l0],
        )?;
        Ok(n > 0)
    })
    .await
}
