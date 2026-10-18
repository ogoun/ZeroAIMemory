//! `POST /render`, `GET /render/highlight.css`, HTML заметки и кэш рендера.
mod common;

use axum::body::Body;
use axum::http::{Method, StatusCode, header};
use common::api::{api, enc, req};
use serde_json::json;

#[tokio::test]
async fn render_body_limit_is_one_megabyte() {
    let a = api("render-limit", |_| {}).await;
    const MB: usize = 1024 * 1024;
    let prefix = r#"{"md":""#;
    let suffix = r#""}"#;
    let make = |total: usize| {
        let pad = "a".repeat(total - prefix.len() - suffix.len());
        format!("{prefix}{pad}{suffix}")
    };
    let send = |body: String| {
        let r = req(Method::POST, "/api/v1/render", Some(&json!({})))
            .header(header::AUTHORIZATION, format!("Bearer {}", a.token))
            .body(Body::from(body))
            .unwrap();
        a.send(r)
    };
    let r = send(make(MB)).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        &r.text()[..200.min(r.text().len())]
    );
    let r = send(make(MB + 1)).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(r.header("content-type"), "application/problem+json");
}

#[tokio::test]
async fn render_sanitizes_xss() {
    let a = api("render-xss", |_| {}).await;
    let md = "# T\n\n<script>alert(1)</script>\n\n<img src=x onerror=alert(2)>\n\n\
              [клик](javascript:alert(3))\n\n<a href=\"#\" onclick=\"alert(4)\">a</a>\n\n\
              <iframe src=\"https://evil\"></iframe>\n";
    let r = a
        .t(Method::POST, "/api/v1/render", Some(json!({ "md": md })))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let html = j["html"].as_str().unwrap().to_ascii_lowercase();
    for bad in ["<script", "onerror", "javascript:", "onclick", "<iframe"] {
        assert!(!html.contains(bad), "{bad} в {html}");
    }
    assert_eq!(j["toc"][0]["title"], "T");
    assert!(j["links"].is_array() && j["warnings"].is_array());
}

#[tokio::test]
async fn render_resolves_wikilinks_in_base_node_space() {
    let a = api("render-wiki", |_| {}).await;
    let target = a.note("knowledge", "Папка/Цель", "# Раздел\nx\n").await;
    let base = a.note("knowledge", "Основа", "y").await;
    let tid = target["id"].as_str().unwrap();
    let md = "[[Цель#Раздел]] [[Папка/Цель]] [[%D0%A6%D0%B5%D0%BB%D1%8C]] [[Нет такой]]";
    let r = a
        .t(
            Method::POST,
            "/api/v1/render",
            Some(json!({ "md": md, "base_node": base["id"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let html = j["html"].as_str().unwrap();
    // не-ASCII в href экранируется рендером (percent-encoding)
    assert!(
        html.contains(&format!("href=\"#/n/{tid}#h-{}\"", enc("раздел"))),
        "{html}"
    );
    assert_eq!(
        html.matches(&format!("href=\"#/n/{tid}\"")).count(),
        2,
        "{html}"
    );
    let w = j["warnings"].as_array().unwrap();
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].as_str().unwrap().contains("Нет такой"));
    // без base_node вики-ссылки не разрешаются
    let j = a
        .t(
            Method::POST,
            "/api/v1/render",
            Some(json!({ "md": "[[Цель]]" })),
        )
        .await
        .json();
    assert_eq!(j["warnings"].as_array().unwrap().len(), 1);
    // неверное тело
    let r = a
        .t(Method::POST, "/api/v1/render", Some(json!({"x": 1})))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn highlight_css_is_cacheable() {
    let a = api("render-css", |_| {}).await;
    let r = a.t(Method::GET, "/api/v1/render/highlight.css", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("content-type"), "text/css; charset=utf-8");
    assert_eq!(r.header("cache-control"), "public, max-age=86400");
    assert!(!r.body.is_empty());
}

#[tokio::test]
async fn node_html_uses_render_cache() {
    let a = api("render-cache", |_| {}).await;
    let n = a
        .note(
            "knowledge",
            "c",
            "# Заголовок\n```rust\nfn main() {}\n```\n",
        )
        .await;
    let uri = format!("/api/v1/nodes/{}?format=html", n["id"].as_str().unwrap());
    let len = || a.st.render_cache.lock().unwrap().len();
    assert_eq!(len(), 0);
    let h1 = a.t(Method::GET, &uri, None).await.json();
    assert_eq!(len(), 1);
    let h2 = a.t(Method::GET, &uri, None).await.json();
    assert_eq!(len(), 1);
    assert_eq!(h1, h2);
    // администратор читает ту же базу (первый токен — его пользователя), но зритель
    // другой (ему видны вложения Human): отдельная запись, HTML тот же
    let h_adm = a.adm(Method::GET, &uri, None, None).await.json();
    assert_eq!(h_adm, h1);
    assert_eq!(len(), 2);
    // тот же текст у другого пользователя — другой контекст резолвера, отдельная запись
    let (_, tok) = a.new_user("ivan").await;
    let r = a
        .tok(
            &tok,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "knowledge", "path": "c", "kind": "note",
                        "content": "# Заголовок
```rust
fn main() {}
```
"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let other = format!(
        "/api/v1/nodes/{}?format=html",
        r.json()["id"].as_str().unwrap()
    );
    let h3 = a.tok(&tok, Method::GET, &other, None).await.json();
    assert_eq!(h3["html"], h1["html"]);
    assert_eq!(len(), 3);
    // старая ревизия рендерится по своему хэшу
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &uri.replace("?format=html", ""),
            Some(json!({"content": "# Новый\n"})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let old = a.t(Method::GET, &format!("{uri}&rev=1"), None).await.json();
    assert_eq!(old["rev"], 1);
    assert_eq!(old["html"], h1["html"]);
    let new = a.t(Method::GET, &uri, None).await.json();
    assert_eq!(new["rev"], 2);
    assert!(new["html"].as_str().unwrap().contains("Новый"));
}

#[tokio::test]
async fn large_html_is_not_cached() {
    let a = api("render-big", |_| {}).await;
    // ~9000 абзацев → HTML заметно больше 256 КиБ при тексте ~27 КиБ
    let md = "a\n\n".repeat(9000);
    let n = a.note("knowledge", "big", &md).await;
    let uri = format!("/api/v1/nodes/{}?format=html", n["id"].as_str().unwrap());
    let r = a.t(Method::GET, &uri, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.json()["html"].as_str().unwrap().len() > 256 * 1024);
    assert_eq!(a.st.render_cache.lock().unwrap().len(), 0);
}
