//! Вложения: `POST /blobs` (multipart, лимит), `GET /blobs/{sha256}/{filename}`
//! (Range, ETag, безопасные заголовки, Human), ссылки `_attachments/…` в рендере.
mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};

const BOUNDARY: &str = "zam-test-boundary-7d1f";

/// Часть multipart: (имя поля, имя файла, тип, байты).
type Part<'a> = (&'a str, Option<&'a str>, Option<&'a str>, &'a [u8]);

fn multipart(parts: &[Part<'_>]) -> Vec<u8> {
    let mut b = Vec::new();
    for (name, file, ct, data) in parts {
        b.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        let mut disp = format!("Content-Disposition: form-data; name=\"{name}\"");
        if let Some(f) = file {
            disp.push_str(&format!("; filename=\"{f}\""));
        }
        b.extend_from_slice(disp.as_bytes());
        b.extend_from_slice(b"\r\n");
        if let Some(ct) = ct {
            b.extend_from_slice(format!("Content-Type: {ct}\r\n").as_bytes());
        }
        b.extend_from_slice(b"\r\n");
        b.extend_from_slice(data);
        b.extend_from_slice(b"\r\n");
    }
    b.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    b
}

async fn upload_raw(a: &Api, token: &str, body: Vec<u8>) -> Resp {
    let r = Request::post("/api/v1/blobs")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .unwrap();
    a.send(r).await
}

async fn upload(a: &Api, node: &str, file: &str, data: &[u8]) -> Resp {
    let body = multipart(&[
        ("node", None, None, node.as_bytes()),
        ("file", Some(file), Some("application/octet-stream"), data),
    ]);
    upload_raw(a, &a.token, body).await
}

fn png() -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend((0..200u32).map(|i| (i % 256) as u8));
    v
}

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

async fn get(a: &Api, uri: &str, extra: &[(&str, &str)]) -> Resp {
    a.tok_h(&a.token, Method::GET, uri, None, extra).await
}

