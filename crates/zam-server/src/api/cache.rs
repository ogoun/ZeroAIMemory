//! Кэш выжимок агентов (§9.3 шаг 3, §9.4, §12; задача 7 M4).
//!
//! ```text
//! POST   /cache {query, summary, sources: [{id, rev?, section?}], solution?, scope?, filters?,
//!                include_history?, promote?: {as: note|insight, path?, kind?}}   → 201
//! GET    /cache?q=&solution=&session=&state=valid|stale&limit=                → {items}
//! GET    /cache/{id}                                                          → выжимка
//! DELETE /cache/{id}                                                          → 204
//! POST   /cache/{id}/promote {as: note|insight, path?, kind?}                 → 201
//! ```
//!
//! - **Пространство кэша**: `solution` (поле, `scope.solution` или `X-Zam-Solution`) —
//!   пространство решения в базе владельца: кэш общий на решение, участнику — только при
//!   действующем доступе (чтение — `ro`, запись — `rw`, иначе 403 `solution-access`); без
//!   решения — Knowledge пользователя запроса. Ключ выжимки — `scope_hash` области поиска
//!   (`scope`, `filters`, `include_history` — как в `POST /search`; по умолчанию — область
//!   поиска по умолчанию с этим решением) и нормализованный запрос **[решение исполнителя]**:
//!   так поиск с той же областью находит выжимку точным совпадением; `scope.spaces` и
//!   `all_solutions` ключ не описывает — 400. Новая выжимка заменяет прежнюю того же ключа
//!   только того же автора (агент или человек, пользователь) и той же сессии (fix round 1);
//! - **источники** (`id` — как в `POST /read`: `n:`, `i:|c:|r:|e:`, голый id, путь
//!   `<пространство>:/<путь>`): должны лежать в базе пространства кэша (иначе 422
//!   `/problems/cache-cross-db`); Human и Vault — 422 `/problems/cache-forbidden-source`;
//!   нет источника, он удалён, папка или нет секции — 422 `/problems/cache-source-not-found`;
//!   insight `superseded` или `retracted` — 422 `/problems/cache-source-inactive`;
//!   `rev` не совпал с текущей — 409 с `rev` (выжимка основана на устаревшем тексте).
//!   Участник решения ссылается только на пространство решения (остальное владельца ему не
//!   существует). Сервер закрепляет ревизии и хэши (`zam_store::cache_put`);
//! - выжимка длиннее `cache.summary_tokens` (600) — 413; dense-вектор запроса — эмбеддером
//!   сервера, без модели — только точное попадание;
//! - **видимость**: выжимка решения с источником вне пространства решения (владелец сослался
//!   на свой Knowledge) участнику не выдаётся — ни в списке, ни по id, ни в поиске;
//! - `DELETE` — автор, владелец пространства (владелец базы) или администратор, мягко;
//! - `promote` — заметка (`path`, по умолчанию `Выжимки/<запрос>`) или insight (`kind`, по
//!   умолчанию `discovery`; заголовок — запрос) в пространстве кэша со связями `derived_from`
//!   на источники, у выжимки — `promoted_to`; `POST /cache` с `promote` сохраняет и
//!   превращает одной транзакцией (409 на занятом пути — выжимки нет);
//! - `query_vec` хранится с `model_id` текущей модели: вектор другой модели не сравнивается.
//!
//! Изменяющие маршруты (`POST /cache`, `DELETE`, `promote`) — в `memory_routes` (решение
//! контроллера R12: счётчик `memory_writes` сессии), чтение — служебное.
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use zam_core::{Id, SolutionGuid, TargetKind, TargetRef};
use zam_store::{
    CacheError, CacheFilter, CacheHit, CachePut, Db, PromoteAs, SourceSpec, Space, SpaceKind,
    StoreError, TITLE_MAX_CHARS, cache_delete, cache_get, cache_list, cache_promote,
    cache_put_with, count_tokens, get_entry, get_node, get_space, vault_get_any,
};

use super::scope::{MemScope, Mode, Role, foreign_solutions, owner_db, space_scope, user_db};
use super::search::targets::{ScopeReq, request_solution};
use super::search::{Filters, cache_scope_hash};
use super::{json_body, parse_id, query};
use crate::app::AppState;
use crate::attrib::{AREA_HEADER, MACHINE_HEADER, SESSION_HEADER, attribution, header_value};
use crate::auth::Principal;
use crate::error::ApiError;

