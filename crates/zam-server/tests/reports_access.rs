//! Чтение, файлы, неизменность и права удаления отчётов (задача 3 M5.5, спецификация §4–5, §8,
//! §12 п. 3 и 5): страница `GET /entries/{id}?format=html`, файлы
//! `GET /entries/{id}/attachments/{sha256}/{имя}`, лента `?kind=report` (и корзина
//! `deleted=only`), 409 `/problems/report-immutable` на изменяющих маршрутах, удаление и
//! восстановление — только владелец решения в своей сессии WebUI.
//!
//! Пользователи: A — владелец решения, B — участник `rw`, C — участник `ro`; у каждого — токен
//! и сессия WebUI. Отчёты пишет агент A (токен). Серверы без моделей (FTS).
mod common;

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use common::api::{Api, Resp, api, req};
use serde_json::{Value, json};
use zam_store::{EntryAttachment, entry_attachments, get_entry};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e12";
const BOUNDARY: &str = "zam-report-access-7d21";
const PASS: &str = "first-pass-123";
const NEW_PASS: &str = "second-pass-456";
const IMMUTABLE: &str = "/problems/report-immutable";

/// Сессия пользователя WebUI: cookie, CSRF и адрес клиента (лимит входа — по IP).
struct Web {
    cookie: String,
    csrf: String,
    ip: String,
}

/// Пользователь: id, токен `rw`, сессия WebUI.
struct User {
    id: String,
    tok: String,
    web: Web,
}

struct W {
    a: Api,
    /// Владелец решения.
    o: User,
    /// Участник `rw`.
    rw: User,
    /// Участник `ro`.
    ro: User,
}

fn with_ip(mut r: Request<Body>, ip: &str) -> Request<Body> {
    r.extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip.parse().unwrap(), 40_000)));
    r
}

fn ok(r: &Resp, s: StatusCode, ctx: &str) {
    assert_eq!(r.status, s, "{ctx}: {}", r.text());
}

async fn login(a: &Api, name: &str, pw: &str, ip: &str) -> Web {
    let body = json!({ "login": name, "password": pw });
    let r = req(Method::POST, "/api/v1/auth/login", Some(&body))
        .body(Body::from(body.to_string()))
        .unwrap();
    let r = a.send(with_ip(r, ip)).await;
    ok(&r, StatusCode::OK, "вход");
    Web {
        cookie: r
            .header("set-cookie")
            .split(';')
            .next()
            .unwrap()
            .to_string(),
        csrf: r.json()["csrf"].as_str().unwrap().to_string(),
        ip: ip.into(),
    }
}

/// Запрос сессии пользователя с дополнительными заголовками.
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

/// Пользователь с паролем после обязательной смены.
async fn ready_user(a: &Api, name: &str, ip: &str) -> User {
    let (id, tok) = a.new_user(name).await;
    let r = a
        .adm(
            Method::POST,
            &format!("/api/v1/admin/users/{id}/password"),
            Some(json!({ "password": PASS })),
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "пароль");
    let w = login(a, name, PASS, ip).await;
    let r = web(
        a,
        &w,
        Method::POST,
        "/api/v1/auth/password",
        Some(json!({ "old": PASS, "new": NEW_PASS })),
    )
    .await;
    ok(&r, StatusCode::NO_CONTENT, "смена пароля");
    User { id, tok, web: w }
}

/// Решение `guid` пользователя `owner` с участником `member` в режиме `access`.
async fn share(a: &Api, guid: &str, owner: &str, member: &User, access: &str) {
    let reg = json!({"agent_solution_id": guid, "name": "Исследование"});
    let r = a
        .tok(&member.tok, Method::POST, "/api/v1/solutions", Some(reg))
        .await;
    ok(&r, StatusCode::ACCEPTED, "запрос доступа");
    let r = a
        .tok(
            owner,
            Method::POST,
            &format!("/api/v1/solutions/{guid}/access/{}/approve", member.id),
            Some(json!({ "access": access })),
        )
        .await;
    ok(&r, StatusCode::OK, "одобрение");
}

async fn world(name: &str) -> W {
    let a = api(&format!("reports-access-{name}"), |_| {}).await;
    let o = ready_user(&a, "Анна", "10.0.0.1").await;
    let rw = ready_user(&a, "Борис", "10.0.0.2").await;
    let ro = ready_user(&a, "Вера", "10.0.0.3").await;
    let reg = json!({"agent_solution_id": G, "name": "Исследование"});
    let r = a
        .tok(&o.tok, Method::POST, "/api/v1/solutions", Some(reg))
        .await;
    ok(&r, StatusCode::CREATED, "решение");
    share(&a, G, &o.tok, &rw, "rw").await;
    share(&a, G, &o.tok, &ro, "ro").await;
    W { a, o, rw, ro }
}

