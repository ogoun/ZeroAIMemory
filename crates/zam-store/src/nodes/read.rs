//! Чтение узлов: по id и пути, содержимое ревизии, дети, поиск по имени.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};
use zam_core::Id;

use super::{NODE_COLS, Node, live_by_path, load_node, node_row, revision_text};
use crate::paths::NodePath;
use crate::{Db, StoreError};

/// Узел по id, в том числе удалённый (`deleted_at` заполнен). Нет → `NotFound`.
pub async fn get_node(db: &Db, id: Id) -> Result<Node, StoreError> {
    db.read(move |c| load_node(c, id)).await
}

/// Живой узел по пути. Нет → `NotFound`.
pub async fn get_node_by_path(db: &Db, space_id: Id, path: &NodePath) -> Result<Node, StoreError> {
    let path = path.as_str().to_string();
    db.read(move |c| live_by_path(c, space_id, &path)?.ok_or(StoreError::NotFound))
        .await
}

/// Текст ревизии `rev` (по умолчанию текущей): `(rev, content)`.
/// Нет узла или ревизии (у папки ревизий нет) → `NotFound`.
pub async fn node_content(db: &Db, id: Id, rev: Option<i64>) -> Result<(i64, String), StoreError> {
    db.read(move |c| {
        let rev = match rev {
            Some(r) => r,
            None => c
                .query_row("SELECT current_rev FROM nodes WHERE id = ?1", [id], |r| {
                    r.get(0)
                })
                .optional()?
                .ok_or(StoreError::NotFound)?,
        };
        Ok((rev, revision_text(c, id, rev)?))
    })
    .await
}

/// Живые дети `parent` (`None` — корень пространства): папки, затем заметки, по имени.
/// `depth > 1` — плоский список в порядке обхода в глубину (pre-order) не глубже
/// `depth` уровней; связь восстанавливается по `parent_id`. `depth = 0` трактуется как 1.
pub async fn list_children(
    db: &Db,
    space_id: Id,
    parent: Option<Id>,
    depth: u32,
) -> Result<Vec<Node>, StoreError> {
    let depth = depth.max(1);
    db.read(move |c| {
        // один запрос — один снимок данных для всего поддерева
        let mut st = c.prepare(&children_sql())?;
        let rows = st.query_map(params![space_id, parent, depth], node_row)?;
        let mut by_parent: HashMap<Option<Id>, Vec<Node>> = HashMap::new();
        for n in rows {
            let n = n?;
            by_parent.entry(n.parent_id).or_default().push(n);
        }
        for v in by_parent.values_mut() {
            v.sort_by(|a, b| (a.kind, &a.name, a.id).cmp(&(b.kind, &b.name, b.id)));
        }
        let mut out = Vec::new();
        preorder(&mut by_parent, parent, &mut out);
        Ok(out)
    })
    .await
}

/// Запрос поддерева для [`list_children`]: `?1` — пространство, `?2` — родитель,
/// `?3` — глубина. Шаг рекурсии ограничен `space_id`, чтобы работал индекс `nodes_parent`.
pub(crate) fn children_sql() -> String {
    format!(
        "WITH RECURSIVE t (id, lvl) AS (
             SELECT id, 1 FROM nodes
             WHERE space_id = ?1 AND parent_id IS ?2 AND deleted_at IS NULL
             UNION ALL
             SELECT c.id, t.lvl + 1 FROM nodes c JOIN t ON c.parent_id = t.id
             WHERE c.space_id = ?1 AND c.deleted_at IS NULL AND t.lvl < ?3
         )
         SELECT {NODE_COLS} FROM nodes n JOIN t ON n.id = t.id"
    )
}

fn preorder(
    by_parent: &mut HashMap<Option<Id>, Vec<Node>>,
    parent: Option<Id>,
    out: &mut Vec<Node>,
) {
    let Some(children) = by_parent.remove(&parent) else {
        return;
    };
    for n in children {
        let id = n.id;
        out.push(n);
        preorder(by_parent, Some(id), out);
    }
}

/// Живые заметки с точно таким именем, по пути (для разрешения вики-ссылок).
pub async fn find_by_name(db: &Db, space_id: Id, name: &str) -> Result<Vec<Node>, StoreError> {
    let name = name.to_string();
    db.read(move |c| {
        let mut st = c.prepare(&format!(
            "SELECT {NODE_COLS} FROM nodes n
             WHERE n.space_id = ?1 AND n.name = ?2 AND n.kind = 'note' AND n.deleted_at IS NULL
             ORDER BY n.path"
        ))?;
        let rows = st.query_map(params![space_id, name], node_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}
