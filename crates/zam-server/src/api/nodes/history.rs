//! Ревизии заметки: список, текст ревизии, дифф и откат.
//!
//! История удалённого узла доступна (как в `zam-store`); откат — только живой заметки.
//!
//! Агенту по токену (задача 5 M7.1, [`crate::api::agent_view`]) текст ревизии, обе стороны диффа
//! (каждая ревизия — маской до диффа), сообщения правок списка ревизий и узел ответа отката — с
//! `‹secret›` вместо значений; сессии WebUI — исходник.
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use zam_store::{RevisionInfo, list_revisions, node_content, revert_note};

use super::read::markdown_response;
use super::{audit_node, diff_blocking};
use crate::api::agent_view;
use crate::api::scope::{Mode, live, node_scope};
use crate::api::{json_body, query};
use crate::app::AppState;
use crate::attrib::attribution;
use crate::auth::{Client, Principal};
use crate::error::ApiError;

/// `GET /nodes/{id}/revisions` → `[RevisionInfo]`, новые первыми.
pub async fn revisions(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Vec<RevisionInfo>>, ApiError> {
    let sc = node_scope(&st, &p, &headers, &id, Mode::Read).await?;
    let mut revs = list_revisions(&sc.db, sc.node.id).await?;
    // сообщение правки — свободный текст автора
    for m in revs.iter_mut().filter_map(|r| r.message.as_mut()) {
        agent_view::view_in_place(&p, m);
    }
    Ok(Json(revs))
}

#[derive(Deserialize)]
pub struct RevQuery {
    format: Option<String>,
}

/// `GET /nodes/{id}/revisions/{rev}?format=md|json`: `md` — байты ревизии с `ETag`,
/// `json` (по умолчанию) — `{node_id, rev, content}`.
pub async fn revision(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path((id, rev)): Path<(String, String)>,
    q: Result<Query<RevQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let q = query(q)?;
    let rev: i64 = rev
        .parse()
        .map_err(|_| ApiError::BadRequest("номер ревизии — целое число".into()))?;
    let md = match q.format.as_deref().unwrap_or("json") {
        "md" => true,
        "json" => false,
        _ => return Err(ApiError::BadRequest("format: md или json".into())),
    };
    let sc = node_scope(&st, &p, &headers, &id, Mode::Read).await?;
    let (rev, text) = node_content(&sc.db, sc.node.id, Some(rev)).await?;
    let text = agent_view::view_owned(&p, text).await?;
    Ok(if md {
        markdown_response(text, rev)
    } else {
        Json(json!({ "node_id": sc.node.id, "rev": rev, "content": text })).into_response()
    })
}

#[derive(Deserialize)]
pub struct DiffQuery {
    from: Option<i64>,
    to: Option<i64>,
}

/// `GET /nodes/{id}/diff?from=&to=` → унифицированный дифф (`text/plain`).
/// По умолчанию `to` — текущая ревизия, `from` — предыдущая перед `to`.
pub async fn diff(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    q: Result<Query<DiffQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let q = query(q)?;
    let sc = node_scope(&st, &p, &headers, &id, Mode::Read).await?;
    let to = q.to.unwrap_or(sc.node.current_rev);
    let from = q.from.unwrap_or(to - 1);
    let (_, new) = node_content(&sc.db, sc.node.id, Some(to)).await?;
    // ревизии 0 нет: дифф от пустого текста
    let old = if from == 0 {
        String::new()
    } else {
        node_content(&sc.db, sc.node.id, Some(from)).await?.1
    };
    // агенту — каждая ревизия маской до диффа
    let (old, new) = (
        agent_view::view_owned(&p, old).await?,
        agent_view::view_owned(&p, new).await?,
    );
    let mut r = diff_blocking(old, new, format!("rev {from}"), format!("rev {to}"))
        .await?
        .into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    Ok(r)
}

#[derive(Deserialize)]
pub struct Revert {
    rev: i64,
    message: Option<String>,
}

/// `POST /nodes/{id}/revert {rev, message?}` → 200 `Node` (новая ревизия с текстом `rev`);
/// аудит `node.revert`.
pub async fn revert(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let sc = node_scope(&st, &p, &headers, &id, Mode::Write).await?;
    live(&sc)?;
    let req: Revert = json_body(body)?;
    let attrib = attribution(&p, &headers, req.message)?;
    let from_rev = sc.node.current_rev;
    let node = revert_note(&sc.db, sc.node.id, req.rev, attrib).await?;
    st.index.notify();
    let details = json!({ "space": sc.space.id, "to_rev": req.rev, "from_rev": from_rev, "new_rev": node.current_rev });
    audit_node(
        &st,
        &p,
        &client,
        sc.user.id,
        sc.owner,
        "node.revert",
        node.id,
        details,
    )
    .await?;
    Ok(Json(agent_view::node_json(&p, &node)?))
}
