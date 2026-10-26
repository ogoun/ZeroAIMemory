//! Граф связей (§8.5, §12): `GET /graph`, `POST /edges`, `DELETE /edges/{id}`, обратные и
//! исходящие ссылки узлов и записей, разбор `links` insight. Хранилище — `zam_store::graph`.
//!
//! - `GET /graph?node=<id>|entry=<id>&depth=1..3&types=a,b` → `{nodes, edges}`: подграф на
//!   `depth` шагов (по умолчанию 1) в обе стороны по рёбрам типов `types` (по умолчанию все);
//!   ровно один из `node`/`entry`, иначе 400; неизвестный тип или глубина вне 1..=3 — 400;
//! - `POST /edges {src, type, dst}` → 201 ребро `explicit`: `src` — `n:<id>` | `e:<id>` (или
//!   `node:<id>` | `entry:<id>`), `dst` — то же или `[[Имя]]` (в пространстве источника);
//!   `type` вне §8.5 или `supersedes` (его ставит замещение) — 422; такая связь уже есть —
//!   409; цель не найдена — 404 (`[[Имя]]` — 422);
//! - `DELETE /edges/{id}` → 204; удаляются только `explicit` (выведенные и системные — 422);
//! - `GET /nodes/{id}/backlinks`, `GET /entries/{id}/backlinks` → `{items, nodes}`: рёбра в
//!   цель от живых источников и узлы-источники; `GET /nodes/{id}/links`,
//!   `GET /entries/{id}/links` → `{items, nodes}`: исходящие рёбра, в том числе неразрешённые
//!   ссылки (`dst: null`, ключ в `dst_raw`).
//!
//! **Область.** Узел, запись и ребро ищутся по правилам `scope` (Human по токену — 404;
//! участник общего решения — только пространство решения в базе владельца). Видимость графа —
//! [`graph_spaces`]: участнику рёбра и узлы вне пространства решения не возвращаются (и не
//! создаются: цель вне области — 404, как несуществующая).
//!
//! **Границы базы.** Цель в другой базе (личная память участника ↔ решение в базе владельца) —
//! 422 «связи между базами — не в M3» **[решение исполнителя]**; цель в другом доступном
//! участнику решении того же владельца — тоже 422 (граф участника — одно пространство решения).
//!
//! **Права.** Запись — по правам пространства источника (`ro` — 403, участник `ro` — 403).
//! Рёбра от `axiom` и к `axiom` агент создаёт и удаляет сам — связь не меняет текст аксиомы
//! (§8.5, решение владельца 2026-10-26; ruling R10 M3 снят).
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use zam_core::{Id, TargetKind, TargetRef};
use zam_store::{
    Db, EdgeOrigin, EdgeType, EdgeView, GRAPH_DEPTH_MAX, GraphStore, NewEdge, Space, graph_node,
    graph_nodes, links,
};

use super::entries::unprocessable;
use super::scope::{Mode, Role, edge_scope, entry_scope, graph_spaces, node_scope};
use super::{json_body, query};
use crate::app::AppState;
use crate::attrib::attribution;
use crate::auth::Principal;
use crate::error::ApiError;

/// Текст 422 для цели в другой базе.
pub const CROSS_DB: &str = "связи между базами — не в M3: цель в другой базе \
                            (личная память и общее решение хранятся раздельно)";
/// Предел `links` одного insight. [решение исполнителя]
pub const LINKS_MAX: usize = 50;

/// Ссылка на узел или запись: `n:<id>` | `e:<id>` | `node:<id>` | `entry:<id>`.
pub fn parse_ref(s: &str) -> Option<TargetRef> {
    let (k, id) = s.trim().split_once(':')?;
    let kind = match k {
        "n" | "node" => TargetKind::Node,
        "e" | "entry" => TargetKind::Entry,
        _ => return None,
    };
    Some(TargetRef {
        kind,
        id: id.parse().ok()?,
    })
}

/// Цель связи из запроса.
enum Dst {
    Ref(TargetRef),
    /// Ключ `[[Имя]]` ([`links::wiki_key`]).
    Wiki(String),
}

/// `[[Имя]]` или ссылка; иначе `None`.
fn parse_dst(s: &str) -> Option<Dst> {
    let t = s.trim();
    if let Some(inner) = t.strip_prefix("[[").and_then(|t| t.strip_suffix("]]")) {
        let inner = inner.split('|').next().unwrap_or(inner);
        let inner = inner.split('#').next().unwrap_or(inner);
        return links::wiki_key(inner).map(Dst::Wiki);
    }
    parse_ref(t).map(Dst::Ref)
}

