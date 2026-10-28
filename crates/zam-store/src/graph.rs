//! Граф связей (§8.5 ТЗ): таблица `edges` и рекурсивные CTE SQLite, трейт [`GraphStore`].
//!
//! **Происхождение рёбер:**
//! - `wikilink` — вики-ссылки `[[…]]` и Markdown-ссылки на относительные пути в тексте заметки
//!   (тип `links_to`); `frontmatter` — строки `relations:` (тип из строки). Эти рёбра выводятся
//!   из текущей ревизии: пересобираются при каждой записи узла ([`rebuild_derived`]: создание,
//!   правка, PATCH, откат, восстановление) в той же транзакции; `dst_raw` у них — ключ ссылки
//!   ([`crate::links`]), неразрешённая ссылка — `dst_id IS NULL`;
//! - `explicit` — явные связи API и `links` insight ([`GraphStore::add_edge`]); удаляются
//!   мягко ([`GraphStore::remove_edge`]), только они;
//! - `system` — `supersedes` при замещении insight (новая → старая); не удаляются вручную.
//!
//! **Разрешение при изменении дерева.** Создание, перенос (переименование), удаление и
//! восстановление узла перепроверяют выведенные рёбра пространства, чей ключ может указывать на
//! затронутые узлы (старые и новые имя и путь) или которые указывают на них ([`tree_changed`]):
//! ожидающая ссылка разрешается, ссылка на удалённую заметку становится неразрешённой, при
//! нескольких заметках с одним именем побеждает первая по пути — как в рендере. Относительные
//! Markdown-ссылки перенесённой заметки пересчитываются от её нового места.
//!
//! **Термины словаря** (M4.5 §2.2): `[[Имя]]` без заметки с таким именем разрешается в
//! действующий термин пространства по имени или псевдониму — ребро ведёт к записи
//! (`dst_kind = entry`, [`crate::links::resolve_target`]). Любая запись `term` перепроверяет
//! такие рёбра пространства ([`terms_changed`]).
//!
//! **Видимость** (`spaces` в чтениях): ребро видно, если пространство источника и цели входят в
//! область, а оба конца живы (узел или запись не удалены); удалённый источник прячет свои рёбра
//! (и обратные ссылки), восстановление возвращает их. Неразрешённые ссылки отдаёт только
//! [`GraphStore::outgoing`]. Участнику общего решения сервер передаёт одно пространство
//! решения — рёбра в Knowledge, Rules и Human владельца ему не видны.
//!
//! **Права `certainty`** ([`crate::certainty`]): явные рёбра от `axiom` и к `axiom` создаёт и
//! удаляет любой автор — связь не меняет текст аксиомы (**решение владельца 2026-10-26**,
//! ruling R10 M3 снят); проверка матрицы в транзакции ребра остаётся точкой для её правил.
//! Выведенные рёбра следуют за текстом, запись которого уже проверена матрицей; дозаполнение
//! ([`backfill_edges`]) матрицу не проверяет (данные уже записаны).

use std::collections::{HashMap, HashSet};
use std::future::Future;

use jiff::Timestamp;
use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use zam_core::{Id, TargetKind, TargetRef};
use zam_markdown::{Doc, LinkKind};

use crate::certainty::{CertaintyChange, check_certainty};
use crate::entries::{Certainty, EntryStatus};
use crate::links::{self, keys_of, link_key, parse_relation, resolve_target};
use crate::nodes::{Attribution, AuthorKind};
use crate::time::{fmt_ts, ts_at};
use crate::{Db, StoreError};

/// Предел глубины обхода [`GraphStore::neighbors`].
pub const GRAPH_DEPTH_MAX: u8 = 3;

/// Предел узлов подграфа [`GraphStore::neighbors`] **[решение исполнителя]**: ближние к началу
/// обхода (по глубине, затем по виду и id) — первыми; сверх предела — `truncated`. Предел
/// ограничивает ответ `GET /graph` и работу `expand=graph` над узлом-хабом (чтение узлов и рёбер
/// найденного), но не сам рекурсивный обход: он ограничен размером графа и глубиной.
pub const GRAPH_MAX_NODES: usize = 500;

/// Предел рёбер подграфа (по порядку создания) — сверх него `truncated`.
pub const GRAPH_MAX_EDGES: usize = 2000;

/// Тип связи (§8.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeType {
    LinksTo,
    RelatesTo,
    DependsOn,
    PartOf,
    CausedBy,
    Fixes,
    Supersedes,
    DerivedFrom,
    Mentions,
    /// Несогласие с записью (спецификация M4.5 §5.2): явный тип — `POST /edges`, `links`
    /// записи, `relations:` заметки.
    Contradicts,
}

impl EdgeType {
    /// Все типы в порядке §8.5 (с M4.5 — и `contradicts`).
    pub const ALL: [EdgeType; 10] = [
        EdgeType::LinksTo,
        EdgeType::RelatesTo,
        EdgeType::DependsOn,
        EdgeType::PartOf,
        EdgeType::CausedBy,
        EdgeType::Fixes,
        EdgeType::Supersedes,
        EdgeType::DerivedFrom,
        EdgeType::Mentions,
        EdgeType::Contradicts,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EdgeType::LinksTo => "links_to",
            EdgeType::RelatesTo => "relates_to",
            EdgeType::DependsOn => "depends_on",
            EdgeType::PartOf => "part_of",
            EdgeType::CausedBy => "caused_by",
            EdgeType::Fixes => "fixes",
            EdgeType::Supersedes => "supersedes",
            EdgeType::DerivedFrom => "derived_from",
            EdgeType::Mentions => "mentions",
            EdgeType::Contradicts => "contradicts",
        }
    }

    /// Тип по имени; неизвестное — `None`.
    pub fn parse(s: &str) -> Option<EdgeType> {
        EdgeType::ALL.into_iter().find(|t| t.as_str() == s)
    }
}

