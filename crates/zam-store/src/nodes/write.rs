//! Запись узлов. Каждая операция — одна транзакция писателя: ревизия, узел,
//! разбор, чанки, FTS и задание outbox (`index_jobs`) фиксируются вместе или не
//! фиксируются вовсе (§8.1 ТЗ).

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use zam_core::Id;

use super::chunks::{NoteMeta, index_note, index_state, note_meta};
use super::{Attribution, NewNote, Node, NodeKind, live_by_path, load_node, sha256_hex};
use crate::certainty::{CertaintyChange, check_certainty};
use crate::paths::NodePath;
use crate::time::fmt_ts;
use crate::user_db::SpaceKind;
use crate::{Db, StoreError};

/// Создаёт заметку и недостающие папки-предки (mkdir -p). Область заметки (`nodes.area`)
/// — `attrib.area` (`X-Zam-Area`).
/// Путь занят живым узлом или предок — заметка → `Conflict`; нет пространства → `NotFound`;
/// неизвестный `certainty:` во frontmatter → `BadInput`; статус против матрицы прав
/// (`theorem` агента без `evidence:`) → `Certainty`.
pub async fn create_note(db: &Db, n: NewNote) -> Result<Node, StoreError> {
    db.write(move |tx| insert_note(tx, &n)).await
}

/// [`create_note`] внутри транзакции писателя (превращение выжимки кэша в заметку, §9.4).
pub(crate) fn insert_note(tx: &Connection, n: &NewNote) -> Result<Node, StoreError> {
    let now = fmt_ts(Timestamp::now());
    let kind = ensure_space(tx, n.space_id)?;
    ensure_free(tx, n.space_id, &n.path)?;
    let parent = ensure_ancestors(tx, n.space_id, &n.path, &now)?;

    let meta = note_meta(&n.content, n.l0.as_deref());
    let certainty = meta.certainty()?;
    check_certainty(
        n.attrib.author_kind,
        None,
        &CertaintyChange::Create(certainty),
        meta.evidence.as_ref(),
        None,
    )?;
    let id = Id::new();
    let hash = sha256_hex(n.content.as_bytes());
    tx.execute(
        "INSERT INTO nodes (id, space_id, parent_id, kind, name, path, current_rev,
                            content_hash, frontmatter, l0, l0_source, index_state,
                            created_at, updated_at, area, certainty)
         VALUES (?1, ?2, ?3, 'note', ?4, ?5, 1, ?6, ?7, ?8, ?9, ?10, ?11, ?11, ?12, ?13)",
        params![
            id,
            n.space_id,
            parent,
            n.path.name(),
            n.path.as_str(),
            hash,
            meta.frontmatter.as_ref().map(|v| v.to_string()),
            meta.l0,
            meta.l0_source,
            index_state(&meta, kind),
            now,
            // область по умолчанию — из `X-Zam-Area` запроса создания
            n.attrib.area,
            certainty,
        ],
    )?;
    store_revision(tx, id, 1, &n.content, &hash, &meta, &n.attrib, None, &now)?;
    // ожидавшие эту заметку ссылки пространства разрешаются
    crate::graph::tree_changed(tx, n.space_id, &[id], &[])?;
    load_node(tx, id)
}

/// Создаёт папку и недостающих предков. Путь занят живым узлом → `Conflict`.
pub async fn create_folder(db: &Db, space_id: Id, path: NodePath) -> Result<Node, StoreError> {
    db.write(move |tx| {
        let now = fmt_ts(Timestamp::now());
        ensure_space(tx, space_id)?;
        ensure_free(tx, space_id, &path)?;
        let parent = ensure_ancestors(tx, space_id, &path, &now)?;
        let id = insert_folder(tx, space_id, parent, &path, &now)?;
        load_node(tx, id)
    })
    .await
}

/// Вид пространства; нет пространства → `NotFound`.
fn ensure_space(c: &Connection, space_id: Id) -> Result<SpaceKind, StoreError> {
    c.query_row("SELECT kind FROM spaces WHERE id = ?1", [space_id], |r| {
        r.get(0)
    })
    .optional()?
    .ok_or(StoreError::NotFound)
}

pub(crate) fn ensure_free(c: &Connection, space_id: Id, path: &NodePath) -> Result<(), StoreError> {
    match live_by_path(c, space_id, path.as_str())? {
        Some(_) => Err(StoreError::Conflict(format!("путь «{path}» уже занят"))),
        None => Ok(()),
    }
}

/// Находит или создаёт папки-предки; возвращает id непосредственного родителя.
pub(crate) fn ensure_ancestors(
    c: &Connection,
    space_id: Id,
    path: &NodePath,
    now: &str,
) -> Result<Option<Id>, StoreError> {
    let mut parent = None;
    for anc in path.ancestors() {
        parent = Some(match live_by_path(c, space_id, anc)? {
            Some(n) if n.kind == NodeKind::Folder => n.id,
            Some(_) => {
                return Err(StoreError::Conflict(format!(
                    "предок «{anc}» — заметка, а не папка"
                )));
            }
            None => {
                let p = NodePath::parse(anc).expect("префикс проверенного пути корректен");
                insert_folder(c, space_id, parent, &p, now)?
            }
        });
    }
    Ok(parent)
}

fn insert_folder(
    c: &Connection,
    space_id: Id,
    parent: Option<Id>,
    path: &NodePath,
    now: &str,
) -> Result<Id, StoreError> {
    let id = Id::new();
    c.execute(
        "INSERT INTO nodes (id, space_id, parent_id, kind, name, path, current_rev,
                            index_state, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'folder', ?4, ?5, 0, 'excluded', ?6, ?6)",
        params![id, space_id, parent, path.name(), path.as_str(), now],
    )?;
    Ok(id)
}

/// Общая часть создания и правки заметки: дописывает ревизию `rev`, пересобирает
/// чанки и FTS по её тексту, ссылки на файлы и ставит задание индексации. Строка `nodes` к этому
/// моменту уже указывает на `rev` и новый `index_state`; `prev_state` — прежний
/// `index_state` (`None` у новой заметки).
#[allow(clippy::too_many_arguments)]
pub(crate) fn store_revision(
    c: &Connection,
    node_id: Id,
    rev: i64,
    content: &str,
    hash: &str,
    meta: &NoteMeta,
    a: &Attribution,
    prev_state: Option<&str>,
    now: &str,
) -> Result<(), StoreError> {
    insert_revision(c, node_id, rev, content, hash, a, now)?;
    index_note(c, node_id, rev, content, meta, prev_state, now)?;
    // ссылки на файлы и `verified_at` из frontmatter ревизии (M4.5 §6.1, §7)
    crate::file_refs::sync_note(c, node_id, meta.doc.frontmatter.as_ref(), a, now)?;
    Ok(())
}

/// Дописывает ревизию (ревизии только дописываются).
fn insert_revision(
    c: &Connection,
    node_id: Id,
    rev: i64,
    content: &str,
    hash: &str,
    a: &Attribution,
    now: &str,
) -> Result<(), StoreError> {
    c.execute(
        "INSERT INTO revisions (node_id, rev, content, content_hash, size, author_kind,
                                token_id, session_id, machine, message, ts, user_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            node_id,
            rev,
            content,
            hash,
            content.len() as i64,
            a.author_kind,
            a.token_id,
            a.session_id,
            a.machine,
            a.message,
            now,
            a.user_id,
        ],
    )?;
    Ok(())
}
