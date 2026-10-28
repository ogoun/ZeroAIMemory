//! Сравнение релизов (§7.4): `GET /releases/{id}/compare?with=prev|<id>`.
//!
//! Ответ: `{from: Entry|null, to: Entry, changelog: {<kind>: [Entry…]}, notes_diff, commits}`.
//! - интервал — записи changelog с `created_at` в `(from.created_at, to.created_at]`,
//!   сгруппированы по `data.kind` (пустые группы опущены);
//! - `with=prev` (по умолчанию) — предыдущий живой релиз пространства по `created_at`; у
//!   первого релиза его нет, и интервал начинается с начала журнала (`from: null`)
//!   [решение исполнителя]; `with=<id>` — релиз того же пространства;
//! - `notes_diff` — унифицированный дифф текстов релизов (`from` отсутствует — от пустого);
//! - `commits` — `to.data.commits`.
//!
//! Доступ — как у записи `to` (`entry_scope`); `with` ищется в той же базе и пространстве,
//! иначе 404.
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use zam_store::{
    CHANGELOG_BETWEEN_MAX, Entry, EntryFilter, EntryKind, changelog_between, get_entry,
    list_entries,
};

use super::entries::CHANGELOG_KINDS;
use super::nodes::diff_blocking;
use super::query;
use super::scope::{Mode, entry_scope, live_entry};
use crate::app::AppState;
use crate::auth::Principal;
use crate::error::ApiError;

#[derive(Deserialize)]
pub struct CompareQuery {
    with: Option<String>,
}

/// Живой релиз.
fn release_of(e: Entry) -> Result<Entry, ApiError> {
    if e.deleted_at.is_some() {
        return Err(ApiError::NotFound);
    }
    if e.kind != EntryKind::Release {
        return Err(ApiError::Unprocessable("запись не является релизом".into()));
    }
    Ok(e)
}

/// Группировка changelog по `data.kind`; неизвестный вид — в `other`.
fn group_by_kind(items: Vec<Entry>) -> Result<Value, ApiError> {
    let mut groups: Map<String, Value> = Map::new();
    for e in items {
        let k = e
            .data
            .get("kind")
            .and_then(Value::as_str)
            .filter(|k| CHANGELOG_KINDS.contains(k))
            .unwrap_or("other");
        let v = serde_json::to_value(&e)
            .map_err(|e| ApiError::Internal(format!("сериализация записи: {e}")))?;
        groups
            .entry(k)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("массив")
            .push(v);
    }
    Ok(Value::Object(groups))
}

/// `GET /releases/{id}/compare?with=prev|<id>`.
pub async fn compare(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    q: Result<Query<CompareQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let sc = entry_scope(&st, &p, &headers, &id, Mode::Read).await?;
    live_entry(&sc)?;
    let to = release_of(sc.entry)?;
    let from = match q.with.as_deref().map(str::trim) {
        None | Some("") | Some("prev") => {
            let f = EntryFilter {
                space_id: to.space_id,
                kind: Some(EntryKind::Release),
                statuses: Vec::new(),
                certainty: Vec::new(),
                area: None,
                tag: None,
                data_kind: None,
                decided_by: None,
                since: None,
                until: Some(to.created_at),
                cursor: None,
                limit: 1,
            };
            list_entries(&sc.db, f).await?.0.into_iter().next()
        }
        Some(raw) => {
            let other = get_entry(&sc.db, super::parse_id(raw)?).await?;
            if other.space_id != to.space_id {
                return Err(ApiError::NotFound);
            }
            Some(release_of(other)?)
        }
    };
    let items = changelog_between(
        &sc.db,
        to.space_id,
        from.as_ref().map(|f| f.created_at),
        to.created_at,
    )
    .await?;
    let truncated = items.len() >= CHANGELOG_BETWEEN_MAX;
    let old = from.as_ref().map(|f| f.body.clone()).unwrap_or_default();
    let label = |e: &Entry| {
        e.data
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("release")
            .to_string()
    };
    let old_label = from.as_ref().map_or_else(|| "(начало)".to_string(), label);
    let notes_diff = diff_blocking(old, to.body.clone(), old_label, label(&to)).await?;
    let commits = to.data.get("commits").cloned().unwrap_or_else(|| json!([]));
    Ok(Json(json!({
        "from": from,
        "to": to,
        "changelog": group_by_kind(items)?,
        "truncated": truncated,
        "notes_diff": notes_diff,
        "commits": commits,
    })))
}