/// Части multipart: `meta`, затем файлы частями `file`.
fn multipart(meta: &Value, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut b = Vec::new();
    let mut part = |disp: String, ct: &str, data: &[u8]| {
        b.extend_from_slice(
            format!("--{BOUNDARY}\r\n{disp}\r\nContent-Type: {ct}\r\n\r\n").as_bytes(),
        );
        b.extend_from_slice(data);
        b.extend_from_slice(b"\r\n");
    };
    part(
        "Content-Disposition: form-data; name=\"meta\"".into(),
        "application/json",
        meta.to_string().as_bytes(),
    );
    for (n, d) in files {
        part(
            format!("Content-Disposition: form-data; name=\"file\"; filename=\"{n}\""),
            "application/octet-stream",
            d,
        );
    }
    b.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    b
}

/// PNG-подобные байты (сигнатура и `n` байт содержимого по `seed`).
fn png(seed: u8, n: usize) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend((0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)));
    v
}

const SVG: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\" onload=\"alert(1)\">\
<script>alert(2)</script><rect width=\"10\" height=\"10\"/></svg>";
const CSV: &[u8] = b"epoch,loss\n1,0.9\n2,0.5\n";

/// Тело отчёта со всеми видами содержимого.
const BODY: &str = "# Итог\n\n![loss](figs/loss.png)\n\n![схема](diag.svg)\n\n\
Формула $E = mc^2$ и блок:\n\n$$\\int_0^1 x\\,dx$$\n\n\
```mermaid\ngraph TD; A-->B\n```\n\n| метрика | значение |\n|---|---|\n| acc | 0,93 |\n\n\
```rust\nfn main() { let x = 1; }\n```\n\n[данные](results.csv)\n";

impl W {
    /// Отчёт агентом владельца (multipart, как `zam report`): id.
    async fn report_with(&self, title: &str, body: &str, files: &[(&str, &[u8])]) -> String {
        let meta = json!({"title": title, "body": body, "tags": ["ml"]});
        let r = Request::post(format!("/api/v1/spaces/{G}/entries"))
            .header(header::AUTHORIZATION, format!("Bearer {}", self.o.tok))
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .header("x-zam-session", "sess-report-1")
            .header("x-zam-machine", "gpu-box")
            .body(Body::from(multipart(&meta, files)))
            .unwrap();
        let r = self.a.send(r).await;
        ok(&r, StatusCode::CREATED, "отчёт");
        r.json()["id"].as_str().unwrap().to_string()
    }

    /// Полный отчёт: картинка PNG, SVG и CSV.
    async fn report(&self, title: &str) -> String {
        let p = png(1, 200);
        self.report_with(
            title,
            BODY,
            &[
                ("figs/loss.png", &p),
                ("diag.svg", SVG),
                ("results.csv", CSV),
            ],
        )
        .await
    }

    async fn db(&self) -> zam_store::Db {
        self.a
            .st
            .users
            .get(self.o.id.parse().unwrap())
            .await
            .unwrap()
    }

    async fn files_of(&self, id: &str) -> Vec<EntryAttachment> {
        entry_attachments(&self.db().await, id.parse().unwrap())
            .await
            .unwrap()
    }

    async fn file(&self, id: &str, name: &str) -> EntryAttachment {
        self.files_of(id)
            .await
            .into_iter()
            .find(|f| f.filename == name)
            .unwrap()
    }

    /// Ревизия и тело записи в базе владельца.
    async fn stored(&self, id: &str) -> (i64, String) {
        let e = get_entry(&self.db().await, id.parse().unwrap())
            .await
            .unwrap();
        (e.rev, e.body)
    }

    /// Заметка в решении (токен владельца): id.
    async fn note(&self, path: &str) -> String {
        let r = self
            .a
            .tok(
                &self.o.tok,
                Method::POST,
                "/api/v1/nodes",
                Some(json!({"space": G, "path": path, "kind": "note", "content": "текст"})),
            )
            .await;
        ok(&r, StatusCode::CREATED, "заметка");
        r.json()["id"].as_str().unwrap().to_string()
    }
}

fn file_uri(id: &str, f: &EntryAttachment) -> String {
    format!(
        "/api/v1/entries/{id}/attachments/{}/{}",
        f.sha256,
        common::api::enc(&f.filename)
    )
}

