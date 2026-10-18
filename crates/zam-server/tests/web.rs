//! WebUI: отдача index.html с base href, SPA fallback, ошибки API остаются problem+json.
mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{cfg_in, tmp};
use http_body_util::BodyExt;
use tower::ServiceExt;
use zam_server::build;

async fn app(name: &str, base: &str) -> Router {
    let mut cfg = cfg_in(tmp(name));
    cfg.server.base_path = base.into();
    build(cfg, Some("pw".into())).await.unwrap().0
}

async fn get(app: &Router, uri: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let r = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (status, headers) = (r.status(), r.headers().clone());
    let body = r.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

fn ctype(h: &axum::http::HeaderMap) -> &str {
    h[header::CONTENT_TYPE].to_str().unwrap()
}

#[tokio::test]
async fn index_served_with_base_href() {
    let app = app("web-index", "/memory").await;
    let (st, h, body) = get(&app, "/memory/").await;
    assert_eq!(st, StatusCode::OK);
    assert!(ctype(&h).starts_with("text/html"), "{}", ctype(&h));
    assert!(body.contains(r#"<base href="/memory/">"#), "{body}");
    let csp = h["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("script-src 'self'") && csp.contains("frame-ancestors 'none'"));
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["referrer-policy"], "same-origin");
}

#[tokio::test]
async fn index_served_at_root_without_base_path() {
    let app = app("web-root", "").await;
    let (st, _, body) = get(&app, "/").await;
    assert_eq!(st, StatusCode::OK);
    assert!(body.contains(r#"<base href="/">"#), "{body}");
}

#[tokio::test]
async fn deep_link_falls_back_to_index() {
    let app = app("web-deep", "/memory").await;
    let (st, h, body) = get(&app, "/memory/users").await;
    assert_eq!(st, StatusCode::OK);
    assert!(ctype(&h).starts_with("text/html"));
    assert!(body.contains(r#"<base href="/memory/">"#));
}

#[tokio::test]
async fn api_404_stays_problem_json() {
    let app = app("web-api404", "/memory").await;
    for uri in ["/memory/api/v1/nope", "/memory/api/zzz", "/memory/mcp/x"] {
        let (st, h, _) = get(&app, uri).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(ctype(&h), "application/problem+json", "{uri}");
    }
}

#[tokio::test]
async fn outside_base_path_is_not_found() {
    let app = app("web-outside", "/memory").await;
    let (st, h, _) = get(&app, "/users").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(ctype(&h), "application/problem+json");
}