/// Выжимок в списке по умолчанию и предел.
const LIST_DEFAULT: usize = 50;
const LIST_MAX: usize = 500;
/// Папка заметок-выжимок по умолчанию.
const PROMOTE_FOLDER: &str = "Выжимки";
/// Предел имени заметки из запроса, символов.
const PROMOTE_NAME_MAX: usize = 80;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    id: String,
    rev: Option<i64>,
    section: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Promote {
    #[serde(rename = "as")]
    to: String,
    path: Option<String>,
    kind: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutReq {
    query: String,
    summary: String,
    sources: Vec<Source>,
    solution: Option<String>,
    #[serde(default)]
    scope: ScopeReq,
    #[serde(default)]
    filters: Filters,
    #[serde(default)]
    include_history: bool,
    promote: Option<Promote>,
}

/// Ссылка выжимки в Markdown: `c_<id>`.
pub fn cache_ref(id: Id) -> String {
    format!("c_{id}")
}

/// Id выжимки из пути: `c_<id>` или голый id; иначе 400.
fn parse_cache_id(raw: &str) -> Result<Id, ApiError> {
    let raw = raw.trim();
    parse_id(raw.strip_prefix("c_").unwrap_or(raw))
}

/// Ошибка сохранения → problem.
fn cache_error(e: CacheError) -> ApiError {
    match e {
        CacheError::Store(e) => e.into(),
        e @ CacheError::ForbiddenSource(_) => ApiError::CacheSource {
            kind: "/problems/cache-forbidden-source",
            detail: e.to_string(),
        },
        e @ CacheError::SourceNotFound { .. } => ApiError::CacheSource {
            kind: "/problems/cache-source-not-found",
            detail: e.to_string(),
        },
        e @ CacheError::SourceInactive { .. } => ApiError::CacheSource {
            kind: "/problems/cache-source-inactive",
            detail: e.to_string(),
        },
        e @ CacheError::SourceChanged { current_rev, .. } => {
            let mut extra = Map::new();
            extra.insert("rev".into(), json!(current_rev));
            ApiError::Conflict {
                detail: e.to_string(),
                extra,
            }
        }
    }
}

fn forbidden_source(raw: &str) -> ApiError {
    cache_error(CacheError::ForbiddenSource(raw.to_string()))
}

fn cross_db(raw: &str) -> ApiError {
    ApiError::CacheSource {
        kind: "/problems/cache-cross-db",
        detail: format!(
            "источник {raw} лежит в другой базе: выжимка и её источники — в одной базе \
             (решение — в базе владельца, личное — в своей)"
        ),
    }
}

fn not_found(raw: &str) -> ApiError {
    cache_error(CacheError::SourceNotFound {
        target: raw.to_string(),
        detail: "не найден".into(),
    })
}

/// Цель по сырой ссылке в базе `db` (в том числе Human, удалённая): узел, запись или Vault.
enum Raw {
    Target(TargetRef, SpaceKind),
    Vault,
    Missing,
}

async fn raw_lookup(db: &Db, prefix: Option<&str>, id: Id) -> Result<Raw, ApiError> {
    let space_kind = async |space: Id| -> Result<SpaceKind, ApiError> {
        get_space(db, space)
            .await?
            .map(|s| s.kind)
            .ok_or(ApiError::NotFound)
    };
    if matches!(prefix, None | Some("n")) {
        match get_node(db, id).await {
            Ok(n) => {
                let k = space_kind(n.space_id).await?;
                return Ok(Raw::Target(
                    TargetRef {
                        kind: TargetKind::Node,
                        id,
                    },
                    k,
                ));
            }
            Err(StoreError::NotFound) => {}
            Err(e) => return Err(e.into()),
        }
    }
    if prefix != Some("n") {
        match get_entry(db, id).await {
            Ok(e) => {
                let k = space_kind(e.space_id).await?;
                return Ok(Raw::Target(
                    TargetRef {
                        kind: TargetKind::Entry,
                        id,
                    },
                    k,
                ));
            }
            Err(StoreError::NotFound) => {}
            Err(e) => return Err(e.into()),
        }
    }
    if prefix.is_none() || prefix == Some("v") {
        match vault_get_any(db, id).await {
            Ok(_) => return Ok(Raw::Vault),
            Err(StoreError::NotFound) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(Raw::Missing)
}

/// Источник выжимки в базе пространства кэша `sc` (см. модуль).
async fn resolve_source(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    sc: &MemScope,
    raw: &str,
) -> Result<TargetRef, ApiError> {
    let raw = raw.trim();
    // путь `<пространство>:/<путь>`
    if let Some((space, path)) = raw.split_once(":/")
        && !space.is_empty()
    {
        if space == "human" {
            return Err(forbidden_source(raw));
        }
        let path = zam_store::NodePath::parse(path).map_err(ApiError::from)?;
        let at = match space_scope(st, p, headers, space, Mode::Read).await {
            Ok(at) => at,
            Err(ApiError::NotFound) => return Err(not_found(raw)),
            Err(e) => return Err(e),
        };
        let node = match zam_store::get_node_by_path(&at.db, at.space.id, &path).await {
            Ok(n) => n,
            Err(StoreError::NotFound) => return Err(not_found(raw)),
            Err(e) => return Err(e.into()),
        };
        if at.owner != sc.owner {
            return Err(cross_db(raw));
        }
        return Ok(TargetRef {
            kind: TargetKind::Node,
            id: node.id,
        });
    }
    let (prefix, rest) = match raw.split_once(':') {
        Some((k @ ("n" | "i" | "c" | "r" | "e" | "v"), rest)) => (Some(k), rest),
        _ => (None, raw),
    };
    let Ok(id) = rest.parse::<Id>() else {
        return Err(not_found(raw));
    };
    let prefix = match prefix {
        Some("i" | "c" | "r" | "e") => Some("e"),
        other => other,
    };
    // база кэша — целиком, кроме участника: ему видно только пространство решения
    let member = matches!(sc.role, Role::Member(_));
    let (me, own) = user_db(st, p, headers).await?;
    let mut dbs: Vec<(Id, Db)> = Vec::new();
    if !member {
        dbs.push((sc.owner, sc.db.clone()));
    }
    if me.id != sc.owner || member {
        dbs.push((me.id, own));
    }
    for (owner, db) in &dbs {
        match raw_lookup(db, prefix, id).await? {
            Raw::Vault => return Err(forbidden_source(raw)),
            Raw::Target(_, SpaceKind::Human) => return Err(forbidden_source(raw)),
            Raw::Target(t, _) if *owner == sc.owner && !member => return Ok(t),
            Raw::Target(..) => return Err(cross_db(raw)),
            Raw::Missing => {}
        }
    }
    // чужие базы, доступные запросу: решения, где пользователь — участник
    let as_node = async || {
        super::scope::node_scope(st, p, headers, rest, Mode::Read)
            .await
            .map(|s| (s.owner, s.space.id, TargetKind::Node))
    };
    let as_entry = async || {
        super::scope::entry_scope(st, p, headers, rest, Mode::Read)
            .await
            .map(|s| (s.owner, s.space.id, TargetKind::Entry))
    };
    let found = match prefix {
        Some("e") => as_entry().await,
        Some("n") => as_node().await,
        _ => match as_node().await {
            Err(ApiError::NotFound) => as_entry().await,
            r => r,
        },
    };
    match found {
        Ok((owner, space, kind)) if owner == sc.owner && space == sc.space.id => {
            Ok(TargetRef { kind, id })
        }
        Ok(_) => Err(cross_db(raw)),
        Err(ApiError::NotFound) => Err(not_found(raw)),
        Err(e) => Err(e),
    }
}

/// Пространство кэша запроса: решение (база владельца) или Knowledge пользователя.
pub(crate) async fn cache_space(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    solution: Option<&SolutionGuid>,
    mode: Mode,
) -> Result<MemScope, ApiError> {
    match solution {
        Some(g) => space_scope(st, p, headers, g.as_str(), mode).await,
        None => space_scope(st, p, headers, "knowledge", mode).await,
    }
}

/// Пространства, видимые запросу в базе кэша: участнику — только решение.
fn visible_spaces(sc_role: Role, space: &Space) -> Option<Vec<Id>> {
    matches!(sc_role, Role::Member(_)).then(|| vec![space.id])
}

/// JSON выжимки: поля записи, итог проверки источников, автор и пространство.
pub async fn cache_json(st: &AppState, h: &CacheHit, space: Option<&Space>) -> Value {
    let e = &h.entry;
    let author = match st.system.get_user(e.author_user_id).await {
        Ok(Some(u)) => Some(u.name),
        _ => None,
    };
    let sources: Vec<Value> = e
        .sources
        .iter()
        .map(|d| json!({"id": d.target, "rev": d.rev, "section": d.section}))
        .collect();
    let mut o = json!({
        "id": e.id,
        "ref": cache_ref(e.id),
        "query": e.query,
        "summary": e.summary,
        "tokens": e.tokens,
        "state": if h.is_valid() { "valid" } else { "stale" },
        "stale": h.stale,
        "sources": sources,
        "author_user_id": e.author_user_id,
        "author": author,
        "session_id": e.session_id,
        "machine": e.machine,
        "area": e.area,
        "created_at": zam_store::time::fmt_ts(e.created_at),
        "last_used_at": zam_store::time::fmt_ts(e.last_used_at),
        "hits": e.hits,
        "promoted_to": e.promoted_to,
        "space_id": e.space_id,
    });
    if let (Some(s), Value::Object(m)) = (space, &mut o) {
        m.insert("space".into(), json!(s.name));
        if let Some(g) = &s.solution_guid {
            m.insert("agent_solution_id".into(), json!(g));
        }
    }
    o
}

/// Во что превратить: `note` (путь или `Выжимки/<запрос>`) или `insight` (вид).
fn promote_as(pr: &Promote, query: &str) -> Result<PromoteAs, ApiError> {
    match pr.to.as_str() {
        "note" => {
            let raw = match pr.path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(p) => p.to_string(),
                None => {
                    let name: String = query
                        .chars()
                        .map(|c| {
                            if c == '/' || c == '\\' || c.is_control() {
                                ' '
                            } else {
                                c
                            }
                        })
                        .take(PROMOTE_NAME_MAX)
                        .collect();
                    format!("{PROMOTE_FOLDER}/{}", name.trim())
                }
            };
            let path = zam_store::NodePath::parse(&raw)
                .map_err(|e| ApiError::BadRequest(format!("path: {e}")))?;
            Ok(PromoteAs::Note { path })
        }
        "insight" => {
            let kind = pr.kind.as_deref().unwrap_or("discovery");
            if !super::insights::INSIGHT_KINDS.contains(&kind) {
                return Err(ApiError::BadRequest(format!(
                    "kind: одно из {}",
                    super::insights::INSIGHT_KINDS.join(", ")
                )));
            }
            let title: String = query.chars().take(TITLE_MAX_CHARS).collect();
            Ok(PromoteAs::Insight {
                kind: kind.to_string(),
                title,
            })
        }
        other => Err(ApiError::BadRequest(format!(
            "as: note или insight, а не «{other}»"
        ))),
    }
}

async fn do_promote(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    sc: &MemScope,
    id: Id,
    pr: &Promote,
    query: &str,
) -> Result<Value, ApiError> {
    let to = promote_as(pr, query)?;
    let a = attribution(p, headers, None)?;
    let (_, created) = cache_promote(&sc.db, id, to, &a).await?;
    let h = cache_get(&sc.db, id).await?.ok_or(ApiError::NotFound)?;
    let mut v = cache_json(st, &h, Some(&sc.space)).await;
    if let Value::Object(m) = &mut v {
        m.insert("promoted".into(), json!(created));
    }
    Ok(v)
}

/// Dense-вектор запроса эмбеддером сервера; нет модели или сбой — `None` (WARN без текста).
async fn query_vec(st: &AppState, q: &str) -> Option<Vec<f32>> {
    let emb = st.index.embedder.clone()?;
    let q = q.to_string();
    match tokio::task::spawn_blocking(move || emb.embed_query(&q)).await {
        Ok(Ok(e)) => Some(e.dense),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "кэш выжимок: эмбеддинг запроса не удался");
            None
        }
        Err(j) => {
            tracing::warn!(
                panic = j.is_panic(),
                "кэш выжимок: эмбеддинг запроса прерван"
            );
            None
        }
    }
}