/// Тип явной связи: из §8.5 и не `supersedes` (иначе 422).
fn explicit_type(raw: &str, field: &str) -> Result<EdgeType, ApiError> {
    match EdgeType::parse(raw.trim()) {
        Some(EdgeType::Supersedes) => Err(unprocessable(format!(
            "{field}: supersedes ставится замещением insight (POST /entries/{{id}}/supersede)"
        ))),
        Some(t) => Ok(t),
        None => Err(unprocessable(format!(
            "{field}: одно из {}",
            EdgeType::ALL
                .iter()
                .filter(|t| **t != EdgeType::Supersedes)
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Узел или запись в доступной области.
struct Target {
    db: Db,
    owner: Id,
    space: Space,
    role: Role,
    r: TargetRef,
    deleted: bool,
}

async fn target_scope(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    r: TargetRef,
    mode: Mode,
) -> Result<Target, ApiError> {
    let raw = r.id.to_string();
    Ok(match r.kind {
        TargetKind::Node => {
            let sc = node_scope(st, p, headers, &raw, mode).await?;
            Target {
                deleted: sc.node.deleted_at.is_some(),
                db: sc.db,
                owner: sc.owner,
                space: sc.space,
                role: sc.role,
                r,
            }
        }
        TargetKind::Entry => {
            let sc = entry_scope(st, p, headers, &raw, mode).await?;
            Target {
                deleted: sc.entry.deleted_at.is_some(),
                db: sc.db,
                owner: sc.owner,
                space: sc.space,
                role: sc.role,
                r,
            }
        }
    })
}

/// Живая цель в доступной области (удалённая — 404).
async fn live_target(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    r: TargetRef,
    mode: Mode,
) -> Result<Target, ApiError> {
    let t = target_scope(st, p, headers, r, mode).await?;
    if t.deleted {
        return Err(ApiError::NotFound);
    }
    Ok(t)
}

/// Цель связи источника из базы `db` владельца `owner` в пространстве `space` при области
/// `visible`: видимая живая цель этой базы — она; `[[Имя]]` не найдено — 422; цель в другой
/// базе, доступной пользователю, — 422 [`CROSS_DB`]; иначе — 404 (вне области участника цель
/// «не существует»). `field` — имя поля в тексте ошибки.
#[allow(clippy::too_many_arguments)]
async fn resolve_dst(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    db: &Db,
    owner: Id,
    space: Id,
    visible: &[Id],
    dst: Dst,
    field: &str,
) -> Result<TargetRef, ApiError> {
    let r = match dst {
        Dst::Wiki(key) => {
            let k = key.clone();
            let id = db
                .read(move |c| Ok(links::resolve_key(c, space, &k)?))
                .await?;
            return match id {
                Some(id) => Ok(TargetRef {
                    kind: TargetKind::Node,
                    id,
                }),
                None => Err(unprocessable(format!(
                    "{field}: заметка {key} не найдена в пространстве источника"
                ))),
            };
        }
        Dst::Ref(r) => r,
    };
    if let Some(n) = graph_node(db, r).await?
        && !n.deleted
        && visible.contains(&n.space_id)
    {
        return Ok(r);
    }
    // цель не в этой базе (или вне области): в другой доступной базе — 422; в этой базе, но в
    // другом доступном участнику решении того же владельца — 422 (граф участника ограничен
    // пространством решения источника); иначе 404
    match target_scope(st, p, headers, r, Mode::Read).await {
        Ok(t) if t.deleted => Err(ApiError::NotFound),
        Ok(t) if t.owner != owner => Err(unprocessable(format!("{field}: {CROSS_DB}"))),
        Ok(_) => Err(unprocessable(format!(
            "{field}: цель вне пространства решения источника — участнику общего решения \
             связи доступны только внутри него"
        ))),
        Err(_) => Err(ApiError::NotFound),
    }
}

/// Связи `links` insight (`[{type, target}]`, `target` — `node:<id>` | `entry:<id>` |
/// `[[Имя]]`) в базе `db` владельца `owner`, пространство записи `space`, роль `role`:
/// проверенные `(тип, цель)` или 422 `links[i]: …` / 404 (цель вне области).
#[allow(clippy::too_many_arguments)]
pub async fn insight_links(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    db: &Db,
    owner: Id,
    space: &Space,
    role: Role,
    raw: Vec<Value>,
) -> Result<Vec<(EdgeType, TargetRef)>, ApiError> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    if raw.len() > LINKS_MAX {
        return Err(unprocessable(format!(
            "links: не больше {LINKS_MAX} на запись"
        )));
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LinkReq {
        #[serde(rename = "type")]
        kind: String,
        target: String,
    }
    let visible = graph_spaces(p, db, space, role).await?;
    let mut out = Vec::with_capacity(raw.len());
    for (i, v) in raw.into_iter().enumerate() {
        let field = format!("links[{i}]");
        let l: LinkReq = serde_json::from_value(v).map_err(|e| {
            unprocessable(format!(
                "{field}: {e} (ожидается {{type, target: node:<id>|entry:<id>|[[Имя]]}})"
            ))
        })?;
        let kind = explicit_type(&l.kind, &format!("{field}.type"))?;
        let dst = parse_dst(&l.target).ok_or_else(|| {
            unprocessable(format!("{field}.target: node:<id>, entry:<id> или [[Имя]]"))
        })?;
        let r = resolve_dst(st, p, headers, db, owner, space.id, &visible, dst, &field).await?;
        out.push((kind, r));
    }
    Ok(out)
}

#[derive(Deserialize)]
pub struct GraphQuery {
    node: Option<String>,
    entry: Option<String>,
    depth: Option<u8>,
    types: Option<String>,
}

/// `types=a,b` → типы (пусто — все); неизвестный — 400.
fn parse_types(raw: Option<&str>) -> Result<Vec<EdgeType>, ApiError> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            EdgeType::parse(s)
                .ok_or_else(|| ApiError::BadRequest(format!("types: неизвестный тип «{s}»")))
        })
        .collect()
}

/// Ссылка из `node`/`entry` запроса: ровно одно из них.
fn start_ref(node: Option<&str>, entry: Option<&str>) -> Result<TargetRef, ApiError> {
    let (kind, raw) = match (node, entry) {
        (Some(n), None) => (TargetKind::Node, n),
        (None, Some(e)) => (TargetKind::Entry, e),
        _ => {
            return Err(ApiError::BadRequest(
                "нужен ровно один из параметров node и entry".into(),
            ));
        }
    };
    Ok(TargetRef {
        kind,
        id: super::parse_id(raw)?,
    })
}

/// `GET /graph?node=|entry=&depth=&types=` → `{nodes, edges, truncated}` (`truncated` —
/// подграф обрезан пределами `zam_store::GRAPH_MAX_NODES` / `GRAPH_MAX_EDGES`).
pub async fn graph(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<GraphQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let start = start_ref(q.node.as_deref(), q.entry.as_deref())?;
    let depth = q.depth.unwrap_or(1);
    if !(1..=GRAPH_DEPTH_MAX).contains(&depth) {
        return Err(ApiError::BadRequest(format!(
            "depth: от 1 до {GRAPH_DEPTH_MAX}"
        )));
    }
    let types = parse_types(q.types.as_deref())?;
    let t = live_target(&st, &p, &headers, start, Mode::Read).await?;
    let spaces = graph_spaces(&p, &t.db, &t.space, t.role).await?;
    let g = t.db.neighbors(&[t.r], depth, &types, &spaces).await?;
    Ok(Json(
        json!({ "nodes": g.nodes, "edges": g.edges, "truncated": g.truncated }),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewEdgeReq {
    src: String,
    #[serde(rename = "type")]
    kind: String,
    dst: String,
}

/// `POST /edges {src, type, dst}` → 201 ребро.
pub async fn create(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<EdgeView>), ApiError> {
    let req: NewEdgeReq = json_body(body)?;
    let src =
        parse_ref(&req.src).ok_or_else(|| ApiError::BadRequest("src: n:<id> или e:<id>".into()))?;
    let dst = parse_dst(&req.dst)
        .ok_or_else(|| ApiError::BadRequest("dst: n:<id>, e:<id> или [[Имя]]".into()))?;
    let kind = explicit_type(&req.kind, "type")?;
    let t = live_target(&st, &p, &headers, src, Mode::Write).await?;
    let visible = graph_spaces(&p, &t.db, &t.space, t.role).await?;
    let dst = resolve_dst(
        &st, &p, &headers, &t.db, t.owner, t.space.id, &visible, dst, "dst",
    )
    .await?;
    let a = attribution(&p, &headers, None)?;
    let e = t.db.add_edge(NewEdge { src, dst, kind }, &a).await?;
    Ok((StatusCode::CREATED, Json(e)))
}

/// `DELETE /edges/{id}` → 204; только `explicit`.
pub async fn remove(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let es = edge_scope(&st, &p, &headers, &id, Mode::Write).await?;
    let visible = graph_spaces(&p, &es.db, &es.space, es.role).await?;
    if es.edge.dst_space.is_some_and(|s| !visible.contains(&s)) {
        return Err(ApiError::NotFound);
    }
    if es.edge.origin != EdgeOrigin::Explicit {
        return Err(unprocessable(format!(
            "удаляются только явные связи; {} — следует из текста или замещения",
            es.edge.origin.as_str()
        )));
    }
    let a = attribution(&p, &headers, None)?;
    es.db.remove_edge(es.edge.id, &a).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `{items, nodes}`: рёбра и узлы их других концов (`other` — какой конец).
async fn edges_reply(
    db: &Db,
    items: Vec<EdgeView>,
    other: impl Fn(&EdgeView) -> Option<TargetRef>,
) -> Result<Json<Value>, ApiError> {
    let refs: Vec<TargetRef> = items.iter().filter_map(&other).collect();
    let nodes = graph_nodes(db, refs).await?;
    Ok(Json(json!({ "items": items, "nodes": nodes })))
}

async fn backlinks_of(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    r: TargetRef,
) -> Result<Json<Value>, ApiError> {
    let t = live_target(st, p, headers, r, Mode::Read).await?;
    let spaces = graph_spaces(p, &t.db, &t.space, t.role).await?;
    let items = t.db.backlinks(t.r, &spaces).await?;
    edges_reply(&t.db, items, |e| Some(e.src)).await
}

/// `GET /nodes/{id}/backlinks`.
pub async fn node_backlinks(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let r = TargetRef {
        kind: TargetKind::Node,
        id: super::parse_id(&id)?,
    };
    backlinks_of(&st, &p, &headers, r).await
}

/// `GET /entries/{id}/backlinks`.
pub async fn entry_backlinks(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let r = TargetRef {
        kind: TargetKind::Entry,
        id: super::parse_id(&id)?,
    };
    backlinks_of(&st, &p, &headers, r).await
}

/// `GET /nodes/{id}/links` — исходящие связи узла, в том числе неразрешённые.
pub async fn node_links(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let r = TargetRef {
        kind: TargetKind::Node,
        id: super::parse_id(&id)?,
    };
    links_of(&st, &p, &headers, r).await
}

/// `GET /entries/{id}/links` — исходящие связи записи журнала.
pub async fn entry_links(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let r = TargetRef {
        kind: TargetKind::Entry,
        id: super::parse_id(&id)?,
    };
    links_of(&st, &p, &headers, r).await
}

/// Исходящие рёбра цели в видимых принципалу пространствах.
async fn links_of(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    r: TargetRef,
) -> Result<Json<Value>, ApiError> {
    let t = live_target(st, p, headers, r, Mode::Read).await?;
    let spaces = graph_spaces(p, &t.db, &t.space, t.role).await?;
    let items = t.db.outgoing(t.r, &spaces).await?;
    edges_reply(&t.db, items, |e| e.dst).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_and_dst_forms() {
        let id = Id::new();
        for s in [format!("n:{id}"), format!("node:{id}"), format!(" n:{id} ")] {
            assert_eq!(
                parse_ref(&s),
                Some(TargetRef {
                    kind: TargetKind::Node,
                    id
                })
            );
        }
        assert_eq!(
            parse_ref(&format!("entry:{id}")).map(|r| r.kind),
            Some(TargetKind::Entry)
        );
        for bad in ["", "x:1", "n:", "мусор", "[[a]]"] {
            assert!(parse_ref(bad).is_none(), "{bad}");
        }
        assert!(matches!(parse_dst("[[Имя#раздел|текст]]"), Some(Dst::Wiki(k)) if k == "[[Имя]]"));
        assert!(parse_dst("[[ ]]").is_none());
        assert!(matches!(parse_dst(&format!("e:{id}")), Some(Dst::Ref(_))));
    }

    #[test]
    fn types_and_explicit() {
        assert_eq!(
            parse_types(Some("links_to, relates_to,")).unwrap(),
            vec![EdgeType::LinksTo, EdgeType::RelatesTo]
        );
        assert!(parse_types(Some("blocks")).is_err());
        assert!(parse_types(None).unwrap().is_empty());
        assert!(matches!(
            explicit_type("supersedes", "type"),
            Err(ApiError::Unprocessable(_))
        ));
        assert_eq!(explicit_type("fixes", "type").unwrap(), EdgeType::Fixes);
    }
}