/// Откуда ребро.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeOrigin {
    Wikilink,
    Frontmatter,
    Explicit,
    System,
}

impl EdgeOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeOrigin::Wikilink => "wikilink",
            EdgeOrigin::Frontmatter => "frontmatter",
            EdgeOrigin::Explicit => "explicit",
            EdgeOrigin::System => "system",
        }
    }

    pub fn parse(s: &str) -> Option<EdgeOrigin> {
        [
            EdgeOrigin::Wikilink,
            EdgeOrigin::Frontmatter,
            EdgeOrigin::Explicit,
            EdgeOrigin::System,
        ]
        .into_iter()
        .find(|o| o.as_str() == s)
    }
}

macro_rules! sql_text {
    ($t:ty, $what:literal) => {
        impl ToSql for $t {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(self.as_str().into())
            }
        }

        impl FromSql for $t {
            fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
                let s = v.as_str()?;
                <$t>::parse(s).ok_or_else(|| {
                    rusqlite::types::FromSqlError::Other(format!(concat!($what, ": {}"), s).into())
                })
            }
        }
    };
}

sql_text!(EdgeType, "неизвестный тип связи");
sql_text!(EdgeOrigin, "неизвестное происхождение связи");

/// Ребро графа.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EdgeView {
    pub id: Id,
    pub src: TargetRef,
    /// Пространство источника.
    pub src_space: Id,
    /// `None` — неразрешённая ссылка (ключ в `dst_raw`).
    pub dst: Option<TargetRef>,
    pub dst_space: Option<Id>,
    /// Ключ ссылки выведенного ребра (`[[Имя]]` или путь); у явных и системных — `None`.
    pub dst_raw: Option<String>,
    pub dst_anchor: Option<String>,
    #[serde(rename = "type")]
    pub kind: EdgeType,
    pub origin: EdgeOrigin,
    #[serde(serialize_with = "crate::time::ser_ts")]
    pub created_at: Timestamp,
}

/// Узел графа: заметка или запись журнала.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphNode {
    #[serde(rename = "ref")]
    pub r: TargetRef,
    pub space_id: Id,
    /// Имя заметки или заголовок записи.
    pub title: String,
    /// Путь заметки; у записи — `None`.
    pub path: Option<String>,
    /// Статус insight.
    pub status: Option<EntryStatus>,
    pub certainty: Option<Certainty>,
    pub deleted: bool,
}

/// Подграф: узлы и рёбра между ними.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Graph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<EdgeView>,
    /// Узлы или рёбра обрезаны пределами [`GRAPH_MAX_NODES`] и [`GRAPH_MAX_EDGES`].
    pub truncated: bool,
}

/// Новое явное ребро.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewEdge {
    pub src: TargetRef,
    pub dst: TargetRef,
    pub kind: EdgeType,
}

/// Хранилище графа (§8.5). `spaces` — область видимости: узлы и рёбра вне неё не
/// возвращаются (пустая — ничего).
pub trait GraphStore: Send + Sync {
    /// Связный подграф вокруг `start` на `depth` шагов (1..=3) в обе стороны по рёбрам типов
    /// `types` (пусто — любые): узлы и все видимые рёбра между ними, не больше
    /// [`GRAPH_MAX_NODES`] узлов (ближние — первыми) и [`GRAPH_MAX_EDGES`] рёбер, сверх —
    /// `truncated`. Глубина вне 1..=3 — `BadInput`.
    fn neighbors(
        &self,
        start: &[TargetRef],
        depth: u8,
        types: &[EdgeType],
        spaces: &[Id],
    ) -> impl Future<Output = Result<Graph, StoreError>> + Send;
    /// Видимые рёбра в `target` от живых источников (обратные ссылки).
    fn backlinks(
        &self,
        target: TargetRef,
        spaces: &[Id],
    ) -> impl Future<Output = Result<Vec<EdgeView>, StoreError>> + Send;
    /// Рёбра из `src`: видимые разрешённые и неразрешённые ссылки.
    fn outgoing(
        &self,
        src: TargetRef,
        spaces: &[Id],
    ) -> impl Future<Output = Result<Vec<EdgeView>, StoreError>> + Send;
    /// Явное ребро. Концы — живые заметки или записи этой базы (нет → `NotFound`); папка,
    /// петля или `supersedes` → `BadInput`; такое же живое явное ребро уже есть → `Conflict`.
    fn add_edge(
        &self,
        e: NewEdge,
        a: &Attribution,
    ) -> impl Future<Output = Result<EdgeView, StoreError>> + Send;
    /// Мягко удаляет явное ребро. Нет или уже удалено → `NotFound`; выведенное или
    /// системное → `BadInput`.
    fn remove_edge(
        &self,
        id: Id,
        a: &Attribution,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;
}

fn kind_str(k: TargetKind) -> &'static str {
    match k {
        TargetKind::Node => "node",
        TargetKind::Entry => "entry",
    }
}

fn kind_of(s: &str, i: usize) -> rusqlite::Result<TargetKind> {
    match s {
        "node" => Ok(TargetKind::Node),
        "entry" => Ok(TargetKind::Entry),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            i,
            rusqlite::types::Type::Text,
            format!("неизвестный вид конца связи: {other}").into(),
        )),
    }
}

/// Колонки ребра для [`edge_row`]; таблица `edges` под псевдонимом `e`.
const EDGE_COLS: &str = "e.id, e.src_kind, e.src_id, e.space_id, e.dst_kind, e.dst_id,
    e.dst_space_id, e.dst_raw, e.dst_anchor, e.type, e.origin, e.created_at";