fn problem(r: &Resp) -> String {
    r.json()["type"].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn report_html_renders_with_entry_attachment_urls() {
    let w = world("html").await;
    let id = w.report("Обучение модели").await;
    let loss = w.file(&id, "loss.png").await;
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/entries/{id}?format=html"),
        None,
    )
    .await;
    ok(&r, StatusCode::OK, "страница");
    let j = r.json();
    let html = j["html"].as_str().unwrap();
    let src = format!(
        "src=\"api/v1/entries/{id}/attachments/{}/loss.png\"",
        loss.sha256
    );
    assert!(html.contains(&src), "{src} в {html}");
    assert!(html.contains("class=\"math\""), "{html}");
    assert!(html.contains("<pre class=\"mermaid-src\""), "{html}");
    assert!(html.contains("<table "), "{html}");
    assert!(html.contains("class=\"hl-"), "{html}");
    // прочий файл — ссылка на скачивание через маршрут записи
    assert!(
        html.contains(&format!("api/v1/entries/{id}/attachments/")) && html.contains("results.csv"),
        "{html}"
    );
    assert!(!html.contains("api/v1/blobs/"), "{html}");
    assert!(j["warnings"].as_array().unwrap().is_empty(), "{j}");
    assert_eq!(j["entry"]["id"], id);
    assert_eq!(j["entry"]["kind"], "report");
    let files = j["files"].as_array().unwrap();
    assert_eq!(files.len(), 3, "{j}");
    let f = files.iter().find(|f| f["filename"] == "loss.png").unwrap();
    assert_eq!(f["sha256"], loss.sha256);
    assert_eq!(f["mime"], "image/png");
    assert_eq!(
        f["url"],
        format!("api/v1/entries/{id}/attachments/{}/loss.png", loss.sha256)
    );
    // повтор — из кэша, тот же HTML
    let r2 = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/entries/{id}?format=html"),
        None,
    )
    .await;
    assert_eq!(r2.json()["html"], j["html"]);

    // администратор от имени владельца — URL с `?user=` (браузер не шлёт X-Zam-User в <img>)
    let r =
        w.a.adm(
            Method::GET,
            &format!("/api/v1/entries/{id}?format=html"),
            None,
            Some(&w.o.id),
        )
        .await;
    ok(&r, StatusCode::OK, "администратор");
    let html = r.json()["html"].as_str().unwrap().to_string();
    let src = format!(
        "src=\"api/v1/entries/{id}/attachments/{}/loss.png?user={}\"",
        loss.sha256, w.o.id
    );
    assert!(html.contains(&src), "{src} в {html}");
    let r =
        w.a.adm(
            Method::GET,
            &format!("{}?user={}", file_uri(&id, &loss), w.o.id),
            None,
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "файл администратору по ?user=");
}

#[tokio::test]
async fn report_html_other_kind_400() {
    let w = world("html-kind").await;
    let r =
        w.a.tok(
            &w.o.tok,
            Method::POST,
            &format!("/api/v1/spaces/{G}/entries"),
            Some(json!({"kind": "changelog", "title": "Сделано", "data": {"kind": "feat"}})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "changelog");
    let cl = r.json()["id"].as_str().unwrap().to_string();
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/entries/{cl}?format=html"),
        None,
    )
    .await;
    ok(&r, StatusCode::BAD_REQUEST, "html у changelog");
    // json — как раньше
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/entries/{cl}?format=json"),
        None,
    )
    .await;
    ok(&r, StatusCode::OK, "json у changelog");
    assert_eq!(r.json()["id"], cl);
    let id = w.report("Отчёт").await;
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/entries/{id}?format=pdf"),
        None,
    )
    .await;
    ok(&r, StatusCode::BAD_REQUEST, "неизвестный формат");
}

#[tokio::test]
async fn report_attachment_download_image_inline_svg_sandbox() {
    let w = world("dl-img").await;
    let id = w.report("Картинки").await;
    let loss = w.file(&id, "loss.png").await;
    let r = web(&w.a, &w.o.web, Method::GET, &file_uri(&id, &loss), None).await;
    ok(&r, StatusCode::OK, "png");
    assert_eq!(r.header("content-type"), "image/png");
    assert_eq!(r.header("content-disposition"), "inline");
    assert_eq!(r.header("x-content-type-options"), "nosniff");
    assert_eq!(r.header("etag"), format!("\"{}\"", loss.sha256));
    assert!(r.header("content-security-policy").contains("sandbox"));
    assert_eq!(&r.body[..], &png(1, 200)[..]);
    // Range
    let r = web_h(
        &w.a,
        &w.o.web,
        Method::GET,
        &file_uri(&id, &loss),
        None,
        &[("range", "bytes=0-3")],
    )
    .await;
    ok(&r, StatusCode::PARTIAL_CONTENT, "range");
    assert_eq!(&r.body[..], b"\x89PNG");
    // If-None-Match
    let etag = format!("\"{}\"", loss.sha256);
    let r = web_h(
        &w.a,
        &w.o.web,
        Method::GET,
        &file_uri(&id, &loss),
        None,
        &[("if-none-match", &etag)],
    )
    .await;
    ok(&r, StatusCode::NOT_MODIFIED, "304");
    assert_eq!(r.header("x-content-type-options"), "nosniff");

    // SVG: тип image/svg+xml, песочница CSP — скрипты при прямом открытии не исполняются
    let svg = w.file(&id, "diag.svg").await;
    let r = web(&w.a, &w.o.web, Method::GET, &file_uri(&id, &svg), None).await;
    ok(&r, StatusCode::OK, "svg");
    assert_eq!(r.header("content-type"), "image/svg+xml");
    let csp = r.header("content-security-policy");
    assert!(csp.starts_with("sandbox"), "{csp}");
    assert!(csp.contains("default-src 'none'"), "{csp}");
    assert!(!csp.contains("script-src"), "{csp}");
    assert_eq!(r.header("x-content-type-options"), "nosniff");
    assert_eq!(&r.body[..], SVG);
}

