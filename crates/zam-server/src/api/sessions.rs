//! Сессии агентов (§6.3, §10.3, §12; задача 5 M4).
//!
//! - `POST /sessions {id?, agent?, solution?, area?, parent_id?, agent_type?}` → 201 `{id}`:
//!   без `id` — UUIDv7; существующая сессия обновляется (известные поля дописываются,
//!   `last_seen_at` — сейчас). Машина и токен — из `X-Zam-Machine` и принципала. `parent_id` и
//!   `agent_type` (до 128 печатных символов) — сессия субагента (M4.5 §9.4): её создаёт хук
//!   `subagent-start` с id `<session>:<agent_id>`;
//! - `PATCH /sessions/{id} {ended: true|false}` → 204 (`false` снимает отметку конца);
//! - `GET /sessions/{id}` → `{id, agent, solution, area, machine, parent_id, agent_type,
//!   started_at, last_seen_at, ended_at, memory_writes, claimed_tasks: [{id, title, status}]}`
//!   (решение контроллера R6, читает хук `checkpoint`; `claimed_tasks` — задачи журнала,
//!   захваченные сессией, M4.5);
//! - `GET /sessions?solution=&limit=` → `{items}` — свои, свежие первыми (50, до 500).
//!
//! Сессия лежит в базе пользователя запроса (у администратора — `X-Zam-User` или он сам):
//! чужая сессия — 404. Помимо явного `POST` сессия создаётся первым запросом с
//! `X-Zam-Session` (поиск, `/read`, `/recall`) и записью в память ([`count_memory_writes`]).
//! Токен `ro` сессии ведёт: это служебный учёт, а не запись в память.
//!
//! **Счётчик записей в память** (решение контроллера R1) — одна точка: слой
//! [`count_memory_writes`] на маршрутах записи в память (узлы, вложения, записи журналов,
//! связи, Vault). Обязательство (решение контроллера R12): **любой** новый путь записи в
//! память идёт через маршруты `memory_routes` (`api::router`) — `cache_put` и `promote`
//! задачи 7 добавляются туда же, а MCP (задача 9) вызывает эти REST-маршруты через внутренний
//! `Router` (`oneshot`) и потому считается тем же слоем; запись в память в обход маршрутов
//! (прямой вызов `zam_store`) счётчик не увидит. Изменяющий запрос с `X-Zam-Session`, завершившийся
//! успехом (2xx), увеличивает `memory_writes` этой сессии в базе пользователя запроса; сессии
//! нет — она создаётся. Неудачная запись и чтение не считаются. Принципал слой получает из
//! [`PrincipalSlot`], который заполняет извлечение обработчика, — токен не проверяется дважды.
//! Сбой счётчика ответ не портит (WARN без id сессии и текста).
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use zam_core::SolutionGuid;
use zam_store::{
    Db, SessionTouch, add_memory_write, claimed_tasks, end_session, get_session, list_sessions,
    touch_session,
};

use super::search::targets::{ScopeReq, resolve_targets};
use super::{json_body, query};
use crate::app::AppState;
use crate::attrib::{MACHINE_HEADER, SESSION_HEADER, header_value, session_touch};
use crate::auth::{Principal, PrincipalSlot};
use crate::error::ApiError;

/// Сессий в списке по умолчанию.
const LIST_DEFAULT: usize = 50;
/// Предел списка сессий.
const LIST_MAX: usize = 500;

/// База пользователя запроса (без проверки существования: принципал уже проверен).
async fn requester_db(st: &AppState, p: &Principal, headers: &HeaderMap) -> Result<Db, ApiError> {
    let user = crate::auth::target_user(st, p, headers).await?;
    Ok(st.users.get(user.id).await?)
}

/// Сессия запроса (касание по запросу) и база пользователя запроса, где она лежит.
pub struct RequestSession {
    pub touch: SessionTouch,
    pub db: Db,
}

/// Сессия запроса без записи: `session_id` из тела сильнее `X-Zam-Session`; `None` — запрос
/// без сессии. Некорректный id — 400.
pub async fn request_session(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    body_session: Option<&str>,
) -> Result<Option<RequestSession>, ApiError> {
    let Some(touch) = session_touch(p, headers, body_session)? else {
        return Ok(None);
    };
    // несуществующий пользователь (`X-Zam-User` администратора) — 404, а не новая пустая база
    // `users/<id>.db` (финальное ревью M4, M-3)
    let user = crate::auth::target_user(st, p, headers).await?;
    let db = st.users.get(user.id).await?;
    Ok(Some(RequestSession { touch, db }))
}

