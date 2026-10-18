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
