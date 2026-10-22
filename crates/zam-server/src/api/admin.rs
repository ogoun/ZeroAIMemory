//! Админ-API: пользователи, токены, аудит, настройки (§12). Только `AdminOnly`.
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use zam_core::{Id, mask_token};
use zam_store::time::fmt_ts;
use zam_store::{Access, AuditRow, TokenInfo, User};

use crate::app::AppState;
use crate::auth::{AdminOnly, Client};
use crate::error::ApiError;

use super::scope::RULES_AGENT_WRITE;
use super::{parse_body, parse_id};

const NAME_MAX_CHARS: usize = 64;
const AUDIT_DEFAULT_LIMIT: u32 = 50;
const AUDIT_MAX_LIMIT: u32 = 500;
const ACTOR: &str = "admin";

// ---- представления ----

fn user_json(u: &User, tokens: usize) -> Value {
    json!({
        "id": u.id,
        "name": u.name,
        "is_admin": u.is_admin,
        "created_at": fmt_ts(u.created_at),
        "disabled_at": u.disabled_at.map(fmt_ts),
        "tokens": tokens,
    })
}

/// Без секрета: только префикс.
fn token_json(t: &TokenInfo) -> Value {
    json!({
        "id": t.id,
        "user_id": t.user_id,
        "name": t.name,
        "prefix": t.prefix,
        "access": t.access.as_str(),
        "created_at": fmt_ts(t.created_at),
        "last_used_at": t.last_used_at.map(fmt_ts),
        "last_ip": t.last_ip,
        "revoked_at": t.revoked_at.map(fmt_ts),
        "human": t.human,
    })
}

fn audit_json(r: &AuditRow) -> Value {
    json!({
        "id": r.id,
        "ts": fmt_ts(r.ts),
        "actor": r.actor,
        "user_id": r.user_id,
        "action": r.action,
        "target": r.target,
        "ip": r.ip,
        "details": r.details,
    })
}

// ---- разбор входных данных ----

/// Имя: непустое после trim, ≤ 64 символов, без управляющих. Возвращает обрезанное.
fn valid_name(raw: &str) -> Result<String, ApiError> {
    let n = raw.trim();
    if n.is_empty() || n.chars().count() > NAME_MAX_CHARS || n.chars().any(char::is_control) {
        return Err(ApiError::BadRequest(format!(
            "имя должно быть непустым, до {NAME_MAX_CHARS} символов, без управляющих"
        )));
    }
    Ok(n.to_string())
}

async fn require_user(st: &AppState, id: Id) -> Result<User, ApiError> {
    st.system.get_user(id).await?.ok_or(ApiError::NotFound)
}

async fn active_tokens(st: &AppState, user: Id) -> Result<usize, ApiError> {
    let t = st.system.list_tokens(user).await?;
    Ok(t.iter().filter(|t| t.revoked_at.is_none()).count())
}

fn is_admin(st: &AppState, u: &User) -> bool {
    u.is_admin || u.id == st.admin_id
}

// ---- пользователи ----

pub async fn list_users(_: AdminOnly, State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let mut out = Vec::new();
    for u in st.system.list_users().await? {
        let n = active_tokens(&st, u.id).await?;
        out.push(user_json(&u, n));
    }
    Ok(Json(Value::Array(out)))
}

#[derive(Deserialize)]
struct CreateUser {
    name: String,
}

pub async fn create_user(
    _: AdminOnly,
    State(st): State<AppState>,
    client: Client,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let req: CreateUser = parse_body(&body)?;
    let name = valid_name(&req.name)?;
    let ip = client.ip.to_string();
    let user = st.system.create_user(&name, false).await?;
    st.users.get(user.id).await?;
    st.system
        .audit(
            ACTOR,
            Some(user.id),
            "user_create",
            Some(&user.id.to_string()),
            Some(&ip),
            json!({ "name": user.name }),
        )
        .await?;
    let (info, plain) = st
        .system
        .issue_token(user.id, "default", Access::Rw, false)
        .await?;
    audit_token_issue(&st, &info, &plain, &ip).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "user": user_json(&user, 1),
            "token": { "info": token_json(&info), "plaintext": plain },
        })),
    ))
}

