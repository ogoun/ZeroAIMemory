//! Admin API: пользователи, токены, аудит.
mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use zam_server::config::ServerConfig;
use zam_server::{AppState, build};

const PW: &str = "pw";

struct Ctx {
    app: Router,
    st: AppState,
    admin_token: String,
    cookie: String,
    csrf: String,
}

async fn ctx(name: &str) -> Ctx {
    let c: ServerConfig = common::cfg_in(common::tmp(&format!("admin-{name}")));
    let (app, st, rep) = build(c, Some(PW.into())).await.unwrap();
    let r = app
        .clone()
        .oneshot(
            Request::post("/api/v1/auth/login")
                .body(Body::from(
                    json!({"login":"admin","password":PW}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let cookie = r.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let csrf = json_of(r).await["csrf"].as_str().unwrap().to_string();
    Ctx {
        app,
        st,
        admin_token: rep.initial_token.unwrap(),
        cookie,
        csrf,
    }
}

async fn json_of(r: axum::response::Response) -> Value {
    let b = r.into_body().collect().await.unwrap().to_bytes();
    if b.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(&b).unwrap()
}

fn bearer_get(uri: &str, token: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

impl Ctx {
    async fn call(&self, m: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(m)
            .uri(uri)
            .header(header::COOKIE, &self.cookie)
            .header("x-csrf-token", &self.csrf);
        let body = match body {
            Some(v) => {
                b = b.header(header::CONTENT_TYPE, "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let r = self
            .app
            .clone()
            .oneshot(b.body(body).unwrap())
            .await
            .unwrap();
        (r.status(), json_of(r).await)
    }

    async fn create(&self, name: &str) -> Value {
        let (s, j) = self
            .call(
                Method::POST,
                "/api/v1/admin/users",
                Some(json!({"name": name})),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{j}");
        j
    }
}

#[tokio::test]
async fn create_user_returns_token_once() {
    let c = ctx("once").await;
    let j = c.create("ivan").await;
    let plain = j["token"]["plaintext"].as_str().unwrap().to_string();
    assert!(plain.starts_with("zam_"));
    assert_eq!(j["user"]["name"], "ivan");
    assert_eq!(j["token"]["info"]["name"], "default");
    assert_eq!(j["token"]["info"]["access"], "rw");
    assert!(j["token"]["info"].get("secret_hash").is_none());
    let id = j["user"]["id"].as_str().unwrap();
    let (s, l) = c
        .call(
            Method::GET,
            &format!("/api/v1/admin/users/{id}/tokens"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(!l.to_string().contains(&plain));
    assert!(!l.to_string().contains("secret_hash"));
    assert_eq!(l[0]["prefix"], plain[4..12]);
    // база пользователя создана
    assert!(c.st.data_dir.users_dir().join(format!("{id}.db")).exists());
    // в списке пользователей — счётчик токенов
    let (_, users) = c.call(Method::GET, "/api/v1/admin/users", None).await;
    let u = users
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == id)
        .unwrap();
    assert_eq!(u["tokens"], 1);
    assert_eq!(u["is_admin"], false);
    assert!(u["disabled_at"].is_null());
}

#[tokio::test]
async fn new_user_token_sees_own_spaces() {
    let c = ctx("spaces").await;
    let j = c.create("olga").await;
    let plain = j["token"]["plaintext"].as_str().unwrap();
    let r = c
        .app
        .clone()
        .oneshot(bearer_get("/api/v1/spaces", plain))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let s = json_of(r).await;
    assert_eq!(s["user"]["name"], "olga");
    // по токену — без Human
    assert_eq!(s["spaces"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn cannot_delete_admin_409() {
    let c = ctx("deladmin").await;
    let id = c.st.admin_id;
    let (s, j) = c
        .call(Method::DELETE, &format!("/api/v1/admin/users/{id}"), None)
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(j["type"], "/problems/conflict");
    let (s, _) = c
        .call(
            Method::PATCH,
            &format!("/api/v1/admin/users/{id}"),
            Some(json!({"disabled": true})),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let u = c.st.system.get_user(id).await.unwrap().unwrap();
    assert!(u.disabled_at.is_none());
}

#[tokio::test]
async fn audit_records_token_issue_masked() {
    let c = ctx("audit").await;
    let j = c.create("ivan").await;
    let plain = j["token"]["plaintext"].as_str().unwrap().to_string();
    let uid = j["user"]["id"].as_str().unwrap().to_string();
    let (s, t) = c
        .call(
            Method::POST,
            &format!("/api/v1/admin/users/{uid}/tokens"),
            Some(json!({"name": "second"})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let plain2 = t["plaintext"].as_str().unwrap();
    let (s, a) = c.call(Method::GET, "/api/v1/admin/audit", None).await;
    assert_eq!(s, StatusCode::OK);
    let text = a.to_string();
    assert!(!text.contains(&plain) && !text.contains(plain2));
    let items = a["items"].as_array().unwrap();
    let actions: Vec<&str> = items
        .iter()
        .map(|r| r["action"].as_str().unwrap())
        .collect();
    assert!(actions.contains(&"user_create") && actions.contains(&"token_issue"));
    let masked = format!("zam_{}…", &plain2[4..12]);
    let row = items
        .iter()
        .find(|r| r["action"] == "token_issue" && r["details"]["token"] == masked)
        .expect("запись token_issue с маской второго токена");
    assert_eq!(row["actor"], "admin");
    assert_eq!(row["user_id"], uid);
    assert!(row["ip"].is_string());
    // в самой таблице тоже нет открытого текста
    for r in c.st.system.list_audit(100, None).await.unwrap() {
        let d = r.details.to_string();
        assert!(!d.contains(&plain) && !d.contains(plain2));
    }
}

#[tokio::test]
async fn ro_token_issued_with_access_ro() {
    let c = ctx("ro").await;
    let uid = c.create("ivan").await["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let uri = format!("/api/v1/admin/users/{uid}/tokens");
    let (s, t) = c
        .call(Method::POST, &uri, Some(json!({"name":"r","access":"ro"})))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(t["info"]["access"], "ro");
    let (s, _) = c
        .call(
            Method::POST,
            &uri,
            Some(json!({"name":"r","access":"admin"})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, d) = c.call(Method::POST, &uri, Some(json!({"name":"d"}))).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(d["info"]["access"], "rw");
    let (s, _) = c.call(Method::POST, &uri, Some(json!({"name":"  "}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn user_name_validation_and_duplicate() {
    let c = ctx("names").await;
    for bad in ["", "   ", &"x".repeat(65), "a\nb"] {
        let (s, _) = c
            .call(
                Method::POST,
                "/api/v1/admin/users",
                Some(json!({"name": bad})),
            )
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad:?}");
    }
    c.create("ivan").await;
    let (s, j) = c
        .call(
            Method::POST,
            "/api/v1/admin/users",
            Some(json!({"name":"ivan"})),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(j["type"], "/problems/conflict");
    // мусорное тело
    let (s, _) = c
        .call(Method::POST, "/api/v1/admin/users", Some(json!({"nme": 1})))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn patch_rename_disable_and_delete() {
    let c = ctx("patch").await;
    let a = c.create("ann").await;
    c.create("bob").await;
    let aid = a["user"]["id"].as_str().unwrap();
    let plain = a["token"]["plaintext"].as_str().unwrap();
    let uri = format!("/api/v1/admin/users/{aid}");
    let (s, j) = c
        .call(Method::PATCH, &uri, Some(json!({"name":"bob"})))
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{j}");
    let (s, j) = c
        .call(Method::PATCH, &uri, Some(json!({"name":"anna"})))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["name"], "anna");
    let (s, j) = c
        .call(Method::PATCH, &uri, Some(json!({"disabled": true})))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(j["disabled_at"].is_string());
    // токен отключённого не работает; выпуск нового — 409
    let r = c
        .app
        .clone()
        .oneshot(bearer_get("/api/v1/spaces", plain))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let (s, _) = c
        .call(
            Method::POST,
            &format!("{uri}/tokens"),
            Some(json!({"name":"x"})),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, j) = c
        .call(Method::PATCH, &uri, Some(json!({"disabled": false})))
        .await;
    assert_eq!((s, j["disabled_at"].is_null()), (StatusCode::OK, true));
    // мягкое удаление
    let (s, _) = c.call(Method::DELETE, &uri, None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let u = c.st.system.get_user(aid.parse().unwrap()).await.unwrap();
    assert!(u.unwrap().disabled_at.is_some());
    let missing = format!("/api/v1/admin/users/{}", zam_core::Id::new());
    let (s, _) = c.call(Method::DELETE, &missing, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = c
        .call(Method::PATCH, &missing, Some(json!({"name":"q"})))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = c
        .call(Method::DELETE, "/api/v1/admin/users/not-an-id", None)
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let actions: Vec<String> =
        c.st.system
            .list_audit(100, None)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.action)
            .collect();
    assert!(actions.iter().any(|a| a == "user_update"));
    assert!(actions.iter().filter(|a| *a == "user_disable").count() >= 2);
}

#[tokio::test]
async fn revoke_token_204_then_401_and_404() {
    let c = ctx("revoke").await;
    let j = c.create("ivan").await;
    let plain = j["token"]["plaintext"].as_str().unwrap();
    let tid = j["token"]["info"]["id"].as_str().unwrap();
    let (s, _) = c
        .call(Method::DELETE, &format!("/api/v1/admin/tokens/{tid}"), None)
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let r = c
        .app
        .clone()
        .oneshot(bearer_get("/api/v1/spaces", plain))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let (s, _) = c
        .call(
            Method::DELETE,
            &format!("/api/v1/admin/tokens/{}", zam_core::Id::new()),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let a = c.st.system.list_audit(100, None).await.unwrap();
    let row = a.iter().find(|r| r.action == "token_revoke").unwrap();
    assert_eq!(row.target.as_deref(), Some(tid));
}

#[tokio::test]
async fn audit_pagination() {
    let c = ctx("page").await;
    for i in 0..5 {
        c.create(&format!("u{i}")).await;
    }
    let (_, p1) = c
        .call(Method::GET, "/api/v1/admin/audit?limit=4", None)
        .await;
    assert_eq!(p1["items"].as_array().unwrap().len(), 4);
    let next = p1["next"].as_i64().unwrap();
    assert_eq!(p1["items"][3]["id"], next);
    let (_, p2) = c
        .call(
            Method::GET,
            &format!("/api/v1/admin/audit?limit=100&before={next}"),
            None,
        )
        .await;
    assert!(p2["next"].is_null());
    assert!(p2["items"][0]["id"].as_i64().unwrap() < next);
    let (s, _) = c
        .call(Method::GET, "/api/v1/admin/audit?limit=abc", None)
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = c
        .call(Method::GET, "/api/v1/admin/audit?limit=0", None)
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // больше максимума — усечение до 500, не ошибка
    let (s, _) = c
        .call(Method::GET, "/api/v1/admin/audit?limit=9999", None)
        .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn admin_routes_reject_tokens_and_anonymous() {
    let c = ctx("authz").await;
    let uri = "/api/v1/admin/users";
    let r = c
        .app
        .clone()
        .oneshot(bearer_get(uri, &c.admin_token))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = c
        .app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    // изменяющий запрос без CSRF
    let r = c
        .app
        .clone()
        .oneshot(
            Request::post(uri)
                .header(header::COOKIE, &c.cookie)
                .body(Body::from(json!({"name":"x"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
}
