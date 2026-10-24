//! Узлы пространства (папки и заметки), ревизии, синхронная индексация FTS и outbox
//! векторной индексации (§6.3, §8.1 ТЗ).
//!
//! Структура модуля:
//! - `mod.rs` — типы и отображение строк `nodes`;
//! - [`write`] — запись: создание папок и заметок в одной транзакции писателя;
//! - [`revise`] — правка заметки: замена с проверкой ревизии, PATCH секций, откат;
//! - [`read`] — чтение: узел, содержимое ревизии, дети, поиск по имени;
//! - [`history`] — список ревизий и унифицированный дифф;
//! - [`tree_ops`] — корзина (мягкое удаление поддерева, восстановление) и перенос узла;
//! - [`chunks`] — метаданные заметки (frontmatter, L0), чанки §8.2 с FTS и задания
//!   индексации (outbox, §8.1) в той же транзакции.

mod chunks;
mod history;
mod l0;
mod read;
mod revise;
mod tree_ops;
mod write;

use std::fmt::Write as _;

use jiff::Timestamp;
use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, Row};
use serde::Serialize;
use sha2::{Digest, Sha256};
use zam_core::Id;

use crate::StoreError;
use crate::paths::NodePath;
use crate::time::{conv_err, ts_at, ts_opt};

pub use chunks::{
    PREFIX_L0_MAX_TOKENS, chunk_params, count_tokens, prefix_l0, set_chunk_params, set_prefix_l0,
    set_token_counter, token_counter_exact,
};
pub(crate) use chunks::{
    backfill_certainty, backfill_embed_keys, chunk_id, chunker_fingerprint, context_prefix,
    frontmatter_tags, mask_tags, note_chunks, rechunk_notes, rescan_secrets, stored_prefix_hit,
    token_counter,
};
pub use history::{RevisionInfo, list_revisions, unified_diff};
pub use l0::{set_auto_l0, set_folder_l0};
pub use read::{find_by_name, get_node, get_node_by_path, list_children, node_content};
pub use revise::{WriteError, patch_note, patch_note_within, revert_note, update_note};
pub use tree_ops::{delete_node, list_trash, move_node, restore_node};
pub use write::{create_folder, create_note};

/// Вид узла.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    // порядок вариантов задаёт порядок выдачи: папки, затем заметки
    Folder,
    Note,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeKind::Folder => "folder",
            NodeKind::Note => "note",
        }
    }
}

impl ToSql for NodeKind {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl FromSql for NodeKind {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        match v.as_str()? {
            "folder" => Ok(NodeKind::Folder),
            "note" => Ok(NodeKind::Note),
            other => Err(rusqlite::types::FromSqlError::Other(
                format!("неизвестный вид узла: {other}").into(),
            )),
        }
    }
}

/// Кто автор ревизии.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthorKind {
    Human,
    Agent,
    System,
}

impl AuthorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthorKind::Human => "human",
            AuthorKind::Agent => "agent",
            AuthorKind::System => "system",
        }
    }
}

impl ToSql for AuthorKind {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl FromSql for AuthorKind {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        match v.as_str()? {
            "human" => Ok(AuthorKind::Human),
            "agent" => Ok(AuthorKind::Agent),
            "system" => Ok(AuthorKind::System),
            other => Err(rusqlite::types::FromSqlError::Other(
                format!("неизвестный автор: {other}").into(),
            )),
        }
    }
}

/// Атрибуция ревизии.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    pub author_kind: AuthorKind,
    pub token_id: Option<Id>,
    pub session_id: Option<String>,
    pub machine: Option<String>,
    pub message: Option<String>,
    /// Пользователь-автор: пользователь токена записи (владелец базы или участник общего решения).
    /// `None` — запись без токена (администратор, система) и ревизии до M3.
    pub user_id: Option<Id>,
    /// Область по умолчанию (`X-Zam-Area`, §11.2) для новых узлов и записей журналов.
    pub area: Option<String>,
}

impl Attribution {
    /// Атрибуция без токена, сессии и сообщения.
    pub fn new(author_kind: AuthorKind) -> Attribution {
        Attribution {
            author_kind,
            token_id: None,
            session_id: None,
            machine: None,
            message: None,
            user_id: None,
            area: None,
        }
    }
}

/// Узел дерева пространства.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Node {
    pub id: Id,
    pub space_id: Id,
    pub parent_id: Option<Id>,
    pub kind: NodeKind,
    pub name: String,
    pub path: String,
    /// 0 у папки.
    pub current_rev: i64,
    /// sha256 hex текущей ревизии; `None` у папки.
    pub content_hash: Option<String>,
    /// `{"tags", "description", "l0", "index"}` из frontmatter, без пустых полей.
    pub frontmatter: Option<serde_json::Value>,
    pub l0: Option<String>,
    /// `auto` | `manual` | `agent`.
    pub l0_source: Option<String>,
    /// `pending` | `indexed` | `failed` | `excluded` (векторная индексация, §8.1).
    pub index_state: String,
    #[serde(serialize_with = "crate::time::ser_ts")]
    pub created_at: Timestamp,
    #[serde(serialize_with = "crate::time::ser_ts")]
    pub updated_at: Timestamp,
    #[serde(serialize_with = "crate::time::ser_ts_opt")]
    pub deleted_at: Option<Timestamp>,
    /// Есть живой дочерний узел.
    pub has_children: bool,
    /// Эпистемический статус заметки (`certainty:` во frontmatter текущей ревизии, §7.5).
    pub certainty: Option<crate::entries::Certainty>,
}

