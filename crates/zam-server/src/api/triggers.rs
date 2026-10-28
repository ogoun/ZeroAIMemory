//! `GET /triggers?solution=<guid>` (§10.3, §12) — действующие триггеры для кэша клиента:
//! включённые триггеры insight со статусом `active`/`revisit` из пространства доступного
//! решения (в базе его владельца) и из Knowledge пользователя запроса. Чужой Knowledge,
//! Rules и Human не участвуют никогда.
//!
//! - Решение — параметр `solution` или заголовок `X-Zam-Solution`; без него — только
//!   Knowledge. Как в поиске (`search::targets`), недоступное решение не отказывает всему
//!   ответу, а исключается с предупреждением (решение владельца 2026-10-26; в M3 было 403
//!   на весь ответ): ожидание, отклонение или отзыв доступа — `solution access
//!   pending|rejected|revoked`; решение не зарегистрировано или у пользователя нет запроса
//!   доступа — `solution unknown` (существование чужого решения не раскрывается); база
//!   владельца не читается — `solution unavailable`. Остаётся Knowledge пользователя.
//!   Параметр `solution` не GUID — 400; некорректный заголовок игнорируется. Решения о
//!   доступе не кэшируются.
//! - Ответ `{etag, triggers: [{id, entry_id, type, pattern, tools, title, insight_kind,
//!   certainty, space}], warnings}`; `space` — GUID решения или `knowledge`. Порядок: решение, затем
//!   Knowledge; внутри — по записи и порядку добавления.
//!   Ответ также несёт `warnings` (массив, пустой без предупреждений).
//! - `ETag` — blake3 канонического JSON `{triggers, warnings}` (поля в фиксированном порядке):
//!   изменение состава предупреждений меняет ETag; `If-None-Match` с тем же значением (или `*`) → 304 без тела. Сопоставление — в CLI (M4).
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use zam_core::{Id, SolutionGuid};
use zam_store::{
    Certainty, Db, SolutionRole, Space, SpaceKind, TriggerKind, TriggerRow, User, active_triggers,
    space_of_kind,
};

use super::query;
use super::scope::{skip_foreign, solution_space, user_db};
use super::search::SOLUTION_HEADER;
use super::search::targets::WARN_SOLUTION_UNAVAILABLE;
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

/// Каноническое представление ответа для `ETag`.
#[derive(Serialize)]
struct Canon<'a> {
    triggers: &'a [Item<'a>],
    warnings: &'a [String],
}

/// Предупреждение: решение не зарегистрировано или у пользователя нет к нему доступа.
const WARN_UNKNOWN: &str = "solution unknown";

/// GUID решения запроса: параметр (не GUID → 400), иначе `X-Zam-Solution` (некорректное
/// значение игнорируется, как в поиске).
fn solution_of(q: &TriggersQuery, headers: &HeaderMap) -> Result<Option<SolutionGuid>, ApiError> {
    if let Some(s) = q
        .solution
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return s
            .parse::<SolutionGuid>()
            .map(Some)
            .map_err(|_| ApiError::BadRequest("solution: GUID решения (UUID с дефисами)".into()));
    }
    Ok(headers
        .get(SOLUTION_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<SolutionGuid>().ok()))
}

/// Пространство решения для чтения или предупреждение, почему его нет.
async fn solution_group(
    st: &AppState,
    p: &Principal,
    user: &User,
    guid: &SolutionGuid,
) -> Result<Result<(Db, Space), &'static str>, ApiError> {
    let Some(info) = st.system.get_solution(guid).await? else {
        return Ok(Err(WARN_UNKNOWN));
    };
    if !p.is_admin() {
        match st.system.solution_role(&info.guid, user.id).await? {
            Some(SolutionRole::Owner | SolutionRole::Member(_)) => {}
            Some(SolutionRole::Pending) => return Ok(Err("solution access pending")),
            Some(SolutionRole::Rejected) => return Ok(Err("solution access rejected")),
            Some(SolutionRole::Revoked) => return Ok(Err("solution access revoked")),
            None => return Ok(Err(WARN_UNKNOWN)),
        }
    }
    match solution_space(st, &info).await {
        Ok(v) => Ok(Ok(v)),
        // пространство решения пропало у владельца — решения для клиента нет
        Err(ApiError::NotFound) => Ok(Err(WARN_UNKNOWN)),
        Err(e) => {
            skip_foreign(&info, &e);
            Ok(Err(WARN_SOLUTION_UNAVAILABLE))
        }
    }
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
    let (user, db) = user_db(&st, &p, &headers).await?;
    // (строки, пространство в ответе)
    let mut groups: Vec<(Vec<TriggerRow>, String)> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    if let Some(g) = &guid {
        match solution_group(&st, &p, &user, g).await? {
            Ok((odb, space)) => {
                groups.push((
                    active_triggers(&odb, &[space.id]).await?,
                    g.as_str().to_string(),
                ));
            }
            Err(w) => warnings.push(w.into()),
        }
    }
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
    let canon = serde_json::to_vec(&Canon {
        triggers: &items,
        warnings: &warnings,
    })
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let etag = blake3::hash(&canon).to_hex().to_string();
    let quoted = HeaderValue::from_str(&format!("\"{etag}\""))
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let mut r = if not_modified(&headers, &etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        axum::Json(serde_json::json!({ "etag": etag, "triggers": items, "warnings": warnings }))
            .into_response()
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