#[derive(Deserialize)]
struct PatchUser {
    disabled: Option<bool>,
    name: Option<String>,
}

pub async fn patch_user(
    _: AdminOnly,
    State(st): State<AppState>,
    client: Client,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let id = parse_id(&id)?;
    let req: PatchUser = parse_body(&body)?;
    if req.disabled.is_none() && req.name.is_none() {
        return Err(ApiError::BadRequest("нужно поле disabled или name".into()));
    }
    let user = require_user(&st, id).await?;
    if req.disabled == Some(true) && is_admin(&st, &user) {
        return Err(ApiError::conflict("администратора нельзя отключить"));
    }
    let new_name = req.name.as_deref().map(valid_name).transpose()?;
    let ip = client.ip.to_string();
    let target = id.to_string();
    if let Some(n) = new_name.as_deref().filter(|n| *n != user.name) {
        st.system.rename_user(id, n).await?;
        st.system
            .audit(
                ACTOR,
                Some(id),
                "user_update",
                Some(&target),
                Some(&ip),
                json!({ "name": { "from": user.name, "to": n } }),
            )
            .await?;
    }
    if let Some(d) = req.disabled
        && d != user.disabled_at.is_some()
    {
        st.system.set_user_disabled(id, d).await?;
        let (action, details) = if d {
            ("user_disable", json!({}))
        } else {
            ("user_update", json!({ "disabled": false }))
        };
        st.system
            .audit(ACTOR, Some(id), action, Some(&target), Some(&ip), details)
            .await?;
    }
    let user = require_user(&st, id).await?;
    let n = active_tokens(&st, id).await?;
    Ok(Json(user_json(&user, n)))
}

/// Мягкое удаление = отключение.
pub async fn delete_user(
    _: AdminOnly,
    State(st): State<AppState>,
    client: Client,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let user = require_user(&st, id).await?;
    if is_admin(&st, &user) {
        return Err(ApiError::conflict("администратора нельзя удалить"));
    }
    if user.disabled_at.is_none() {
        st.system.set_user_disabled(id, true).await?;
        st.system
            .audit(
                ACTOR,
                Some(id),
                "user_disable",
                Some(&id.to_string()),
                Some(&client.ip.to_string()),
                json!({ "soft_delete": true }),
            )
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---- токены ----

async fn audit_token_issue(
    st: &AppState,
    info: &TokenInfo,
    plain: &str,
    ip: &str,
) -> Result<(), ApiError> {
    st.system
        .audit(
            ACTOR,
            Some(info.user_id),
            "token_issue",
            Some(&info.id.to_string()),
            Some(ip),
            json!({
                "token": mask_token(plain),
                "name": info.name,
                "access": info.access.as_str(),
                "human": info.human,
            }),
        )
        .await?;
    Ok(())
}

pub async fn list_tokens(
    _: AdminOnly,
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_id(&id)?;
    require_user(&st, id).await?;
    let tokens = st.system.list_tokens(id).await?;
    Ok(Json(Value::Array(tokens.iter().map(token_json).collect())))
}

#[derive(Deserialize)]
struct IssueToken {
    name: String,
    access: Option<String>,
    /// Человеческий токен (§7.5): запись по нему — `author_kind = human` (ставит и меняет
    /// `axiom`). Выпускает только администратор (маршрут `AdminOnly`) **[решение
    /// исполнителя]**; хранить его в CLAUDE.md/AGENTS.md нельзя — агент станет «человеком».
    #[serde(default)]
    human: bool,
}

pub async fn issue_token(
    _: AdminOnly,
    State(st): State<AppState>,
    client: Client,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let id = parse_id(&id)?;
    let req: IssueToken = parse_body(&body)?;
    let name = valid_name(&req.name)?;
    let access = match req.access.as_deref() {
        None | Some("rw") => Access::Rw,
        Some("ro") => Access::Ro,
        Some(_) => return Err(ApiError::BadRequest("access: допустимо rw или ro".into())),
    };
    let user = require_user(&st, id).await?;
    if user.disabled_at.is_some() {
        return Err(ApiError::conflict(
            "пользователь отключён: токен выпустить нельзя",
        ));
    }
    let (info, plain) = st.system.issue_token(id, &name, access, req.human).await?;
    audit_token_issue(&st, &info, &plain, &client.ip.to_string()).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "info": token_json(&info), "plaintext": plain })),
    ))
}

