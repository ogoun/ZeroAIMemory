//! Админ-API: пользователи, токены, пароли входа пользователей (§11.4), аудит, настройки
//! (§12). Только `AdminOnly`.
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

use crate::agent_setup::{self, AgentKind, Lang};
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
        "has_password": u.has_password,
        "must_change_password": u.must_change_password,
    })
}

/// Без секрета: только префикс.
pub(crate) fn token_json(t: &TokenInfo) -> Value {
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
        .issue_token(user.id, "default", Access::Rw)
        .await?;
    audit_token_issue(&st, ACTOR, &info, &plain, &ip).await?;
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

/// Аудит выпуска токена: `actor` — `admin` или `user` (свой токен в «Аккаунте», §11.4).
pub(crate) async fn audit_token_issue(
    st: &AppState,
    actor: &str,
    info: &TokenInfo,
    plain: &str,
    ip: &str,
) -> Result<(), ApiError> {
    st.system
        .audit(
            actor,
            Some(info.user_id),
            "token_issue",
            Some(&info.id.to_string()),
            Some(ip),
            json!({
                "token": mask_token(plain),
                "name": info.name,
                "access": info.access.as_str(),
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
    /// Флаг человеческого токена M3: `true` — 400 (упразднён, §11.4); `false` принимается
    /// для совместимости старых клиентов.
    #[serde(default)]
    human: bool,
}

/// Отказ в выпуске человеческого токена (§11.4).
pub(crate) const HUMAN_TOKEN_REMOVED: &str =
    "человеческие токены упразднены (решение владельца 2026-10-26): подтверждайте в WebUI";

/// Тело выпуска токена `{name, access?}` → имя и режим (`human: true` — 400, §11.4).
pub(crate) fn parse_issue(body: &Bytes) -> Result<(String, Access), ApiError> {
    let req: IssueToken = parse_body(body)?;
    if req.human {
        return Err(ApiError::BadRequest(HUMAN_TOKEN_REMOVED.into()));
    }
    let name = valid_name(&req.name)?;
    let access = match req.access.as_deref() {
        None | Some("rw") => Access::Rw,
        Some("ro") => Access::Ro,
        Some(_) => return Err(ApiError::BadRequest("access: допустимо rw или ro".into())),
    };
    Ok((name, access))
}

pub async fn issue_token(
    _: AdminOnly,
    State(st): State<AppState>,
    client: Client,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let id = parse_id(&id)?;
    let (name, access) = parse_issue(&body)?;
    let user = require_user(&st, id).await?;
    if user.disabled_at.is_some() {
        return Err(ApiError::conflict(
            "пользователь отключён: токен выпустить нельзя",
        ));
    }
    let (info, plain) = st.system.issue_token(id, &name, access).await?;
    audit_token_issue(&st, ACTOR, &info, &plain, &client.ip.to_string()).await?;
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
    revoke(&st, ACTOR, &info, &client.ip.to_string()).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Отзыв токена с аудитом `token_revoke` (повторный отзыв — без записи).
pub(crate) async fn revoke(
    st: &AppState,
    actor: &str,
    info: &TokenInfo,
    ip: &str,
) -> Result<(), ApiError> {
    if info.revoked_at.is_some() {
        return Ok(());
    }
    st.system.revoke_token(info.id).await?;
    st.system
        .audit(
            actor,
            Some(info.user_id),
            "token_revoke",
            Some(&info.id.to_string()),
            Some(ip),
            json!({ "token": format!("zam_{}…", info.prefix), "name": info.name }),
        )
        .await?;
    Ok(())
}

// ---- пароли входа пользователей в WebUI (§11.4) ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetPassword {
    password: String,
}

/// Пользователь, которому задаётся пароль: не администратор (его пароль — в конфигурации).
async fn password_target(st: &AppState, id: &str) -> Result<User, ApiError> {
    let user = require_user(st, parse_id(id)?).await?;
    if is_admin(st, &user) {
        return Err(ApiError::conflict(
            "пароль администратора задаётся в конфигурации сервера (zam-server hash-password)",
        ));
    }
    // вход проверяет администратора первым: с таким именем пользователь войти не сможет
    if user.name.to_lowercase() == st.cfg.admin.login.to_lowercase() {
        return Err(ApiError::conflict(format!(
            "имя пользователя «{}» совпадает с логином администратора: вход под ним — \
             администратора; переименуйте пользователя",
            user.name
        )));
    }
    Ok(user)
}

/// `POST /admin/users/{id}/password {password}` → 200 пользователь: задаёт пароль (argon2id,
/// не короче 10 символов) и обязательную смену при входе; сессии пользователя закрываются.
/// Аудит `user.password.set` (без пароля).
pub async fn set_password(
    _: AdminOnly,
    State(st): State<AppState>,
    client: Client,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let user = password_target(&st, &id).await?;
    let req: SetPassword = parse_body(&body)?;
    crate::auth::user::check_password(&req.password)?;
    let h = crate::auth::user::hash(req.password).await?;
    st.system.set_password(user.id, Some(h), true, None).await?;
    st.system
        .audit(
            ACTOR,
            Some(user.id),
            "user.password.set",
            Some(&user.id.to_string()),
            Some(&client.ip.to_string()),
            json!({ "must_change_password": true }),
        )
        .await?;
    let user = require_user(&st, user.id).await?;
    let n = active_tokens(&st, user.id).await?;
    Ok(Json(user_json(&user, n)))
}

/// `DELETE /admin/users/{id}/password` → 204: закрывает вход в WebUI, сессии пользователя
/// закрываются (токены не затрагиваются). Аудит `user.password.clear`.
pub async fn clear_password(
    _: AdminOnly,
    State(st): State<AppState>,
    client: Client,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let user = password_target(&st, &id).await?;
    st.system.set_password(user.id, None, false, None).await?;
    st.system
        .audit(
            ACTOR,
            Some(user.id),
            "user.password.clear",
            Some(&user.id.to_string()),
            Some(&client.ip.to_string()),
            json!({}),
        )
        .await?;
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

/// `{rules: {agent_write}, agent_setup: {version, templates: {"<agent>.<lang>": {text, custom}}}}`.
async fn settings_json(st: &AppState) -> Result<Value, ApiError> {
    let agent_write = st
        .system
        .get_setting::<bool>(RULES_AGENT_WRITE)
        .await?
        .unwrap_or(false);
    let mut templates = serde_json::Map::new();
    for agent in AgentKind::ALL {
        for lang in Lang::ALL {
            let (text, custom) = agent_setup::template(&st.system, agent, lang).await?;
            templates.insert(
                format!("{}.{}", agent.as_str(), lang.as_str()),
                json!({ "text": text, "custom": custom }),
            );
        }
    }
    Ok(json!({
        "rules": { "agent_write": agent_write },
        "agent_setup": {
            "version": agent_setup::current_version(&st.system).await?,
            "templates": templates,
        },
    }))
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
struct AgentSetupPatch {
    /// `"<agent>.<lang>"` → текст шаблона или `null` (сброс к шаблону по умолчанию).
    #[serde(default)]
    templates: serde_json::Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsPatch {
    rules: Option<RulesPatch>,
    agent_setup: Option<AgentSetupPatch>,
}

/// Правки шаблонов: ключ `"<agent>.<lang>"` → (agent, lang, новый текст или `None` — сброс).
fn template_edits(p: &AgentSetupPatch) -> Result<Vec<(AgentKind, Lang, Option<String>)>, ApiError> {
    let mut out = Vec::new();
    for (k, v) in &p.templates {
        let parsed = k
            .split_once('.')
            .and_then(|(a, l)| Some((AgentKind::parse(a)?, Lang::parse(l)?)));
        let Some((agent, lang)) = parsed else {
            return Err(ApiError::BadRequest(format!(
                "agent_setup.templates: ключ «{k}» — <claude|agents|codex>.<ru|en>"
            )));
        };
        let text = match v {
            Value::Null => None,
            Value::String(t) => Some(
                agent_setup::validate_template(t)
                    .map_err(|e| ApiError::BadRequest(format!("agent_setup.templates.{k}: {e}")))?,
            ),
            _ => {
                return Err(ApiError::BadRequest(format!(
                    "agent_setup.templates.{k}: строка или null"
                )));
            }
        };
        out.push((agent, lang, text));
    }
    Ok(out)
}

/// `PATCH /admin/settings {rules?: {agent_write}, agent_setup?: {templates}}`; аудит
/// `settings.update` на каждое изменение. Сохранение шаблона, которое меняет его текст
/// (в том числе сброс своего), увеличивает версию блока на 1 — одну на запрос (§15.5).
pub async fn patch_settings(
    _: AdminOnly,
    State(st): State<AppState>,
    client: Client,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let req: SettingsPatch = parse_body(&body)?;
    let rules_to = req.rules.as_ref().and_then(|r| r.agent_write);
    let edits = match &req.agent_setup {
        Some(p) => template_edits(p)?,
        None => Vec::new(),
    };
    if rules_to.is_none() && edits.is_empty() {
        return Err(ApiError::BadRequest(
            "нужно поле rules.agent_write или agent_setup.templates".into(),
        ));
    }
    let ip = client.ip.to_string();
    if let Some(to) = rules_to {
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
                    Some(&ip),
                    json!({ "key": RULES_AGENT_WRITE, "from": from, "to": to }),
                )
                .await?;
        }
    }
    // шаблоны и версия — одной транзакцией; версия растёт, только если текст изменился (M-4)
    if let Some((version, changed)) = agent_setup::save_templates(&st.system, edits).await? {
        for (key, reset) in changed {
            // текст шаблона в аудит не пишется: только ключ, сброс и новая версия
            st.system
                .audit(
                    ACTOR,
                    None,
                    "settings.update",
                    Some(&key),
                    Some(&ip),
                    json!({ "key": key, "reset": reset, "version": version }),
                )
                .await?;
        }
    }
    Ok(Json(settings_json(&st).await?))
}