#[tokio::test]
async fn upload_png_and_mp4_then_download_with_range() {
    let a = api("blobs-upload", |_| {}).await;
    let n = a.note("knowledge", "Галерея", "# Г\n").await;
    let data = png();
    let r = upload(&a, &id(&n), "кот 1.png", &data).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = r.json();
    let sha = j["sha256"].as_str().unwrap().to_string();
    assert_eq!(sha.len(), 64);
    assert_eq!(j["mime"], "image/png");
    assert_eq!(j["size"], data.len());
    assert_eq!(
        j["link"],
        format!("_attachments/{}/кот%201.png", &sha[..16])
    );

    let mp4: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 256) as u8).collect();
    let r = upload(&a, &id(&n), "clip.mp4", &mp4).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let msha = r.json()["sha256"].as_str().unwrap().to_string();
    assert_eq!(r.json()["mime"], "video/mp4");

    // целиком
    let uri = format!("/api/v1/blobs/{sha}/{}", common::api::enc("кот 1.png"));
    let r = get(&a, &uri, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body.as_ref(), &data[..]);
    assert_eq!(r.header("content-type"), "image/png");
    assert_eq!(r.header("content-disposition"), "inline");
    assert_eq!(r.header("x-content-type-options"), "nosniff");
    assert_eq!(
        r.header("cache-control"),
        "private, max-age=31536000, immutable"
    );
    let etag = r.header("etag").to_string();
    assert_eq!(etag, format!("\"{sha}\""));

    // Range
    let muri = format!("/api/v1/blobs/{msha}/clip.mp4");
    let r = get(&a, &muri, &[("range", "bytes=0-9")]).await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.body.len(), 10);
    assert_eq!(r.body.as_ref(), &mp4[..10]);
    assert_eq!(
        r.header("content-range"),
        format!("bytes 0-9/{}", mp4.len())
    );
    assert_eq!(r.header("content-type"), "video/mp4");
    assert_eq!(r.header("content-disposition"), "inline");

    // If-None-Match → 304
    let r = get(&a, &uri, &[("if-none-match", &etag)]).await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    assert!(r.body.is_empty());

    // неверный sha → 400; неизвестная пара → 404
    let r = get(&a, "/api/v1/blobs/ABC/x.png", &[]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = get(&a, &format!("/api/v1/blobs/{sha}/other.png"), &[]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = get(&a, &format!("/api/v1/blobs/{}/x.png", "0".repeat(64)), &[]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn svg_and_html_are_attachment_with_sandbox() {
    let a = api("blobs-svg", |_| {}).await;
    let n = a.note("knowledge", "s", "x").await;
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
    let r = upload(&a, &id(&n), "evil.svg", svg).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let sha = r.json()["sha256"].as_str().unwrap().to_string();
    let r = get(&a, &format!("/api/v1/blobs/{sha}/evil.svg"), &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("content-type"), "image/svg+xml");
    assert_eq!(
        r.header("content-disposition"),
        "attachment; filename*=UTF-8''evil.svg"
    );
    assert_eq!(
        r.header("content-security-policy"),
        "sandbox; default-src 'none'; img-src 'self'; media-src 'self'; style-src 'unsafe-inline'"
    );
    assert_eq!(r.header("x-content-type-options"), "nosniff");

    let r = upload(&a, &id(&n), "страница.html", b"<script>alert(2)</script>").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let sha = r.json()["sha256"].as_str().unwrap().to_string();
    let r = get(
        &a,
        &format!("/api/v1/blobs/{sha}/{}", common::api::enc("страница.html")),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.header("content-disposition"),
        "attachment; filename*=UTF-8''%D1%81%D1%82%D1%80%D0%B0%D0%BD%D0%B8%D1%86%D0%B0.html"
    );
    assert!(r.header("content-security-policy").starts_with("sandbox"));
}

#[tokio::test]
async fn over_limit_is_413_and_nothing_stored() {
    let a = api("blobs-413", |c| c.limits.blob_max_bytes = 1000).await;
    let n = a.note("knowledge", "big", "x").await;
    let r = upload(&a, &id(&n), "big.bin", &vec![1u8; 1001]).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
    assert_eq!(r.header("content-type"), "application/problem+json");
    let r = upload(&a, &id(&n), "ok.bin", &vec![1u8; 1000]).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    // в .incoming ничего не осталось
    let incoming =
        a.st.data_dir
            .blobs_dir()
            .join(a.admin_id())
            .join(".incoming");
    assert_eq!(std::fs::read_dir(incoming).unwrap().count(), 0);
}

#[tokio::test]
async fn upload_validation() {
    let a = api("blobs-bad", |_| {}).await;
    let n = a.note("knowledge", "n", "x").await;
    let nid = id(&n);
    // нет поля file
    let r = upload_raw(
        &a,
        &a.token,
        multipart(&[("node", None, None, nid.as_bytes())]),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // нет поля node
    let r = upload_raw(
        &a,
        &a.token,
        multipart(&[("file", Some("a.png"), None, b"x")]),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // недопустимое имя
    let r = upload(&a, &nid, "..", b"x").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // не multipart
    let r = a.t(Method::POST, "/api/v1/blobs", Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // папка — не заметка
    let f = a
        .t(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "knowledge", "path": "dir", "kind": "folder"})),
        )
        .await
        .json();
    let r = upload(&a, &id(&f), "a.png", b"x").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // неизвестный узел
    let r = upload(&a, &zam_core::Id::new().to_string(), "a.png", b"x").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    // ro-токен не пишет
    let ro = a.issue(&a.admin_id(), "ro").await;
    let body = multipart(&[
        ("node", None, None, nid.as_bytes()),
        ("file", Some("a.png"), None, b"x"),
    ]);
    let r = upload_raw(&a, &ro, body).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // MIME из части, если по имени не угадывается
    let body = multipart(&[
        ("node", None, None, nid.as_bytes()),
        ("file", Some("data.zamx"), Some("text/plain"), b"x"),
    ]);
    let r = upload_raw(&a, &a.token, body).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["mime"], "text/plain");
    let body = multipart(&[
        ("node", None, None, nid.as_bytes()),
        ("file", Some("data2.zamx"), None, b"y"),
    ]);
    let r = upload_raw(&a, &a.token, body).await;
    assert_eq!(r.json()["mime"], "application/octet-stream");
}

#[tokio::test]
async fn human_attachment_hidden_from_token_visible_to_admin() {
    let a = api("blobs-human", |_| {}).await;
    // администратор (cookie) пишет в Human
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "human", "path": "личное", "kind": "note", "content": "x"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let hid = id(&r.json());
    // загрузка в Human по токену → 404
    let r = upload(&a, &hid, "a.png", &png()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    // по cookie администратора
    let body = multipart(&[
        ("node", None, None, hid.as_bytes()),
        ("file", Some("a.png"), None, &png()),
    ]);
    let req = Request::post("/api/v1/blobs")
        .header(header::COOKIE, &a.cookie)
        .header("x-csrf-token", &a.csrf)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .unwrap();
    let r = a.send(req).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let sha = r.json()["sha256"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/blobs/{sha}/a.png");
    assert_eq!(get(&a, &uri, &[]).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        a.adm(Method::GET, &uri, None, None).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn admin_reads_user_blob_with_user_query() {
    let a = api("blobs-user", |_| {}).await;
    let (uid, tok) = a.new_user("ivan").await;
    let r = a
        .tok(
            &tok,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "knowledge", "path": "n", "kind": "note", "content": "x"})),
        )
        .await;
    let nid = id(&r.json());
    let body = multipart(&[
        ("node", None, None, nid.as_bytes()),
        ("file", Some("a.png"), None, &png()),
    ]);
    let r = upload_raw(&a, &tok, body).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let sha = r.json()["sha256"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/blobs/{sha}/a.png");
    // в базе администратора такого вложения нет
    assert_eq!(
        a.adm(Method::GET, &uri, None, None).await.status,
        StatusCode::NOT_FOUND
    );
    let r = a
        .adm(Method::GET, &format!("{uri}?user={uid}"), None, None)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    // токен не может выбрать чужого пользователя через ?user=
    let r = get(&a, &format!("{uri}?user={uid}"), &[]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // владелец читает
    let r = a.tok(&tok, Method::GET, &uri, None).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn render_resolves_attachment_links() {
    let a = api("blobs-render", |_| {}).await;
    let n = a.note("knowledge", "n", "x").await;
    let r = upload(&a, &id(&n), "кот 1.png", &png()).await;
    let j = r.json();
    let (sha, link) = (
        j["sha256"].as_str().unwrap().to_string(),
        j["link"].as_str().unwrap().to_string(),
    );
    let md = format!(
        "# Кот\n\n![кот]({link})\n\n![нет](_attachments/{}/нет.png)\n",
        &sha[..16]
    );
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{}", id(&n)),
            Some(json!({ "content": md })),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let uri = format!("/api/v1/nodes/{}?format=html", id(&n));
    let j = a.t(Method::GET, &uri, None).await.json();
    let html = j["html"].as_str().unwrap();
    let src = format!("<img src=\"api/v1/blobs/{sha}/%D0%BA%D0%BE%D1%82%201.png\"");
    assert!(html.contains(&src), "{html}");
    assert_eq!(j["warnings"].as_array().unwrap().len(), 1, "{j}");

    // предпросмотр с base_node тоже разрешает
    let j = a
        .t(
            Method::POST,
            "/api/v1/render",
            Some(json!({ "md": format!("![x]({link})"), "base_node": id(&n) })),
        )
        .await
        .json();
    assert!(j["html"].as_str().unwrap().contains(&src), "{j}");

    // администратор от имени другого пользователя получает ?user=
    let (uid, tok) = a.new_user("petr").await;
    let r = a
        .tok(
            &tok,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "knowledge", "path": "p", "kind": "note", "content": "x"})),
        )
        .await;
    let pid = id(&r.json());
    let body = multipart(&[
        ("node", None, None, pid.as_bytes()),
        ("file", Some("b.png"), None, &png()),
    ]);
    let up = upload_raw(&a, &tok, body).await.json();
    let plink = up["link"].as_str().unwrap();
    let r = a
        .tok_h(
            &tok,
            Method::PUT,
            &format!("/api/v1/nodes/{pid}"),
            Some(json!({ "content": format!("![b]({plink})") })),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let puri = format!("/api/v1/nodes/{pid}?format=html");
    let own = a.tok(&tok, Method::GET, &puri, None).await.json();
    let own_html = own["html"].as_str().unwrap();
    assert!(!own_html.contains("?user="), "{own_html}");
    let adm = a.adm(Method::GET, &puri, None, Some(&uid)).await.json();
    let adm_html = adm["html"].as_str().unwrap();
    assert!(
        adm_html.contains(&format!(
            "api/v1/blobs/{}/b.png?user={uid}",
            up["sha256"].as_str().unwrap()
        )),
        "{adm_html}"
    );
    // и токен по-прежнему получает свой вариант (разные записи кэша)
    let own2 = a.tok(&tok, Method::GET, &puri, None).await.json();
    assert_eq!(own2["html"], own["html"]);
}
