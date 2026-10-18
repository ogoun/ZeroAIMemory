//! `GET /spaces` — пространства пользователя запроса (§6.3);
//! `GET /spaces/{space}/tree` — дерево узлов пространства.
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{Value, json};
use zam_store::{NodePath, get_node_by_path, list_children, list_spaces};

use super::query;
use super::scope::{Mode, space_scope, token_can_read};
use crate::app::AppState;
use crate::auth::{Principal, target_user};
use crate::error::ApiError;

/// Допустимая глубина выдачи дерева.
const DEPTH_RANGE: std::ops::RangeInclusive<u32> = 1..=5;

pub async fn list(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let user = target_user(&st, &p, &headers).await?;
    let spaces = list_spaces(&st.users.get(user.id).await?).await?;
    let access = match &p {
        Principal::Token(t) => t.access.as_str(),
        Principal::Admin { .. } => "admin",
    };
    Ok(Json(json!({
        "user": { "id": user.id, "name": user.name },
        "access": access,
        "spaces": spaces,
    })))
}

#[derive(Deserialize)]
pub struct TreeQuery {
    #[serde(default)]
    path: String,
    depth: Option<u32>,
    /// `L0` — сводки уже входят в узлы; параметр принимается для совместимости.
    #[allow(dead_code)]
    tier: Option<String>,
}

/// `{space, items}`: узлы под `path` (пусто — корень) до глубины `depth` (1..=5,
/// по умолчанию 1) в прямом порядке обхода; у каждого узла есть `l0`.
pub async fn tree(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(space): Path<String>,
    q: Result<Query<TreeQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let depth = q.depth.unwrap_or(1);
    if !DEPTH_RANGE.contains(&depth) {
        return Err(ApiError::BadRequest("depth: от 1 до 5".into()));
    }
    let sc = space_scope(&st, &p, &headers, &space, Mode::Read).await?;
    let path = q.path.trim_matches('/');
    let parent = if path.is_empty() {
        None
    } else {
        let np = NodePath::parse(path)?;
        Some(get_node_by_path(&sc.db, sc.space.id, &np).await?.id)
    };
    let items = list_children(&sc.db, sc.space.id, parent, depth).await?;
    Ok(Json(json!({ "space": sc.space, "items": items })))
}

/// Пространства, которые принципал может читать (для сводных списков).
pub(crate) async fn readable_space_ids(
    db: &zam_store::Db,
    p: &Principal,
) -> Result<Vec<zam_core::Id>, ApiError> {
    Ok(list_spaces(db)
        .await?
        .into_iter()
        .filter(|s| token_can_read(p, s))
        .map(|s| s.id)
        .collect())
}