/// Новая заметка.
#[derive(Debug, Clone)]
pub struct NewNote {
    pub space_id: Id,
    pub path: NodePath,
    pub content: String,
    /// Сводка от агента (`l0_source = agent`).
    pub l0: Option<String>,
    pub attrib: Attribution,
}

/// Колонки узла для [`node_row`]; таблица `nodes` под псевдонимом `n`.
/// `has_children` ограничен `space_id`, чтобы подзапрос шёл по индексу `nodes_parent`.
pub(crate) const NODE_COLS: &str = "n.id, n.space_id, n.parent_id, n.kind, n.name, n.path,
    n.current_rev, n.content_hash, n.frontmatter, n.l0, n.l0_source,
    n.created_at, n.updated_at, n.deleted_at, n.index_state,
    EXISTS (SELECT 1 FROM nodes c
            WHERE c.space_id = n.space_id AND c.parent_id = n.id AND c.deleted_at IS NULL),
    n.certainty";

pub(crate) fn node_row(r: &Row) -> rusqlite::Result<Node> {
    let frontmatter = r
        .get::<_, Option<String>>(8)?
        .map(|s| serde_json::from_str(&s).map_err(|e| conv_err(8, e)))
        .transpose()?;
    Ok(Node {
        id: r.get(0)?,
        space_id: r.get(1)?,
        parent_id: r.get(2)?,
        kind: r.get(3)?,
        name: r.get(4)?,
        path: r.get(5)?,
        current_rev: r.get(6)?,
        content_hash: r.get(7)?,
        frontmatter,
        l0: r.get(9)?,
        l0_source: r.get(10)?,
        created_at: ts_at(r, 11)?,
        updated_at: ts_at(r, 12)?,
        deleted_at: ts_opt(r, 13)?,
        index_state: r.get(14)?,
        has_children: r.get(15)?,
        certainty: r.get(16)?,
    })
}

/// Узел по id (в том числе удалённый).
pub(crate) fn load_node(c: &Connection, id: Id) -> Result<Node, StoreError> {
    c.query_row(
        &format!("SELECT {NODE_COLS} FROM nodes n WHERE n.id = ?1"),
        [id],
        node_row,
    )
    .optional()?
    .ok_or(StoreError::NotFound)
}

/// Живой узел по пути.
pub(crate) fn live_by_path(
    c: &Connection,
    space_id: Id,
    path: &str,
) -> Result<Option<Node>, StoreError> {
    Ok(c.query_row(
        &format!(
            "SELECT {NODE_COLS} FROM nodes n
             WHERE n.space_id = ?1 AND n.path = ?2 AND n.deleted_at IS NULL"
        ),
        rusqlite::params![space_id, path],
        node_row,
    )
    .optional()?)
}

/// Текст ревизии `rev` узла `node_id`. Нет такой ревизии → `NotFound`.
pub(crate) fn revision_text(c: &Connection, node_id: Id, rev: i64) -> Result<String, StoreError> {
    c.query_row(
        "SELECT content FROM revisions WHERE node_id = ?1 AND rev = ?2",
        rusqlite::params![node_id, rev],
        |r| r.get(0),
    )
    .optional()?
    .ok_or(StoreError::NotFound)
}

/// sha256 hex в нижнем регистре.
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            write!(s, "{b:02x}").expect("запись в String");
            s
        })
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::NODE_COLS;
    use super::read::children_sql;

    fn schema() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        for m in crate::USER_MIGRATIONS {
            c.execute_batch(m.sql).unwrap();
        }
        c
    }

    fn plan(c: &Connection, sql: &str, params: usize) -> String {
        let mut st = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let args = rusqlite::params_from_iter(vec!["x"; params]);
        let rows = st.query_map(args, |r| r.get::<_, String>(3)).unwrap();
        rows.map(Result::unwrap).collect::<Vec<_>>().join(
            "
",
        )
    }

    #[test]
    fn has_children_uses_parent_index() {
        let c = schema();
        let sql = format!("SELECT {NODE_COLS} FROM nodes n WHERE n.id = ?1");
        let p = plan(&c, &sql, 1);
        assert!(p.contains("nodes_parent"), "{p}");
        assert!(!p.contains("nodes_deleted"), "{p}");
    }

    #[test]
    fn children_cte_uses_parent_index() {
        let c = schema();
        let p = plan(&c, &children_sql(), 3);
        let parent_uses = p.matches("nodes_parent").count();
        // якорь, шаг рекурсии и has_children
        assert!(parent_uses >= 3, "{p}");
        assert!(!p.contains("nodes_deleted"), "{p}");
    }
}
