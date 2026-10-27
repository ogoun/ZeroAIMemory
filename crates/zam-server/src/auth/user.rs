//! Вход пользователей в WebUI (§11.4, решение владельца 2026-10-26): проверка пароля
//! пользователя при общем `POST /auth/login`, `POST /auth/password` (смена своего пароля) и
//! свои токены (`GET/POST /auth/tokens`, `DELETE /auth/tokens/{id}`).
//!
//! - Пароль — argon2id в `users.password_hash` с параметрами по умолчанию крейта `argon2`
//!   (как у [`crate::config::hash_password`] — хэш администратора из `ZAM_ADMIN_PASSWORD`),
//!   не короче [`PASSWORD_MIN_CHARS`] символов.
//! - Логин — `users.name` без учёта регистра (Unicode); точное совпадение сильнее, при
//!   нескольких совпадениях без учёта регистра и без точного вход не выполняется.
//! - Пользователь администратора (`users.is_admin`) по паролю из базы не входит: его вход —
//!   логин и пароль из конфигурации.
//! - В аудит неудачного входа введённый логин не пишется (там мог оказаться пароль): только
//!   id найденного пользователя.
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use zam_core::{Id, generate_token};
use zam_store::User;

use super::rate::retry_after_secs;
use super::{Client, Principal, Session};
use crate::api::admin::{audit_token_issue, parse_issue, token_json};
use crate::api::{json_body, parse_id};
use crate::app::AppState;
use crate::error::ApiError;

/// Наименьшая длина пароля пользователя, символов (§11.4).
pub const PASSWORD_MIN_CHARS: usize = 10;
/// Наибольшая длина пароля, символов: защита от заведомо огромных тел.
pub const PASSWORD_MAX_CHARS: usize = 1024;

/// Пароль: не короче 10 и не длиннее 1024 символов, без управляющих.
pub fn check_password(pw: &str) -> Result<(), ApiError> {
    let n = pw.chars().count();
    if n < PASSWORD_MIN_CHARS {
        return Err(ApiError::BadRequest(format!(
            "пароль не короче {PASSWORD_MIN_CHARS} символов"
        )));
    }
    if n > PASSWORD_MAX_CHARS || pw.chars().any(char::is_control) {
        return Err(ApiError::BadRequest(format!(
            "пароль до {PASSWORD_MAX_CHARS} символов, без управляющих"
        )));
    }
    Ok(())
}

/// Фиктивный хэш случайного пароля с параметрами паролей пользователей (как у [`hash`]):
/// неизвестный логин проверяется столько же, сколько известный.
pub(crate) fn make_user_dummy_hash() -> Result<String, String> {
    crate::config::hash_password(&generate_token().plaintext).map_err(|e| e.to_string())
}

