//! Снимок пространства для экспорта (M6 §12б.5): живые заметки и папки, записи журналов, явные
//! связи и история — **одной транзакцией чтения**, чтобы архив был согласован (заметка и её
//! вложения, запись и её триггеры — из одного состояния базы).
//!
//! **Что входит.** Только пространство `space_id` и только живое: удалённые узлы и записи
//! (корзина) не выбираются. Не читаются вовсе: Vault, карта кода (`code_*`), эмбеддинги, чанки,
//! кэш выжимок, сессии, снимки сессий, задания индексации — снимок о них не знает.
//!
//! **Связи** — только явные (`origin = explicit`) живые рёбра с источником в снимке: рёбра
//! wikilink и frontmatter, связи идей и задач строятся заново при записи (импорт, §12б.5).
//! Цель ребра обязана быть живой, в пространстве из `link_spaces` (то, что принципал читает) и
//! не отчётом при `with_reports = false` — иначе ребро не выдаётся: экспорт не раскрывает id
//! целей, которых принципал не видит.
use std::collections::HashSet;

use jiff::Timestamp;
use rusqlite::{Connection, params};
use serde::Serialize;
use zam_core::{Id, TargetKind};

use crate::entries::{
    ATT_COLS, AuthorView, ENTRY_COLS, Entry, EntryAttachment, EntryKind, EntryRevision, Trigger,
    att_row, entry_row, json_col, tools_col,
};
use crate::graph::{EDGE_COLS, EdgeView, edge_row};
use crate::nodes::{NODE_COLS, Node, NodeKind, RevisionInfo, node_row};
use crate::time::ts_at;
use crate::{Db, StoreError};

/// Что выбрать в снимок.
#[derive(Debug, Clone)]
pub struct ExportSelect {
    pub space_id: Id,
    /// Заметки и папки: `None` — все, `Some(префиксы)` — с путём, равным префиксу или под ним
    /// (по целым сегментам, с учётом регистра); `Some(пусто)` — ни одной.
    pub paths: Option<Vec<String>>,
    /// Записи журналов: `None` — все виды, `Some(виды)` — только эти (пусто — ни одной).
    pub kinds: Option<Vec<EntryKind>>,
    /// Отчёты (`report`): только сессиям WebUI (агенту они не существуют, M5.5).
    pub with_reports: bool,
    /// Ревизии заметок и записей.
    pub history: bool,
    /// Пространства, в которые могут вести выданные связи (читаемые принципалом).
    pub link_spaces: Vec<Id>,
}

/// Вложение заметки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteAttachment {
    pub sha256: String,
    pub filename: String,
    pub mime: Option<String>,
    pub size: u64,
    #[serde(serialize_with = "crate::time::ser_ts")]
    pub created_at: Timestamp,
}

/// Ссылка на файл решения (`file_refs`) заметки или записи.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefRow {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub solution: Option<String>,
}

/// Заметка снимка: узел, текст текущей ревизии, вложения и ссылки на файлы.
#[derive(Debug, Clone)]
pub struct NoteDump {
    pub node: Node,
    pub content: String,
    pub attachments: Vec<NoteAttachment>,
    pub refs: Vec<RefRow>,
}

/// Запись журнала снимка с триггерами, ссылками на файлы и файлами отчёта.
#[derive(Debug, Clone)]
pub struct EntryDump {
    pub entry: Entry,
    pub triggers: Vec<Trigger>,
    pub refs: Vec<RefRow>,
    pub files: Vec<EntryAttachment>,
}

/// Ревизия заметки с текстом.
#[derive(Debug, Clone, Serialize)]
pub struct NodeRevision {
    #[serde(flatten)]
    pub info: RevisionInfo,
    pub content: String,
}

/// Снимок пространства.
#[derive(Debug, Clone, Default)]
pub struct SpaceDump {
    /// Живые папки (по пути).
    pub folders: Vec<Node>,
    /// Живые заметки (по пути).
    pub notes: Vec<NoteDump>,
    /// Живые записи (по `created_at`, затем id).
    pub entries: Vec<EntryDump>,
    /// Явные живые связи (см. модуль).
    pub links: Vec<EdgeView>,
    /// Ревизии заметок снимка (по возрастанию `rev`), при `history`.
    pub node_history: Vec<(Id, Vec<NodeRevision>)>,
    /// Ревизии записей снимка (по возрастанию `rev`), при `history`.
    pub entry_history: Vec<(Id, Vec<EntryRevision>)>,
}

