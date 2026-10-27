//! Вход пользователей в WebUI (§11.4, §15.1; задача 12 M4): пароль от администратора,
//! обязательная смена, доступ сессии пользователя как у его `rw`-токена плюс свой Human,
//! `author_kind = human`, свои токены и решения.
mod common;

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, StatusCode, header};
use common::api::{Api, Resp, api, req};
use serde_json::{Value, json};

const PASS: &str = "first-pass-123";
const NEW_PASS: &str = "second-pass-456";

/// Сессия пользователя: cookie, CSRF и адрес клиента (лимит входа — 5 в минуту на IP).
struct Web {
    cookie: String,
    csrf: String,
    ip: String,
}

fn with_ip(mut r: axum::http::Request<Body>, ip: &str) -> axum::http::Request<Body> {
    r.extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip.parse().unwrap(), 40_000)));
    r
}

async fn login(a: &Api, name: &str, pw: &str, ip: &str) -> Resp {
    let body = json!({ "login": name, "password": pw });
    let r = req(Method::POST, "/api/v1/auth/login", Some(&body))
        .body(Body::from(body.to_string()))
        .unwrap();
    a.send(with_ip(r, ip)).await
}

async fn login_ok(a: &Api, name: &str, pw: &str, ip: &str) -> Web {
    let r = login(a, name, pw, ip).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let cookie = r
        .header("set-cookie")
        .split(';')
        .next()
        .unwrap()
        .to_string();
    Web {
        cookie,
        csrf: r.json()["csrf"].as_str().unwrap().to_string(),
        ip: ip.into(),
    }
}

/// Запрос сессии пользователя (с CSRF) и дополнительными заголовками.
async fn web_h(
    a: &Api,
    w: &Web,
    m: Method,
    uri: &str,
    body: Option<Value>,
    extra: &[(&str, &str)],
) -> Resp {
    let mut b = req(m, uri, body.as_ref())
        .header(header::COOKIE, &w.cookie)
        .header("x-csrf-token", &w.csrf);
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let body = body.map_or_else(Body::empty, |v| Body::from(v.to_string()));
    a.send(with_ip(b.body(body).unwrap(), &w.ip)).await
}

async fn web(a: &Api, w: &Web, m: Method, uri: &str, body: Option<Value>) -> Resp {
    web_h(a, w, m, uri, body, &[]).await
}

fn ok(r: &Resp, s: StatusCode) {
    assert_eq!(r.status, s, "{}", r.text());
}

/// Пароль пользователю задаёт администратор.
async fn set_pw(a: &Api, uid: &str, pw: &str) -> Resp {
    a.adm(
        Method::POST,
        &format!("/api/v1/admin/users/{uid}/password"),
        Some(json!({ "password": pw })),
        None,
    )
    .await
}

/// Пользователь с паролем после обязательной смены: (id, токен, сессия).
async fn ready_user(a: &Api, name: &str, ip: &str) -> (String, String, Web) {
    let (uid, tok) = a.new_user(name).await;
    ok(&set_pw(a, &uid, PASS).await, StatusCode::OK);
    let w = login_ok(a, name, PASS, ip).await;
    let r = web(
        a,
        &w,
        Method::POST,
        "/api/v1/auth/password",
        Some(json!({ "old": PASS, "new": NEW_PASS })),
    )
    .await;
    ok(&r, StatusCode::NO_CONTENT);
    (uid, tok, w)
}

async fn audit(a: &Api) -> Vec<zam_store::AuditRow> {
    a.st.system.list_audit(200, None).await.unwrap()
}

