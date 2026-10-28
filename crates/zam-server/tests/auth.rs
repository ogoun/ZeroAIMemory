//! Аутентификация: токены, вход администратора, CSRF, лимиты, прокси.
mod common;

use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use zam_server::auth::AdminOnly;
use zam_server::config::ServerConfig;
use zam_server::{AppState, build};

const PW: &str = "pw";

fn cfg(name: &str) -> ServerConfig {
    common::cfg_in(common::tmp(&format!("auth-{name}")))
}

async fn app(c: ServerConfig) -> (Router, AppState, String) {
    let (app, st, rep) = build(c, Some(PW.into())).await.unwrap();
    (app, st, rep.initial_token.unwrap())
}

fn peer(ip: &str) -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::new(ip.parse().unwrap(), 50_000))
}

fn bearer(uri: &str, token: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

fn login_req(login: &str, password: &str) -> Request<Body> {
    let mut r = Request::post("/api/v1/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "login": login, "password": password }).to_string(),
        ))
        .unwrap();
    r.extensions_mut().insert(peer("127.0.0.1"));
    r
}

async fn body_json(r: axum::response::Response) -> Value {
    let b = r.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&b).unwrap()
}

fn set_cookie(r: &axum::response::Response) -> String {
    r.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_string()
}

/// `zam_session=<id>` из Set-Cookie.
fn cookie_pair(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap().trim().to_string()
}

/// Успешный вход: (cookie для запросов, csrf).
async fn login_ok(app: &Router) -> (String, String) {
    let r = app.clone().oneshot(login_req("admin", PW)).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let c = cookie_pair(&set_cookie(&r));
    let csrf = body_json(r).await["csrf"].as_str().unwrap().to_string();
    (c, csrf)
}

async fn audit_actions(st: &AppState) -> Vec<zam_store::AuditRow> {
    st.system.list_audit(100, None).await.unwrap()
}

