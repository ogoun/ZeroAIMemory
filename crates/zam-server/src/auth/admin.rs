//! Вход в WebUI: `/auth/login` (администратор из конфигурации, затем пользователь — §11.4,
//! [`super::user`]), `/auth/logout`, `/auth/me`.
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use cookie::{Cookie, SameSite};
use serde::Deserialize;
use serde_json::json;
use zam_core::generate_token;

use super::rate::retry_after_secs;
use super::{Client, Principal, SESSION_COOKIE, Session, ct_eq};
use crate::app::AppState;
use crate::error::ApiError;

/// Срок cookie совпадает со сроком веб-сессии в `system.db` (12 ч).
const SESSION_MAX_AGE_S: i64 = 12 * 3600;

#[derive(Deserialize)]
struct LoginReq {
    login: String,
    password: String,
}

/// Cookie сессии: `HttpOnly`, `SameSite=Strict`, `Path` = base_path или `/`, `Secure` за HTTPS.
fn session_cookie(st: &AppState, value: String, max_age_s: i64, secure: bool) -> HeaderValue {
    let base = &st.cfg.server.base_path;
    let path = if base.is_empty() { "/" } else { base.as_str() };
    let c = Cookie::build((SESSION_COOKIE, value))
        .http_only(true)
        .same_site(SameSite::Strict)
        .path(path.to_string())
        .max_age(cookie::time::Duration::seconds(max_age_s))
        .secure(secure)
        .build();
    HeaderValue::from_str(&c.to_string()).expect("cookie из безопасных символов")
}

/// Хэш случайного пароля с параметрами настоящего: проверка по нему длится столько же.
pub(crate) fn make_dummy_hash(real: &str) -> Result<String, String> {
    let parsed = PasswordHash::new(real).map_err(|e| e.to_string())?;
    let alg = Algorithm::try_from(parsed.algorithm).map_err(|e| e.to_string())?;
    let ver = parsed
        .version
        .map_or(Ok(Version::V0x13), Version::try_from)
        .map_err(|e| e.to_string())?;
    let params = Params::try_from(&parsed).map_err(|e| e.to_string())?;
    let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
    Argon2::new(alg, ver, params)
        .hash_password(generate_token().plaintext.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

pub(crate) fn verify(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| {
        Argon2::default()
            .verify_password(password.as_bytes(), &h)
            .is_ok()
    })
}

/// Ответ успешного входа: 200 `{csrf}` + `Set-Cookie` сессии, без кэширования.
pub(crate) fn session_response(st: &AppState, sid: String, csrf: String, https: bool) -> Response {
    let mut resp = (StatusCode::OK, Json(json!({ "csrf": csrf }))).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::SET_COOKIE,
        session_cookie(st, sid, SESSION_MAX_AGE_S, https),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// `POST /auth/login {login, password}` → 200 `{csrf}` + `Set-Cookie`. Сначала администратор
/// из конфигурации, затем пользователь по `users.name` без учёта регистра ([`super::user`]).
/// Лимит — общий: каждая попытка, 5 в минуту на IP.
pub async fn login(
    State(st): State<AppState>,
    client: Client,
    body: Bytes,
) -> Result<Response, ApiError> {
    let ip = client.ip.to_string();
    st.auth
        .login
        .check(client.ip)
        .map_err(|d| ApiError::RateLimited {
            retry_after_s: retry_after_secs(d),
        })?;
    let req: LoginReq = serde_json::from_slice(&body)
        .map_err(|_| ApiError::BadRequest("ожидается JSON {login, password}".into()))?;
    let login_ok = ct_eq(req.login.as_bytes(), st.cfg.admin.login.as_bytes());
    if !login_ok {
        // не администратор — пользователь (неизвестный логин проверяется по фиктивному хэшу)
        return super::user::login(&st, client, &req.login, req.password).await;
    }
    let hash = st.admin_hash.to_string();
    let password = req.password;
    let verified = tokio::task::spawn_blocking(move || verify(&hash, &password))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    if !verified {
        tracing::warn!(ip = %ip, "неудачный вход администратора");
        st.system
            .audit(
                "anonymous",
                None,
                "admin_login_failed",
                None,
                Some(&ip),
                json!({ "login_matches": login_ok }),
            )
            .await?;
        return Err(ApiError::Unauthorized);
    }

    let (sid, csrf) = st.system.create_web_session(Some(&ip)).await?;
    st.system
        .audit(
            "admin",
            Some(st.admin_id),
            "admin_login",
            None,
            Some(&ip),
            json!({}),
        )
        .await?;
    tracing::info!(ip = %ip, "вход администратора");
    Ok(session_response(&st, sid, csrf, client.https))
}

/// Id cookie-сессии принципала сессии WebUI.
fn session_id(p: &Principal) -> Result<&str, ApiError> {
    match p {
        Principal::Admin { session_id } | Principal::User { session_id, .. } => Ok(session_id),
        Principal::Token(_) => Err(ApiError::Forbidden),
    }
}

/// `POST /auth/logout` (требует CSRF) → 204, cookie стирается. Администратор и пользователь
/// (в том числе до обязательной смены пароля).
pub async fn logout(
    State(st): State<AppState>,
    client: Client,
    s: Session,
) -> Result<Response, ApiError> {
    let sid = session_id(&s.principal)?;
    st.system.delete_web_session(sid).await?;
    let ip = client.ip.to_string();
    let (actor, user, action) = match &s.principal {
        Principal::User { user_id, .. } => ("user", *user_id, "auth.logout.user"),
        _ => ("admin", st.admin_id, "admin_logout"),
    };
    st.system
        .audit(actor, Some(user), action, None, Some(&ip), json!({}))
        .await?;
    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie(&st, String::new(), 0, client.https),
    );
    Ok(resp)
}

/// `GET /auth/me` → `{kind: "admin"|"user", user_id, name, login, must_change_password,
/// csrf}` по живой cookie-сессии (восстановление после F5; доступен и до обязательной смены
/// пароля). `name` — имя пользователя базы, `login` — чем входили (у администратора — логин
/// из конфигурации). Сессия WebUI всегда пишет как человек (§7.5, §11.4).
pub async fn me(
    State(st): State<AppState>,
    s: Session,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = st
        .system
        .get_web_session(session_id(&s.principal)?)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let (kind, user_id, login) = match &s.principal {
        Principal::User { user_id, .. } => ("user", *user_id, None),
        _ => ("admin", st.admin_id, Some(st.cfg.admin.login.clone())),
    };
    let user = st
        .system
        .get_user(user_id)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    Ok(Json(json!({
        "kind": kind,
        "user_id": user_id,
        "name": user.name,
        "login": login.unwrap_or_else(|| user.name.clone()),
        "must_change_password": s.must_change_password,
        "csrf": ws.csrf_token,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dummy_hash_uses_real_params() {
        let real = crate::config::hash_password("x").unwrap();
        let d = make_dummy_hash(&real).unwrap();
        let (pr, pd) = (
            PasswordHash::new(&real).unwrap(),
            PasswordHash::new(&d).unwrap(),
        );
        assert_eq!(pr.algorithm, pd.algorithm);
        assert_eq!(
            Params::try_from(&pr).unwrap(),
            Params::try_from(&pd).unwrap()
        );
        assert!(!verify(&d, "x"));
        assert!(verify(&real, "x"));
    }
}
