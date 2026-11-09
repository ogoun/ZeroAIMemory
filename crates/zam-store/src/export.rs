//! Снимок пространства для экспорта (M6 §12б.5): живые заметки и папки, записи журналов, явные
//! связи и история — **в одной транзакции чтения**, чтобы архив был согласован (заметка и её
//! вложения, запись и её триггеры — из одного состояния базы).
//!
//! **Поток, а не список.** [`export_space`] открывает транзакцию на соединении из пула и
//! отдаёт вызывающему [`SpaceReader`]: метаданные узлов — списком (пути нужны заранее для имён
//! файлов), а тексты заметок, записи журналов и ревизии — по одной ([`SpaceReader::note_content`],
//! [`SpaceReader::for_each_entry`], [`SpaceReader::for_each_node_revision`]). В памяти — метаданные
//! узлов и один текст за раз, независимо от объёма истории. Соединение и транзакция держатся,
//! пока работает вызывающий (весь поток архива), — поэтому экспорт ограничен слотами на сервере.
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
    /// Документ вложения (`attachment_docs.id`, ссылка `d:<id>`): импорт переписывает ссылки
    /// `d:` на документы, созданные заново (18b2).
    pub doc: Option<Id>,
}

/// Ссылка на файл решения (`file_refs`) заметки или записи.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefRow {
    pub path: String,
    /// Символ ссылки `путь#qual` (M5.6).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub solution: Option<String>,
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

/// Путь `path` под префиксом `prefix` по целым сегментам (или равен ему).
pub fn under_prefix(path: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_matches('/');
    prefix.is_empty()
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Чтение снимка внутри транзакции [`export_space`].
pub struct SpaceReader<'c> {
    c: &'c Connection,
    sel: &'c ExportSelect,
}

/// Вызывает `f` со снимком пространства по `sel` (см. модуль) внутри одной транзакции чтения
/// на соединении пула; `f` работает в потоке `spawn_blocking`.
pub async fn export_space<R: Send + 'static>(
    db: &Db,
    sel: ExportSelect,
    f: impl FnOnce(&SpaceReader) -> Result<R, StoreError> + Send + 'static,
) -> Result<R, StoreError> {
    // долгая транзакция: контрольные точки базы — только PASSIVE, пока она идёт
    let guard = db.long_reader();
    db.read(move |c| {
        let _guard = guard;
        // одна транзакция: все выборки — из одного состояния базы
        let tx = c.unchecked_transaction()?;
        let r = f(&SpaceReader { c: &tx, sel: &sel })?;
        tx.finish()?;
        Ok(r)
    })
    .await
}