#[tokio::test]
async fn bearer_token_lists_spaces() {
    let (app, st, token) = app(cfg("spaces")).await;
    let r = app.oneshot(bearer("/api/v1/spaces", &token)).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let j = body_json(r).await;
    let kinds: Vec<&str> = j["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap())
        .collect();
    // Human токену не виден ни в каком виде (как и 404 на его узлы)
    assert_eq!(kinds, ["rules", "knowledge"]);
    let admin = st.system.admin_user().await.unwrap().unwrap();
    assert_eq!(j["user"]["id"], admin.id.to_string());
    assert_eq!(j["user"]["name"], "admin");
    assert_eq!(j["access"], "rw");
    let s0 = &j["spaces"][0];
    assert!(s0["id"].is_string() && s0["name"] == "Rules" && s0["agent_solution_id"].is_null());
}

#[tokio::test]
async fn revoked_token_401_on_next_request() {
    let (app, st, token) = app(cfg("revoke")).await;
    let r = app
        .clone()
        .oneshot(bearer("/api/v1/spaces", &token))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let admin = st.system.admin_user().await.unwrap().unwrap();
    for t in st.system.list_tokens(admin.id).await.unwrap() {
        st.system.revoke_token(t.id).await.unwrap();
    }
    let r = app.oneshot(bearer("/api/v1/spaces", &token)).await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(r.headers()[header::WWW_AUTHENTICATE], "Bearer");
    assert_eq!(
        r.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
}

#[tokio::test]
async fn no_credentials_401() {
    let (app, ..) = app(cfg("nocred")).await;
    let r = app
        .oneshot(Request::get("/api/v1/spaces").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn me_csrf_is_accepted_on_post() {
    let (app, ..) = app(cfg("mecsrf")).await;
    let (cookie, _) = login_ok(&app).await;
    let me = Request::get("/api/v1/auth/me")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let csrf = body_json(app.clone().oneshot(me).await.unwrap()).await["csrf"]
        .as_str()
        .unwrap()
        .to_string();
    let out = |tok: &str| {
        Request::post("/api/v1/auth/logout")
            .header(header::COOKIE, &cookie)
            .header("x-csrf-token", tok)
            .body(Body::empty())
            .unwrap()
    };
    let r = app.clone().oneshot(out("wrong")).await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = app.oneshot(out(&csrf)).await.unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn login_sets_strict_httponly_cookie() {
    let (app, st, _) = app(cfg("cookie")).await;
    let r = app.clone().oneshot(login_req("admin", PW)).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let sc = set_cookie(&r);
    assert!(sc.starts_with("zam_session="), "{sc}");
    assert!(sc.contains("HttpOnly"), "{sc}");
    assert!(sc.contains("SameSite=Strict"), "{sc}");
    assert!(sc.contains("Path=/"), "{sc}");
    assert!(sc.contains("Max-Age=43200"), "{sc}");
    assert!(!sc.contains("Secure"), "{sc}");
    let csrf = body_json(r).await["csrf"].as_str().unwrap().to_string();
    assert!(!csrf.is_empty());
    // сессия работает: /auth/me и /spaces по cookie
    let me = Request::get("/api/v1/auth/me")
        .header(header::COOKIE, cookie_pair(&sc))
        .body(Body::empty())
        .unwrap();
    let r = app.clone().oneshot(me).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let admin = st.system.admin_user().await.unwrap().unwrap();
    let j = body_json(r).await;
    assert_eq!(j["login"], "admin");
    assert_eq!(j["user_id"], admin.id.to_string());
    assert_eq!(j["csrf"], csrf);
    let sp = Request::get("/api/v1/spaces")
        .header(header::COOKIE, format!("other=1; {}", cookie_pair(&sc)))
        .body(Body::empty())
        .unwrap();
    let r = app.oneshot(sp).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let j = body_json(r).await;
    assert_eq!(j["access"], "admin");
    assert_eq!(j["user"]["id"], admin.id.to_string());
    let a = audit_actions(&st).await;
    let row = a.iter().find(|r| r.action == "admin_login").unwrap();
    assert_eq!(row.ip.as_deref(), Some("127.0.0.1"));
}

#[tokio::test]
async fn cookie_path_is_base_path() {
    let mut c = cfg("basepath");
    c.server.base_path = "/memory".into();
    let (app, ..) = app(c).await;
    let mut req = login_req("admin", PW);
    *req.uri_mut() = "/memory/api/v1/auth/login".parse().unwrap();
    let r = app.oneshot(req).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(
        set_cookie(&r).contains("Path=/memory"),
        "{}",
        set_cookie(&r)
    );
}

#[tokio::test]
async fn secure_cookie_behind_trusted_https_proxy() {
    let mut c = cfg("secure");
    c.server.trusted_proxies = vec!["127.0.0.1/32".parse().unwrap()];
    let (app, st, _) = app(c).await;
    let mut req = login_req("admin", PW);
    req.headers_mut()
        .insert("x-forwarded-proto", "https".parse().unwrap());
    req.headers_mut()
        .insert("x-forwarded-for", "203.0.113.7, 127.0.0.1".parse().unwrap());
    let r = app.oneshot(req).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(set_cookie(&r).contains("Secure"), "{}", set_cookie(&r));
    let a = audit_actions(&st).await;
    let row = a.iter().find(|r| r.action == "admin_login").unwrap();
    assert_eq!(row.ip.as_deref(), Some("203.0.113.7"));
}

#[tokio::test]
async fn forwarded_headers_ignored_from_untrusted_peer() {
    let mut c = cfg("untrusted");
    c.server.trusted_proxies = vec!["127.0.0.1/32".parse().unwrap()];
    let (app, st, _) = app(c).await;
    let mut req = login_req("admin", PW);
    req.extensions_mut().insert(peer("10.1.1.1"));
    req.headers_mut()
        .insert("x-forwarded-proto", "https".parse().unwrap());
    req.headers_mut()
        .insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
    let r = app.oneshot(req).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(!set_cookie(&r).contains("Secure"), "{}", set_cookie(&r));
    let a = audit_actions(&st).await;
    let row = a.iter().find(|r| r.action == "admin_login").unwrap();
    assert_eq!(row.ip.as_deref(), Some("10.1.1.1"));
}

#[tokio::test]
async fn admin_post_without_csrf_403() {
    let (app, st, _) = app(cfg("csrf")).await;
    let (cookie, csrf) = login_ok(&app).await;
    let logout = |csrf: Option<&str>| {
        let mut b = Request::post("/api/v1/auth/logout").header(header::COOKIE, &cookie);
        if let Some(c) = csrf {
            b = b.header("x-csrf-token", c);
        }
        b.body(Body::empty()).unwrap()
    };
    let r = app.clone().oneshot(logout(None)).await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = app.clone().oneshot(logout(Some("wrong"))).await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = app.clone().oneshot(logout(Some(&csrf))).await.unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    let sc = set_cookie(&r);
    assert!(
        sc.starts_with("zam_session=;") && sc.contains("Max-Age=0"),
        "{sc}"
    );
    // сессия удалена
    let me = Request::get("/api/v1/auth/me")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.oneshot(me).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(
        audit_actions(&st)
            .await
            .iter()
            .any(|r| r.action == "admin_logout")
    );
}

#[tokio::test]
async fn wrong_login_or_password_same_401_and_audited() {
    let (app, st, _) = app(cfg("wrong")).await;
    let r1 = app
        .clone()
        .oneshot(login_req("admin", "nope-secret"))
        .await
        .unwrap();
    let r2 = app
        .clone()
        .oneshot(login_req("root", "nope-secret"))
        .await
        .unwrap();
    assert_eq!(r1.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(r2.status(), StatusCode::UNAUTHORIZED);
    assert!(r1.headers().get(header::SET_COOKIE).is_none());
    assert_eq!(body_json(r1).await, body_json(r2).await);
    let a = audit_actions(&st).await;
    // неверный пароль администратора и неизвестный логин (§11.4, fix round 1 задачи 12 M4)
    let failed: Vec<_> = a
        .iter()
        .filter(|r| r.action == "admin_login_failed" || r.action == "auth.login.failed")
        .collect();
    assert_eq!(failed.len(), 2);
    for row in failed {
        assert_eq!(row.ip.as_deref(), Some("127.0.0.1"));
        assert!(!row.details.to_string().contains("nope-secret"));
    }
}

#[tokio::test]
async fn malformed_login_body_400() {
    let (app, ..) = app(cfg("badbody")).await;
    let mut r = login_req("admin", PW);
    *r.body_mut() = Body::from("not json");
    let r = app.oneshot(r).await.unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        r.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
}

#[tokio::test]
async fn sixth_login_attempt_in_minute_429() {
    let (app, ..) = app(cfg("ratelogin")).await;
    for _ in 0..5 {
        let r = app
            .clone()
            .oneshot(login_req("admin", "bad"))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    }
    // даже верный пароль: лимит считает каждую попытку
    let r = app.clone().oneshot(login_req("admin", PW)).await.unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    let ra: u64 = r.headers()[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&ra), "{ra}");
    assert_eq!(body_json(r).await["type"], "/problems/rate-limited");
    // другой IP не затронут
    let mut other = login_req("admin", PW);
    other.extensions_mut().insert(peer("10.9.9.9"));
    assert_eq!(app.oneshot(other).await.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn bad_tokens_rate_limited_valid_token_still_passes() {
    let (app, _, token) = app(cfg("ratetoken")).await;
    let with_peer = |t: &str, ip: &str| {
        let mut r = bearer("/api/v1/spaces", t);
        r.extensions_mut().insert(peer(ip));
        r
    };
    // успешные запросы лимит не расходуют
    for _ in 0..6 {
        let r = app
            .clone()
            .oneshot(with_peer(&token, "10.0.0.5"))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }
    let bad = format!("zam_{}", "x".repeat(40));
    for t in [bad.as_str(), "garbage", &bad, "zam_short", &bad] {
        let r = app.clone().oneshot(with_peer(t, "10.0.0.5")).await.unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "{t}");
    }
    // превышение не блокирует верный токен с того же IP (общий NAT/прокси)
    let r = app
        .clone()
        .oneshot(with_peer(&token, "10.0.0.5"))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    // неверный в формате и не в формате — 429 с Retry-After
    for t in [bad.as_str(), "garbage"] {
        let r = app.clone().oneshot(with_peer(t, "10.0.0.5")).await.unwrap();
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS, "{t}");
        assert!(r.headers().contains_key(header::RETRY_AFTER));
        assert_eq!(body_json(r).await["type"], "/problems/rate-limited");
    }
    let r = app.oneshot(with_peer(&token, "10.0.0.6")).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
}

#[tokio::test]
async fn token_cannot_call_admin_routes() {
    let (app, _st, token) = app(cfg("adminonly")).await;
    let r = app
        .clone()
        .oneshot(bearer("/api/v1/admin/users", &token))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = app
        .clone()
        .oneshot(bearer("/api/v1/auth/me", &token))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    // администратор по cookie проходит
    let (cookie, _) = login_ok(&app).await;
    let req = Request::get("/api/v1/admin/users")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn admin_selects_user_via_header() {
    let (app, st, _) = app(cfg("xzamuser")).await;
    let bob = st.system.create_user("bob", false).await.unwrap();
    let (cookie, _) = login_ok(&app).await;
    let req = |v: &str| {
        Request::get("/api/v1/spaces")
            .header(header::COOKIE, &cookie)
            .header("x-zam-user", v)
            .body(Body::empty())
            .unwrap()
    };
    let r = app.clone().oneshot(req(&bob.id.to_string())).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let j = body_json(r).await;
    assert_eq!(j["user"]["name"], "bob");
    assert_eq!(j["spaces"].as_array().unwrap().len(), 3);
    let r = app.clone().oneshot(req("not-an-id")).await.unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    let r = app
        .clone()
        .oneshot(req(&zam_core::Id::new().to_string()))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    // токен пользователя заголовок игнорирует: видит только своё
    let (_, bob_token) = st
        .system
        .issue_token(bob.id, "t", zam_store::Access::Ro)
        .await
        .unwrap();
    let admin = st.system.admin_user().await.unwrap().unwrap();
    let mut r = bearer("/api/v1/spaces", &bob_token);
    r.headers_mut()
        .insert("x-zam-user", admin.id.to_string().parse().unwrap());
    let j = body_json(app.oneshot(r).await.unwrap()).await;
    assert_eq!(j["user"]["name"], "bob");
    assert_eq!(j["access"], "ro");
}

#[tokio::test]
async fn failed_login_audit_has_no_entered_login() {
    let (app, st, _) = app(cfg("auditlogin")).await;
    let r = app
        .clone()
        .oneshot(login_req("s3cretpass", "whatever"))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = app.oneshot(login_req("admin", "wrong")).await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let rows = audit_actions(&st).await;
    let all = format!("{rows:?}");
    assert!(
        !all.contains("s3cretpass") && !all.contains("whatever"),
        "{all}"
    );
    let failed: Vec<_> = rows
        .iter()
        .filter(|r| r.action == "admin_login_failed" || r.action == "auth.login.failed")
        .map(|r| r.action.as_str())
        .collect();
    // новые первыми: неверный пароль администратора, затем неизвестный логин
    assert_eq!(failed, ["admin_login_failed", "auth.login.failed"]);
    let unknown = rows
        .iter()
        .find(|r| r.action == "auth.login.failed")
        .unwrap();
    assert_eq!(
        (unknown.user_id, &unknown.details),
        (None, &serde_json::json!({}))
    );
}

#[test]
fn principal_debug_masks_session_id() {
    let sid = "abcdefghijklmnopqrstuvwxyz0123456789";
    let p = zam_server::auth::Principal::Admin {
        session_id: sid.into(),
    };
    let a = AdminOnly(sid.into());
    let dbg = format!("{p:?} {a:?}");
    assert!(!dbg.contains("efgh"), "{dbg}");
    assert!(dbg.contains("abcd…"), "{dbg}");
}