fn edge_row(r: &Row) -> rusqlite::Result<EdgeView> {
    let src_kind: String = r.get(1)?;
    let dst_kind: Option<String> = r.get(4)?;
    let dst_id: Option<Id> = r.get(5)?;
    let dst = match (dst_kind, dst_id) {
        (Some(k), Some(id)) => Some(TargetRef {
            kind: kind_of(&k, 4)?,
            id,
        }),
        _ => None,
    };
    Ok(EdgeView {
        id: r.get(0)?,
        src: TargetRef {
            kind: kind_of(&src_kind, 1)?,
            id: r.get(2)?,
        },
        src_space: r.get(3)?,
        dst,
        dst_space: r.get(6)?,
        dst_raw: r.get(7)?,
        dst_anchor: r.get(8)?,
        kind: r.get(9)?,
        origin: r.get(10)?,
        created_at: ts_at(r, 11)?,
    })
}

/// Конец `(k, i)` жив: узел или запись не удалены.
fn live_sql(k: &str, i: &str) -> String {
    format!(
        "(({k} = 'node' AND EXISTS (SELECT 1 FROM nodes ln WHERE ln.id = {i} AND ln.deleted_at IS NULL))
          OR ({k} = 'entry' AND EXISTS (SELECT 1 FROM entries le WHERE le.id = {i} AND le.deleted_at IS NULL)))"
    )
}

/// Разрешённое ребро `e` видно в области `?{s}` (JSON-массив id пространств): живое, оба конца
/// живы и в области.
fn visible_sql(s: usize) -> String {
    format!(
        "e.deleted_at IS NULL AND e.dst_id IS NOT NULL
         AND e.space_id IN (SELECT value FROM json_each(?{s}))
         AND e.dst_space_id IN (SELECT value FROM json_each(?{s}))
         AND {} AND {}",
        live_sql("e.src_kind", "e.src_id"),
        live_sql("e.dst_kind", "e.dst_id")
    )
}

fn json_ids(ids: &[Id]) -> String {
    serde_json::Value::from(ids.iter().map(|i| i.to_string()).collect::<Vec<_>>()).to_string()
}

fn json_refs(refs: &[TargetRef]) -> String {
    serde_json::Value::from(
        refs.iter()
            .map(|r| serde_json::json!([kind_str(r.kind), r.id.to_string()]))
            .collect::<Vec<_>>(),
    )
    .to_string()
}

fn json_types(types: &[EdgeType]) -> Option<String> {
    (!types.is_empty()).then(|| {
        serde_json::Value::from(types.iter().map(|t| t.as_str()).collect::<Vec<_>>()).to_string()
    })
}

/// Ребро по id (в том числе удалённое мягко).
fn load_edge(c: &Connection, id: Id) -> Result<Option<(EdgeView, bool)>, StoreError> {
    Ok(c.prepare_cached(&format!(
        "SELECT {EDGE_COLS}, e.deleted_at IS NOT NULL FROM edges e WHERE e.id = ?1"
    ))?
    .query_row([id], |r| Ok((edge_row(r)?, r.get(12)?)))
    .optional()?)
}

/// Живое (не удалённое) ребро по id; нет → `None`. Видимость концов проверяет вызывающий.
pub async fn get_edge(db: &Db, id: Id) -> Result<Option<EdgeView>, StoreError> {
    db.read(move |c| {
        Ok(load_edge(c, id)?
            .filter(|(_, deleted)| !deleted)
            .map(|e| e.0))
    })
    .await
}

/// Что известно о конце связи.
#[derive(Debug, Clone, Copy)]
struct EndMeta {
    space: Id,
    live: bool,
    /// Узел — папка (концом связи не бывает).
    folder: bool,
    certainty: Option<Certainty>,
}

fn end_meta(c: &Connection, r: TargetRef) -> Result<Option<EndMeta>, StoreError> {
    let row = match r.kind {
        TargetKind::Node => c
            .prepare_cached(
                "SELECT space_id, deleted_at IS NULL, kind = 'folder', certainty
                 FROM nodes WHERE id = ?1",
            )?
            .query_row([r.id], |r| {
                Ok(EndMeta {
                    space: r.get(0)?,
                    live: r.get(1)?,
                    folder: r.get(2)?,
                    certainty: r.get(3)?,
                })
            })
            .optional()?,
        TargetKind::Entry => c
            .prepare_cached(
                "SELECT space_id, deleted_at IS NULL, 0, certainty FROM entries WHERE id = ?1",
            )?
            .query_row([r.id], |r| {
                Ok(EndMeta {
                    space: r.get(0)?,
                    live: r.get(1)?,
                    folder: r.get(2)?,
                    certainty: r.get(3)?,
                })
            })
            .optional()?,
    };
    Ok(row)
}

/// Узел графа по ссылке (в том числе удалённый); нет — `None`.
pub async fn graph_node(db: &Db, r: TargetRef) -> Result<Option<GraphNode>, StoreError> {
    db.read(move |c| Ok(nodes_of(c, &[r])?.into_iter().next()))
        .await
}

/// Узлы графа по ссылкам (в том числе удалённые) в порядке `refs`, без повторов;
/// отсутствующие пропускаются. Видимость проверяет вызывающий.
pub async fn graph_nodes(db: &Db, refs: Vec<TargetRef>) -> Result<Vec<GraphNode>, StoreError> {
    db.read(move |c| {
        let mut found: HashMap<TargetRef, GraphNode> =
            nodes_of(c, &refs)?.into_iter().map(|n| (n.r, n)).collect();
        Ok(refs.iter().filter_map(|r| found.remove(r)).collect())
    })
    .await
}