#[tokio::test]
async fn user_login_and_me() {
    let a = api("ul-me", |_| {}).await;
    let (uid, _) = a.new_user("Алиса").await;
    let r = set_pw(&a, &uid, PASS).await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["has_password"], true);
    assert_eq!(r.json()["must_change_password"], true);
    // логин без учёта регистра (Unicode)
    let w = login_ok(&a, "алиса", PASS, "10.1.0.1").await;
    let me = web(&a, &w, Method::GET, "/api/v1/auth/me", None).await;
    ok(&me, StatusCode::OK);
    let me = me.json();
    assert_eq!(me["kind"], "user");
    assert_eq!(me["user_id"], uid);
    assert_eq!(me["name"], "Алиса");
    assert_eq!(me["must_change_password"], true);
    assert_eq!(me["csrf"], w.csrf);
    // администратор — kind admin
    let am = a
        .adm(Method::GET, "/api/v1/auth/me", None, None)
        .await
        .json();
    assert_eq!(am["kind"], "admin");
    assert_eq!(am["must_change_password"], false);
    assert_eq!(am["user_id"], a.admin_id());
    // неверный пароль — 401; введённое в аудит не попадает
    ok(
        &login(&a, "Алиса", "wrong-password-x", "10.1.0.1").await,
        StatusCode::UNAUTHORIZED,
    );
    ok(
        &login(&a, "нет-такого", "secret-in-login", "10.1.0.1").await,
        StatusCode::UNAUTHORIZED,
    );
    let rows = audit(&a).await;
    let all = format!("{rows:?}");
    for leaked in ["wrong-password-x", "secret-in-login", "нет-такого", PASS] {
        assert!(!all.contains(leaked), "{leaked}: {all}");
    }
    let acts: Vec<&str> = rows.iter().map(|r| r.action.as_str()).collect();
    for want in [
        "user.password.set",
        "auth.login.user",
        "auth.login.user_failed",
        "auth.login.failed",
    ] {
        assert!(acts.contains(&want), "{want}: {acts:?}");
    }
    let login_row = rows.iter().find(|r| r.action == "auth.login.user").unwrap();
    assert_eq!(login_row.user_id.map(|u| u.to_string()), Some(uid.clone()));
    assert_eq!(login_row.actor, "user");
    // смена пароля — аудит user.password.change, me без обязательной смены
    let r = web(
        &a,
        &w,
        Method::POST,
        "/api/v1/auth/password",
        Some(json!({ "old": PASS, "new": NEW_PASS })),
    )
    .await;
    ok(&r, StatusCode::NO_CONTENT);
    let me = web(&a, &w, Method::GET, "/api/v1/auth/me", None)
        .await
        .json();
    assert_eq!(me["must_change_password"], false);
    assert!(
        audit(&a)
            .await
            .iter()
            .any(|r| r.action == "user.password.change")
    );
    // выход
    ok(
        &web(&a, &w, Method::POST, "/api/v1/auth/logout", None).await,
        StatusCode::NO_CONTENT,
    );
    ok(
        &web(&a, &w, Method::GET, "/api/v1/auth/me", None).await,
        StatusCode::UNAUTHORIZED,
    );
}