/// Путь `path` под префиксом `prefix` по целым сегментам (или равен ему).
pub fn under_prefix(path: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_matches('/');
    prefix.is_empty()
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Снимок пространства по `sel` (см. модуль) одной транзакцией чтения.
pub async fn export_space(db: &Db, sel: ExportSelect) -> Result<SpaceDump, StoreError> {
    db.read(move |c| {
        // одна транзакция: все выборки — из одного состояния базы
        let tx = c.unchecked_transaction()?;
        let dump = snapshot(&tx, &sel)?;
        tx.finish()?;
        Ok(dump)
    })
    .await
}

fn snapshot(c: &Connection, sel: &ExportSelect) -> Result<SpaceDump, StoreError> {
    let mut d = SpaceDump::default();
    let keep_path = |p: &str| match &sel.paths {
        None => true,
        Some(ps) => ps.iter().any(|x| under_prefix(p, x)),
    };
    let mut st = c.prepare(&format!(
        "SELECT {NODE_COLS} FROM nodes n
         WHERE n.space_id = ?1 AND n.deleted_at IS NULL ORDER BY n.path"
    ))?;
    let nodes = st
        .query_map([sel.space_id], node_row)?
        .collect::<Result<Vec<_>, _>>()?;
    for n in nodes.into_iter().filter(|n| keep_path(&n.path)) {
        match n.kind {
            NodeKind::Folder => d.folders.push(n),
            NodeKind::Note => {
                let content = crate::nodes::revision_text(c, n.id, n.current_rev)?;
                let attachments = note_attachments(c, n.id)?;
                let refs = refs_of(c, "node", n.id)?;
                d.notes.push(NoteDump {
                    node: n,
                    content,
                    attachments,
                    refs,
                });
            }
        }
    }
    let kinds_ok = |k: EntryKind| {
        (k != EntryKind::Report || sel.with_reports)
            && sel.kinds.as_ref().is_none_or(|ks| ks.contains(&k))
    };
    let mut st = c.prepare(&format!(
        "SELECT {ENTRY_COLS} FROM entries e
         WHERE e.space_id = ?1 AND e.deleted_at IS NULL ORDER BY e.created_at, e.id"
    ))?;
    let entries = st
        .query_map([sel.space_id], entry_row)?
        .collect::<Result<Vec<_>, _>>()?;
    for e in entries.into_iter().filter(|e| kinds_ok(e.kind)) {
        let triggers = triggers_of(c, e.id)?;
        let refs = refs_of(c, "entry", e.id)?;
        let files = files_of(c, e.id)?;
        d.entries.push(EntryDump {
            entry: e,
            triggers,
            refs,
            files,
        });
    }
    let srcs: HashSet<Id> = d
        .notes
        .iter()
        .map(|n| n.node.id)
        .chain(d.entries.iter().map(|e| e.entry.id))
        .collect();
    d.links = links_of(c, sel, &srcs)?;
    if sel.history {
        for n in &d.notes {
            d.node_history
                .push((n.node.id, node_revisions(c, n.node.id)?));
        }
        for e in &d.entries {
            d.entry_history
                .push((e.entry.id, entry_revisions(c, e.entry.id)?));
        }
    }
    Ok(d)
}

fn note_attachments(c: &Connection, node: Id) -> Result<Vec<NoteAttachment>, StoreError> {
    let mut st = c.prepare_cached(
        "SELECT blob_sha256, filename, mime, size, created_at FROM attachments
         WHERE node_id = ?1 ORDER BY created_at, blob_sha256, filename",
    )?;
    let rows = st.query_map([node], |r| {
        let size: i64 = r.get(3)?;
        Ok(NoteAttachment {
            sha256: r.get(0)?,
            filename: r.get(1)?,
            mime: r.get(2)?,
            size: u64::try_from(size).unwrap_or_default(),
            created_at: ts_at(r, 4)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn refs_of(c: &Connection, kind: &str, owner: Id) -> Result<Vec<RefRow>, StoreError> {
    let mut st = c.prepare_cached(
        "SELECT path, content_hash, git_commit, solution_guid FROM file_refs
         WHERE owner_kind = ?1 AND owner_id = ?2 ORDER BY path",
    )?;
    let rows = st.query_map(params![kind, owner], |r| {
        Ok(RefRow {
            path: r.get(0)?,
            hash: r.get(1)?,
            commit: r.get(2)?,
            solution: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn triggers_of(c: &Connection, entry: Id) -> Result<Vec<Trigger>, StoreError> {
    let mut st = c.prepare_cached(
        "SELECT id, type, pattern, tools, created_at FROM triggers
         WHERE entry_id = ?1 ORDER BY rowid",
    )?;
    let rows = st.query_map([entry], |r| {
        Ok(Trigger {
            id: r.get(0)?,
            kind: r.get(1)?,
            pattern: r.get(2)?,
            tools: tools_col(r, 3)?,
            created_at: ts_at(r, 4)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn files_of(c: &Connection, entry: Id) -> Result<Vec<EntryAttachment>, StoreError> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {ATT_COLS} FROM entry_attachments WHERE entry_id = ?1 ORDER BY created_at, rowid"
    ))?;
    let rows = st.query_map([entry], att_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Явные живые рёбра с источником в `srcs` и видимой целью (см. модуль).
fn links_of(
    c: &Connection,
    sel: &ExportSelect,
    srcs: &HashSet<Id>,
) -> Result<Vec<EdgeView>, StoreError> {
    let spaces = serde_json::Value::from(
        sel.link_spaces
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
    )
    .to_string();
    let mut st = c.prepare(&format!(
        "SELECT {EDGE_COLS} FROM edges e
         WHERE e.space_id = ?1 AND e.deleted_at IS NULL AND e.origin = 'explicit'
           AND e.dst_id IS NOT NULL
           AND e.dst_space_id IN (SELECT value FROM json_each(?2))
           AND ((e.dst_kind = 'node' AND EXISTS (SELECT 1 FROM nodes dn
                     WHERE dn.id = e.dst_id AND dn.deleted_at IS NULL))
             OR (e.dst_kind = 'entry' AND EXISTS (SELECT 1 FROM entries de
                     WHERE de.id = e.dst_id AND de.deleted_at IS NULL
                       AND (?3 OR de.kind <> 'report'))))
         ORDER BY e.created_at, e.id"
    ))?;
    let rows = st
        .query_map(params![sel.space_id, spaces, sel.with_reports], edge_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|e| {
            srcs.contains(&e.src.id)
                && e.dst
                    .is_some_and(|d| matches!(d.kind, TargetKind::Node | TargetKind::Entry))
        })
        .collect())
}

fn node_revisions(c: &Connection, node: Id) -> Result<Vec<NodeRevision>, StoreError> {
    let mut st = c.prepare_cached(
        "SELECT rev, content_hash, size, author_kind, token_id, session_id, machine,
                message, ts, user_id, content
         FROM revisions WHERE node_id = ?1 ORDER BY rev",
    )?;
    let rows = st.query_map([node], |r| {
        Ok(NodeRevision {
            info: RevisionInfo {
                rev: r.get(0)?,
                content_hash: r.get(1)?,
                size: r.get(2)?,
                author_kind: r.get(3)?,
                token_id: r.get(4)?,
                session_id: r.get(5)?,
                machine: r.get(6)?,
                message: r.get(7)?,
                ts: ts_at(r, 8)?,
                user_id: r.get(9)?,
            },
            content: r.get(10)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn entry_revisions(c: &Connection, entry: Id) -> Result<Vec<EntryRevision>, StoreError> {
    let mut st = c.prepare_cached(
        "SELECT rev, title, body, data, tags, area, status, certainty, reason, ts,
                author_kind, user_id, token_id, session_id, machine
         FROM entry_revisions WHERE entry_id = ?1 ORDER BY rev",
    )?;
    let rows = st.query_map([entry], |r| {
        Ok(EntryRevision {
            rev: r.get(0)?,
            title: r.get(1)?,
            body: r.get(2)?,
            data: json_col(r, 3)?,
            tags: json_col(r, 4)?,
            area: r.get(5)?,
            status: r.get(6)?,
            certainty: r.get(7)?,
            reason: r.get(8)?,
            ts: ts_at(r, 9)?,
            author: AuthorView {
                kind: r.get(10)?,
                user_id: r.get(11)?,
                token_id: r.get(12)?,
                session_id: r.get(13)?,
                machine: r.get(14)?,
            },
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_by_whole_segments() {
        assert!(under_prefix("docs/a", "docs"));
        assert!(under_prefix("docs", "docs"));
        assert!(under_prefix("docs/a", "docs/"));
        assert!(!under_prefix("docsx/a", "docs"));
        assert!(!under_prefix("Docs/a", "docs"));
        assert!(under_prefix("x", ""));
    }
}