/// Описания узлов графа (порядок не гарантирован; отсутствующие пропускаются).
fn nodes_of(c: &Connection, refs: &[TargetRef]) -> Result<Vec<GraphNode>, StoreError> {
    let ids = |k: TargetKind| {
        json_ids(
            &refs
                .iter()
                .filter(|r| r.kind == k)
                .map(|r| r.id)
                .collect::<Vec<_>>(),
        )
    };
    let mut out = Vec::with_capacity(refs.len());
    let mut st = c.prepare_cached(
        "SELECT n.id, n.space_id, n.name, n.path, n.certainty, n.deleted_at IS NOT NULL
         FROM json_each(?1) j CROSS JOIN nodes n ON n.id = j.value",
    )?;
    let rows = st.query_map([ids(TargetKind::Node)], |r| {
        Ok(GraphNode {
            r: TargetRef {
                kind: TargetKind::Node,
                id: r.get(0)?,
            },
            space_id: r.get(1)?,
            title: r.get(2)?,
            path: Some(r.get(3)?),
            status: None,
            certainty: r.get(4)?,
            deleted: r.get(5)?,
        })
    })?;
    for n in rows {
        out.push(n?);
    }
    let mut st = c.prepare_cached(
        "SELECT e.id, e.space_id, e.title, e.status, e.certainty, e.deleted_at IS NOT NULL
         FROM json_each(?1) j CROSS JOIN entries e ON e.id = j.value",
    )?;
    let rows = st.query_map([ids(TargetKind::Entry)], |r| {
        Ok(GraphNode {
            r: TargetRef {
                kind: TargetKind::Entry,
                id: r.get(0)?,
            },
            space_id: r.get(1)?,
            title: r.get(2)?,
            path: None,
            status: r.get(3)?,
            certainty: r.get(4)?,
            deleted: r.get(5)?,
        })
    })?;
    for n in rows {
        out.push(n?);
    }
    Ok(out)
}

fn neighbors_sync(
    c: &Connection,
    start: &[TargetRef],
    depth: u8,
    types: &[EdgeType],
    spaces: &[Id],
) -> Result<Graph, StoreError> {
    let sp = json_ids(spaces);
    let ty = json_types(types);
    let tf = "(?4 IS NULL OR e.type IN (SELECT value FROM json_each(?4)))";
    let vis = visible_sql(3);
    // обход в обе стороны; UNION (без ALL) и предел глубины делают циклы безопасными
    let sql = format!(
        "WITH RECURSIVE walk (kind, id, d) AS (
             SELECT json_extract(j.value, '$[0]'), json_extract(j.value, '$[1]'), 0
             FROM json_each(?1) j
             UNION
             SELECT e.dst_kind, e.dst_id, w.d + 1 FROM walk w
             JOIN edges e ON e.src_kind = w.kind AND e.src_id = w.id
             WHERE w.d < ?2 AND {tf} AND {vis}
             UNION
             SELECT e.src_kind, e.src_id, w.d + 1 FROM walk w
             JOIN edges e ON e.dst_kind = w.kind AND e.dst_id = w.id
             WHERE w.d < ?2 AND {tf} AND {vis}
         )
         SELECT kind, id, min(d) FROM walk GROUP BY kind, id ORDER BY min(d), kind, id
         LIMIT ?5"
    );
    let mut st = c.prepare_cached(&sql)?;
    // на один больше предела: так видно, что узлы обрезаны
    let limit = (GRAPH_MAX_NODES + 1) as i64;
    let mut found: Vec<(TargetRef, i64)> = st
        .query_map(params![json_refs(start), depth, sp, ty, limit], |r| {
            let k: String = r.get(0)?;
            Ok((
                TargetRef {
                    kind: kind_of(&k, 0)?,
                    id: r.get(1)?,
                },
                r.get(2)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    let mut truncated = found.len() > GRAPH_MAX_NODES;
    found.truncate(GRAPH_MAX_NODES);
    let order: HashMap<TargetRef, usize> = found
        .iter()
        .enumerate()
        .map(|(i, (r, _))| (*r, i))
        .collect();
    let refs: Vec<TargetRef> = found.iter().map(|f| f.0).collect();
    let allowed: HashSet<Id> = spaces.iter().copied().collect();
    let mut nodes: Vec<GraphNode> = nodes_of(c, &refs)?
        .into_iter()
        .filter(|n| allowed.contains(&n.space_id))
        .collect();
    nodes.sort_by_key(|n| order.get(&n.r).copied().unwrap_or(usize::MAX));
    let present: HashSet<TargetRef> = nodes.iter().map(|n| n.r).collect();
    // рёбра между найденными узлами (те же фильтры видимости и типов)
    let mut st = c.prepare_cached(&format!(
        "SELECT {EDGE_COLS} FROM json_each(?1) j
         CROSS JOIN edges e ON e.src_kind = json_extract(j.value, '$[0]')
                           AND e.src_id = json_extract(j.value, '$[1]')
         WHERE {tf} AND {vis}
         ORDER BY e.created_at, e.rowid"
    ))?;
    let present_refs: Vec<TargetRef> = nodes.iter().map(|n| n.r).collect();
    let mut edges: Vec<EdgeView> = st
        .query_map(params![json_refs(&present_refs), depth, sp, ty], edge_row)?
        .filter_map(|e| match e {
            Ok(e) if e.dst.is_some_and(|d| present.contains(&d)) => Some(Ok(e)),
            Ok(_) => None,
            Err(err) => Some(Err(err)),
        })
        .take(GRAPH_MAX_EDGES + 1)
        .collect::<Result<_, _>>()?;
    truncated |= edges.len() > GRAPH_MAX_EDGES;
    edges.truncate(GRAPH_MAX_EDGES);
    Ok(Graph {
        nodes,
        edges,
        truncated,
    })
}

fn backlinks_sync(
    c: &Connection,
    target: TargetRef,
    spaces: &[Id],
) -> Result<Vec<EdgeView>, StoreError> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {EDGE_COLS} FROM edges e
         WHERE e.dst_kind = ?1 AND e.dst_id = ?2 AND {}
         ORDER BY e.created_at, e.rowid",
        visible_sql(3)
    ))?;
    let rows = st.query_map(
        params![kind_str(target.kind), target.id, json_ids(spaces)],
        edge_row,
    )?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn outgoing_sync(
    c: &Connection,
    src: TargetRef,
    spaces: &[Id],
) -> Result<Vec<EdgeView>, StoreError> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {EDGE_COLS} FROM edges e
         WHERE e.src_kind = ?1 AND e.src_id = ?2 AND e.deleted_at IS NULL
           AND e.space_id IN (SELECT value FROM json_each(?3))
           AND {}
           AND (e.dst_id IS NULL
                OR (e.dst_space_id IN (SELECT value FROM json_each(?3)) AND {}))
         ORDER BY e.created_at, e.rowid",
        live_sql("e.src_kind", "e.src_id"),
        live_sql("e.dst_kind", "e.dst_id")
    ))?;
    let rows = st.query_map(
        params![kind_str(src.kind), src.id, json_ids(spaces)],
        edge_row,
    )?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Вставка ребра; `dst` — `(цель, её пространство)` или `None` (неразрешённая ссылка).
