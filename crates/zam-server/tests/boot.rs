mod common;

use axum::body::Body;
use axum::http::{Request, header};
use common::{cfg_in, tmp};
use http_body_util::BodyExt;
use tower::ServiceExt;
use zam_server::build;

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

async fn json(r: axum::response::Response) -> serde_json::Value {
    let b = r.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&b).unwrap()
}

#[tokio::test]
async fn refuses_start_without_admin_password() {
    let e = build(cfg_in(tmp("b1")), None)
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("ZAM_ADMIN_PASSWORD"), "{e}");
}

#[tokio::test]
async fn first_run_creates_admin_and_token_once() {
    let d = tmp("b2");
    let (app, st, rep) = build(cfg_in(d.clone()), Some("pw".into())).await.unwrap();
    let t = rep.initial_token.unwrap();
    assert!(t.starts_with("zam_"));
    assert!(st.system.verify_token(&t, None).await.unwrap().is_some());
    // роутер держит клон состояния (и лок каталога данных)
    drop(app);
    drop(st);
    let (_, _, rep2) = build(cfg_in(d), Some("pw".into())).await.unwrap();
    assert!(rep2.initial_token.is_none());
}

#[tokio::test]
async fn health_ok_under_base_path() {
    let mut c = cfg_in(tmp("b3"));
    c.server.base_path = "/memory".into();
    let (app, ..) = build(c, Some("pw".into())).await.unwrap();
    let r = app
        .clone()
        .oneshot(get("/memory/api/v1/health"))
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let j = json(r).await;
    assert_eq!(j["status"], "ok");
    assert_eq!(j["version"], env!("CARGO_PKG_VERSION"));
    assert!(j["git"].is_string() && j["uptime_s"].is_u64());
    // вне base_path маршрута нет
    let r = app.oneshot(get("/api/v1/health")).await.unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn ready_ok() {
    let (app, ..) = build(cfg_in(tmp("b4")), Some("pw".into())).await.unwrap();
    let r = app.oneshot(get("/api/v1/ready")).await.unwrap();
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn unknown_route_is_problem_json() {
    for (i, (base, uri)) in [
        ("", "/nope"),
        ("/memory", "/memory/nope"),
        ("/memory", "/other"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut c = cfg_in(tmp(&format!("b5-{i}")));
        c.server.base_path = base.into();
        let (app, _st, _) = build(c, Some("pw".into())).await.unwrap();
        let r = app.oneshot(get(uri)).await.unwrap();
        assert_eq!(r.status(), 404, "{uri}");
        assert_eq!(
            r.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        let j = json(r).await;
        assert_eq!(j["status"], 404);
        assert_eq!(j["type"], "about:blank");
    }
}

#[tokio::test]
async fn second_instance_on_same_dir_is_refused() {
    let d = tmp("b6");
    let first = build(cfg_in(d.clone()), Some("pw".into())).await.unwrap();
    let e = build(cfg_in(d), Some("pw".into()))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("уже используется"), "{e}");
    drop(first);
}

#[tokio::test]
async fn no_password_means_no_side_effects() {
    let d = common::tmp("b7").join("nested");
    let e = build(cfg_in(d.clone()), None)
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("ZAM_ADMIN_PASSWORD"), "{e}");
    assert!(!d.exists());
}

#[tokio::test]
async fn admin_without_tokens_gets_new_initial_token() {
    let d = common::tmp("b8");
    let (app, st, rep) = build(cfg_in(d.clone()), Some("pw".into())).await.unwrap();
    let first = rep.initial_token.unwrap();
    let admin = st.system.admin_user().await.unwrap().unwrap();
    for t in st.system.list_tokens(admin.id).await.unwrap() {
        st.system.revoke_token(t.id).await.unwrap();
    }
    drop((app, st));
    let (_, st, rep) = build(cfg_in(d), Some("pw".into())).await.unwrap();
    let second = rep.initial_token.expect("токен выпущен заново");
    assert_ne!(first, second);
    assert!(
        st.system
            .verify_token(&second, None)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn non_admin_with_login_name_refuses_start() {
    let d = common::tmp("b9");
    {
        let dd = zam_store::DataDir::open(&d).unwrap();
        let s = zam_store::SystemStore::open(&dd).unwrap();
        s.create_user("admin", false).await.unwrap();
    }
    let e = build(cfg_in(d), Some("pw".into()))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("не является администратором"), "{e}");
}

#[tokio::test]
async fn bootstrap_audit_has_mask_and_debug_hides_secrets() {
    let (app, st, rep) = build(cfg_in(common::tmp("b10")), Some("pw".into()))
        .await
        .unwrap();
    let plain = rep.initial_token.clone().unwrap();
    let rows = st.system.list_audit(10, None).await.unwrap();
    let row = rows.iter().find(|r| r.action == "bootstrap").unwrap();
    let det = row.details.to_string();
    assert!(!det.contains(&plain[12..]), "{det}");
    assert!(det.contains(&zam_core::mask_token(&plain)), "{det}");
    let dbg = format!("{st:?} {rep:?} {app:?}");
    assert!(!dbg.contains(&plain[12..]));
    assert!(!dbg.contains("argon2"));
}

#[tokio::test]
async fn unavailable_is_503_problem_json() {
    use axum::response::IntoResponse;
    let r = zam_server::error::ApiError::Unavailable("system.db недоступна".into()).into_response();
    assert_eq!(r.status(), 503);
    assert_eq!(
        r.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    assert_eq!(json(r).await["status"], 503);
}