#[tokio::test]
async fn report_attachment_other_file_attachment_nosniff() {
    let w = world("dl-csv").await;
    let id = w.report("Данные").await;
    let csv = w.file(&id, "results.csv").await;
    let r = web(&w.a, &w.o.web, Method::GET, &file_uri(&id, &csv), None).await;
    ok(&r, StatusCode::OK, "csv");
    assert_eq!(
        r.header("content-disposition"),
        "attachment; filename*=UTF-8''results.csv"
    );
    assert_eq!(r.header("x-content-type-options"), "nosniff");
    assert!(r.header("content-security-policy").contains("sandbox"));
    assert_eq!(&r.body[..], CSV);
}

#[tokio::test]
async fn report_attachment_wrong_entry_404() {
    let w = world("dl-wrong").await;
    let a_id = w.report("Отчёт A").await;
    let other = png(9, 50);
    let b_id = w
        .report_with("Отчёт B", "![x](b.png)\n", &[("b.png", &other)])
        .await;
    let loss = w.file(&a_id, "loss.png").await;
    let nf = |r: &Resp, ctx: &str| ok(r, StatusCode::NOT_FOUND, ctx);
    // файл отчёта A через отчёт B
    let r = web(&w.a, &w.o.web, Method::GET, &file_uri(&b_id, &loss), None).await;
    nf(&r, "sha отчёта A через B");
    // то же sha с чужим именем
    let uri = format!(
        "/api/v1/entries/{a_id}/attachments/{}/other.png",
        loss.sha256
    );
    nf(
        &web(&w.a, &w.o.web, Method::GET, &uri, None).await,
        "чужое имя",
    );
    // неизвестный sha
    let uri = format!(
        "/api/v1/entries/{a_id}/attachments/{}/loss.png",
        "0".repeat(64)
    );
    nf(
        &web(&w.a, &w.o.web, Method::GET, &uri, None).await,
        "нет sha",
    );
    // sha не hex — 400
    let uri = format!("/api/v1/entries/{a_id}/attachments/XYZ/loss.png");
    ok(
        &web(&w.a, &w.o.web, Method::GET, &uri, None).await,
        StatusCode::BAD_REQUEST,
        "sha не hex",
    );

    // вложение заметки: ни через id заметки, ни через отчёт
    let note = w.note("notes/n1").await;
    let data = png(7, 80);
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"node\"\r\n\r\n{note}\r\n\
             --{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"n.png\"\r\n\
             Content-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&data);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    let r = Request::post("/api/v1/blobs")
        .header(header::AUTHORIZATION, format!("Bearer {}", w.o.tok))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .unwrap();
    let r = w.a.send(r).await;
    ok(&r, StatusCode::CREATED, "вложение заметки");
    let sha = r.json()["sha256"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/entries/{note}/attachments/{sha}/n.png");
    nf(
        &web(&w.a, &w.o.web, Method::GET, &uri, None).await,
        "id заметки",
    );
    let uri = format!("/api/v1/entries/{a_id}/attachments/{sha}/n.png");
    nf(
        &web(&w.a, &w.o.web, Method::GET, &uri, None).await,
        "блоб заметки через отчёт",
    );
    // блоб заметки по своему маршруту доступен, файл отчёта по нему — нет
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/blobs/{sha}/n.png"),
        None,
    )
    .await;
    ok(&r, StatusCode::OK, "блоб заметки");
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/blobs/{}/loss.png", loss.sha256),
        None,
    )
    .await;
    nf(&r, "файл отчёта через /blobs");

    // маршрут файлов отчёта — только сессиям WebUI: токен получает 404
    let r =
        w.a.tok(&w.o.tok, Method::GET, &file_uri(&a_id, &loss), None)
            .await;
    nf(&r, "токен");
}