#[allow(clippy::too_many_arguments)]
fn insert_edge(
    c: &Connection,
    src: TargetRef,
    space: Id,
    dst: Option<(TargetRef, Id)>,
    raw: Option<&str>,
    anchor: Option<&str>,
    kind: EdgeType,
    origin: EdgeOrigin,
    a: Option<&Attribution>,
    now: &str,
) -> Result<Id, rusqlite::Error> {
    let id = Id::new();
    c.prepare_cached(
        "INSERT INTO edges (id, space_id, src_kind, src_id, dst_kind, dst_id, dst_space_id,
                            dst_raw, dst_anchor, type, origin, user_id, token_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
    )?
    .execute(params![
        id,
        space,
        kind_str(src.kind),
        src.id,
        dst.map(|d| kind_str(d.0.kind)),
        dst.map(|d| d.0.id),
        dst.map(|d| d.1),
        raw,
        anchor,
        kind,
        origin,
        a.and_then(|a| a.user_id),
        a.and_then(|a| a.token_id),
        now,
    ])?;
    Ok(id)
}

/// Явное ребро от записи или узла: проверки концов и прав (см. [`GraphStore::add_edge`]).
pub(crate) fn add_explicit(
    c: &Connection,
    e: NewEdge,
    a: &Attribution,
    now: &str,
) -> Result<Id, StoreError> {
    if e.kind == EdgeType::Supersedes {
        return Err(StoreError::BadInput(
            "supersedes ставится замещением insight, а не явной связью".into(),
        ));
    }
    if e.src == e.dst {
        return Err(StoreError::BadInput("связь узла с самим собой".into()));
    }
    let end = |r: TargetRef| -> Result<EndMeta, StoreError> {
        let m = end_meta(c, r)?
            .filter(|m| m.live)
            .ok_or(StoreError::NotFound)?;
        if m.folder {
            return Err(StoreError::BadInput(
                "конец связи — заметка или запись журнала, не папка".into(),
            ));
        }
        Ok(m)
    };
    let src = end(e.src)?;
    let dst = end(e.dst)?;
    // ребро по матрице certainty: с M4 от axiom — любому автору (§7.5, §8.5)
    check_certainty(
        a.author_kind,
        src.certainty,
        &CertaintyChange::ModifyOther,
        None,
        None,
    )?;
    insert_edge(
        c,
        e.src,
        src.space,
        Some((e.dst, dst.space)),
        None,
        None,
        e.kind,
        EdgeOrigin::Explicit,
        Some(a),
        now,
    )
    .map_err(|err| {
        if crate::system::is_unique_violation(&err) {
            StoreError::Conflict(format!(
                "связь {} {} → {} уже есть",
                e.kind.as_str(),
                e.src,
                e.dst
            ))
        } else {
            err.into()
        }
    })
}

/// Явные рёбра `links` новой записи `src` (в транзакции её создания); повторы в списке
/// пропускаются.
pub(crate) fn insert_entry_links(
    c: &Connection,
    src: Id,
    links: &[(EdgeType, TargetRef)],
    a: &Attribution,
    now: &str,
) -> Result<(), StoreError> {
    let mut seen = HashSet::new();
    for &(kind, dst) in links {
        if !seen.insert((kind, dst)) {
            continue;
        }
        let e = NewEdge {
            src: TargetRef {
                kind: TargetKind::Entry,
                id: src,
            },
            dst,
            kind,
        };
        add_explicit(c, e, a, now)?;
    }
    Ok(())
}

/// Системное ребро `supersedes` (новая → старая) в транзакции замещения; повтор не дублирует.
pub(crate) fn insert_supersedes(
    c: &Connection,
    new: Id,
    old: Id,
    space: Id,
    a: Option<&Attribution>,
    now: &str,
) -> Result<(), StoreError> {
    let exists: bool = c
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM edges
                            WHERE src_kind = 'entry' AND src_id = ?1 AND dst_kind = 'entry'
                              AND dst_id = ?2 AND type = 'supersedes' AND origin = 'system'
                              AND deleted_at IS NULL)",
        )?
        .query_row(params![new, old], |r| r.get(0))?;
    if !exists {
        insert_edge(
            c,
            TargetRef {
                kind: TargetKind::Entry,
                id: new,
            },
            space,
            Some((
                TargetRef {
                    kind: TargetKind::Entry,
                    id: old,
                },
                space,
            )),
            None,
            None,
            EdgeType::Supersedes,
            EdgeOrigin::System,
            a,
            now,
        )?;
    }
    Ok(())
}

