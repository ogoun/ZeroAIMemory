//! Решения и совместный доступ (§7.1, §11.6, §12).
//!
//! - `POST /solutions {agent_solution_id | id, name}`:
//!   - GUID свободен → регистрация в реестре `system.db` (заранее выбранный `space_id`),
//!     затем пространство в базе вызывающего — 201;
//!   - владелец → 200 (пространство создаётся, если его нет после сбоя);
//!   - одобренный участник → 200;
//!   - чужое решение без запроса или с ожидающим → 202 `{agent_solution_id, state:
//!     "pending"}`, своё пространство с этим GUID не создаётся;
//!   - отклонено или отозвано → 403 `/problems/solution-access`.
//!
//!   Гонка двух первых регистраций разрешается реестром: проигравший получает 202.
//!   Регистрирует токен `rw` или администратор.
//! - `GET /solutions` — доступные решения с ролью (администратору — все);
//! - `GET /solutions/{guid}` — пространство решения, `owner`, `role`, `access`;
//! - `GET /solutions/{guid}/access` — владелец любым своим токеном или администратор;
//! - `POST /solutions/{guid}/access/{user}/approve {access?}` (по умолчанию `rw`
//!   **[решение исполнителя]**), `…/reject {reason?}`, `…/revoke` — `rw`-токен владельца
//!   или администратор; недопустимый переход → 409.
//!
//! GUID — UUID с дефисами, хранится в нижнем регистре; в JSON — `agent_solution_id`.
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use zam_core::{Id, SolutionGuid};
use zam_store::{
    Access, AccessDecision, AccessState, SolutionInfo, SolutionRole, Space,
    create_solution_with_id, get_solution, get_space,
};

use super::scope::{Mode, Role, require_rw, solution_space, space_scope};
use super::{json_body, parse_id};
use crate::app::AppState;
use crate::auth::{Principal, target_user};
use crate::error::ApiError;

/// Предел длины имени решения, символов.
const NAME_MAX_CHARS: usize = 128;
/// Предел длины причины отклонения, символов.
const REASON_MAX_CHARS: usize = 500;

#[derive(Deserialize)]
pub struct Register {
    /// Старое имя поля — `id` (M1–M2).
    #[serde(alias = "id")]
    agent_solution_id: String,
    name: String,
}

fn parse_guid(raw: &str) -> Result<SolutionGuid, ApiError> {
    raw.trim()
        .parse()
        .map_err(|_| ApiError::BadRequest("agent_solution_id — UUID с дефисами".into()))
}

fn valid_name(raw: &str) -> Result<&str, ApiError> {
    let name = raw.trim();
    if name.is_empty()
        || name.chars().count() > NAME_MAX_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(ApiError::BadRequest(format!(
            "имя решения: непустое, до {NAME_MAX_CHARS} символов, без управляющих"
        )));
    }
    Ok(name)
}

/// `{id, name}` пользователя (удалённого — только id).
async fn user_ref(st: &AppState, id: Id) -> Result<Value, ApiError> {
    let name = st.system.get_user(id).await?.map(|u| u.name);
    Ok(json!({ "id": id, "name": name }))
}

/// Пространство решения с владельцем, ролью и режимом доступа.
async fn solution_json(
    st: &AppState,
    space: &Space,
    owner: Id,
    role: Role,
) -> Result<Value, ApiError> {
    let mut v = serde_json::to_value(space)
        .map_err(|e| ApiError::Internal(format!("сериализация пространства: {e}")))?;
    if let Value::Object(o) = &mut v {
        o.insert("owner".into(), user_ref(st, owner).await?);
        o.insert("role".into(), json!(role.as_str()));
        o.insert("access".into(), json!(role.access_str()));
    }
    Ok(v)
}

fn reply(status: StatusCode, v: Value) -> Response {
    (status, Json(v)).into_response()
}