#[tokio::test]
async fn report_list_cookie_first_image_author_cursor() {
    let w = world("list").await;
    let first = w.report("Первый").await;
    let (p2, p3) = (png(2, 60), png(3, 70));
    let second = w
        .report_with(
            "Второй",
            "Текст [данные](d.csv)\n\n![a](a.png)\n\n![b](b.png)\n",
            &[("d.csv", CSV), ("a.png", &p2), ("b.png", &p3)],
        )
        .await;
    let third = w
        .report_with(
            "Третий",
            "Без картинок, только [файл](d.csv).\n",
            &[("d.csv", CSV)],
        )
        .await;
    // changelog в той же ленте без вида — отчётов там нет
    let r =
        w.a.tok(
            &w.o.tok,
            Method::POST,
            &format!("/api/v1/spaces/{G}/entries"),
            Some(json!({"kind": "changelog", "title": "Сделано", "data": {"kind": "feat"}})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "changelog");
    let all = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/spaces/{G}/entries"),
        None,
    )
    .await;
    ok(&all, StatusCode::OK, "лента без вида");
    let kinds: Vec<Value> = all.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].clone())
        .collect();
    assert_eq!(kinds, vec![json!("changelog")]);

    let uri = format!("/api/v1/spaces/{G}/entries?kind=report&limit=2");
    let r = web(&w.a, &w.o.web, Method::GET, &uri, None).await;
    ok(&r, StatusCode::OK, "лента отчётов");
    let j = r.json();
    let items = j["items"].as_array().unwrap();
    let ids: Vec<&str> = items.iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec![third.as_str(), second.as_str()], "{j}");
    assert!(items[0]["first_image"].is_null(), "{j}");
    let a = w.file(&second, "a.png").await;
    let fi = &items[1]["first_image"];
    assert_eq!(fi["filename"], "a.png", "{j}");
    assert_eq!(fi["sha256"], a.sha256);
    assert_eq!(fi["mime"], "image/png");
    assert_eq!(
        fi["url"],
        format!("api/v1/entries/{second}/attachments/{}/a.png", a.sha256)
    );
    for e in items {
        assert_eq!(e["kind"], "report");
        assert_eq!(e["author"]["kind"], "agent", "{e}");
        assert_eq!(e["author"]["session_id"], "sess-report-1", "{e}");
        assert_eq!(e["author"]["machine"], "gpu-box", "{e}");
        assert_eq!(e["tags"], json!(["ml"]));
    }
    let cursor = j["next_cursor"].as_str().unwrap().to_string();
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("{uri}&cursor={}", common::api::enc(&cursor)),
        None,
    )
    .await;
    ok(&r, StatusCode::OK, "вторая страница");
    let j = r.json();
    let items = j["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{j}");
    assert_eq!(items[0]["id"], first);
    assert_eq!(items[0]["first_image"]["filename"], "loss.png");
    assert!(j["next_cursor"].is_null(), "{j}");
    // участник ro видит ленту решения
    let r = web(&w.a, &w.ro.web, Method::GET, &uri, None).await;
    ok(&r, StatusCode::OK, "участник ro");
    assert_eq!(r.json()["items"].as_array().unwrap().len(), 2);
    // `deleted` — только `only`
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/spaces/{G}/entries?kind=report&deleted=yes"),
        None,
    )
    .await;
    ok(&r, StatusCode::BAD_REQUEST, "deleted=yes");
    // токену лента отчётов не существует: 400 тем же текстом, что у неизвестного вида
    let detail = |r: &Resp| r.json()["detail"].as_str().unwrap().to_string();
    let r = w.a.tok(&w.o.tok, Method::GET, &uri, None).await;
    ok(&r, StatusCode::BAD_REQUEST, "токен kind=report");
    let unknown =
        w.a.tok(
            &w.o.tok,
            Method::GET,
            &format!("/api/v1/spaces/{G}/entries?kind=xyz"),
            None,
        )
        .await;
    assert_eq!(
        detail(&r).replace("report", "xyz"),
        detail(&unknown),
        "текст как у неизвестного вида"
    );
}

/// Правки отчёта: (что, тело).
fn patch_bodies() -> Vec<(&'static str, Value)> {
    vec![
        ("title", json!({"title": "Новый"})),
        ("body", json!({"body": "Новый текст"})),
        ("append_body", json!({"append_body": "\nдописано"})),
        ("tags", json!({"tags": ["x"]})),
        ("area", json!({"area": "ml"})),
        ("area null", json!({"area": null})),
        ("data", json!({"data": {"refs": {"commits": ["abc"]}}})),
        ("refs", json!({"refs": {"files": [{"path": "src/a.rs"}]}})),
        ("certainty", json!({"certainty": "axiom", "reason": "r"})),
        ("status", json!({"status": "active"})),
        (
            "triggers",
            json!({"triggers": [{"type": "keyword", "pattern": "x"}]}),
        ),
        ("пусто", json!({})),
    ]
}