#[tokio::test]
async fn user_without_password_cannot_login() {
    let a = api("ul-nopw", |_| {}).await;
    let (uid, _) = a.new_user("bob").await;
    ok(
        &login(&a, "bob", PASS, "10.2.0.1").await,
        StatusCode::UNAUTHORIZED,
    );
    // пароль слишком короткий — 400; администратору — 409 (пароль в конфигурации)
    ok(&set_pw(&a, &uid, "short").await, StatusCode::BAD_REQUEST);
    ok(&set_pw(&a, &a.admin_id(), PASS).await, StatusCode::CONFLICT);
    // пользователь администратора по паролю из базы не входит
    ok(
        &login(&a, "admin", PASS, "10.2.0.1").await,
        StatusCode::UNAUTHORIZED,
    );
    ok(&set_pw(&a, &uid, PASS).await, StatusCode::OK);
    let w = login_ok(&a, "bob", PASS, "10.2.0.1").await;
    // закрытый вход: сессия больше не действует, новый вход — 401
    let r = a
        .adm(
            Method::DELETE,
            &format!("/api/v1/admin/users/{uid}/password"),
            None,
            None,
        )
        .await;
    ok(&r, StatusCode::NO_CONTENT);
    ok(
        &web(&a, &w, Method::GET, "/api/v1/auth/me", None).await,
        StatusCode::UNAUTHORIZED,
    );
    ok(
        &login(&a, "bob", PASS, "10.2.0.2").await,
        StatusCode::UNAUTHORIZED,
    );
    let u =
        a.st.system
            .get_user(uid.parse().unwrap())
            .await
            .unwrap()
            .unwrap();
    assert!(!u.has_password && !u.must_change_password);
    assert!(
        audit(&a)
            .await
            .iter()
            .any(|r| r.action == "user.password.clear")
    );
    // по токену пароль не задаётся
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/admin/users/{uid}/password"),
            Some(json!({ "password": PASS })),
        )
        .await;
    ok(&r, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn disabled_user_401() {
    let a = api("ul-disabled", |_| {}).await;
    let (uid, _, w) = ready_user(&a, "carol", "10.3.0.1").await;
    ok(
        &web(&a, &w, Method::GET, "/api/v1/spaces", None).await,
        StatusCode::OK,
    );
    let r = a
        .adm(
            Method::PATCH,
            &format!("/api/v1/admin/users/{uid}"),
            Some(json!({ "disabled": true })),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    ok(
        &web(&a, &w, Method::GET, "/api/v1/spaces", None).await,
        StatusCode::UNAUTHORIZED,
    );
    ok(
        &login(&a, "carol", NEW_PASS, "10.3.0.2").await,
        StatusCode::UNAUTHORIZED,
    );
    let failed = audit(&a)
        .await
        .into_iter()
        .find(|r| r.action == "auth.login.user_failed")
        .unwrap();
    assert_eq!(failed.details["disabled"], true);
}

#[tokio::test]
async fn must_change_password_blocks_until_changed() {
    let a = api("ul-mustchange", |_| {}).await;
    let (uid, _) = a.new_user("dave").await;
    ok(&set_pw(&a, &uid, PASS).await, StatusCode::OK);
    let w = login_ok(&a, "dave", PASS, "10.4.0.1").await;
    for (m, uri) in [
        (Method::GET, "/api/v1/spaces"),
        (Method::GET, "/api/v1/auth/tokens"),
        (Method::GET, "/api/v1/index/status"),
    ] {
        let r = web(&a, &w, m, uri, None).await;
        ok(&r, StatusCode::FORBIDDEN);
        assert_eq!(
            r.json()["type"],
            "/problems/password-change-required",
            "{uri}"
        );
    }
    ok(
        &web(&a, &w, Method::GET, "/api/v1/auth/me", None).await,
        StatusCode::OK,
    );
    let change = |old: &str, new: &str| json!({ "old": old, "new": new });
    let r = web(
        &a,
        &w,
        Method::POST,
        "/api/v1/auth/password",
        Some(change("wrong-old-pass", NEW_PASS)),
    )
    .await;
    ok(&r, StatusCode::FORBIDDEN);
    let r = web(
        &a,
        &w,
        Method::POST,
        "/api/v1/auth/password",
        Some(change(PASS, "short")),
    )
    .await;
    ok(&r, StatusCode::BAD_REQUEST);
    let r = web(
        &a,
        &w,
        Method::POST,
        "/api/v1/auth/password",
        Some(change(PASS, PASS)),
    )
    .await;
    ok(&r, StatusCode::BAD_REQUEST);
    let r = web(
        &a,
        &w,
        Method::POST,
        "/api/v1/auth/password",
        Some(change(PASS, NEW_PASS)),
    )
    .await;
    ok(&r, StatusCode::NO_CONTENT);
    // текущая сессия продолжает работать
    ok(
        &web(&a, &w, Method::GET, "/api/v1/spaces", None).await,
        StatusCode::OK,
    );
    ok(
        &login(&a, "dave", PASS, "10.4.0.2").await,
        StatusCode::UNAUTHORIZED,
    );
    let w2 = login_ok(&a, "dave", NEW_PASS, "10.4.0.2").await;
    // новый пароль от администратора: сессии закрываются, снова обязательная смена
    ok(&set_pw(&a, &uid, "third-pass-789").await, StatusCode::OK);
    ok(
        &web(&a, &w2, Method::GET, "/api/v1/spaces", None).await,
        StatusCode::UNAUTHORIZED,
    );
    let w3 = login_ok(&a, "dave", "third-pass-789", "10.4.0.3").await;
    let r = web(&a, &w3, Method::GET, "/api/v1/spaces", None).await;
    assert_eq!(r.json()["type"], "/problems/password-change-required");
    // пароль администратора через /auth/password не меняется
    let r = a
        .adm(
            Method::POST,
            "/api/v1/auth/password",
            Some(change("pw", NEW_PASS)),
            None,
        )
        .await;
    ok(&r, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn user_session_cannot_use_x_zam_user_403() {
    let a = api("ul-xzam", |_| {}).await;
    let (uid, _, w) = ready_user(&a, "erin", "10.5.0.1").await;
    let admin = a.admin_id();
    let r = web_h(
        &a,
        &w,
        Method::GET,
        "/api/v1/spaces",
        None,
        &[("x-zam-user", &admin)],
    )
    .await;
    ok(&r, StatusCode::FORBIDDEN);
    let r = web_h(
        &a,
        &w,
        Method::GET,
        "/api/v1/spaces",
        None,
        &[("x-zam-user", "мусор")],
    )
    .await;
    ok(&r, StatusCode::FORBIDDEN);
    // свой id — как без заголовка (WebUI шлёт его всегда)
    let r = web_h(
        &a,
        &w,
        Method::GET,
        "/api/v1/spaces",
        None,
        &[("x-zam-user", &uid)],
    )
    .await;
    ok(&r, StatusCode::OK);
    let j = r.json();
    assert_eq!(j["user"]["id"], uid);
    assert_eq!(j["access"], "rw");
    let r = web_h(
        &a,
        &w,
        Method::POST,
        "/api/v1/nodes",
        Some(json!({"space": "knowledge", "path": "x", "kind": "note", "content": "# x"})),
        &[("x-zam-user", &admin)],
    )
    .await;
    ok(&r, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn user_session_admin_routes_403() {
    let a = api("ul-admin403", |_| {}).await;
    let (uid, _, w) = ready_user(&a, "frank", "10.6.0.1").await;
    for (m, uri, body) in [
        (Method::GET, "/api/v1/admin/users".to_string(), None),
        (
            Method::POST,
            "/api/v1/admin/users".to_string(),
            Some(json!({"name": "x"})),
        ),
        (
            Method::GET,
            format!("/api/v1/admin/users/{uid}/tokens"),
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/admin/users/{uid}/password"),
            Some(json!({"password": NEW_PASS})),
        ),
        (Method::GET, "/api/v1/admin/audit".to_string(), None),
        (Method::GET, "/api/v1/admin/settings".to_string(), None),
        (
            Method::POST,
            "/api/v1/admin/reindex".to_string(),
            Some(json!({})),
        ),
    ] {
        let r = web(&a, &w, m.clone(), &uri, body).await;
        ok(&r, StatusCode::FORBIDDEN);
        assert_ne!(
            r.json()["type"],
            "/problems/password-change-required",
            "{m} {uri}"
        );
    }
    // статус индекса — только своя база (баннер перенарезки у пользователя, R9)
    let r = web(&a, &w, Method::GET, "/api/v1/index/status", None).await;
    ok(&r, StatusCode::OK);
    let j = r.json();
    let dbs: Vec<&str> = j["dbs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["user_id"].as_str().unwrap())
        .collect();
    assert_eq!(dbs, [uid.as_str()]);
    assert!(j["worker"].is_null());
}

#[tokio::test]
async fn user_session_sees_own_human_not_others() {
    let a = api("ul-human", |_| {}).await;
    let (_, tok, w) = ready_user(&a, "gina", "10.7.0.1").await;
    let r = web(
        &a,
        &w,
        Method::POST,
        "/api/v1/nodes",
        Some(json!({"space": "human", "path": "дневник", "kind": "note", "content": "# Личное\n\nсекрет"})),
    )
    .await;
    ok(&r, StatusCode::CREATED);
    let own = r.json()["id"].as_str().unwrap().to_string();
    ok(
        &web(&a, &w, Method::GET, &format!("/api/v1/nodes/{own}"), None).await,
        StatusCode::OK,
    );
    let kinds: Vec<String> = web(&a, &w, Method::GET, "/api/v1/spaces", None)
        .await
        .json()["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap().to_string())
        .collect();
    assert!(kinds.contains(&"human".to_string()), "{kinds:?}");
    // свой Human — не по токену
    ok(
        &a.tok(&tok, Method::GET, &format!("/api/v1/nodes/{own}"), None)
            .await,
        StatusCode::NOT_FOUND,
    );
    // чужой Human (администратора) и чужая Knowledge — 404
    let foreign = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "human", "path": "адм", "kind": "note", "content": "# адм"})),
            None,
        )
        .await;
    ok(&foreign, StatusCode::CREATED);
    let fid = foreign.json()["id"].as_str().unwrap().to_string();
    ok(
        &web(&a, &w, Method::GET, &format!("/api/v1/nodes/{fid}"), None).await,
        StatusCode::NOT_FOUND,
    );
    let kn = a.note("knowledge", "адм-знание", "# знание").await;
    let kid = kn["id"].as_str().unwrap();
    ok(
        &web(&a, &w, Method::GET, &format!("/api/v1/nodes/{kid}"), None).await,
        StatusCode::NOT_FOUND,
    );
    // поиск с include_human — свой Human находится (FTS)
    let r = web(
        &a,
        &w,
        Method::POST,
        "/api/v1/search",
        Some(json!({"query": "секрет", "scope": {"include_human": true}, "format": "json"})),
    )
    .await;
    ok(&r, StatusCode::OK);
    let found = r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["node_id"] == own);
    assert!(found, "{}", r.text());
}

#[tokio::test]
async fn user_session_solution_access_like_token() {
    let a = api("ul-member", |_| {}).await;
    let (_, _, w) = ready_user(&a, "hank", "10.8.0.1").await;
    let (_, owner_tok) = a.new_user("owner").await;
    let guid = "0e2e0000-0000-4000-8000-0000000000a1";
    let r = a
        .tok(
            &owner_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": guid, "name": "Чужое"})),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    let note = a
        .tok(
            &owner_tok,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": guid, "path": "doc", "kind": "note", "content": "# doc"})),
        )
        .await;
    ok(&note, StatusCode::CREATED);
    let nid = note.json()["id"].as_str().unwrap().to_string();
    // постороннему — 404; запрос доступа сессией — 202 pending, затем 403 solution-access
    ok(
        &web(&a, &w, Method::GET, &format!("/api/v1/nodes/{nid}"), None).await,
        StatusCode::NOT_FOUND,
    );
    let r = web(
        &a,
        &w,
        Method::POST,
        "/api/v1/solutions",
        Some(json!({"agent_solution_id": guid, "name": "Чужое"})),
    )
    .await;
    ok(&r, StatusCode::ACCEPTED);
    let r = web(
        &a,
        &w,
        Method::GET,
        &format!("/api/v1/solutions/{guid}"),
        None,
    )
    .await;
    ok(&r, StatusCode::FORBIDDEN);
    assert_eq!(r.json()["type"], "/problems/solution-access");
    // владелец одобряет ro: чтение есть, запись — 403
    let hank =
        a.st.system
            .list_users()
            .await
            .unwrap()
            .into_iter()
            .find(|u| u.name == "hank")
            .unwrap()
            .id;
    let r = a
        .tok(
            &owner_tok,
            Method::POST,
            &format!("/api/v1/solutions/{guid}/access/{hank}/approve"),
            Some(json!({"access": "ro"})),
        )
        .await;
    ok(&r, StatusCode::OK);
    ok(
        &web(&a, &w, Method::GET, &format!("/api/v1/nodes/{nid}"), None).await,
        StatusCode::OK,
    );
    let r = web(
        &a,
        &w,
        Method::POST,
        "/api/v1/nodes",
        Some(json!({"space": guid, "path": "mine", "kind": "note", "content": "# m"})),
    )
    .await;
    ok(&r, StatusCode::FORBIDDEN);
    // управление доступом чужого решения — 403
    ok(
        &web(
            &a,
            &w,
            Method::GET,
            &format!("/api/v1/solutions/{guid}/access"),
            None,
        )
        .await,
        StatusCode::FORBIDDEN,
    );
    let spaces = web(&a, &w, Method::GET, "/api/v1/spaces", None)
        .await
        .json();
    let member = spaces["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["agent_solution_id"] == guid)
        .unwrap()
        .clone();
    assert_eq!(
        (member["role"].as_str(), member["access"].as_str()),
        (Some("member"), Some("ro"))
    );
}

#[tokio::test]
async fn user_session_writes_as_human_author() {
    let a = api("ul-author", |_| {}).await;
    let (uid, _, w) = ready_user(&a, "ivy", "10.9.0.1").await;
    let r = web(
        &a,
        &w,
        Method::POST,
        "/api/v1/nodes",
        Some(json!({"space": "knowledge", "path": "n", "kind": "note", "content": "# n"})),
    )
    .await;
    ok(&r, StatusCode::CREATED);
    let nid = r.json()["id"].as_str().unwrap().to_string();
    let revs = web(
        &a,
        &w,
        Method::GET,
        &format!("/api/v1/nodes/{nid}/revisions"),
        None,
    )
    .await;
    ok(&revs, StatusCode::OK);
    let rev0 = &revs.json()[0];
    assert_eq!(rev0["author_kind"], "human");
    assert!(rev0["token_id"].is_null());
    // insight без certainty: у человека он остаётся пустым; смена статуса без причины
    let r = web(
        &a,
        &w,
        Method::POST,
        "/api/v1/spaces/knowledge/entries",
        Some(
            json!({"kind": "insight", "title": "Вывод человека", "body": "Тело.",
                    "data": {"kind": "discovery"}, "force": true}),
        ),
    )
    .await;
    ok(&r, StatusCode::CREATED);
    let e = r.json();
    assert!(e["certainty"].is_null(), "{e}");
    assert_eq!(e["author"]["kind"], "human");
    assert_eq!(e["author"]["user_id"], uid);
    let id = e["id"].as_str().unwrap();
    let uri = format!("/api/v1/entries/{id}");
    // без If-Match — 428
    let r = web(
        &a,
        &w,
        Method::PATCH,
        &uri,
        Some(json!({"certainty": "axiom"})),
    )
    .await;
    ok(&r, StatusCode::PRECONDITION_REQUIRED);
    let rev = web(&a, &w, Method::GET, &uri, None).await.json()["rev"].to_string();
    let r = web_h(
        &a,
        &w,
        Method::PATCH,
        &uri,
        Some(json!({"certainty": "axiom"})),
        &[("if-match", &rev)],
    )
    .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["certainty"], "axiom");
}

#[tokio::test]
async fn user_manages_own_solution_access() {
    let a = api("ul-mysol", |_| {}).await;
    let (_, _, w) = ready_user(&a, "jack", "10.10.0.1").await;
    let guid = "0e2e0000-0000-4000-8000-0000000000b2";
    let r = web(
        &a,
        &w,
        Method::POST,
        "/api/v1/solutions",
        Some(json!({"agent_solution_id": guid, "name": "Моё"})),
    )
    .await;
    ok(&r, StatusCode::CREATED);
    let (kim, kim_tok) = a.new_user("kim").await;
    let r = a
        .tok(
            &kim_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": guid, "name": "Моё"})),
        )
        .await;
    ok(&r, StatusCode::ACCEPTED);
    // список решений сессии: своё с ролью owner
    let list = web(&a, &w, Method::GET, "/api/v1/solutions", None)
        .await
        .json();
    let mine = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["agent_solution_id"] == guid)
        .unwrap();
    assert_eq!(mine["role"], "owner");
    let acc = web(
        &a,
        &w,
        Method::GET,
        &format!("/api/v1/solutions/{guid}/access"),
        None,
    )
    .await;
    ok(&acc, StatusCode::OK);
    assert_eq!(acc.json()["pending"], 1);
    let r = web(
        &a,
        &w,
        Method::POST,
        &format!("/api/v1/solutions/{guid}/access/{kim}/approve"),
        Some(json!({"access": "rw"})),
    )
    .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["state"], "approved");
    let r = web(
        &a,
        &w,
        Method::POST,
        &format!("/api/v1/solutions/{guid}/access/{kim}/revoke"),
        None,
    )
    .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["state"], "revoked");
    let r = web(
        &a,
        &w,
        Method::PATCH,
        &format!("/api/v1/solutions/{guid}"),
        Some(json!({"name": "Моё новое"})),
    )
    .await;
    ok(&r, StatusCode::OK);
    // участник (бывший) не управляет доступом
    ok(
        &a.tok(
            &kim_tok,
            Method::GET,
            &format!("/api/v1/solutions/{guid}/access"),
            None,
        )
        .await,
        StatusCode::FORBIDDEN,
    );
}

#[tokio::test]
async fn user_issues_and_revokes_own_tokens() {
    let a = api("ul-tokens", |_| {}).await;
    let (uid, first_tok, w) = ready_user(&a, "lena", "10.11.0.1").await;
    let r = web(
        &a,
        &w,
        Method::POST,
        "/api/v1/auth/tokens",
        Some(json!({"name": "ноутбук", "access": "ro"})),
    )
    .await;
    ok(&r, StatusCode::CREATED);
    let j = r.json();
    let plain = j["plaintext"].as_str().unwrap().to_string();
    let tid = j["info"]["id"].as_str().unwrap().to_string();
    assert_eq!(j["info"]["user_id"], uid);
    assert_eq!(j["info"]["access"], "ro");
    let list = web(&a, &w, Method::GET, "/api/v1/auth/tokens", None)
        .await
        .json();
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert!(!list.to_string().contains(&plain));
    // ro-токен читает, но не пишет
    ok(
        &a.tok(&plain, Method::GET, "/api/v1/spaces", None).await,
        StatusCode::OK,
    );
    let w_note = json!({"space": "knowledge", "path": "t", "kind": "note", "content": "# t"});
    ok(
        &a.tok(&plain, Method::POST, "/api/v1/nodes", Some(w_note))
            .await,
        StatusCode::FORBIDDEN,
    );
    // human: true — 400; по токену свои токены не управляются
    let r = web(
        &a,
        &w,
        Method::POST,
        "/api/v1/auth/tokens",
        Some(json!({"name": "h", "human": true})),
    )
    .await;
    ok(&r, StatusCode::BAD_REQUEST);
    ok(
        &a.tok(&first_tok, Method::GET, "/api/v1/auth/tokens", None)
            .await,
        StatusCode::FORBIDDEN,
    );
    // чужой токен — 404
    let admin_tok =
        a.st.system
            .list_tokens(a.st.admin_id)
            .await
            .unwrap()
            .remove(0)
            .id;
    ok(
        &web(
            &a,
            &w,
            Method::DELETE,
            &format!("/api/v1/auth/tokens/{admin_tok}"),
            None,
        )
        .await,
        StatusCode::NOT_FOUND,
    );
    ok(
        &web(
            &a,
            &w,
            Method::DELETE,
            &format!("/api/v1/auth/tokens/{tid}"),
            None,
        )
        .await,
        StatusCode::NO_CONTENT,
    );
    ok(
        &a.tok(&plain, Method::GET, "/api/v1/spaces", None).await,
        StatusCode::UNAUTHORIZED,
    );
    let rows = audit(&a).await;
    let issue = rows
        .iter()
        .find(|r| r.action == "token_issue" && r.target.as_deref() == Some(&tid))
        .unwrap();
    assert_eq!(issue.actor, "user");
    assert!(
        rows.iter()
            .any(|r| r.action == "token_revoke" && r.actor == "user")
    );
}

#[tokio::test]
async fn csrf_required_for_user_session() {
    let a = api("ul-csrf", |_| {}).await;
    let (_, _, w) = ready_user(&a, "mia", "10.12.0.1").await;
    let body = json!({"space": "knowledge", "path": "c", "kind": "note", "content": "# c"});
    let no_csrf = req(Method::POST, "/api/v1/nodes", Some(&body))
        .header(header::COOKIE, &w.cookie)
        .body(Body::from(body.to_string()))
        .unwrap();
    ok(
        &a.send(with_ip(no_csrf, &w.ip)).await,
        StatusCode::FORBIDDEN,
    );
    let bad = Web {
        cookie: w.cookie.clone(),
        csrf: "чужой".into(),
        ip: w.ip.clone(),
    };
    ok(
        &web(&a, &bad, Method::POST, "/api/v1/nodes", Some(body.clone())).await,
        StatusCode::FORBIDDEN,
    );
    // GET — без CSRF
    let get = req(Method::GET, "/api/v1/spaces", None)
        .header(header::COOKIE, &w.cookie)
        .body(Body::empty())
        .unwrap();
    ok(&a.send(with_ip(get, &w.ip)).await, StatusCode::OK);
    ok(
        &web(&a, &w, Method::POST, "/api/v1/nodes", Some(body)).await,
        StatusCode::CREATED,
    );
}

#[tokio::test]
async fn login_rate_limit_shared_with_admin() {
    let a = api("ul-rate", |_| {}).await;
    let (uid, _) = a.new_user("nina").await;
    ok(&set_pw(&a, &uid, PASS).await, StatusCode::OK);
    let ip = "10.13.0.1";
    for _ in 0..3 {
        ok(
            &login(&a, "admin", "wrong", ip).await,
            StatusCode::UNAUTHORIZED,
        );
    }
    ok(
        &login(&a, "nina", "wrong-password", ip).await,
        StatusCode::UNAUTHORIZED,
    );
    login_ok(&a, "nina", PASS, ip).await;
    ok(
        &login(&a, "nina", PASS, ip).await,
        StatusCode::TOO_MANY_REQUESTS,
    );
}

#[tokio::test]
async fn reenabled_user_old_cookie_401() {
    let a = api("ul-reenable", |_| {}).await;
    let (uid, _, w) = ready_user(&a, "olga", "10.14.0.1").await;
    let patch = |d: bool| json!({ "disabled": d });
    let uri = format!("/api/v1/admin/users/{uid}");
    ok(
        &a.adm(Method::PATCH, &uri, Some(patch(true)), None).await,
        StatusCode::OK,
    );
    ok(
        &a.adm(Method::PATCH, &uri, Some(patch(false)), None).await,
        StatusCode::OK,
    );
    // сессия закрыта при отключении: включение её не оживляет
    ok(
        &web(&a, &w, Method::GET, "/api/v1/spaces", None).await,
        StatusCode::UNAUTHORIZED,
    );
    // новый вход — работает
    let w2 = login_ok(&a, "olga", NEW_PASS, "10.14.0.2").await;
    ok(
        &web(&a, &w2, Method::GET, "/api/v1/spaces", None).await,
        StatusCode::OK,
    );
}

#[tokio::test]
async fn deleted_user_cookie_401() {
    let a = api("ul-deleted", |_| {}).await;
    let (uid, _, w) = ready_user(&a, "pavel", "10.15.0.1").await;
    let r = a
        .adm(
            Method::DELETE,
            &format!("/api/v1/admin/users/{uid}"),
            None,
            None,
        )
        .await;
    ok(&r, StatusCode::NO_CONTENT);
    ok(
        &web(&a, &w, Method::GET, "/api/v1/auth/me", None).await,
        StatusCode::UNAUTHORIZED,
    );
    let n =
        a.st.system
            .get_web_session(w.cookie.trim_start_matches("zam_session="))
            .await
            .unwrap();
    assert!(n.is_none());
}

#[tokio::test]
async fn password_for_admin_login_name_409() {
    let a = api("ul-adminname", |_| {}).await;
    // логин администратора сверяется точно: «ADMIN» при логине «admin» — обычный пользователь,
    // пароль ему задаётся и вход работает
    let (uid, _) = a.new_user("ADMIN").await;
    ok(&set_pw(&a, &uid, PASS).await, StatusCode::OK);
}
