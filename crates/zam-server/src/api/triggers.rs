//! `GET /triggers?solution=<guid>` (§10.3, §12) — действующие триггеры для кэша клиента:
//! включённые триггеры insight со статусом `active`/`revisit` из пространства доступного
//! решения (в базе его владельца) и из Knowledge пользователя запроса. Чужой Knowledge,
//! Rules и Human не участвуют никогда.
//!
//! - Решение — параметр `solution` или заголовок `X-Zam-Solution`; без него — только
//!   Knowledge. Доступ — как у пространства (`space_scope`, чтение): ожидание, отклонение
//!   или отзыв доступа — 403 `/problems/solution-access`, неизвестное решение — 404,
//!   не GUID — 400. Решения о доступе не кэшируются.
//! - Ответ `{etag, triggers: [{id, entry_id, type, pattern, tools, title, insight_kind,
//!   certainty, space}]}`; `space` — GUID решения или `knowledge`. Порядок: решение, затем
//!   Knowledge; внутри — по записи и порядку добавления.
//! - `ETag` — blake3 канонического JSON массива `triggers` (поля в фиксированном порядке);
//!   `If-None-Match` с тем же значением (или `*`) → 304 без тела. Сопоставление — в CLI (M4).
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use zam_core::{Id, SolutionGuid};
use zam_store::{Certainty, SpaceKind, TriggerKind, TriggerRow, active_triggers, space_of_kind};

use super::query;
use super::scope::{Mode, space_scope, user_db};
use super::search::SOLUTION_HEADER;
use crate::app::AppState;
use crate::auth::Principal;
use crate::error::ApiError;

#[derive(Deserialize)]
pub struct TriggersQuery {
    solution: Option<String>,
}

/// Элемент ответа (порядок полей — часть канонического JSON для `ETag`).
#[derive(Serialize)]
struct Item<'a> {
    id: Id,
    entry_id: Id,
    #[serde(rename = "type")]
    kind: TriggerKind,
    pattern: &'a str,
    tools: &'a Option<Vec<String>>,
    title: &'a str,
    insight_kind: &'a str,
    certainty: Option<Certainty>,
    space: &'a str,
}

/// GUID решения запроса: параметр, иначе `X-Zam-Solution`; не GUID → 400.
fn solution_of(q: &TriggersQuery, headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let raw = match q
        .solution
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(s) => Some(s.to_string()),
        None => headers
            .get(SOLUTION_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    };
    raw.map(|s| {
        s.parse::<SolutionGuid>()
            .map(|g| g.as_str().to_string())
            .map_err(|_| ApiError::BadRequest("solution: GUID решения (UUID с дефисами)".into()))
    })
    .transpose()
}

/// `If-None-Match` совпадает с `etag` (список значений, слабые `W/`, `*`).
fn not_modified(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().trim_start_matches("W/").trim_matches('"'))
        .any(|t| t == "*" || t == etag)
}

/// `GET /triggers` → 200 `{etag, triggers}` с `ETag` / 304.
pub async fn list(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<TriggersQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let q = query(q)?;
    let guid = solution_of(&q, &headers)?;
    // (строки, пространство в ответе)
    let mut groups: Vec<(Vec<TriggerRow>, String)> = Vec::new();
    if let Some(g) = &guid {
        let sc = space_scope(&st, &p, &headers, g, Mode::Read).await?;
        groups.push((active_triggers(&sc.db, &[sc.space.id]).await?, g.clone()));
    }
    let (_, db) = user_db(&st, &p, &headers).await?;
    let k = space_of_kind(&db, SpaceKind::Knowledge).await?;
    groups.push((active_triggers(&db, &[k.id]).await?, "knowledge".into()));
    let items: Vec<Item> = groups
        .iter()
        .flat_map(|(rows, space)| {
            rows.iter().map(move |t| Item {
                id: t.id,
                entry_id: t.entry_id,
                kind: t.kind,
                pattern: &t.pattern,
                tools: &t.tools,
                title: &t.title,
                insight_kind: &t.insight_kind,
                certainty: t.certainty,
                space,
            })
        })
        .collect();
    let canon = serde_json::to_vec(&items).map_err(|e| ApiError::Internal(e.to_string()))?;
    let etag = blake3::hash(&canon).to_hex().to_string();
    let quoted = HeaderValue::from_str(&format!("\"{etag}\""))
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let mut r = if not_modified(&headers, &etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        axum::Json(serde_json::json!({ "etag": etag, "triggers": items })).into_response()
    };
    let h = r.headers_mut();
    h.insert(header::ETAG, quoted);
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn if_none_match_forms() {
        let h = |v: &str| {
            let mut m = HeaderMap::new();
            m.insert(header::IF_NONE_MATCH, HeaderValue::from_str(v).unwrap());
            m
        };
        assert!(not_modified(&h("\"abc\""), "abc"));
        assert!(not_modified(&h("W/\"abc\""), "abc"));
        assert!(not_modified(&h("\"x\", \"abc\""), "abc"));
        assert!(not_modified(&h("*"), "abc"));
        assert!(!not_modified(&h("\"abd\""), "abc"));
        assert!(!not_modified(&HeaderMap::new(), "abc"));
    }
}