#[tokio::test]
async fn report_patch_any_field_409() {
    let w = world("patch").await;
    let id = w.report("Неизменный").await;
    let (rev0, body0) = w.stored(&id).await;
    assert_eq!(rev0, 1);
    let uri = format!("/api/v1/entries/{id}");
    for (what, body) in patch_bodies() {
        // с If-Match и без: неизменность — раньше ревизии
        for extra in [&[("if-match", "1")][..], &[][..]] {
            let r = web_h(
                &w.a,
                &w.o.web,
                Method::PATCH,
                &uri,
                Some(body.clone()),
                extra,
            )
            .await;
            ok(&r, StatusCode::CONFLICT, what);
            assert_eq!(problem(&r), IMMUTABLE, "{what}");
        }
        // администратор (своя сессия от имени владельца) — тоже 409
        let r =
            w.a.adm_h(
                Method::PATCH,
                &uri,
                Some(body.clone()),
                &[("x-zam-user", &w.o.id), ("if-match", "1")],
            )
            .await;
        ok(&r, StatusCode::CONFLICT, what);
        assert_eq!(problem(&r), IMMUTABLE, "{what}");
        // участник rw — 409; ro — 403 (права записи — раньше вида)
        let r = web_h(
            &w.a,
            &w.rw.web,
            Method::PATCH,
            &uri,
            Some(body.clone()),
            &[("if-match", "1")],
        )
        .await;
        ok(&r, StatusCode::CONFLICT, what);
        let r = web_h(
            &w.a,
            &w.ro.web,
            Method::PATCH,
            &uri,
            Some(body.clone()),
            &[("if-match", "1")],
        )
        .await;
        ok(&r, StatusCode::FORBIDDEN, what);
    }
    // токен не узнаёт об отчёте по 409: 404, как у несуществующей записи
    let r =
        w.a.tok_h(
            &w.o.tok,
            Method::PATCH,
            &uri,
            Some(json!({"title": "x"})),
            &[("if-match", "1")],
        )
        .await;
    ok(&r, StatusCode::NOT_FOUND, "токен");
    let (rev, body) = w.stored(&id).await;
    assert_eq!((rev, body), (rev0, body0));
}