/// `POST /cache`.
pub async fn put(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut req: PutReq = json_body(body)?;
    let q = req.query.trim().to_string();
    if q.is_empty() {
        return Err(ApiError::BadRequest("query пуст".into()));
    }
    if q.chars().count() > zam_store::CACHE_QUERY_MAX_CHARS {
        return Err(ApiError::BadRequest(format!(
            "query длиннее {} символов",
            zam_store::CACHE_QUERY_MAX_CHARS
        )));
    }
    if req.summary.trim().is_empty() {
        return Err(ApiError::BadRequest("summary пуст".into()));
    }
    let tokens = count_tokens(&req.summary);
    let max = st.cfg.cache.summary_tokens;
    if tokens > max {
        return Err(ApiError::PayloadTooLarge(format!(
            "выжимка — {tokens} токенов, предел cache.summary_tokens = {max}"
        )));
    }
    if req.sources.is_empty() {
        return Err(ApiError::BadRequest(
            "sources: хотя бы один источник".into(),
        ));
    }
    if req.sources.len() > zam_store::CACHE_SOURCES_MAX {
        return Err(ApiError::BadRequest(format!(
            "sources: не больше {} источников",
            zam_store::CACHE_SOURCES_MAX
        )));
    }
    // ключ кэша не описывает явный список пространств и «все решения» (§9.4)
    if req.scope.spaces.is_some() || req.scope.all_solutions {
        return Err(ApiError::BadRequest(
            "scope.spaces и scope.all_solutions для выжимки не поддерживаются: \
             кэш — по решению или Knowledge"
                .into(),
        ));
    }
    if req.solution.is_some() {
        req.scope.solution = req.solution.clone();
    }
    let solution = request_solution(req.scope.solution.as_deref(), &headers)?;
    let sc = cache_space(&st, &p, &headers, solution.as_ref(), Mode::Write).await?;
    let scope_hash = cache_scope_hash(
        &req.scope,
        &req.filters,
        req.include_history,
        solution.as_ref(),
    )?;
    let mut sources = Vec::with_capacity(req.sources.len());
    for s in &req.sources {
        let target = resolve_source(&st, &p, &headers, &sc, &s.id).await?;
        sources.push(SourceSpec {
            target,
            rev: s.rev,
            section: s.section.clone(),
        });
    }
    let promote = match &req.promote {
        Some(pr) => Some((promote_as(pr, &q)?, attribution(&p, &headers, None)?)),
        None => None,
    };
    let author_kind = attribution(&p, &headers, None)?.author_kind;
    let (query_vec, model_id) = match query_vec(&st, &q).await {
        Some(v) => (Some(v), st.index.model_id().map(str::to_string)),
        None => (None, None),
    };
    let put = CachePut {
        space_id: sc.space.id,
        area: header_value(&headers, AREA_HEADER)?
            .or_else(|| zam_store::normalize_area(req.scope.area.as_deref())),
        author_user_id: sc.user.id,
        author_kind,
        session_id: header_value(&headers, SESSION_HEADER)?,
        machine: header_value(&headers, MACHINE_HEADER)?,
        scope_hash,
        query_vec,
        model_id,
        query: q.clone(),
        summary: req.summary.clone(),
        tokens: tokens as i64,
        sources,
    };
    // сохранение и превращение — одна транзакция: сбой превращения не оставляет выжимку
    let (e, made) = cache_put_with(&sc.db, &put, promote)
        .await
        .map_err(cache_error)?;
    let h = cache_get(&sc.db, e.id).await?.ok_or(ApiError::NotFound)?;
    let mut v = cache_json(&st, &h, Some(&sc.space)).await;
    if let (Some(t), Value::Object(m)) = (made, &mut v) {
        m.insert("promoted".into(), json!(t));
    }
    Ok((StatusCode::CREATED, Json(v)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    q: Option<String>,
    solution: Option<String>,
    session: Option<String>,
    state: Option<String>,
    limit: Option<usize>,
}

/// `GET /cache`: выжимки своих областей (с `solution` — решения, иначе личной базы).
pub async fn list(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let limit = q.limit.unwrap_or(LIST_DEFAULT);
    if !(1..=LIST_MAX).contains(&limit) {
        return Err(ApiError::BadRequest(format!("limit: от 1 до {LIST_MAX}")));
    }
    let valid = match q.state.as_deref() {
        None | Some("") => None,
        Some("valid") => Some(true),
        Some("stale") => Some(false),
        Some(_) => return Err(ApiError::BadRequest("state: valid или stale".into())),
    };
    let solution = q
        .solution
        .as_deref()
        .map(|g| {
            g.trim()
                .parse::<SolutionGuid>()
                .map_err(|_| ApiError::BadRequest("solution: GUID решения".into()))
        })
        .transpose()?;
    let mut f = CacheFilter {
        q: q.q,
        session_id: q.session,
        valid,
        limit,
        ..CacheFilter::default()
    };
    let (db, spaces) = match &solution {
        Some(g) => {
            let sc = space_scope(&st, &p, &headers, g.as_str(), Mode::Read).await?;
            f.space_ids = Some(vec![sc.space.id]);
            f.visible_spaces = visible_spaces(sc.role, &sc.space);
            (sc.db, vec![sc.space])
        }
        None => {
            let (_, db) = user_db(&st, &p, &headers).await?;
            let spaces = zam_store::list_spaces(&db).await?;
            (db, spaces)
        }
    };
    let hits = cache_list(&db, &f).await?;
    let mut items = Vec::with_capacity(hits.len());
    for h in &hits {
        let space = spaces.iter().find(|s| s.id == h.entry.space_id);
        items.push(cache_json(&st, h, space).await);
    }
    Ok(Json(json!({ "items": items })))
}

/// Выжимка по id в доступной области: личная база, затем базы чужих решений (только
/// выжимки пространства решения). Участнику выжимка с источником вне решения не существует.
async fn locate(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    raw: &str,
    mode: Mode,
) -> Result<(MemScope, CacheHit), ApiError> {
    let id = parse_cache_id(raw)?;
    let (user, db) = user_db(st, p, headers).await?;
    if let Some(h) = cache_get(&db, id).await? {
        let sc = space_scope(st, p, headers, &h.entry.space_id.to_string(), mode).await?;
        return Ok((sc, h));
    }
    for (info, _) in foreign_solutions(st, p, &user).await? {
        let Some(odb) = owner_db(st, &info).await else {
            continue;
        };
        let h = match cache_get(&odb, id).await {
            Ok(Some(h)) if h.entry.space_id == info.space_id => h,
            Ok(_) => continue,
            Err(e) => {
                super::scope::skip_foreign(&info, &e);
                continue;
            }
        };
        let sc = space_scope(st, p, headers, info.guid.as_str(), mode).await?;
        if let Some(v) = visible_spaces(sc.role, &sc.space)
            && !h.source_spaces.iter().all(|s| v.contains(s))
        {
            return Err(ApiError::NotFound);
        }
        return Ok((sc, h));
    }
    Err(ApiError::NotFound)
}

/// `GET /cache/{id}`.
pub async fn get(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (sc, h) = locate(&st, &p, &headers, &id, Mode::Read).await?;
    Ok(Json(cache_json(&st, &h, Some(&sc.space)).await))
}

/// `DELETE /cache/{id}`: автор, владелец пространства или администратор.
pub async fn remove(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let (sc, h) = locate(&st, &p, &headers, &id, Mode::Write).await?;
    let allowed = matches!(sc.role, Role::Admin | Role::Owner | Role::Personal)
        || h.entry.author_user_id == sc.user.id;
    if !allowed {
        return Err(ApiError::Denied(
            "удалить выжимку может автор, владелец пространства или администратор".into(),
        ));
    }
    cache_delete(&sc.db, h.entry.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /cache/{id}/promote`.
pub async fn promote(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let pr: Promote = json_body(body)?;
    let (sc, h) = locate(&st, &p, &headers, &id, Mode::Write).await?;
    let v = do_promote(&st, &p, &headers, &sc, h.entry.id, &pr, &h.entry.query).await?;
    Ok((StatusCode::CREATED, Json(v)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_ids_and_promote_forms() {
        let id = Id::new();
        assert_eq!(parse_cache_id(&cache_ref(id)).unwrap(), id);
        assert_eq!(parse_cache_id(&id.to_string()).unwrap(), id);
        assert!(parse_cache_id("c_мусор").is_err());
        let pr = |to: &str, path: Option<&str>, kind: Option<&str>| Promote {
            to: to.into(),
            path: path.map(Into::into),
            kind: kind.map(Into::into),
        };
        match promote_as(&pr("note", None, None), "как / деплоим").unwrap() {
            PromoteAs::Note { path } => assert_eq!(path.as_str(), "Выжимки/как   деплоим"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            promote_as(&pr("insight", None, None), "q").unwrap(),
            PromoteAs::Insight { kind, .. } if kind == "discovery"
        ));
        assert!(promote_as(&pr("insight", None, Some("x")), "q").is_err());
        assert!(promote_as(&pr("folder", None, None), "q").is_err());
    }
}
