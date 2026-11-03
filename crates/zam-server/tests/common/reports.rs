//! Мир тестов отчётов (M5.5, задачи 3–4): владелец решения, участники `rw` и `ro` — у каждого
//! токен и сессия WebUI; отчёты пишет агент владельца (multipart, как `zam report`).
use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use zam_server::config::ServerConfig;
use zam_store::{EntryAttachment, entry_attachments, get_entry};

use super::api::{Api, Resp, api, req};

pub const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e12";
pub const BOUNDARY: &str = "zam-report-access-7d21";
pub const PASS: &str = "first-pass-123";
pub const NEW_PASS: &str = "second-pass-456";
pub const IMMUTABLE: &str = "/problems/report-immutable";

/// Сессия пользователя WebUI: cookie, CSRF и адрес клиента (лимит входа — по IP).
pub struct Web {
    pub cookie: String,
    pub csrf: String,
    pub ip: String,
}

/// Пользователь: id, токен `rw`, сессия WebUI.
pub struct User {
    pub id: String,
    pub tok: String,
    pub web: Web,
}

pub struct W {
    pub a: Api,
    /// Владелец решения.
    pub o: User,
    /// Участник `rw`.
    pub rw: User,
    /// Участник `ro`.
    pub ro: User,
}

pub fn with_ip(mut r: Request<Body>, ip: &str) -> Request<Body> {
    r.extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip.parse().unwrap(), 40_000)));
    r
}

pub fn ok(r: &Resp, s: StatusCode, ctx: &str) {
    assert_eq!(r.status, s, "{ctx}: {}", r.text());
}

pub async fn login(a: &Api, name: &str, pw: &str, ip: &str) -> Web {
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
pub async fn web_h(
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

pub async fn web(a: &Api, w: &Web, m: Method, uri: &str, body: Option<Value>) -> Resp {
    web_h(a, w, m, uri, body, &[]).await
}

/// Пользователь с паролем после обязательной смены.
pub async fn ready_user(a: &Api, name: &str, ip: &str) -> User {
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
pub async fn share(a: &Api, guid: &str, owner: &str, member: &User, access: &str) {
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

pub async fn world(name: &str) -> W {
    world_cfg(name, |_| {}).await
}

/// Мир с правкой конфигурации сервера.
pub async fn world_cfg(name: &str, tweak: impl FnOnce(&mut ServerConfig)) -> W {
    let a = api(name, tweak).await;
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
pub fn multipart(meta: &Value, files: &[(&str, &[u8])]) -> Vec<u8> {
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
pub fn png(seed: u8, n: usize) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend((0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)));
    v
}

pub const SVG: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\" onload=\"alert(1)\">\
<script>alert(2)</script><rect width=\"10\" height=\"10\"/></svg>";
pub const CSV: &[u8] = b"epoch,loss\n1,0.9\n2,0.5\n";

/// Тело отчёта со всеми видами содержимого.
pub const BODY: &str = "# Итог\n\n![loss](figs/loss.png)\n\n![схема](diag.svg)\n\n\
Формула $E = mc^2$ и блок:\n\n$$\\int_0^1 x\\,dx$$\n\n\
```mermaid\ngraph TD; A-->B\n```\n\n| метрика | значение |\n|---|---|\n| acc | 0,93 |\n\n\
```rust\nfn main() { let x = 1; }\n```\n\n[данные](results.csv)\n";

impl W {
    /// Отчёт агентом владельца (multipart, как `zam report`): id.
    pub async fn report_with(&self, title: &str, body: &str, files: &[(&str, &[u8])]) -> String {
        let r = self.report_as(&self.o.tok, G, title, body, files).await;
        ok(&r, StatusCode::CREATED, "отчёт");
        r.json()["id"].as_str().unwrap().to_string()
    }

    /// Отчёт токеном `tok` в пространство `space` (multipart): ответ как есть.
    pub async fn report_as(
        &self,
        tok: &str,
        space: &str,
        title: &str,
        body: &str,
        files: &[(&str, &[u8])],
    ) -> Resp {
        let meta = json!({"title": title, "body": body, "tags": ["ml"]});
        let r = Request::post(format!("/api/v1/spaces/{space}/entries"))
            .header(header::AUTHORIZATION, format!("Bearer {tok}"))
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .header("x-zam-session", "sess-report-1")
            .header("x-zam-machine", "gpu-box")
            .body(Body::from(multipart(&meta, files)))
            .unwrap();
        self.a.send(r).await
    }

    /// Полный отчёт: картинка PNG, SVG и CSV.
    pub async fn report(&self, title: &str) -> String {
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

    pub async fn db(&self) -> zam_store::Db {
        self.a
            .st
            .users
            .get(self.o.id.parse().unwrap())
            .await
            .unwrap()
    }

    pub async fn files_of(&self, id: &str) -> Vec<EntryAttachment> {
        entry_attachments(&self.db().await, id.parse().unwrap())
            .await
            .unwrap()
    }

    pub async fn file(&self, id: &str, name: &str) -> EntryAttachment {
        self.files_of(id)
            .await
            .into_iter()
            .find(|f| f.filename == name)
            .unwrap()
    }

    /// Ревизия и тело записи в базе владельца.
    pub async fn stored(&self, id: &str) -> (i64, String) {
        let e = get_entry(&self.db().await, id.parse().unwrap())
            .await
            .unwrap();
        (e.rev, e.body)
    }

    /// Заметка в решении (токен владельца): id.
    pub async fn note(&self, path: &str) -> String {
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

pub fn file_uri(id: &str, f: &EntryAttachment) -> String {
    format!(
        "/api/v1/entries/{id}/attachments/{}/{}",
        f.sha256,
        super::api::enc(&f.filename)
    )
}

pub fn problem(r: &Resp) -> String {
    r.json()["type"].as_str().unwrap_or_default().to_string()
}