/// argon2id пароля (блокирующий расчёт — в `spawn_blocking`).
pub async fn hash(pw: String) -> Result<String, ApiError> {
    tokio::task::spawn_blocking(move || crate::config::hash_password(&pw))
        .await
        .map_err(|e| ApiError::aborted("хэширование пароля", &e))?
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// Проверка пароля по хэшу (блокирующий расчёт — в `spawn_blocking`).
pub async fn verify(hash: String, pw: String) -> Result<bool, ApiError> {
    tokio::task::spawn_blocking(move || super::admin::verify(&hash, &pw))
        .await
        .map_err(|e| ApiError::aborted("проверка пароля", &e))
}

/// Пользователь для входа по логину: точное имя, иначе единственное совпадение без учёта
/// регистра; пользователь администратора не входит по паролю из базы.
async fn candidate(st: &AppState, login: &str) -> Result<Option<(User, String)>, ApiError> {
    let mut found = st.system.login_candidates(login).await?;
    found.retain(|(u, _)| !u.is_admin && u.id != st.admin_id);
    let exact = found.first().is_some_and(|(u, _)| u.name == login.trim());
    if found.len() > 1 && !exact {
        tracing::warn!(
            matches = found.len(),
            "вход пользователя: имя совпадает с несколькими без учёта регистра — вход отклонён"
        );
        return Ok(None);
    }
    Ok(found.into_iter().next())
}

/// Вход пользователя (логин не совпал с администратором): 200 `{csrf}` + cookie сессии или
/// 401. Неизвестный логин проверяется по фиктивному хэшу — время ответа одинаково.
pub(crate) async fn login(
    st: &AppState,
    client: Client,
    login: &str,
    password: String,
) -> Result<Response, ApiError> {
    let ip = client.ip.to_string();
    let cand = candidate(st, login).await?;
    let hash_s = match &cand {
        Some((_, h)) => h.clone(),
        None => st.auth.user_dummy_hash.clone(),
    };
    let verified = verify(hash_s, password).await?;
    let Some((user, _)) = cand else {
        tracing::warn!(ip = %ip, "неудачный вход");
        st.system
            .audit(
                "anonymous",
                None,
                "auth.login.failed",
                None,
                Some(&ip),
                json!({}),
            )
            .await?;
        return Err(ApiError::Unauthorized);
    };
    let disabled = user.disabled_at.is_some();
    if !verified || disabled {
        tracing::warn!(ip = %ip, user = %user.id, "неудачный вход пользователя");
        st.system
            .audit(
                "anonymous",
                Some(user.id),
                "auth.login.user_failed",
                Some(&user.id.to_string()),
                Some(&ip),
                json!({ "disabled": disabled }),
            )
            .await?;
        return Err(ApiError::Unauthorized);
    }
    let (sid, csrf) = st.system.create_user_session(Some(&ip), user.id).await?;
    st.system
        .audit(
            "user",
            Some(user.id),
            "auth.login.user",
            Some(&user.id.to_string()),
            Some(&ip),
            json!({ "must_change_password": user.must_change_password }),
        )
        .await?;
    tracing::info!(ip = %ip, user = %user.id, "вход пользователя");
    Ok(super::admin::session_response(st, sid, csrf, client.https))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangePassword {
    old: String,
    new: String,
}

/// `POST /auth/password {old, new}` → 204: смена своего пароля пользователем (в том числе при
/// обязательной смене). Неверный `old` — 403; попытки — в общем лимите входа (5/мин на IP).
/// Прочие сессии пользователя закрываются, текущая остаётся. Администратору — 403: его
/// пароль задаётся в конфигурации.
pub async fn change_password(
    State(st): State<AppState>,
    client: Client,
    s: Session,
    body: Result<Bytes, BytesRejection>,
) -> Result<StatusCode, ApiError> {
    let Principal::User {
        session_id,
        user_id,
    } = &s.principal
    else {
        return Err(ApiError::Denied(
            "пароль администратора задаётся в конфигурации сервера (zam-server hash-password)"
                .into(),
        ));
    };
    st.auth
        .login
        .check(client.ip)
        .map_err(|d| ApiError::RateLimited {
            retry_after_s: retry_after_secs(d),
        })?;
    let req: ChangePassword =
        json_body(body).map_err(|_| ApiError::BadRequest("ожидается JSON {old, new}".into()))?;
    check_password(&req.new)?;
    if req.new == req.old {
        return Err(ApiError::BadRequest(
            "новый пароль должен отличаться от текущего".into(),
        ));
    }
    let ip = client.ip.to_string();
    let current = st
        .system
        .password_hash(*user_id)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    if !verify(current, req.old).await? {
        tracing::warn!(ip = %ip, user = %user_id, "смена пароля: неверный текущий пароль");
        return Err(ApiError::Denied("текущий пароль неверен".into()));
    }
    let h = hash(req.new).await?;
    st.system
        .set_password(*user_id, Some(h), false, Some(session_id))
        .await?;
    st.system
        .audit(
            "user",
            Some(*user_id),
            "user.password.change",
            Some(&user_id.to_string()),
            Some(&ip),
            json!({}),
        )
        .await?;
    tracing::info!(user = %user_id, "пользователь сменил пароль");
    Ok(StatusCode::NO_CONTENT)
}

/// Пользователь, чьими токенами управляет сессия WebUI: свой (у администратора — его
/// пользователь). Токен — 403: агент токены не выпускает.
fn own_user(st: &AppState, p: &Principal) -> Result<Id, ApiError> {
    match p {
        Principal::User { user_id, .. } => Ok(*user_id),
        Principal::Admin { .. } => Ok(st.admin_id),
        Principal::Token(_) => Err(ApiError::Forbidden),
    }
}

/// `GET /auth/tokens` — свои токены (без секретов).
pub async fn list_tokens(
    State(st): State<AppState>,
    p: Principal,
) -> Result<Json<Value>, ApiError> {
    let user = own_user(&st, &p)?;
    let tokens = st.system.list_tokens(user).await?;
    Ok(Json(Value::Array(tokens.iter().map(token_json).collect())))
}

/// `POST /auth/tokens {name, access?}` → 201 `{info, plaintext}`: свой `rw`/`ro`-токен.
pub async fn issue_token(
    State(st): State<AppState>,
    client: Client,
    p: Principal,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let user = own_user(&st, &p)?;
    let (name, access) = parse_issue(&body)?;
    let (info, plain) = st.system.issue_token(user, &name, access).await?;
    audit_token_issue(&st, &p.actor(), &info, &plain, &client.ip.to_string()).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "info": token_json(&info), "plaintext": plain })),
    ))
}

/// `DELETE /auth/tokens/{id}` → 204: отзыв своего токена; чужой — 404.
pub async fn revoke_token(
    State(st): State<AppState>,
    client: Client,
    p: Principal,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let user = own_user(&st, &p)?;
    let id = parse_id(&id)?;
    let info = st
        .system
        .get_token(id)
        .await?
        .filter(|t| t.user_id == user)
        .ok_or(ApiError::NotFound)?;
    crate::api::admin::revoke(&st, &p.actor(), &info, &client.ip.to_string()).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_dummy_hash_has_user_password_params() {
        use argon2::password_hash::PasswordHash;
        let real = crate::config::hash_password("пароль-пользователя").unwrap();
        let dummy = make_user_dummy_hash().unwrap();
        let (r, d) = (
            PasswordHash::new(&real).unwrap(),
            PasswordHash::new(&dummy).unwrap(),
        );
        assert_eq!(r.algorithm, d.algorithm);
        assert_eq!(r.version, d.version);
        assert_eq!(
            argon2::Params::try_from(&r).unwrap(),
            argon2::Params::try_from(&d).unwrap()
        );
        assert!(!super::super::admin::verify(&dummy, "пароль-пользователя"));
    }

    #[test]
    fn password_rules() {
        assert!(check_password("123456789").is_err());
        assert!(check_password("1234567890").is_ok());
        // символы, а не байты
        assert!(check_password("пароль-дес").is_ok());
        assert!(check_password("пароль\nдесять").is_err());
        assert!(check_password(&"x".repeat(1025)).is_err());
    }
}