/// Отмечает обращение сессии запроса (оценка, `/read`, `/recall`): сессия создаётся, если её
/// нет ([`request_session`] и `touch_session`).
pub async fn touch_from_request(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    body_session: Option<&str>,
) -> Result<Option<RequestSession>, ApiError> {
    let Some(s) = request_session(st, p, headers, body_session).await? else {
        return Ok(None);
    };
    touch_session(&s.db, &s.touch).await?;
    Ok(Some(s))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewSession {
    id: Option<String>,
    agent: Option<String>,
    solution: Option<String>,
    area: Option<String>,
    parent_id: Option<String>,
    agent_type: Option<String>,
}

/// `POST /sessions`.
pub async fn create(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let req: NewSession = json_body(body)?;
    let solution_guid = req
        .solution
        .as_deref()
        .map(|g| {
            g.trim()
                .parse::<SolutionGuid>()
                .map_err(|_| ApiError::BadRequest("solution: GUID решения".into()))
        })
        .transpose()?;
    let blank = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let touch = SessionTouch {
        id: blank(req.id).unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
        token_id: p.token_id(),
        solution_guid,
        area: blank(req.area),
        machine: header_value(&headers, MACHINE_HEADER)?,
        agent: blank(req.agent),
        parent_id: blank(req.parent_id),
        agent_type: blank(req.agent_type),
    };
    let db = requester_db(&st, &p, &headers).await?;
    touch_session(&db, &touch).await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": touch.id }))))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchSession {
    ended: bool,
}

/// `PATCH /sessions/{id}`.
pub async fn patch(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<StatusCode, ApiError> {
    let req: PatchSession = json_body(body)?;
    let db = requester_db(&st, &p, &headers).await?;
    if !end_session(&db, &id, req.ended).await? {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /sessions/{id}` — сессия и `claimed_tasks: [{id, title, status}]` (задачи, захваченные
/// сессией: в её базе и в базе решения сессии, M4.5 §3). Сбой чтения задач — пустой список
/// (WARN без текста): сессию он не портит.
pub async fn get(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let db = requester_db(&st, &p, &headers).await?;
    let s = get_session(&db, &id).await?.ok_or(ApiError::NotFound)?;
    let tasks = claimed_tasks_of(&st, &p, &headers, &db, &s).await;
    let mut v = serde_json::to_value(&s).map_err(|e| ApiError::Internal(e.to_string()))?;
    v["claimed_tasks"] = json!(tasks);
    Ok(Json(v))
}

/// Задачи, захваченные сессией `s`: личная база и база решения сессии (если оно доступно).
async fn claimed_tasks_of(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    own: &Db,
    s: &zam_store::Session,
) -> Vec<zam_store::ClaimedTask> {
    let now = jiff::Timestamp::now();
    let mut dbs: Vec<Db> = vec![own.clone()];
    if let Some(g) = &s.solution_guid {
        let scope = ScopeReq {
            solution: Some(g.clone()),
            ..ScopeReq::default()
        };
        match resolve_targets(st, p, headers, &scope).await {
            Ok((targets, _)) => {
                for t in targets {
                    if t.solution.is_some() {
                        dbs.push(t.db);
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "сессия: база решения для задач недоступна"),
        }
    }
    let mut out = Vec::new();
    for db in &dbs {
        match claimed_tasks(db, &s.id, now).await {
            Ok(v) => out.extend(v),
            Err(e) => tracing::warn!(error = %e, "сессия: захваченные задачи не прочитаны"),
        }
    }
    out
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    solution: Option<String>,
    limit: Option<usize>,
}

/// `GET /sessions`.
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
    let solution = q
        .solution
        .as_deref()
        .map(|g| {
            g.trim()
                .parse::<SolutionGuid>()
                .map_err(|_| ApiError::BadRequest("solution: GUID решения".into()))
        })
        .transpose()?;
    let db = requester_db(&st, &p, &headers).await?;
    let items = list_sessions(&db, solution.as_ref(), limit).await?;
    Ok(Json(json!({ "items": items })))
}

/// Слой маршрутов записи в память: успешный изменяющий запрос с `X-Zam-Session` —
/// `memory_writes + 1` у этой сессии (см. модуль).
pub async fn count_memory_writes(
    State(st): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let write = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    // некорректный заголовок отклонит сам обработчик (атрибуция, 400)
    if !write || !matches!(header_value(req.headers(), SESSION_HEADER), Ok(Some(_))) {
        return next.run(req).await;
    }
    let headers = req.headers().clone();
    let slot = PrincipalSlot::default();
    req.extensions_mut().insert(slot.clone());
    let resp = next.run(req).await;
    if !resp.status().is_success() {
        return resp;
    }
    if let Some(p) = slot.take()
        && let Err(e) = count(&st, &p, &headers).await
    {
        tracing::warn!(error = %e, "счётчик записей в память сессии не обновлён");
    }
    resp
}

async fn count(st: &AppState, p: &Principal, headers: &HeaderMap) -> Result<(), ApiError> {
    let Some(touch) = session_touch(p, headers, None)? else {
        return Ok(());
    };
    let user = crate::auth::target_user(st, p, headers).await?;
    let db = st.users.get(user.id).await?;
    add_memory_write(&db, &touch).await?;
    Ok(())
}
