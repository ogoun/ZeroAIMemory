//! Правка существующей заметки (§11.3 ТЗ): замена текста с проверкой ревизии
//! (`If-Match`), PATCH секций относительно `base_rev` и откат к старой ревизии.
//!
//! Каждая операция — одна транзакция писателя. Новая ревизия только дописывается
//! (§4.1 п.4); frontmatter, L0, `content_hash`, размер и чанки пересчитываются так же,
//! как при создании. Текст с тем же `content_hash`, что у текущей ревизии, новой
//! ревизии не создаёт — возвращается текущий узел.

use jiff::Timestamp;
use rusqlite::{Connection, params};
use zam_core::Id;
use zam_markdown::{PatchError, PatchOp, apply_patch, section_bytes};

use super::chunks::note_meta;
use super::write::{index_state, store_revision};
use super::{Attribution, Node, NodeKind, load_node, revision_text, sha256_hex};
use crate::time::fmt_ts;
use crate::{Db, StoreError};

/// Ошибка правки заметки. Сервер отображает `RevMismatch` и `SectionChanged` в 409,
/// `Patch` — в 422. Сообщения не содержат текста заметки (его нельзя писать в логи).
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error(transparent)]
    Store(#[from] StoreError),
    /// `base_rev` не совпадает с текущей ревизией; `current` — полный текст текущей.
    #[error("ревизия устарела: текущая ревизия {current_rev}")]
    RevMismatch { current_rev: i64, current: String },
    #[error("операция PATCH не применима: {0}")]
    Patch(#[from] PatchError),
    /// Затронутая секция изменилась после `base_rev`.
    #[error("секция изменена после базовой ревизии: текущая ревизия {current_rev}")]
    SectionChanged { section: String, current_rev: i64 },
}

/// Заменяет текст заметки. `base_rev` должен совпадать с текущей ревизией, иначе
/// `RevMismatch` с текущим текстом. `l0 = Some` — сводка агента; `None` — L0
/// пересчитывается по тексту (прежняя сводка агента не сохраняется, §8.3).
/// Нет узла или он удалён → `NotFound`; папка → `BadInput`.
pub async fn update_note(
    db: &Db,
    id: Id,
    base_rev: i64,
    content: String,
    l0: Option<String>,
    a: Attribution,
) -> Result<Node, WriteError> {
    write_note(db, move |tx| {
        let node = live_note(tx, id)?;
        if base_rev != node.current_rev {
            return Err(rev_mismatch(tx, &node)?);
        }
        Ok(commit_text(tx, node, &content, l0.as_deref(), &a)?)
    })
    .await
}

/// Применяет операции PATCH последовательно к тексту текущей ревизии.
///
/// Для операции с секцией сравниваются байты секции в `base_rev` и в текущей
/// ревизии (до применения всей цепочки): различаются (или секция есть только в
/// одной из них) → `SectionChanged`. При `base_rev == current_rev` проверка не
/// нужна; `Append` и `Prepend` применяются всегда. `base_rev` новее текущей →
/// `RevMismatch`, такой ревизии нет → `NotFound`. Любая ошибка — ничего не записано.
pub async fn patch_note(
    db: &Db,
    id: Id,
    base_rev: i64,
    ops: Vec<PatchOp>,
    a: Attribution,
) -> Result<Node, WriteError> {
    write_note(db, move |tx| {
        let node = live_note(tx, id)?;
        if ops.is_empty() {
            return Err(StoreError::BadInput("пустой список операций PATCH".into()).into());
        }
        let cur_rev = node.current_rev;
        if base_rev > cur_rev {
            return Err(rev_mismatch(tx, &node)?);
        }
        let current = revision_text(tx, id, cur_rev)?;
        let base = if base_rev == cur_rev {
            None
        } else {
            Some(revision_text(tx, id, base_rev)?)
        };
        let mut text = current.clone();
        for op in &ops {
            if let (Some(base), Some(section)) = (&base, op_section(op))
                && section_bytes(base, section) != section_bytes(&current, section)
            {
                return Err(WriteError::SectionChanged {
                    section: section.to_string(),
                    current_rev: cur_rev,
                });
            }
            text = apply_patch(&text, op)?.text;
        }
        Ok(commit_text(tx, node, &text, None, &a)?)
    })
    .await
}

/// Откат: новая ревизия с текстом ревизии `to_rev`, L0 пересчитывается по тексту.
/// Нет узла, он удалён или нет такой ревизии → `NotFound`; папка → `BadInput`.
/// Текст `to_rev` совпадает с текущим → новой ревизии нет.
pub async fn revert_note(db: &Db, id: Id, to_rev: i64, a: Attribution) -> Result<Node, StoreError> {
    db.write(move |tx| {
        let node = live_note(tx, id)?;
        let text = revision_text(tx, id, to_rev)?;
        commit_text(tx, node, &text, None, &a)
    })
    .await
}

/// Транзакция писателя для операции с [`WriteError`]: ошибки хранилища идут через
/// `Db::write`, конфликты возвращаются как значение. Конфликт выявляется до любой
/// записи, поэтому фиксация такой транзакции ничего не меняет.
async fn write_note(
    db: &Db,
    f: impl FnOnce(&Connection) -> Result<Node, WriteError> + Send + 'static,
) -> Result<Node, WriteError> {
    db.write(move |tx| match f(tx) {
        Ok(n) => Ok(Ok(n)),
        Err(WriteError::Store(e)) => Err(e),
        Err(e) => Ok(Err(e)),
    })
    .await?
}

/// Живая заметка: нет или удалена → `NotFound`, папка → `BadInput`.
fn live_note(c: &Connection, id: Id) -> Result<Node, StoreError> {
    let node = load_node(c, id)?;
    if node.deleted_at.is_some() {
        return Err(StoreError::NotFound);
    }
    if node.kind != NodeKind::Note {
        return Err(StoreError::BadInput("узел — папка, а не заметка".into()));
    }
    Ok(node)
}

fn rev_mismatch(c: &Connection, node: &Node) -> Result<WriteError, StoreError> {
    Ok(WriteError::RevMismatch {
        current_rev: node.current_rev,
        current: revision_text(c, node.id, node.current_rev)?,
    })
}

/// Путь секции операции; `None` у `Append` и `Prepend`.
fn op_section(op: &PatchOp) -> Option<&str> {
    match op {
        PatchOp::ReplaceSection { section, .. }
        | PatchOp::AppendToSection { section, .. }
        | PatchOp::InsertAfterHeading { section, .. } => Some(section),
        PatchOp::Append { .. } | PatchOp::Prepend { .. } => None,
    }
}

/// Записывает `content` новой ревизией заметки `node` (если текст изменился).
fn commit_text(
    tx: &Connection,
    node: Node,
    content: &str,
    agent_l0: Option<&str>,
    a: &Attribution,
) -> Result<Node, StoreError> {
    let hash = sha256_hex(content.as_bytes());
    if node.content_hash.as_deref() == Some(hash.as_str()) {
        return Ok(node);
    }
    let now = fmt_ts(Timestamp::now());
    let meta = note_meta(content, agent_l0);
    let rev = node.current_rev + 1;
    tx.execute(
        "UPDATE nodes SET current_rev = ?2, content_hash = ?3, frontmatter = ?4, l0 = ?5,
                          l0_source = ?6, index_state = ?7, updated_at = ?8
         WHERE id = ?1",
        params![
            node.id,
            rev,
            hash,
            meta.frontmatter.as_ref().map(|v| v.to_string()),
            meta.l0,
            meta.l0_source,
            index_state(&meta),
            now,
        ],
    )?;
    store_revision(tx, node.id, rev, content, &hash, &meta, a, &now)?;
    load_node(tx, node.id)
}
