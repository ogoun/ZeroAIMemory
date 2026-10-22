//! Запись узлов: создание, замена (`If-Match`), PATCH секций, перенос, удаление в
//! корзину, восстановление и список корзины.
//!
//! Пределы: текст заметки при создании и замене и итог PATCH — не больше
//! `limits.note_max_bytes` (иначе 413, ничего не записано); тело запроса ограничено
//! слоем `DefaultBodyLimit` (см. `api::router`).
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use serde::Deserialize;
use serde_json::{Value, json};
use zam_markdown::PatchOp;
use zam_store::{
    NewNote, Node, NodePath, create_folder, create_note, delete_node, list_trash, move_node,
    patch_note_within, restore_node, update_note,
};

use super::{audit_node, write_error};
use crate::api::scope::{Mode, live, node_scope, space_scope, user_db};
use crate::api::spaces::readable_space_ids;
use crate::api::{json_body, query};
use crate::app::AppState;
use crate::attrib::attribution;
use crate::auth::{Client, Principal};
use crate::error::ApiError;

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Note,
    Folder,
}

#[derive(Deserialize)]
pub struct Create {
    space: String,
    path: String,
    kind: Kind,
    content: Option<String>,
    l0: Option<String>,
    message: Option<String>,
}

fn check_size(st: &AppState, content: &str) -> Result<(), ApiError> {
    let max = st.cfg.limits.note_max_bytes;
    if content.len() > max {
        return Err(ApiError::PayloadTooLarge(format!(
            "заметка — {} байт, предел {max}",
            content.len()
        )));
    }
    Ok(())
}

/// `POST /nodes` → 201 `Node`.
pub async fn create(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<Node>), ApiError> {
    let req: Create = json_body(body)?;
    let path = NodePath::parse(&req.path)?;
    let sc = space_scope(&st, &p, &headers, &req.space, Mode::Write).await?;
    let node = match req.kind {
        Kind::Folder => {
            if req.content.is_some() || req.l0.is_some() {
                return Err(ApiError::BadRequest("у папки нет content и l0".into()));
            }
            create_folder(&sc.db, sc.space.id, path).await?
        }
        Kind::Note => {
            let content = req.content.unwrap_or_default();
            check_size(&st, &content)?;
            let attrib = attribution(&p, &headers, req.message)?;
            let n = NewNote {
                space_id: sc.space.id,
                path,
                content,
                l0: req.l0,
                attrib,
            };
            create_note(&sc.db, n).await?
        }
    };
    st.render_gen.bump_space(sc.owner, sc.space.id);
    st.index.notify();
    Ok((StatusCode::CREATED, Json(node)))
}

/// Ревизия из `If-Match`: `3`, `"3"` или `W/"3"`.
pub(crate) fn if_match(headers: &HeaderMap) -> Result<i64, ApiError> {
    let v = headers
        .get(header::IF_MATCH)
        .ok_or(ApiError::PreconditionRequired)?;
    let s = v.to_str().unwrap_or("").trim();
    let s = s.strip_prefix("W/").unwrap_or(s);
    s.trim_matches('"')
        .parse::<i64>()
        .map_err(|_| ApiError::BadRequest("If-Match: номер ревизии".into()))
}

#[derive(Deserialize)]
pub struct Replace {
    content: String,
    l0: Option<String>,
    message: Option<String>,
}