impl SpaceReader<'_> {
    /// Живые папки и заметки под фильтром путей (по пути), без текстов.
    pub fn nodes(&self) -> Result<(Vec<Node>, Vec<Node>), StoreError> {
        let keep = |p: &str| match &self.sel.paths {
            None => true,
            Some(ps) => ps.iter().any(|x| under_prefix(p, x)),
        };
        let mut st = self.c.prepare(&format!(
            "SELECT {NODE_COLS} FROM nodes n
             WHERE n.space_id = ?1 AND n.deleted_at IS NULL ORDER BY n.path"
        ))?;
        let rows = st.query_map([self.sel.space_id], node_row)?;
        let (mut folders, mut notes) = (Vec::new(), Vec::new());
        for n in rows {
            let n = n?;
            if !keep(&n.path) {
                continue;
            }
            match n.kind {
                NodeKind::Folder => folders.push(n),
                NodeKind::Note => notes.push(n),
            }
        }
        Ok((folders, notes))
    }

    /// Текст текущей ревизии заметки.
    pub fn note_content(&self, n: &Node) -> Result<String, StoreError> {
        crate::nodes::revision_text(self.c, n.id, n.current_rev)
    }

    /// Вложения заметки.
    pub fn note_attachments(&self, node: Id) -> Result<Vec<NoteAttachment>, StoreError> {
        let mut st = self.c.prepare_cached(
            "SELECT a.blob_sha256, a.filename, a.mime, a.size, a.created_at, d.id
             FROM attachments a
             LEFT JOIN attachment_docs d ON d.node_id = a.node_id
                  AND d.blob_sha256 = a.blob_sha256 AND d.filename = a.filename
                  AND d.deleted_at IS NULL
             WHERE a.node_id = ?1 ORDER BY a.created_at, a.blob_sha256, a.filename",
        )?;
        let rows = st.query_map([node], |r| {
            let size: i64 = r.get(3)?;
            Ok(NoteAttachment {
                sha256: r.get(0)?,
                filename: r.get(1)?,
                mime: r.get(2)?,
                size: u64::try_from(size).unwrap_or_default(),
                created_at: ts_at(r, 4)?,
                doc: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Ссылки на файлы решения владельца (`node` | `entry`).
    pub fn refs(&self, kind: &str, owner: Id) -> Result<Vec<RefRow>, StoreError> {
        let mut st = self.c.prepare_cached(
            "SELECT path, content_hash, git_commit, solution_guid, symbol, symbol_hash
             FROM file_refs
             WHERE owner_kind = ?1 AND owner_id = ?2 ORDER BY path, coalesce(symbol, '')",
        )?;
        let rows = st.query_map(params![kind, owner], |r| {
            Ok(RefRow {
                path: r.get(0)?,
                symbol: r.get(4)?,
                hash: r.get(1)?,
                symbol_hash: r.get(5)?,
                commit: r.get(2)?,
                solution: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Живые записи под фильтром видов (по `created_at`, затем id) — по одной.
    pub fn for_each_entry(
        &self,
        mut f: impl FnMut(EntryDump) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let sel = self.sel;
        let mut st = self.c.prepare(&format!(
            "SELECT {ENTRY_COLS} FROM entries e
             WHERE e.space_id = ?1 AND e.deleted_at IS NULL ORDER BY e.created_at, e.id"
        ))?;
        for e in st.query_map([sel.space_id], entry_row)? {
            let e = e?;
            let k = e.kind;
            if (k == EntryKind::Report && !sel.with_reports)
                || sel.kinds.as_ref().is_some_and(|ks| !ks.contains(&k))
            {
                continue;
            }
            let triggers = self.triggers_of(e.id)?;
            let refs = self.refs("entry", e.id)?;
            let files = self.files_of(e.id)?;
            f(EntryDump {
                entry: e,
                triggers,
                refs,
                files,
            })?;
        }
        Ok(())
    }

    fn triggers_of(&self, entry: Id) -> Result<Vec<Trigger>, StoreError> {
        let mut st = self.c.prepare_cached(
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

    fn files_of(&self, entry: Id) -> Result<Vec<EntryAttachment>, StoreError> {
        let mut st = self.c.prepare_cached(&format!(
            "SELECT {ATT_COLS} FROM entry_attachments WHERE entry_id = ?1 ORDER BY created_at, rowid"
        ))?;
        let rows = st.query_map([entry], att_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Явные живые рёбра с источником в `srcs` и видимой целью (см. модуль).
    pub fn links(&self, srcs: &HashSet<Id>) -> Result<Vec<EdgeView>, StoreError> {
        let sel = self.sel;
        let spaces = serde_json::Value::from(
            sel.link_spaces
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        )
        .to_string();
        let mut st = self.c.prepare(&format!(
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

    /// Верхняя оценка объёма истории узла или записи в JSONL, байт: текст при экранировании JSON
    /// растёт не больше чем вшестеро (`\u00XX`), плюс поля ревизии.
    pub fn history_bound(&self, kind: TargetKind, id: Id) -> Result<u64, StoreError> {
        let sql = match kind {
            TargetKind::Node => {
                "SELECT COALESCE(SUM(length(CAST(content AS BLOB))), 0), COUNT(*)
                 FROM revisions WHERE node_id = ?1"
            }
            _ => {
                "SELECT COALESCE(SUM(length(CAST(title AS BLOB)) + length(CAST(body AS BLOB))
                        + length(CAST(data AS BLOB)) + length(CAST(tags AS BLOB))
                        + COALESCE(length(CAST(reason AS BLOB)), 0)), 0), COUNT(*)
                 FROM entry_revisions WHERE entry_id = ?1"
            }
        };
        let (text, n): (i64, i64) = self
            .c
            .prepare_cached(sql)?
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok((text.max(0) as u64)
            .saturating_mul(6)
            .saturating_add((n.max(0) as u64).saturating_mul(4096)))
    }

    /// Ревизии заметки по возрастанию `rev` — по одной.
    pub fn for_each_node_revision(
        &self,
        node: Id,
        mut f: impl FnMut(NodeRevision) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let mut st = self.c.prepare_cached(
            "SELECT rev, content_hash, size, author_kind, token_id, session_id, machine,
                    message, ts, user_id, content
             FROM revisions WHERE node_id = ?1 ORDER BY rev",
        )?;
        for r in st.query_map([node], |r| {
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
        })? {
            f(r?)?;
        }
        Ok(())
    }

    /// Ревизии записи по возрастанию `rev` — по одной.
    pub fn for_each_entry_revision(
        &self,
        entry: Id,
        mut f: impl FnMut(EntryRevision) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let mut st = self.c.prepare_cached(
            "SELECT rev, title, body, data, tags, area, status, certainty, reason, ts,
                    author_kind, user_id, token_id, session_id, machine
             FROM entry_revisions WHERE entry_id = ?1 ORDER BY rev",
        )?;
        for r in st.query_map([entry], |r| {
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
        })? {
            f(r?)?;
        }
        Ok(())
    }
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
