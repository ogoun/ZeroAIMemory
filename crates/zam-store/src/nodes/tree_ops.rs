//! Операции над поддеревом: мягкое удаление в корзину, восстановление, список
//! корзины и перенос (переименование) узла (§6.3, §11 ТЗ).
//!
//! Удаление помечает узел и всех его живых потомков одной меткой `deleted_at`:
//! метка задаёт «пакет удаления». Восстановление возвращает ровно этот пакет;
//! потомки, удалённые раньше отдельно, остаются в корзине. Поддерево строится по
//! `parent_id` (индексы `nodes_live_children` и `nodes_parent`), а не по префиксу пути:
//! удалённые узлы могут делить путь с живыми. Каждая операция — одна транзакция писателя и ставит
//! задания outbox (§8.1): удаление — `delete`, восстановление и перенос — `upsert`. В той же
//! транзакции перепроверяются выведенные рёбра графа пространства (`crate::graph::tree_changed`).

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use zam_core::{Id, TargetKind, TargetRef};

use super::chunks::{
    frontmatter_tags, index_note, index_state, note_meta, note_prefix, space_kind,
};
use super::write::{ensure_ancestors, ensure_free};
use super::{
    AuthorKind, NODE_COLS, Node, NodeKind, live_by_path, load_node, node_row, revision_text,
};
use crate::cache::{StaleKind, invalidate_target};
use crate::certainty::{CertaintyChange, check_certainty};
use crate::embeddings::embed_key_of;
use crate::entries::Certainty;
use crate::jobs::{JobOp, PRIORITY_WRITE, enqueue};
use crate::paths::NodePath;
use crate::time::fmt_ts;
use crate::{Db, StoreError};