#[tokio::test]
async fn report_other_mutations_409() {
    let w = world("mutations").await;
    let id = w.report("Неизменный").await;
    let (rev0, body0) = w.stored(&id).await;
    let cases: Vec<(&str, String, Value)> = vec![
        (
            "supersede",
            format!("/api/v1/entries/{id}/supersede"),
            json!({"title": "Новый", "body": "т", "data": {"kind": "gotcha"}}),
        ),
        (
            "retract",
            format!("/api/v1/entries/{id}/retract"),
            json!({"reason": "неверно"}),
        ),
        (
            "answer",
            format!("/api/v1/entries/{id}/answer"),
            json!({"text": "да"}),
        ),
        ("claim", format!("/api/v1/entries/{id}/claim"), json!({})),
        (
            "release",
            format!("/api/v1/entries/{id}/release"),
            json!({}),
        ),
        (
            "feedback",
            format!("/api/v1/entries/{id}/feedback"),
            json!({"value": "useful"}),
        ),
    ];
    for (what, uri, body) in &cases {
        let r = web_h(
            &w.a,
            &w.o.web,
            Method::POST,
            uri,
            Some(body.clone()),
            &[("x-zam-session", "sess-ui")],
        )
        .await;
        ok(&r, StatusCode::CONFLICT, what);
        assert_eq!(problem(&r), IMMUTABLE, "{what}");
        // токен владельца — 404 (существование отчёта не раскрывается)
        let r =
            w.a.tok_h(
                &w.o.tok,
                Method::POST,
                uri,
                Some(body.clone()),
                &[("x-zam-session", "sess-agent")],
            )
            .await;
        ok(&r, StatusCode::NOT_FOUND, &format!("{what} по токену"));
    }
    // замена триггеров
    let r = web_h(
        &w.a,
        &w.o.web,
        Method::PATCH,
        &format!("/api/v1/entries/{id}"),
        Some(json!({"triggers": [{"type": "keyword", "pattern": "x"}]})),
        &[("if-match", "1")],
    )
    .await;
    ok(&r, StatusCode::CONFLICT, "triggers");

    // рёбра: от отчёта — 409, к отчёту — 422
    let note = w.note("notes/n1").await;
    let edge = |src: String, dst: String| json!({"src": src, "type": "relates_to", "dst": dst});
    let r = web(
        &w.a,
        &w.o.web,
        Method::POST,
        "/api/v1/edges",
        Some(edge(format!("e:{id}"), format!("n:{note}"))),
    )
    .await;
    ok(&r, StatusCode::CONFLICT, "ребро от отчёта");
    assert_eq!(problem(&r), IMMUTABLE);
    let r = web(
        &w.a,
        &w.o.web,
        Method::POST,
        "/api/v1/edges",
        Some(edge(format!("n:{note}"), format!("e:{id}"))),
    )
    .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "ребро к отчёту");
    assert!(
        r.text().contains("на отчёт нельзя ссылаться"),
        "{}",
        r.text()
    );
    // токен: оба конца — 404
    for (src, dst) in [
        (format!("e:{id}"), format!("n:{note}")),
        (format!("n:{note}"), format!("e:{id}")),
    ] {
        let r =
            w.a.tok(
                &w.o.tok,
                Method::POST,
                "/api/v1/edges",
                Some(edge(src, dst)),
            )
            .await;
        ok(&r, StatusCode::NOT_FOUND, "ребро по токену");
    }
    let (rev, body) = w.stored(&id).await;
    assert_eq!((rev, body), (rev0, body0));
    let usage: i64 = w
        .db()
        .await
        .read(|c| Ok(c.query_row("SELECT count(*) FROM usage", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(usage, 0, "оценка отчёта не записана");
}

/// Ответ удаления: 204 или текст отказа.
async fn delete_as(w: &W, who: &str, id: &str) -> Resp {
    let uri = format!("/api/v1/entries/{id}");
    let ifm = [("if-match", "1")];
    match who {
        "owner" => web_h(&w.a, &w.o.web, Method::DELETE, &uri, None, &ifm).await,
        "rw" => web_h(&w.a, &w.rw.web, Method::DELETE, &uri, None, &ifm).await,
        "ro" => web_h(&w.a, &w.ro.web, Method::DELETE, &uri, None, &ifm).await,
        "owner-token" => w.a.tok_h(&w.o.tok, Method::DELETE, &uri, None, &ifm).await,
        "member-token" => w.a.tok_h(&w.rw.tok, Method::DELETE, &uri, None, &ifm).await,
        "admin-for" => {
            w.a.adm_h(
                Method::DELETE,
                &uri,
                None,
                &[("x-zam-user", &w.o.id), ("if-match", "1")],
            )
            .await
        }
        _ => unreachable!(),
    }
}

async fn restore_as(w: &W, who: &str, id: &str) -> Resp {
    let uri = format!("/api/v1/entries/{id}/restore");
    match who {
        "owner" => web(&w.a, &w.o.web, Method::POST, &uri, None).await,
        "rw" => web(&w.a, &w.rw.web, Method::POST, &uri, None).await,
        "ro" => web(&w.a, &w.ro.web, Method::POST, &uri, None).await,
        "owner-token" => w.a.tok(&w.o.tok, Method::POST, &uri, None).await,
        "member-token" => w.a.tok(&w.rw.tok, Method::POST, &uri, None).await,
        "admin-for" => {
            w.a.adm_h(Method::POST, &uri, None, &[("x-zam-user", &w.o.id)])
                .await
        }
        _ => unreachable!(),
    }
}

const NOT_OWNERS: [&str; 5] = ["rw", "ro", "owner-token", "member-token", "admin-for"];

#[tokio::test]
async fn report_delete_restore_owner_only() {
    let w = world("trash").await;
    let id = w.report("Удаляемый").await;
    for who in NOT_OWNERS {
        let r = delete_as(&w, who, &id).await;
        ok(&r, StatusCode::FORBIDDEN, &format!("удаление: {who}"));
        if who != "ro" {
            // участник ro отбивается правами записи раньше
            assert!(
                r.text().contains("только владелец решения"),
                "{who}: {}",
                r.text()
            );
        }
    }
    let e = get_entry(&w.db().await, id.parse().unwrap()).await.unwrap();
    assert!(e.deleted_at.is_none());
    ok(
        &delete_as(&w, "owner", &id).await,
        StatusCode::NO_CONTENT,
        "владелец удаляет",
    );
    for who in NOT_OWNERS {
        let r = restore_as(&w, who, &id).await;
        ok(&r, StatusCode::FORBIDDEN, &format!("восстановление: {who}"));
    }
    let r = restore_as(&w, "owner", &id).await;
    ok(&r, StatusCode::OK, "владелец восстанавливает");
    assert!(r.json()["deleted_at"].is_null());

    // администратор в своей базе — владелец своего решения
    let g2 = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e99";
    let r =
        w.a.t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": g2, "name": "Своё"})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "решение администратора");
    let r =
        w.a.t(
            Method::POST,
            &format!("/api/v1/spaces/{g2}/entries"),
            Some(json!({"kind": "report", "title": "Отчёт администратора"})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "отчёт администратора");
    let own = r.json()["id"].as_str().unwrap().to_string();
    let r =
        w.a.adm_h(
            Method::DELETE,
            &format!("/api/v1/entries/{own}"),
            None,
            &[("if-match", "1")],
        )
        .await;
    ok(&r, StatusCode::NO_CONTENT, "администратор удаляет свой");
    let r =
        w.a.adm(
            Method::POST,
            &format!("/api/v1/entries/{own}/restore"),
            None,
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "администратор восстанавливает свой");
    // а его токен — нет
    let r =
        w.a.tok_h(
            &w.a.token,
            Method::DELETE,
            &format!("/api/v1/entries/{own}"),
            None,
            &[("if-match", "1")],
        )
        .await;
    ok(&r, StatusCode::FORBIDDEN, "токен администратора");

    // прочие записи по-прежнему удаляет участник rw
    let r =
        w.a.tok(
            &w.o.tok,
            Method::POST,
            &format!("/api/v1/spaces/{G}/entries"),
            Some(json!({"kind": "changelog", "title": "Сделано", "data": {"kind": "feat"}})),
        )
        .await;
    let cl = r.json()["id"].as_str().unwrap().to_string();
    ok(
        &delete_as(&w, "rw", &cl).await,
        StatusCode::NO_CONTENT,
        "changelog участником",
    );
}

#[tokio::test]
async fn report_deleted_feed_owner_only_files_404() {
    let w = world("trash-feed").await;
    let id = w.report("В корзину").await;
    let live = w.report("Живой").await;
    let loss = w.file(&id, "loss.png").await;
    ok(
        &delete_as(&w, "owner", &id).await,
        StatusCode::NO_CONTENT,
        "удаление",
    );
    let trash = format!("/api/v1/spaces/{G}/entries?kind=report&deleted=only");
    let r = web(&w.a, &w.o.web, Method::GET, &trash, None).await;
    ok(&r, StatusCode::OK, "корзина владельца");
    let j = r.json();
    let items = j["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{j}");
    assert_eq!(items[0]["id"], id);
    assert!(!items[0]["deleted_at"].is_null());
    // файлы удалённого не отдаются — миниатюры в корзине нет
    assert!(items[0]["first_image"].is_null(), "{j}");
    for (who, web_) in [("rw", &w.rw.web), ("ro", &w.ro.web)] {
        let r = web(&w.a, web_, Method::GET, &trash, None).await;
        ok(&r, StatusCode::FORBIDDEN, &format!("корзина: {who}"));
    }
    let r = w.a.adm(Method::GET, &trash, None, Some(&w.o.id)).await;
    ok(&r, StatusCode::FORBIDDEN, "корзина: администратор от имени");
    // `deleted=only` — только у ленты отчётов
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/spaces/{G}/entries?deleted=only"),
        None,
    )
    .await;
    ok(&r, StatusCode::BAD_REQUEST, "deleted=only без вида");
    // живая лента — без удалённого
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/spaces/{G}/entries?kind=report"),
        None,
    )
    .await;
    let ids: Vec<Value> = r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].clone())
        .collect();
    assert_eq!(ids, vec![json!(live)]);
    // страница и файлы удалённого — 404
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/entries/{id}?format=html"),
        None,
    )
    .await;
    ok(&r, StatusCode::NOT_FOUND, "страница удалённого");
    let r = web(&w.a, &w.o.web, Method::GET, &file_uri(&id, &loss), None).await;
    ok(&r, StatusCode::NOT_FOUND, "файл удалённого");
}