pub async fn revoke_token(
    _: AdminOnly,
    State(st): State<AppState>,
    client: Client,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let info = st.system.get_token(id).await?.ok_or(ApiError::NotFound)?;
    if info.revoked_at.is_none() {
        st.system.revoke_token(id).await?;
        st.system
            .audit(
                ACTOR,
                Some(info.user_id),
                "token_revoke",
                Some(&id.to_string()),
                Some(&client.ip.to_string()),
                json!({ "token": format!("zam_{}…", info.prefix), "name": info.name }),
            )
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---- аудит ----

#[derive(Deserialize)]
pub struct AuditQuery {
    limit: Option<u32>,
    before: Option<i64>,
}

pub async fn list_audit(
    _: AdminOnly,
    State(st): State<AppState>,
    q: Result<Query<AuditQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(q) = q.map_err(|e| ApiError::BadRequest(format!("некорректный запрос: {e}")))?;
    let limit = match q.limit {
        None => AUDIT_DEFAULT_LIMIT,
        Some(0) => return Err(ApiError::BadRequest("limit должен быть ≥ 1".into())),
        Some(n) => n.min(AUDIT_MAX_LIMIT),
    };
    let rows = st.system.list_audit(limit, q.before).await?;
    // Ровно limit записей — возможно, есть продолжение.
    let next = (rows.len() == limit as usize)
        .then(|| rows.last().map(|r| r.id))
        .flatten();
    Ok(Json(json!({
        "items": rows.iter().map(audit_json).collect::<Vec<_>>(),
        "next": next,
    })))
}

// ---- настройки ----

/// `{rules: {agent_write}}`.
async fn settings_json(st: &AppState) -> Result<Value, ApiError> {
    let agent_write = st
        .system
        .get_setting::<bool>(RULES_AGENT_WRITE)
        .await?
        .unwrap_or(false);
    Ok(json!({ "rules": { "agent_write": agent_write } }))
}

/// `GET /admin/settings`.
pub async fn get_settings(
    _: AdminOnly,
    State(st): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(settings_json(&st).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RulesPatch {
    agent_write: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsPatch {
    rules: Option<RulesPatch>,
}

/// `PATCH /admin/settings {rules: {agent_write}}`; аудит `settings.update` при изменении.
pub async fn patch_settings(
    _: AdminOnly,
    State(st): State<AppState>,
    client: Client,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let req: SettingsPatch = parse_body(&body)?;
    let Some(to) = req.rules.and_then(|r| r.agent_write) else {
        return Err(ApiError::BadRequest("нужно поле rules.agent_write".into()));
    };
    let from = st
        .system
        .get_setting::<bool>(RULES_AGENT_WRITE)
        .await?
        .unwrap_or(false);
    if from != to {
        st.system.set_setting(RULES_AGENT_WRITE, &to).await?;
        st.system
            .audit(
                ACTOR,
                None,
                "settings.update",
                Some(RULES_AGENT_WRITE),
                Some(&client.ip.to_string()),
                json!({ "key": RULES_AGENT_WRITE, "from": from, "to": to }),
            )
            .await?;
    }
    Ok(Json(settings_json(&st).await?))
}
