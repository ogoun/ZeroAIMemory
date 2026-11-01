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

/// Эталон CSP главной страницы: просмотрщики документов (M5) её не ослабляют.
const MAIN_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'";

#[tokio::test]
async fn main_csp_unchanged() {
    let app = app("web-csp", "/memory").await;
    for uri in ["/memory/", "/memory/users", "/memory/u/x/s/knowledge/n/y"] {
        let (st, h, _) = get(&app, uri).await;
        assert_eq!(st, StatusCode::OK, "{uri}");
        assert_eq!(h["content-security-policy"], MAIN_CSP, "{uri}");
    }
    assert_eq!(zam_server::web::CSP, MAIN_CSP);
}

#[tokio::test]
async fn viewer_pdf_has_own_csp_and_base_href() {
    for (base, uri) in [("/memory", "/memory/viewer/pdf"), ("", "/viewer/pdf")] {
        let app = app("web-viewer", base).await;
        let (st, h, body) = get(&app, uri).await;
        assert_eq!(st, StatusCode::OK, "{uri}");
        assert!(ctype(&h).starts_with("text/html"), "{}", ctype(&h));
        assert!(
            body.contains(&format!(r#"<base href="{base}/">"#)),
            "{body}"
        );
        let csp = h["content-security-policy"].to_str().unwrap();
        assert_eq!(csp, zam_server::web::VIEWER_PDF_CSP);
        assert!(csp.contains("'wasm-unsafe-eval'") && csp.contains("worker-src 'self' blob:"));
        assert!(!csp.contains("'unsafe-eval'") && csp.contains("frame-ancestors 'self'"));
        assert_eq!(h["x-content-type-options"], "nosniff");
        // документ просмотрщика — не ассет: по имени файла его нет
        let (st, _, _) = get(&app, &format!("{base}/viewer-pdf.html")).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }
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

#[tokio::test]
async fn missing_file_paths_are_not_found() {
    let app = app("web-files", "/memory").await;
    for uri in [
        "/memory/assets/nope.js",
        "/memory/favicon.ico",
        "/memory/a.b/c.css",
    ] {
        let (st, h, _) = get(&app, uri).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(ctype(&h), "application/problem+json", "{uri}");
    }
}

/// `Access-Control-Allow-Origin: *` — только у публичной статики (`/assets/`, `/pdfjs/`), которую
/// читает документ просмотрщика PDF с непрозрачным источником; API и страницы — без него.
#[tokio::test]
async fn cors_only_on_public_static() {
    let app = app("web-cors", "/memory").await;
    let (st, h, _) = get(&app, "/memory/pdfjs/wasm/openjpeg.wasm").await;
    if st == StatusCode::OK {
        assert_eq!(h["access-control-allow-origin"], "*");
    } else {
        eprintln!("web/dist не собран — статика pdf.js не проверена");
    }
    for uri in [
        "/memory/api/v1/health",
        "/memory/api/v1/spaces",
        "/memory/",
        "/memory/viewer/pdf",
        "/memory/THIRD-PARTY-NOTICES.txt",
    ] {
        let (_, h, _) = get(&app, uri).await;
        assert!(
            h.get("access-control-allow-origin").is_none(),
            "{uri}: {:?}",
            h.get("access-control-allow-origin")
        );
    }
}