/// Выведенные рёбра текста заметки: `(тип, происхождение, ключ, якорь)` без повторов по
/// `(тип, происхождение, ключ)` (первый якорь).
fn derived_links(path: &str, doc: &Doc) -> Vec<(EdgeType, EdgeOrigin, String, Option<String>)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |t: EdgeType, o: EdgeOrigin, k: String, a: Option<String>| {
        if seen.insert((t, o, k.clone())) {
            out.push((t, o, k, a));
        }
    };
    for l in &doc.links {
        if l.kind == LinkKind::Image {
            continue;
        }
        if let Some(k) = link_key(path, l) {
            let a = l
                .anchor
                .as_deref()
                .map(links::percent_decode)
                .filter(|a| !a.trim().is_empty());
            push(EdgeType::LinksTo, EdgeOrigin::Wikilink, k, a);
        }
    }
    if let Some(fm) = &doc.frontmatter {
        for r in &fm.relations {
            if let Ok(r) = parse_relation(r) {
                push(r.kind, EdgeOrigin::Frontmatter, r.key, r.anchor);
            }
        }
    }
    out
}

/// Пересобирает выведенные рёбра заметки `node_id` по разобранному тексту `doc` её текущей
/// ревизии (в транзакции записи): прежние `wikilink`/`frontmatter` удаляются, новые
/// разрешаются в пространстве заметки. Явные и системные не трогаются.
pub(crate) fn rebuild_derived(
    c: &Connection,
    node_id: Id,
    doc: &Doc,
    now: &str,
) -> Result<(), StoreError> {
    c.prepare_cached(
        "DELETE FROM edges WHERE src_kind = 'node' AND src_id = ?1 AND deleted_at IS NULL
           AND origin IN ('wikilink', 'frontmatter')",
    )?
    .execute([node_id])?;
    let (space, path): (Id, String) = c
        .prepare_cached("SELECT space_id, path FROM nodes WHERE id = ?1")?
        .query_row([node_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let src = TargetRef {
        kind: TargetKind::Node,
        id: node_id,
    };
    for (kind, origin, key, anchor) in derived_links(&path, doc) {
        let dst = resolve_target(c, space, &key)?.map(|t| (t, space));
        insert_edge(
            c,
            src,
            space,
            dst,
            Some(&key),
            anchor.as_deref(),
            kind,
            origin,
            None,
            now,
        )?;
    }
    Ok(())
}

/// Узлы `ids` пространства `space` созданы, перенесены, удалены или восстановлены (в той же
/// транзакции): выведенные рёбра пространства, указывающие на них или с ключом их старых
/// (`old` — `(имя, путь)` до переноса) и текущих имени и пути, разрешаются заново. Рёбра
/// удалённых источников пропускаются: их никто не читает (выборки графа — по живым концам), а
/// восстановление источника пересобирает его рёбра (`index_note` → [`rebuild_derived`]) —
/// корзина не увеличивает запись при массовых изменениях дерева.
pub(crate) fn tree_changed(
    c: &Connection,
    space: Id,
    ids: &[Id],
    old: &[(String, String)],
) -> Result<(), StoreError> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut keys: HashSet<String> = HashSet::new();
    for (name, path) in old {
        keys.extend(keys_of(name, path));
    }
    {
        let mut st = c.prepare_cached("SELECT name, path FROM nodes WHERE id = ?1")?;
        for &id in ids {
            if let Some((name, path)) = st
                .query_row([id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .optional()?
            {
                keys.extend(keys_of(&name, &path));
            }
        }
    }
    let keys: Vec<String> = keys.into_iter().collect();
    let cands: Vec<(Id, String, Option<Id>)> = {
        let mut st = c.prepare_cached(
            "SELECT e.id, e.dst_raw, e.dst_id FROM edges e
             WHERE e.dst_kind = 'node' AND e.dst_id IN (SELECT value FROM json_each(?2))
               AND e.deleted_at IS NULL AND e.space_id = ?1
               AND e.origin IN ('wikilink', 'frontmatter') AND e.dst_raw IS NOT NULL
               AND EXISTS (SELECT 1 FROM nodes s WHERE s.id = e.src_id AND s.deleted_at IS NULL)
             UNION
             SELECT e.id, e.dst_raw, e.dst_id FROM edges e
             WHERE e.space_id = ?1 AND e.dst_raw IN (SELECT value FROM json_each(?3))
               AND e.deleted_at IS NULL AND e.origin IN ('wikilink', 'frontmatter')
               AND EXISTS (SELECT 1 FROM nodes s WHERE s.id = e.src_id AND s.deleted_at IS NULL)",
        )?;
        st.query_map(
            params![
                space,
                json_ids(ids),
                serde_json::Value::from(keys).to_string()
            ],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?
        .collect::<Result<_, _>>()?
    };
    re_resolve(c, space, cands)
}

/// Пересчитывает цели выведенных рёбер `(id ребра, ключ, текущая цель)` пространства `space`:
/// заметка или, для `[[Имя]]`, термин словаря ([`resolve_target`]).
fn re_resolve(
    c: &Connection,
    space: Id,
    cands: Vec<(Id, String, Option<Id>)>,
) -> Result<(), StoreError> {
    let mut upd = c.prepare_cached(
        "UPDATE edges SET dst_kind = ?2, dst_id = ?3,
                          dst_space_id = CASE WHEN ?3 IS NULL THEN NULL ELSE ?4 END
         WHERE id = ?1",
    )?;
    for (id, raw, cur) in cands {
        let new = resolve_target(c, space, &raw)?;
        if new.map(|t| t.id) != cur {
            upd.execute(params![
                id,
                new.map(|t| kind_str(t.kind)),
                new.map(|t| t.id),
                space
            ])?;
        }
    }
    Ok(())
}

/// Термин пространства `space` записан, изменён, замещён, отозван, удалён или восстановлен
/// (в той же транзакции): `[[Имя]]` выведенных рёбер заметок пространства, не нашедшие
/// заметку, разрешаются заново — ожидающая ссылка находит термин, ссылка на выбывший термин
/// становится неразрешённой или переходит к его преемнику. Рёбра удалённых источников
/// пропускаются (как в [`tree_changed`]).
pub(crate) fn terms_changed(c: &Connection, space: Id) -> Result<(), StoreError> {
    let cands: Vec<(Id, String, Option<Id>)> = {
        let mut st = c.prepare_cached(
            "SELECT e.id, e.dst_raw, e.dst_id FROM edges e
             WHERE e.space_id = ?1 AND e.deleted_at IS NULL AND e.origin IN ('wikilink', 'frontmatter')
               AND e.dst_raw LIKE '[[%' AND (e.dst_id IS NULL OR e.dst_kind = 'entry')
               AND e.src_kind = 'node'
               AND EXISTS (SELECT 1 FROM nodes s WHERE s.id = e.src_id AND s.deleted_at IS NULL)",
        )?;
        st.query_map([space], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?
    };
    re_resolve(c, space, cands)
}

/// Перенесённая живая заметка с относительными Markdown-ссылками (ключ — путь, а не `[[…]]`):
/// её выведенные рёбра пересобираются от нового места (в транзакции переноса).
pub(crate) fn rebuild_moved(c: &Connection, node_id: Id, now: &str) -> Result<(), StoreError> {
    let has_paths: bool = c
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM edges
                            WHERE src_kind = 'node' AND src_id = ?1 AND deleted_at IS NULL
                              AND origin = 'wikilink' AND dst_raw NOT LIKE '[[%')",
        )?
        .query_row([node_id], |r| r.get(0))?;
    if !has_paths {
        return Ok(());
    }
    let rev: Option<i64> = c
        .prepare_cached(
            "SELECT current_rev FROM nodes WHERE id = ?1 AND kind = 'note' AND deleted_at IS NULL",
        )?
        .query_row([node_id], |r| r.get(0))
        .optional()?;
    let Some(rev) = rev else {
        return Ok(());
    };
    let content = crate::nodes::revision_text(c, node_id, rev)?;
    rebuild_derived(c, node_id, &zam_markdown::parse(&content), now)
}

impl GraphStore for Db {
    async fn neighbors(
        &self,
        start: &[TargetRef],
        depth: u8,
        types: &[EdgeType],
        spaces: &[Id],
    ) -> Result<Graph, StoreError> {
        if !(1..=GRAPH_DEPTH_MAX).contains(&depth) {
            return Err(StoreError::BadInput(format!(
                "depth: от 1 до {GRAPH_DEPTH_MAX}"
            )));
        }
        if start.is_empty() || spaces.is_empty() {
            return Ok(Graph::default());
        }
        let (start, types, spaces) = (start.to_vec(), types.to_vec(), spaces.to_vec());
        self.read(move |c| neighbors_sync(c, &start, depth, &types, &spaces))
            .await
    }

    async fn backlinks(
        &self,
        target: TargetRef,
        spaces: &[Id],
    ) -> Result<Vec<EdgeView>, StoreError> {
        if spaces.is_empty() {
            return Ok(Vec::new());
        }
        let spaces = spaces.to_vec();
        self.read(move |c| backlinks_sync(c, target, &spaces)).await
    }

    async fn outgoing(&self, src: TargetRef, spaces: &[Id]) -> Result<Vec<EdgeView>, StoreError> {
        if spaces.is_empty() {
            return Ok(Vec::new());
        }
        let spaces = spaces.to_vec();
        self.read(move |c| outgoing_sync(c, src, &spaces)).await
    }

    async fn add_edge(&self, e: NewEdge, a: &Attribution) -> Result<EdgeView, StoreError> {
        let a = a.clone();
        self.write(move |tx| {
            let now = fmt_ts(Timestamp::now());
            let id = add_explicit(tx, e, &a, &now)?;
            Ok(load_edge(tx, id)?.ok_or(StoreError::NotFound)?.0)
        })
        .await
    }

    async fn remove_edge(&self, id: Id, a: &Attribution) -> Result<(), StoreError> {
        let author: AuthorKind = a.author_kind;
        self.write(move |tx| {
            let Some((e, false)) = load_edge(tx, id)? else {
                return Err(StoreError::NotFound);
            };
            if e.origin != EdgeOrigin::Explicit {
                return Err(StoreError::BadInput(format!(
                    "удаляются только явные связи, это — {}",
                    e.origin.as_str()
                )));
            }
            let src = end_meta(tx, e.src)?.and_then(|m| m.certainty);
            check_certainty(author, src, &CertaintyChange::ModifyOther, None, None)?;
            tx.execute(
                "UPDATE edges SET deleted_at = ?2 WHERE id = ?1",
                params![id, fmt_ts(Timestamp::now())],
            )?;
            Ok(())
        })
        .await
    }
}

/// Ключ `meta`: рёбра существующих заметок и системные `supersedes` ещё не дозаполнены
/// (миграция 0010).
pub(crate) const EDGES_BACKFILL: &str = "edges_backfill";

/// Заметок в одной транзакции дозаполнения рёбер.
const BACKFILL_BATCH: i64 = 200;

/// Нужно ли дозаполнение рёбер при открытии. База без заметок и замещённых записей (новая)
/// снимает флаг сразу, без фоновой задачи.
pub(crate) async fn backfill_pending(db: &Db) -> Result<bool, StoreError> {
    db.write(|tx| {
        let flag = tx
            .query_row(
                "SELECT 1 FROM meta WHERE key = ?1",
                [EDGES_BACKFILL],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !flag {
            return Ok(false);
        }
        let any: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM nodes WHERE kind = 'note')
                 OR EXISTS (SELECT 1 FROM entries WHERE supersedes_id IS NOT NULL)",
            [],
            |r| r.get(0),
        )?;
        if !any {
            tx.execute("DELETE FROM meta WHERE key = ?1", [EDGES_BACKFILL])?;
        }
        Ok(any)
    })
    .await
}

/// Дозаполнение рёбер (миграция 0010): выведенные рёбра всех заметок (живых и удалённых — их
/// восстановление всё равно пересоберёт) по текущей ревизии, пакетами по [`BACKFILL_BATCH`]
/// в отдельных транзакциях, затем системные `supersedes` замещённых insight. Матрица
/// `certainty` не проверяется: данные уже записаны. Идемпотентно (выведенные пересобираются,
/// системные не дублируются); флаг снимается в последней транзакции, прерванное дозаполнение
/// повторится при следующем открытии. Возвращает число заметок.
pub(crate) async fn backfill_edges(db: &Db) -> Result<u64, StoreError> {
    let started = std::time::Instant::now();
    let (mut notes, mut after) = (0u64, None::<Id>);
    loop {
        let cursor = after;
        let (n, last) = db
            .write(move |tx| {
                let now = fmt_ts(Timestamp::now());
                let batch: Vec<(Id, i64)> = {
                    let mut st = tx.prepare_cached(
                        "SELECT id, current_rev FROM nodes
                         WHERE kind = 'note' AND (?1 IS NULL OR id > ?1)
                         ORDER BY id LIMIT ?2",
                    )?;
                    st.query_map(params![cursor, BACKFILL_BATCH], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?
                    .collect::<Result<_, _>>()?
                };
                for &(id, rev) in &batch {
                    let content = crate::nodes::revision_text(tx, id, rev)?;
                    rebuild_derived(tx, id, &zam_markdown::parse(&content), &now)?;
                }
                Ok((batch.len() as i64, batch.last().map(|b| b.0)))
            })
            .await?;
        notes += n as u64;
        match last {
            Some(l) if n == BACKFILL_BATCH => after = Some(l),
            _ => break,
        }
    }
    db.write(|tx| {
        let now = fmt_ts(Timestamp::now());
        let pairs: Vec<(Id, Id, Id)> = {
            let mut st = tx.prepare(
                "SELECT id, supersedes_id, space_id FROM entries WHERE supersedes_id IS NOT NULL",
            )?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<_, _>>()?
        };
        for (new, old, space) in pairs {
            insert_supersedes(tx, new, old, space, None, &now)?;
        }
        tx.execute("DELETE FROM meta WHERE key = ?1", [EDGES_BACKFILL])?;
        Ok(())
    })
    .await?;
    tracing::info!(
        notes,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "рёбра графа дозаполнены (v10)"
    );
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_type_names() {
        for t in EdgeType::ALL {
            assert_eq!(EdgeType::parse(t.as_str()), Some(t));
            assert_eq!(
                serde_json::to_string(&t).unwrap(),
                format!("\"{}\"", t.as_str())
            );
        }
        assert_eq!(EdgeType::ALL.len(), 10);
        assert_eq!(EdgeType::parse("contradicts"), Some(EdgeType::Contradicts));
        assert_eq!(
            serde_json::to_string(&EdgeType::Contradicts).unwrap(),
            "\"contradicts\""
        );
        assert_eq!(EdgeType::parse("LinksTo"), None);
        assert_eq!(EdgeOrigin::parse("system"), Some(EdgeOrigin::System));
    }

    #[test]
    fn backlinks_and_unresolved_use_indexes() {
        let c = Connection::open_in_memory().unwrap();
        for m in crate::USER_MIGRATIONS {
            c.execute_batch(m.sql).unwrap();
        }
        let plan = |sql: &str, n: usize| {
            let mut st = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            let args = rusqlite::params_from_iter(vec!["x"; n]);
            st.query_map(args, |r| r.get::<_, String>(3))
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let p = plan(
            &format!(
                "SELECT {EDGE_COLS} FROM edges e WHERE e.dst_kind = ?1 AND e.dst_id = ?2 AND {}",
                visible_sql(3)
            ),
            3,
        );
        assert!(p.contains("edges_dst"), "{p}");
        let p = plan(
            "SELECT e.id FROM edges e WHERE e.space_id = ?1
               AND e.dst_raw IN (SELECT value FROM json_each(?2))
               AND e.deleted_at IS NULL AND e.origin IN ('wikilink', 'frontmatter')",
            2,
        );
        assert!(p.contains("edges_derived_raw"), "{p}");
        let p = plan(
            "SELECT id FROM nodes WHERE space_id = ?1 AND name = ?2 AND kind = 'note'
               AND deleted_at IS NULL ORDER BY path LIMIT 1",
            2,
        );
        assert!(p.contains("nodes_live_name"), "{p}");
    }
}
