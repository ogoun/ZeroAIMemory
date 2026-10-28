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

pub(crate) use chunks::{
    ChunkInsert, PARAM_ALGORITHM, PARAM_TOKENIZER, backfill_certainty, backfill_embed_keys,
    changed_params, chunk_id, chunker_fingerprint, chunker_params, context_prefix,
    entry_chunker_fingerprint, frontmatter_tags, mask_tags, note_chunks, param_order,
    params_from_fingerprint, rechunk_notes, replace_chunks, rescan_secrets, same_chunker,
    stored_prefix_hit, token_counter,
};
pub use chunks::{
    PREFIX_L0_MAX_TOKENS, chunk_params, count_tokens, prefix_l0, set_chunk_params, set_prefix_l0,
    set_token_counter, token_counter_exact,
};
pub use history::{RevisionInfo, list_revisions, unified_diff};
pub use l0::{set_auto_l0, set_folder_l0};
pub use read::{find_by_name, get_node, get_node_by_path, list_children, node_content};
pub use revise::{WriteError, patch_note, patch_note_within, revert_note, update_note};
pub use tree_ops::{delete_node, list_trash, move_node, restore_node};
pub(crate) use write::insert_note;
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
/// `has_children` ограничен `space_id`, чтобы подзапрос шёл по частичному индексу
/// `nodes_live_children` (миграция 0012).
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
    use super::tree_ops::{DESCENDANTS_SQL, SUBTREE_SQL, SUBTREE_STAMP_SQL};

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
        assert!(p.contains("nodes_live_children"), "{p}");
        assert!(!p.contains("nodes_deleted"), "{p}");
    }

    #[test]
    fn children_cte_uses_parent_index() {
        let c = schema();
        let p = plan(&c, &children_sql(), 3);
        let parent_uses = p.matches("nodes_live_children").count();
        // якорь, шаг рекурсии и has_children
        assert!(parent_uses >= 3, "{p}");
        assert!(!p.contains("nodes_deleted"), "{p}");
    }

    /// Обходы дерева идут от очереди `t` к детям по полному ключу `(space_id, parent_id)`, а
    /// итоговая выборка — по `t` к строке узла (задача 19b M3): без полного просмотра узлов
    /// пространства на каждом шаге рекурсии и без `SCAN` таблицы `nodes`.
    #[test]
    fn tree_walks_search_children_by_full_key() {
        let c = schema();
        for (name, sql, n) in [
            ("children", children_sql(), 3),
            ("subtree", SUBTREE_SQL.to_string(), 2),
            ("subtree stamp", SUBTREE_STAMP_SQL.to_string(), 3),
            ("descendants", DESCENDANTS_SQL.to_string(), 2),
        ] {
            let p = plan(&c, &sql, n);
            // позитивная проверка: шаг рекурсии ищет детей по полному ключу; если строки
            // нет (сменился формат плана или индекс), тест падает, а не проходит пусто
            let step = recursive_step(&c, &sql, n);
            assert!(
                step.iter().any(|l| l.starts_with("SEARCH c USING INDEX")
                    && l.ends_with("(space_id=? AND parent_id=?)")),
                "{name}: шаг рекурсии не по (space_id, parent_id): {step:?}
{p}"
            );
            for line in p.lines() {
                let l = line.trim();
                assert!(
                    !(l.starts_with("SEARCH c") && l.ends_with("(space_id=?)")),
                    "{name}: шаг рекурсии по одному space_id
{p}"
                );
                assert!(
                    !(l.starts_with("SCAN n")
                        || l.starts_with("SCAN c")
                        || l.starts_with("SCAN nodes")),
                    "{name}: просмотр nodes
{p}"
                );
            }
        }
    }

    /// Строки плана шага рекурсии (дети узла `RECURSIVE STEP` по `parent` плана).
    fn recursive_step(c: &Connection, sql: &str, params: usize) -> Vec<String> {
        let mut st = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let args = rusqlite::params_from_iter(vec!["x"; params]);
        let rows: Vec<(i64, i64, String)> = st
            .query_map(args, |r| Ok((r.get(0)?, r.get(1)?, r.get(3)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let Some(step) = rows.iter().find(|r| r.2 == "RECURSIVE STEP").map(|r| r.0) else {
            return Vec::new();
        };
        rows.into_iter()
            .filter(|r| r.1 == step)
            .map(|r| r.2)
            .collect()
    }

    fn ins(
        c: &Connection,
        space: &str,
        parent: Option<&str>,
        kind: &str,
        path: &str,
        del: bool,
    ) -> String {
        ins_at(c, space, parent, kind, path, del.then_some("t"))
    }

    /// Узел с меткой удаления `deleted_at` (`None` — живой).
    fn ins_at(
        c: &Connection,
        space: &str,
        parent: Option<&str>,
        kind: &str,
        path: &str,
        deleted_at: Option<&str>,
    ) -> String {
        let id = zam_core::Id::new().to_string();
        let name = path.rsplit('/').next().unwrap();
        c.execute(
            "INSERT INTO nodes (id, space_id, parent_id, kind, name, path, index_state,
                                created_at, updated_at, deleted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', 't', 't', ?7)",
            rusqlite::params![id, space, parent, kind, name, path, deleted_at],
        )
        .unwrap();
        id
    }

    /// Шагов VM SQLite на полный проход запроса `sql` и число строк.
    fn steps(c: &Connection, sql: &str, args: &[&dyn rusqlite::ToSql]) -> (usize, i32) {
        let mut st = c.prepare(sql).unwrap();
        let mut rows = st.query(args).unwrap();
        let mut n = 0;
        while rows.next().unwrap().is_some() {
            n += 1;
        }
        drop(rows);
        (n, st.get_status(rusqlite::StatementStatus::VmStep))
    }

    /// Работа обходов живого дерева не зависит от размера корзины (задача 19b M3): база
    /// с `trash` удалёнными заметками — половина в соседнем удалённом поддереве, половина
    /// прямо среди детей обходимой папки (вставлены раньше живых — так `EXISTS` по
    /// `nodes_parent` шёл бы через них).
    fn walk_steps(trash: usize) -> Vec<(&'static str, usize, i32)> {
        let c = schema();
        c.execute(
            "INSERT INTO spaces (id, kind, name, created_at) VALUES ('s', 'knowledge', 'k', 't')",
            [],
        )
        .unwrap();
        let live = ins(&c, "s", None, "folder", "live", false);
        let f = ins(&c, "s", Some(&live), "folder", "live/f", false);
        let gone = ins(&c, "s", None, "folder", "gone", true);
        for i in 0..trash / 2 {
            ins(&c, "s", Some(&gone), "note", &format!("gone/n{i}"), true);
            ins(&c, "s", Some(&f), "note", &format!("live/f/d{i}"), true);
        }
        for i in 0..3 {
            ins(&c, "s", Some(&f), "note", &format!("live/f/n{i}"), false);
        }
        let other = ins(&c, "s", None, "folder", "other", false);
        ins(&c, "s", Some(&other), "note", "other/n", false);
        let mut out = Vec::new();
        let (n, s) = steps(&c, &children_sql(), &[&"s", &live, &3]);
        out.push(("children live", n, s));
        let (n, s) = steps(&c, &children_sql(), &[&"s", &f, &1]);
        out.push(("children f", n, s));
        let (n, s) = steps(&c, SUBTREE_SQL, &[&"s", &live]);
        out.push(("subtree live", n, s));
        let (n, s) = steps(&c, DESCENDANTS_SQL, &[&"s", &other]);
        out.push(("descendants other", n, s));
        out
    }

    /// Шаги VM обхода восстановления (`SUBTREE_STAMP_SQL`): папка с `pack` заметками,
    /// удалёнными одной меткой, и `other` удалённых другой меткой вне её.
    fn restore_steps(pack: usize, other: usize) -> (usize, i32) {
        let c = schema();
        c.execute(
            "INSERT INTO spaces (id, kind, name, created_at) VALUES ('s', 'knowledge', 'k', 't')",
            [],
        )
        .unwrap();
        let root = ins_at(&c, "s", None, "folder", "pack", Some("p"));
        let gone = ins_at(&c, "s", None, "folder", "gone", Some("q"));
        for i in 0..pack {
            ins_at(
                &c,
                "s",
                Some(&root),
                "note",
                &format!("pack/n{i}"),
                Some("p"),
            );
        }
        for i in 0..other {
            ins_at(
                &c,
                "s",
                Some(&gone),
                "note",
                &format!("gone/n{i}"),
                Some("q"),
            );
        }
        steps(&c, SUBTREE_STAMP_SQL, &[&"s", &root, &"p"])
    }

    /// Восстановление пакета линейно по его размеру и не зависит от чужой корзины (fix round 1
    /// задачи 19b): шаг рекурсии по `nodes_deleted (deleted_at=?)` перебирал для каждой строки
    /// очереди весь пакет — K².
    #[test]
    fn restore_walk_is_linear_in_pack() {
        let (n1, s1) = restore_steps(1000, 0);
        let (n4, s4) = restore_steps(4000, 0);
        let (n0, s0) = restore_steps(100, 0);
        let (nq, sq) = restore_steps(100, 4000);
        eprintln!(
            "restore: шагов VM {s1} (1000), {s4} (4000); {s0} → {sq} (чужая корзина 0 → 4000)"
        );
        assert_eq!((n1, n4, n0, nq), (1001, 4001, 101, 101));
        // линейный рост — 4×, квадратичный — 16×
        assert!(s4 <= s1 * 5, "пакет 1000 → 4000: шагов {s1} → {s4}");
        assert!(sq <= s0 + 64, "чужая корзина 0 → 4000: шагов {s0} → {sq}");
    }

    #[test]
    fn tree_walks_do_not_grow_with_trash() {
        let base = walk_steps(0);
        let big = walk_steps(4000);
        for (b, t) in base.iter().zip(&big) {
            eprintln!("{}: шагов VM {} → {} (корзина 0 → 4000)", b.0, b.2, t.2);
            assert_eq!(b.1, t.1, "{}: строк", b.0);
            // запас — на разницу глубины B-дерева индекса
            assert!(
                t.2 <= b.2 + 64,
                "{}: шагов {} при пустой корзине, {} при 4000",
                b.0,
                b.2,
                t.2
            );
        }
    }
}
