//! Вход администратора: `/auth/login`, `/auth/logout`, `/auth/me`.
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
use super::{AdminOnly, Client, SESSION_COOKIE, ct_eq};
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

fn verify(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| {
        Argon2::default()
            .verify_password(password.as_bytes(), &h)
            .is_ok()
    })
}

/// `POST /auth/login {login, password}` → 200 `{csrf}` + `Set-Cookie`.
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
    // Неверный логин тоже проходит полную проверку argon2 — время ответа одинаково.
    let hash = if login_ok {
        st.admin_hash.to_string()
    } else {
        st.auth.dummy_hash.clone()
    };
    let password = req.password;
    let verified = tokio::task::spawn_blocking(move || verify(&hash, &password))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    if !(login_ok && verified) {
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
    let mut resp = (StatusCode::OK, Json(json!({ "csrf": csrf }))).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::SET_COOKIE,
        session_cookie(&st, sid, SESSION_MAX_AGE_S, client.https),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(resp)
}

/// `POST /auth/logout` (требует CSRF) → 204, cookie стирается.
pub async fn logout(
    State(st): State<AppState>,
    client: Client,
    AdminOnly(sid): AdminOnly,
) -> Result<Response, ApiError> {
    st.system.delete_web_session(&sid).await?;
    let ip = client.ip.to_string();
    st.system
        .audit(
            "admin",
            Some(st.admin_id),
            "admin_logout",
            None,
            Some(&ip),
            json!({}),
        )
        .await?;
    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie(&st, String::new(), 0, client.https),
    );
    Ok(resp)
}

/// `GET /auth/me` → `{login, user_id}`.
pub async fn me(State(st): State<AppState>, _: AdminOnly) -> Json<serde_json::Value> {
    Json(json!({ "login": st.cfg.admin.login, "user_id": st.admin_id }))
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