#[tokio::test]
async fn report_member_ro_reads_html_and_files() {
    let w = world("member-ro").await;
    let id = w.report("Для участника").await;
    let loss = w.file(&id, "loss.png").await;
    for (who, u) in [("ro", &w.ro), ("rw", &w.rw)] {
        let r = web(
            &w.a,
            &u.web,
            Method::GET,
            &format!("/api/v1/entries/{id}?format=html"),
            None,
        )
        .await;
        ok(&r, StatusCode::OK, &format!("страница: {who}"));
        let html = r.json()["html"].as_str().unwrap().to_string();
        let src = format!(
            "src=\"api/v1/entries/{id}/attachments/{}/loss.png\"",
            loss.sha256
        );
        assert!(html.contains(&src), "{who}: {html}");
        assert!(!html.contains("?user="), "{who}: {html}");
        let r = web(&w.a, &u.web, Method::GET, &file_uri(&id, &loss), None).await;
        ok(&r, StatusCode::OK, &format!("файл: {who}"));
        assert_eq!(&r.body[..], &png(1, 200)[..]);
        let csv = w.file(&id, "results.csv").await;
        let r = web(&w.a, &u.web, Method::GET, &file_uri(&id, &csv), None).await;
        ok(&r, StatusCode::OK, &format!("csv: {who}"));
    }
}