pub async fn register(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, ApiError> {
    require_rw(&p)?;
    let req: Register = json_body(body)?;
    let guid = parse_guid(&req.agent_solution_id)?;
    let name = valid_name(&req.name)?;
    let user = target_user(&st, &p, &headers).await?;
    let info = match st.system.get_solution(&guid).await? {
        Some(i) => i,
        None => {
            let db = st.users.get(user.id).await?;
            // локальное пространство с этим GUID без записи в реестре (сбой регистрации при
            // старте) регистрируется со своим id
            let space_id = get_solution(&db, guid.as_str())
                .await?
                .map_or_else(Id::new, |s| s.id);
            let (info, created) = st
                .system
                .register_solution(&guid, user.id, space_id, name)
                .await?;
            if info.owner_id == user.id && info.space_id == space_id {
                let (space, _) = create_solution_with_id(&db, space_id, &guid, &info.name).await?;
                tracing::info!(guid = %guid, user = %user.id, "решение зарегистрировано");
                let status = if created {
                    StatusCode::CREATED
                } else {
                    StatusCode::OK
                };
                return Ok(reply(
                    status,
                    solution_json(&st, &space, user.id, Role::Owner).await?,
                ));
            }
            // гонку первой регистрации выиграл другой пользователь
            info
        }
    };
    if info.owner_id == user.id {
        let db = st.users.get(user.id).await?;
        let space = match get_space(&db, info.space_id).await? {
            Some(s) => s,
            None => {
                tracing::warn!(guid = %guid, "пространство решения владельца пересоздано по реестру");
                create_solution_with_id(&db, info.space_id, &guid, &info.name)
                    .await?
                    .0
            }
        };
        return Ok(reply(
            StatusCode::OK,
            solution_json(&st, &space, user.id, Role::Owner).await?,
        ));
    }
    if matches!(p, Principal::Admin { .. }) {
        let (_, space) = solution_space(&st, &info).await?;
        return Ok(reply(
            StatusCode::OK,
            solution_json(&st, &space, info.owner_id, Role::Admin).await?,
        ));
    }
    let token = match &p {
        Principal::Token(t) => Some(t.token_id),
        Principal::Admin { .. } => None,
    };
    let role = match st.system.solution_role(&guid, user.id).await? {
        None | Some(SolutionRole::Pending) => {
            match st.system.request_access(&guid, user.id, token).await? {
                AccessState::Approved => st.system.solution_role(&guid, user.id).await?,
                AccessState::Pending => Some(SolutionRole::Pending),
                AccessState::Rejected => Some(SolutionRole::Rejected),
                AccessState::Revoked => Some(SolutionRole::Revoked),
            }
        }
        r => r,
    };
    member_reply(&st, &info, role).await
}

/// Ответ `POST /solutions` для чужого решения по роли пользователя.
async fn member_reply(
    st: &AppState,
    info: &SolutionInfo,
    role: Option<SolutionRole>,
) -> Result<Response, ApiError> {
    let denied = |state| ApiError::SolutionAccess {
        guid: info.guid.clone(),
        state,
    };
    match role {
        Some(SolutionRole::Member(a)) => {
            let (_, space) = solution_space(st, info).await?;
            Ok(reply(
                StatusCode::OK,
                solution_json(st, &space, info.owner_id, Role::Member(a)).await?,
            ))
        }
        Some(SolutionRole::Pending) => Ok(reply(
            StatusCode::ACCEPTED,
            json!({ "agent_solution_id": info.guid, "state": "pending" }),
        )),
        Some(SolutionRole::Rejected) => Err(denied("rejected")),
        Some(SolutionRole::Revoked) => Err(denied("revoked")),
        // владелец обработан раньше; запрос только что создан
        Some(SolutionRole::Owner) | None => Err(ApiError::Internal(
            "роль в решении не определилась после запроса доступа".into(),
        )),
    }
}

/// `GET /solutions/{guid}`.
pub async fn get(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(raw): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let guid: SolutionGuid = raw.trim().parse().map_err(|_| ApiError::NotFound)?;
    let sc = space_scope(&st, &p, &headers, guid.as_str(), Mode::Read).await?;
    Ok(Json(
        solution_json(&st, &sc.space, sc.owner, sc.role).await?,
    ))
}

/// Элемент `GET /solutions`.
async fn list_item(st: &AppState, info: &SolutionInfo, role: Role) -> Result<Value, ApiError> {
    Ok(json!({
        "agent_solution_id": info.guid,
        "name": info.name,
        "space_id": info.space_id,
        "owner": user_ref(st, info.owner_id).await?,
        "role": role.as_str(),
        "access": role.access_str(),
        "created_at": zam_store::time::fmt_ts(info.created_at),
    }))
}

/// `GET /solutions` — решения пользователя запроса (свои и одобренные); администратору —
/// все решения (роль — отношение пользователя `X-Zam-User` или `admin`).
pub async fn list(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let user = target_user(&st, &p, &headers).await?;
    let mut out = Vec::new();
    if matches!(p, Principal::Admin { .. }) {
        for info in st.system.list_solutions().await? {
            let role = if info.owner_id == user.id {
                Role::Owner
            } else {
                match st.system.solution_role(&info.guid, user.id).await? {
                    Some(SolutionRole::Member(a)) => Role::Member(a),
                    _ => Role::Admin,
                }
            };
            out.push(list_item(&st, &info, role).await?);
        }
    } else {
        for (info, role) in st.system.accessible_solutions(user.id).await? {
            let role = match role {
                SolutionRole::Member(a) => Role::Member(a),
                _ => Role::Owner,
            };
            out.push(list_item(&st, &info, role).await?);
        }
    }
    Ok(Json(Value::Array(out)))
}

/// Решение для управления доступом: администратор или владелец (токен его пользователя;
/// `write` — только `rw`-токен). Остальным — 403, незарегистрированный GUID — 404.
async fn managed(
    st: &AppState,
    p: &Principal,
    raw: &str,
    write: bool,
) -> Result<SolutionInfo, ApiError> {
    let guid: SolutionGuid = raw.trim().parse().map_err(|_| ApiError::NotFound)?;
    let info = st
        .system
        .get_solution(&guid)
        .await?
        .ok_or(ApiError::NotFound)?;
    match p {
        Principal::Admin { .. } => Ok(info),
        Principal::Token(t) if t.user_id == info.owner_id => {
            if write {
                require_rw(p)?;
            }
            Ok(info)
        }
        Principal::Token(_) => Err(ApiError::Forbidden),
    }
}

/// Кто решает: `admin` или id владельца.
fn decided_by(p: &Principal, info: &SolutionInfo) -> String {
    match p {
        Principal::Admin { .. } => "admin".into(),
        Principal::Token(_) => info.owner_id.to_string(),
    }
}

/// `GET /solutions/{guid}/access` → `{agent_solution_id, owner, members, pending}`.
pub async fn access_list(
    State(st): State<AppState>,
    p: Principal,
    Path(raw): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let info = managed(&st, &p, &raw, false).await?;
    let members = st.system.list_access(&info.guid).await?;
    let pending = members
        .iter()
        .filter(|m| m.state == AccessState::Pending)
        .count();
    Ok(Json(json!({
        "agent_solution_id": info.guid,
        "owner": user_ref(&st, info.owner_id).await?,
        "members": members,
        "pending": pending,
    })))
}

/// Запрос участника `user` после решения — тело ответа.
async fn entry_of(st: &AppState, info: &SolutionInfo, user: Id) -> Result<Json<Value>, ApiError> {
    let e = st
        .system
        .list_access(&info.guid)
        .await?
        .into_iter()
        .find(|e| e.user_id == user)
        .ok_or(ApiError::NotFound)?;
    let v = serde_json::to_value(e)
        .map_err(|e| ApiError::Internal(format!("сериализация доступа: {e}")))?;
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ApproveBody {
    access: Option<Access>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RejectBody {
    reason: Option<String>,
}

/// Необязательное тело JSON: пустое — значение по умолчанию.
fn optional_body<T: serde::de::DeserializeOwned + Default>(
    body: Result<Bytes, BytesRejection>,
) -> Result<T, ApiError> {
    match body {
        Ok(b) if b.iter().all(u8::is_ascii_whitespace) => Ok(T::default()),
        other => json_body(other),
    }
}

/// `POST /solutions/{guid}/access/{user}/approve {access?}` — по умолчанию `rw`.
pub async fn approve(
    State(st): State<AppState>,
    p: Principal,
    Path((raw, user)): Path<(String, String)>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let info = managed(&st, &p, &raw, true).await?;
    let user = parse_id(&user)?;
    let req: ApproveBody = optional_body(body)?;
    let access = req.access.unwrap_or(Access::Rw);
    st.system
        .decide_access(
            &info.guid,
            user,
            AccessDecision::Approve(access),
            &decided_by(&p, &info),
        )
        .await?;
    tracing::info!(guid = %info.guid, %user, access = access.as_str(), "доступ к решению одобрен");
    entry_of(&st, &info, user).await
}

/// `POST /solutions/{guid}/access/{user}/reject {reason?}`.
pub async fn reject(
    State(st): State<AppState>,
    p: Principal,
    Path((raw, user)): Path<(String, String)>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let info = managed(&st, &p, &raw, true).await?;
    let user = parse_id(&user)?;
    let req: RejectBody = optional_body(body)?;
    let reason = req
        .reason
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty());
    if reason.as_ref().is_some_and(|r| {
        r.chars().count() > REASON_MAX_CHARS || r.chars().any(|c| c.is_control() && c != '\n')
    }) {
        return Err(ApiError::BadRequest(format!(
            "reason: до {REASON_MAX_CHARS} символов, без управляющих"
        )));
    }
    st.system
        .decide_access(
            &info.guid,
            user,
            AccessDecision::Reject { reason },
            &decided_by(&p, &info),
        )
        .await?;
    tracing::info!(guid = %info.guid, %user, "запрос доступа к решению отклонён");
    entry_of(&st, &info, user).await
}

/// `POST /solutions/{guid}/access/{user}/revoke`.
pub async fn revoke(
    State(st): State<AppState>,
    p: Principal,
    Path((raw, user)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let info = managed(&st, &p, &raw, true).await?;
    let user = parse_id(&user)?;
    st.system
        .revoke_access(&info.guid, user, &decided_by(&p, &info))
        .await?;
    tracing::info!(guid = %info.guid, %user, "доступ к решению отозван");
    entry_of(&st, &info, user).await
}
