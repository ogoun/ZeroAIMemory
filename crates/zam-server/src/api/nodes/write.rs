//! Запись узлов: создание, замена (`If-Match`), PATCH секций, перенос, удаление в
//! корзину, восстановление и список корзины.
//!
//! Ответ записи заметки (создание, замена, PATCH) — `Node` и непустые `warnings`:
//! `certainty_not_recognized` — во frontmatter ключ `certainty`/`evidence` в нераспознаваемом
//! написании (в кавычках, с заглавными буквами): такой ключ статус не задаёт (§7.5);
//! `unknown relation type: <тип>` и `invalid relation: <строка>` — строка `relations:`, не
//! давшая ребра графа (§8.5); `term_alias: «<псевдоним>» → «<имя>»` — в имени или тексте
//! заметки псевдоним термина словаря вместо имени (M4.5 §2.2, до 5); `secrets_detected` (+
//! `secrets_detected: [{kind, line}]`) — в тексте найдены секреты, в индекс они уходят
//! замаскированными (§7.6, [`crate::api::secrets`]); `verified_at_invalid` — `verified_at:`
//! не дата `YYYY-MM-DD`; `invalid_ref: <путь> (<причина>)` — путь `refs:` не стал ссылкой на
//! файл; `refs_truncated` — ссылок больше 64 (M4.5 §6.1, §7).
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
    node_content, patch_note_within, restore_node, update_note,
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

/// Предупреждение: ключ `certainty`/`evidence` во frontmatter написан так, что не распознан.
pub const CERTAINTY_NOT_RECOGNIZED: &str = "certainty_not_recognized";

/// Предупреждения записи заметки по её тексту.
fn note_warnings(content: &str) -> Vec<String> {
    let doc = zam_markdown::parse(content);
    let mut out = Vec::new();
    if let Some(f) = &doc.frontmatter {
        if !zam_markdown::unrecognized_certainty_keys(&f.raw).is_empty() {
            out.push(CERTAINTY_NOT_RECOGNIZED.to_string());
        }
        // `verified_at_invalid` и ссылки на файлы, которые не стали ссылками (M4.5 §6.1, §7)
        out.extend(f.warnings.iter().cloned());
        let mut valid = 0;
        for r in &f.refs {
            match zam_store::normalize_ref_path(r) {
                Ok(_) => valid += 1,
                Err(e) => out.push(format!("invalid_ref: {r} ({e})")),
            }
        }
        if valid > zam_store::file_refs::REFS_MAX {
            out.push(format!(
                "refs_truncated: учтены первые {}",
                zam_store::file_refs::REFS_MAX
            ));
        }
    }
    out.extend(zam_store::links::relation_warnings(&doc));
    out
}

/// `Node` как JSON с `warnings` (только непустые); `db` и `space` — база и пространство
/// заметки для предупреждений о псевдонимах словаря (кэш словаря — в `st`).
async fn node_reply(
    st: &AppState,
    db: &zam_store::Db,
    space: zam_core::Id,
    node: &Node,
    content: &str,
) -> Result<Value, ApiError> {
    let mut v = serde_json::to_value(node).map_err(|e| ApiError::Internal(e.to_string()))?;
    let mut w = note_warnings(content);
    if !content.is_empty() {
        w.extend(crate::api::glossary::alias_warnings(st, db, space, &node.name, content).await?);
    }
    if !w.is_empty()
        && let Value::Object(o) = &mut v
    {
        o.insert("warnings".into(), json!(w));
    }
    crate::api::secrets::attach(&mut v, content);
    Ok(v)
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
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let req: Create = json_body(body)?;
    let path = NodePath::parse(&req.path)?;
    let sc = space_scope(&st, &p, &headers, &req.space, Mode::Write).await?;
    let (node, content) = match req.kind {
        Kind::Folder => {
            if req.content.is_some() || req.l0.is_some() {
                return Err(ApiError::BadRequest("у папки нет content и l0".into()));
            }
            (
                create_folder(&sc.db, sc.space.id, path).await?,
                String::new(),
            )
        }
        Kind::Note => {
            let content = req.content.unwrap_or_default();
            check_size(&st, &content)?;
            let attrib = attribution(&p, &headers, req.message)?;
            let n = NewNote {
                space_id: sc.space.id,
                path,
                content: content.clone(),
                l0: req.l0,
                attrib,
            };
            (create_note(&sc.db, n).await?, content)
        }
    };
    st.render_gen.bump_space(sc.owner, sc.space.id);
    st.index.notify();
    Ok((
        StatusCode::CREATED,
        Json(node_reply(&st, &sc.db, sc.space.id, &node, &content).await?),
    ))
}

/// Ревизия из `If-Match`: `3`, `"3"` или `W/"3"`.
pub(crate) fn if_match(headers: &HeaderMap) -> Result<i64, ApiError> {
    let v = headers.get(header::IF_MATCH).ok_or_else(|| {
        ApiError::PreconditionRequired("передайте If-Match: <rev> текущей ревизии".into())
    })?;
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
) -> Result<Json<Value>, ApiError> {
    let sc = node_scope(&st, &p, &headers, &id, Mode::Write).await?;
    live(&sc)?;
    let base = if_match(&headers)?;
    let req: Replace = json_body(body)?;
    check_size(&st, &req.content)?;
    let attrib = attribution(&p, &headers, req.message)?;
    let content = req.content.clone();
    match update_note(&sc.db, sc.node.id, base, req.content, req.l0, attrib).await {
        Ok(n) => {
            st.index.notify();
            Ok(Json(
                node_reply(&st, &sc.db, sc.space.id, &n, &content).await?,
            ))
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
) -> Result<Json<Value>, ApiError> {
    let sc = node_scope(&st, &p, &headers, &id, Mode::Write).await?;
    live(&sc)?;
    let req: Patch = json_body(body)?;
    let attrib = attribution(&p, &headers, req.message)?;
    let max = st.cfg.limits.note_max_bytes;
    match patch_note_within(&sc.db, sc.node.id, req.base_rev, req.ops, attrib, max).await {
        Ok(n) => {
            st.index.notify();
            let (_, text) = node_content(&sc.db, n.id, Some(n.current_rev)).await?;
            Ok(Json(node_reply(&st, &sc.db, sc.space.id, &n, &text).await?))
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
    let author = attribution(&p, &headers, None)?.author_kind;
    let node = move_node(&sc.db, sc.node.id, path, author).await?;
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
    let author = attribution(&p, &headers, None)?.author_kind;
    let count = delete_node(&sc.db, sc.node.id, author).await?;
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
    let author = attribution(&p, &headers, None)?.author_kind;
    let node = restore_node(&sc.db, sc.node.id, author).await?;
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
        assert!(matches!(
            if_match(&h),
            Err(ApiError::PreconditionRequired(_))
        ));
        for (v, want) in [("3", 3), ("\"3\"", 3), ("W/\"12\"", 12), (" 4 ", 4)] {
            h.insert(header::IF_MATCH, HeaderValue::from_static(v));
            assert_eq!(if_match(&h).unwrap(), want, "{v}");
        }
        h.insert(header::IF_MATCH, HeaderValue::from_static("*"));
        assert!(matches!(if_match(&h), Err(ApiError::BadRequest(_))));
    }
}