/// Мягко удаляет узел и всех его живых потомков одной меткой времени; их чанки
/// удаляются (триггеры убирают их из FTS), заметкам, которые могли попасть в векторный
/// индекс (не `excluded`), ставится `delete`. Возвращает число помеченных узлов.
/// Нет узла или он уже удалён → `NotFound`; права `certainty` поддерева — по матрице (§7.5;
/// с M4 заметки `axiom` удаляет любой автор).
pub async fn delete_node(db: &Db, id: Id, author: AuthorKind) -> Result<u64, StoreError> {
    db.write(move |tx| {
        let node = load_node(tx, id)?;
        if node.deleted_at.is_some() {
            return Err(StoreError::NotFound);
        }
        let now = fmt_ts(Timestamp::now());
        let ids = subtree(tx, &node, None)?;
        ensure_may_modify_all(tx, &ids, author)?;
        let mut mark = tx.prepare_cached("UPDATE nodes SET deleted_at = ?2 WHERE id = ?1")?;
        let mut drop_chunks = tx.prepare_cached("DELETE FROM chunks WHERE node_id = ?1")?;
        let mut state = tx.prepare_cached(
            "SELECT current_rev, index_state FROM nodes WHERE id = ?1 AND kind = 'note'",
        )?;
        for &nid in &ids {
            mark.execute(params![nid, now])?;
            drop_chunks.execute([nid])?;
            // выжимки кэша с этим источником — недействительны (§9.4)
            invalidate_target(tx, node_ref(nid), StaleKind::Deleted)?;
            let note: Option<(i64, String)> = state
                .query_row([nid], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            if let Some((rev, st)) = note
                && st != "excluded"
            {
                enqueue(tx, nid, rev, JobOp::Delete, PRIORITY_WRITE, &now)?;
            }
        }
        // ссылки на удалённые заметки разрешаются заново (другая с тем же именем или битая);
        // рёбра самих удалённых остаются, но не видны, пока источник в корзине
        crate::graph::tree_changed(tx, node.space_id, &ids, &[])?;
        Ok(ids.len() as u64)
    })
    .await
}

/// Восстанавливает удалённый узел вместе с поддеревом, удалённым той же меткой
/// времени, и удалённых предков (каждого — только сам узел, без его поддерева).
/// Путь любого восстанавливаемого узла занят живым → `Conflict`, ничего не меняется.
/// Чанки заметок пересобираются из текущей ревизии, `index_state` пересчитывается,
/// индексируемым ставится `upsert` (заменяя `delete` от удаления).
/// Нет узла → `NotFound`; узел жив → `BadInput`; права `certainty` — по матрице (§7.5).
pub async fn restore_node(db: &Db, id: Id, author: AuthorKind) -> Result<Node, StoreError> {
    db.write(move |tx| {
        let node = load_node(tx, id)?;
        let Some(stamp) = raw_deleted_at(tx, id)? else {
            return Err(StoreError::BadInput("узел не удалён".into()));
        };
        let mut ids = deleted_ancestors(tx, &node)?;
        ids.extend(subtree(tx, &node, Some(&stamp))?);

        let mut restored = Vec::with_capacity(ids.len());
        for &nid in &ids {
            let n = load_node(tx, nid)?;
            if live_by_path(tx, n.space_id, &n.path)?.is_some() {
                return Err(StoreError::Conflict(format!("путь «{}» уже занят", n.path)));
            }
            restored.push(n);
        }
        let restored_ids: Vec<Id> = restored.iter().map(|n| n.id).collect();
        ensure_may_modify_all(tx, &restored_ids, author)?;
        let mut unmark = tx.prepare_cached("UPDATE nodes SET deleted_at = NULL WHERE id = ?1")?;
        for n in &restored {
            unmark.execute([n.id])?;
        }
        let now = fmt_ts(Timestamp::now());
        let mut set_state = tx.prepare_cached("UPDATE nodes SET index_state = ?2 WHERE id = ?1")?;
        for n in restored.iter().filter(|n| n.kind == NodeKind::Note) {
            let content = revision_text(tx, n.id, n.current_rev)?;
            let meta = note_meta(&content, None);
            let kind = space_kind(tx, n.space_id)?;
            set_state.execute(params![n.id, index_state(&meta, kind)])?;
            index_note(
                tx,
                n.id,
                n.current_rev,
                &content,
                &meta,
                Some(&n.index_state),
                &now,
            )?;
        }
        crate::graph::tree_changed(tx, node.space_id, &restored_ids, &[])?;
        load_node(tx, id)
    })
    .await
}

/// Корни пакетов удаления: удалённые узлы, родитель которых жив, отсутствует или
/// удалён другой меткой. Новые удаления первыми. `None` — все пространства.
pub async fn list_trash(db: &Db, space_id: Option<Id>) -> Result<Vec<Node>, StoreError> {
    db.read(move |c| {
        let mut st = c.prepare(&format!(
            "SELECT {NODE_COLS} FROM nodes n LEFT JOIN nodes p ON p.id = n.parent_id
             WHERE n.deleted_at IS NOT NULL
               AND (?1 IS NULL OR n.space_id = ?1)
               AND (p.id IS NULL OR p.deleted_at IS NULL OR p.deleted_at <> n.deleted_at)
             ORDER BY n.deleted_at DESC, n.path, n.id"
        ))?;
        let rows = st.query_map([space_id], node_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}

/// Переносит (переименовывает) живой узел в `new_path` того же пространства:
/// недостающие папки-предки создаются, пути потомков (в том числе удалённых —
/// чтобы восстановление вернуло их на место) переписываются заменой префикса,
/// `context_prefix` чанков узла и потомков пересчитывается, живым индексируемым
/// заметкам поддерева ставится `upsert` (в payload точек меняются путь и префикс).
/// Тот же путь → узел без изменений; перенос в собственное поддерево → `BadInput`;
/// путь занят или предок назначения — заметка → `Conflict`; нет узла или удалён → `NotFound`;
/// права `certainty` поддерева (в том числе удалённых потомков) — по матрице (§7.5).
pub async fn move_node(
    db: &Db,
    id: Id,
    new_path: NodePath,
    author: AuthorKind,
) -> Result<Node, StoreError> {
    db.write(move |tx| {
        let node = load_node(tx, id)?;
        if node.deleted_at.is_some() {
            return Err(StoreError::NotFound);
        }
        let old = node.path.as_str();
        let new = new_path.as_str();
        if new == old {
            return Ok(node);
        }
        if new
            .strip_prefix(old)
            .is_some_and(|rest| rest.starts_with('/'))
        {
            return Err(StoreError::BadInput(format!(
                "нельзя перенести «{old}» внутрь самого себя"
            )));
        }
        // права (403) — раньше занятости пути (409)
        let descendants = all_descendants(tx, node.space_id, id)?;
        let moved: Vec<Id> = std::iter::once(id)
            .chain(descendants.iter().map(|d| d.0))
            .collect();
        ensure_may_modify_all(tx, &moved, author)?;
        ensure_free(tx, node.space_id, &new_path)?;
        // прежние имена и пути: ссылки на них разрешаются заново
        let old_keys: Vec<(String, String)> =
            std::iter::once((node.name.clone(), node.path.clone()))
                .chain(descendants.iter().map(|(_, p)| {
                    let name = p.rsplit_once('/').map_or(p.as_str(), |(_, n)| n);
                    (name.to_string(), p.clone())
                }))
                .collect();
        let now = fmt_ts(Timestamp::now());
        let parent = ensure_ancestors(tx, node.space_id, &new_path, &now)?;

        tx.execute(
            "UPDATE nodes SET path = ?2, name = ?3, parent_id = ?4, updated_at = ?5
             WHERE id = ?1",
            params![id, new, new_path.name(), parent, now],
        )?;
        let mut set_path = tx.prepare_cached("UPDATE nodes SET path = ?2 WHERE id = ?1")?;
        for (nid, path) in &descendants {
            let rest = path
                .strip_prefix(old)
                .ok_or_else(|| StoreError::Corrupt(format!("путь потомка «{path}» вне «{old}»")))?;
            set_path.execute(params![nid, format!("{new}{rest}")])?;
        }

        let space: String = tx.query_row(
            "SELECT name FROM spaces WHERE id = ?1",
            [node.space_id],
            |r| r.get(0),
        )?;
        refresh_prefixes(tx, &space, id)?;
        requeue_moved(tx, id, &now)?;
        for (nid, _) in &descendants {
            refresh_prefixes(tx, &space, *nid)?;
            requeue_moved(tx, *nid, &now)?;
        }
        // граф: относительные ссылки перенесённых заметок — от нового места; ссылки на
        // старые и новые имена и пути пространства — заново
        for &nid in &moved {
            crate::graph::rebuild_moved(tx, nid, &now)?;
            // путь источника изменился: выжимка могла ссылаться на него (§9.4)
            invalidate_target(tx, node_ref(nid), StaleKind::Changed)?;
        }
        crate::graph::tree_changed(tx, node.space_id, &moved, &old_keys)?;
        load_node(tx, id)
    })
    .await
}

fn node_ref(id: Id) -> TargetRef {
    TargetRef {
        kind: TargetKind::Node,
        id,
    }
}

/// Операция над узлами `ids` (удаление, восстановление, перенос) — по матрице `certainty`
/// (§7.5; с M4 разрешена всем, в том числе у `axiom`).
fn ensure_may_modify_all(c: &Connection, ids: &[Id], author: AuthorKind) -> Result<(), StoreError> {
    if author == AuthorKind::Human {
        return Ok(());
    }
    let mut st = c.prepare_cached("SELECT certainty FROM nodes WHERE id = ?1")?;
    for &nid in ids {
        let cur: Option<Certainty> = st.query_row([nid], |r| r.get(0))?;
        check_certainty(author, cur, &CertaintyChange::ModifyOther, None, None)?;
    }
    Ok(())
}

/// Запрос [`subtree`] по живым потомкам: `?1` — пространство, `?2` — корень.
/// `CROSS JOIN` — от строки очереди к детям по `(space_id, parent_id)` частичного индекса
/// `nodes_live_children` (задача 19b M3, см. `children_sql`): при обычном `JOIN` каждый шаг
/// просматривал все узлы пространства вместе с корзиной.
pub(crate) const SUBTREE_SQL: &str = "WITH RECURSIVE t (id) AS (
             SELECT ?2
             UNION ALL
             SELECT c.id FROM t CROSS JOIN nodes c
             ON c.space_id = ?1 AND c.parent_id = t.id AND c.deleted_at IS NULL
         )
         SELECT id FROM t";

/// Запрос [`subtree`] по потомкам, удалённым меткой `?3` (восстановление; порядок
/// соединений — как в [`SUBTREE_SQL`]). Дети ищутся по `nodes_parent (space_id, parent_id)`,
/// метка — фильтр: унарный `+` снимает `deleted_at = ?3` с индекса. Без него планировщик
/// (статистики нет) брал частичный `nodes_deleted (deleted_at=?)` и для каждой строки
/// очереди перебирал весь пакет удаления — восстановление K узлов стоило K² шагов
/// (fix round 1 задачи 19b M3). `+`, а не `INDEXED BY nodes_parent`: план остаётся за
/// планировщиком (подходящих индексов у оставшихся условий нет, кроме `nodes_parent`), а
/// переименование индекса не превращается в ошибку запроса; план закреплён тестом
/// `tree_walks_search_children_by_full_key`.
pub(crate) const SUBTREE_STAMP_SQL: &str = "WITH RECURSIVE t (id) AS (
             SELECT ?2
             UNION ALL
             SELECT c.id FROM t CROSS JOIN nodes c
             ON c.space_id = ?1 AND c.parent_id = t.id AND +c.deleted_at = ?3
         )
         SELECT id FROM t";

/// Запрос [`all_descendants`]: `?1` — пространство, `?2` — корень (порядок соединений —
/// как в [`SUBTREE_SQL`]).
pub(crate) const DESCENDANTS_SQL: &str = "WITH RECURSIVE t (id) AS (
             SELECT c.id FROM nodes c WHERE c.space_id = ?1 AND c.parent_id = ?2
             UNION ALL
             SELECT c.id FROM t CROSS JOIN nodes c ON c.space_id = ?1 AND c.parent_id = t.id
         )
         SELECT n.id, n.path FROM t CROSS JOIN nodes n ON n.id = t.id";

/// Сам узел и его потомки по `parent_id`, в порядке обхода в ширину.
/// `stamp = None` — только живые потомки; `Some(t)` — удалённые меткой `t`.
fn subtree(c: &Connection, root: &Node, stamp: Option<&str>) -> Result<Vec<Id>, StoreError> {
    let rows: Vec<Id> = match stamp {
        None => c
            .prepare_cached(SUBTREE_SQL)?
            .query_map(params![root.space_id, root.id], |r| r.get(0))?
            .collect::<Result<_, _>>()?,
        Some(t) => c
            .prepare_cached(SUBTREE_STAMP_SQL)?
            .query_map(params![root.space_id, root.id, t], |r| r.get(0))?
            .collect::<Result<_, _>>()?,
    };
    Ok(rows)
}

/// Все потомки узла по `parent_id` (живые и удалённые) с их путями.
fn all_descendants(
    c: &Connection,
    space_id: Id,
    root: Id,
) -> Result<Vec<(Id, String)>, StoreError> {
    let mut st = c.prepare_cached(DESCENDANTS_SQL)?;
    let rows = st.query_map(params![space_id, root], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Удалённые предки узла, от корня вниз. Подъём останавливается на первом живом:
/// предки живого узла живы (восстановление всегда поднимает и предков).
fn deleted_ancestors(c: &Connection, node: &Node) -> Result<Vec<Id>, StoreError> {
    let mut out = Vec::new();
    let mut cur = node.parent_id;
    while let Some(pid) = cur {
        let p = load_node(c, pid)?;
        if p.deleted_at.is_none() {
            break;
        }
        out.push(p.id);
        cur = p.parent_id;
    }
    out.reverse();
    Ok(out)
}

/// `deleted_at` строкой из БД — точное значение для сравнения меток пакета.
fn raw_deleted_at(c: &Connection, id: Id) -> Result<Option<String>, StoreError> {
    Ok(
        c.query_row("SELECT deleted_at FROM nodes WHERE id = ?1", [id], |r| {
            r.get::<_, Option<String>>(0)
        })
        .optional()?
        .flatten(),
    )
}

/// Пересчитывает `context_prefix` и `embed_key` чанков узла по его текущим пути, имени и тегам
/// (триггер `chunks_au` обновляет FTS). У папки и удалённого узла чанков нет.
fn refresh_prefixes(c: &Connection, space: &str, node_id: Id) -> Result<(), StoreError> {
    let (path, name, fm, l0): (String, String, Option<String>, Option<String>) = c.query_row(
        "SELECT path, name, frontmatter, l0 FROM nodes WHERE id = ?1",
        [node_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let tags = match fm.as_deref() {
        Some(s) => frontmatter_tags(s)
            .map_err(|e| StoreError::Corrupt(format!("frontmatter узла {node_id}: {e}")))?,
        None => Vec::new(),
    };
    let chunks: Vec<(i64, String, String)> = {
        let mut st =
            c.prepare_cached("SELECT seq, heading_path, text FROM chunks WHERE node_id = ?1")?;
        st.query_map([node_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?
    };
    // ключ кэша зависит от префикса: пересчитывается вместе с ним
    let mut upd =
        c.prepare_cached("UPDATE chunks SET context_prefix = ?2, embed_key = ?3 WHERE seq = ?1")?;
    for (seq, heading, text) in chunks {
        let prefix = note_prefix(space, &path, &name, &heading, &tags, l0.as_deref());
        upd.execute(params![seq, prefix, embed_key_of(&prefix, &text)])?;
    }
    Ok(())
}

/// `upsert` для живой индексируемой заметки после переноса; ревизия не меняется.
fn requeue_moved(c: &Connection, node_id: Id, now: &str) -> Result<(), StoreError> {
    let rev: Option<i64> = c
        .prepare_cached(
            "UPDATE nodes SET index_state = 'pending'
             WHERE id = ?1 AND kind = 'note' AND deleted_at IS NULL
               AND index_state <> 'excluded'
             RETURNING current_rev",
        )?
        .query_row([node_id], |r| r.get(0))
        .optional()?;
    if let Some(rev) = rev {
        enqueue(c, node_id, rev, JobOp::Upsert, PRIORITY_WRITE, now)?;
    }
    Ok(())
}