/// `PUT /nodes/{id}` с `If-Match: <rev>` → 200 `Node`; без заголовка — 428;
/// устаревшая ревизия — 409 с `current_rev` и `diff`.
pub async fn replace(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Node>, ApiError> {
    let sc = node_scope(&st, &p, &headers, &id, Mode::Write).await?;
    live(&sc)?;
    let base = if_match(&headers)?;
    let req: Replace = json_body(body)?;
    check_size(&st, &req.content)?;
    let attrib = attribution(&p, &headers, req.message)?;
    match update_note(&sc.db, sc.node.id, base, req.content, req.l0, attrib).await {
        Ok(n) => {
            st.index.notify();
            Ok(Json(n))
        }
        Err(e) => Err(write_error(&sc.db, sc.node.id, base, e).await),
    }
}

#[derive(Deserialize)]
pub struct Patch {
    base_rev: i64,
    ops: Vec<PatchOp>,
    message: Option<String>,
}

/// `PATCH /nodes/{id} {base_rev, ops, message?}` → 200 `Node` / 409 / 413 / 422.
pub async fn patch(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Node>, ApiError> {
    let sc = node_scope(&st, &p, &headers, &id, Mode::Write).await?;
    live(&sc)?;
    let req: Patch = json_body(body)?;
    let attrib = attribution(&p, &headers, req.message)?;
    let max = st.cfg.limits.note_max_bytes;
    match patch_note_within(&sc.db, sc.node.id, req.base_rev, req.ops, attrib, max).await {
        Ok(n) => {
            st.index.notify();
            Ok(Json(n))
        }
        Err(e) => Err(write_error(&sc.db, sc.node.id, req.base_rev, e).await),
    }
}

#[derive(Deserialize)]
pub struct Move {
    path: String,
}

/// `POST /nodes/{id}/move {path}` → 200 `Node`.
pub async fn move_to(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Node>, ApiError> {
    let sc = node_scope(&st, &p, &headers, &id, Mode::Write).await?;
    live(&sc)?;
    let req: Move = json_body(body)?;
    let path = NodePath::parse(&req.path)?;
    let node = move_node(&sc.db, sc.node.id, path).await?;
    st.render_gen.bump_space(sc.owner, sc.space.id);
    // свёртка L0 старого родителя: задания индексации знают только новое положение
    if let Some(old) = sc.node.parent_id
        && node.parent_id != Some(old)
    {
        st.index.rollup_hint(sc.owner, old);
    }
    st.index.notify();
    Ok(Json(node))
}

/// `DELETE /nodes/{id}` — мягкое удаление поддерева → 204; аудит `node.delete`.
pub async fn remove(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let sc = node_scope(&st, &p, &headers, &id, Mode::Write).await?;
    let count = delete_node(&sc.db, sc.node.id).await?;
    st.render_gen.bump_space(sc.owner, sc.space.id);
    st.index.notify();
    let details = json!({ "space": sc.space.id, "path": sc.node.path, "nodes": count });
    audit_node(
        &st,
        &p,
        &client,
        sc.user.id,
        sc.owner,
        "node.delete",
        sc.node.id,
        details,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /nodes/{id}/restore` → 200 `Node`; аудит `node.restore`.
pub async fn restore(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
) -> Result<Json<Node>, ApiError> {
    let sc = node_scope(&st, &p, &headers, &id, Mode::Write).await?;
    let node = restore_node(&sc.db, sc.node.id).await?;
    st.render_gen.bump_space(sc.owner, sc.space.id);
    st.index.notify();
    let details = json!({ "space": sc.space.id, "path": node.path });
    audit_node(
        &st,
        &p,
        &client,
        sc.user.id,
        sc.owner,
        "node.restore",
        node.id,
        details,
    )
    .await?;
    Ok(Json(node))
}

#[derive(Deserialize)]
pub struct TrashQuery {
    space: Option<String>,
}

/// `GET /trash?space=` → `{items}`: корни пакетов удаления, новые первыми.
/// Без `space` — все пространства, доступные принципалу (по токену — без Human).
pub async fn trash(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<TrashQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let items = match q.space.as_deref() {
        Some(s) => {
            let sc = space_scope(&st, &p, &headers, s, Mode::Read).await?;
            list_trash(&sc.db, Some(sc.space.id)).await?
        }
        None => {
            let (_, db) = user_db(&st, &p, &headers).await?;
            let allowed = readable_space_ids(&db, &p).await?;
            let mut all = list_trash(&db, None).await?;
            all.retain(|n| allowed.contains(&n.space_id));
            all
        }
    };
    Ok(Json(json!({ "items": items })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn if_match_forms() {
        let mut h = HeaderMap::new();
        assert!(matches!(if_match(&h), Err(ApiError::PreconditionRequired)));
        for (v, want) in [("3", 3), ("\"3\"", 3), ("W/\"12\"", 12), (" 4 ", 4)] {
            h.insert(header::IF_MATCH, HeaderValue::from_static(v));
            assert_eq!(if_match(&h).unwrap(), want, "{v}");
        }
        h.insert(header::IF_MATCH, HeaderValue::from_static("*"));
        assert!(matches!(if_match(&h), Err(ApiError::BadRequest(_))));
    }
}
