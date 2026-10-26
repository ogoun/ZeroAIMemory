//! Помощники тестов REST API: сервер, токены, cookie администратора, запросы.
use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use zam_server::config::ServerConfig;
use zam_server::{AppState, build};

const PW: &str = "pw";

/// Сервер с первым токеном администратора и cookie-сессией.
pub struct Api {
    pub app: Router,
    pub st: AppState,
    /// Первый токен (`rw`) пользователя администратора.
    pub token: String,
    pub cookie: String,
    pub csrf: String,
}

/// Ответ: статус, заголовки, тело.
pub struct Resp {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Resp {
    pub fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("не JSON ({e}): {}", self.text()))
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .unwrap_or_else(|| panic!("нет заголовка {name}"))
            .to_str()
            .unwrap()
    }
}

/// Percent-encoding всех байтов, кроме ASCII-букв и цифр.
pub fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

pub async fn api(name: &str, tweak: impl FnOnce(&mut ServerConfig)) -> Api {
    let mut c = super::cfg_in(super::tmp(&format!("api-{name}")));
    tweak(&mut c);
    api_with(c).await
}

/// Сервер по готовой конфигурации (каталог данных не очищается: повторный старт).
/// Первый токен есть только при первом старте каталога; при повторном — пустая строка.
pub async fn api_with(c: ServerConfig) -> Api {
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
    let b = r.into_body().collect().await.unwrap().to_bytes();
    let csrf = serde_json::from_slice::<Value>(&b).unwrap()["csrf"]
        .as_str()
        .unwrap()
        .to_string();
    Api {
        app,
        st,
        token: rep.initial_token.unwrap_or_default(),
        cookie,
        csrf,
    }
}

/// Построитель запроса с телом JSON (или пустым).
pub fn req(m: Method, uri: &str, body: Option<&Value>) -> axum::http::request::Builder {
    let b = Request::builder().method(m).uri(uri);
    match body {
        Some(_) => b.header(header::CONTENT_TYPE, "application/json"),
        None => b,
    }
}

fn body_of(body: Option<&Value>) -> Body {
    body.map_or_else(Body::empty, |v| Body::from(v.to_string()))
}

impl Api {
    pub async fn send(&self, r: Request<Body>) -> Resp {
        let r = self.app.clone().oneshot(r).await.unwrap();
        let (status, headers) = (r.status(), r.headers().clone());
        let body = r.into_body().collect().await.unwrap().to_bytes();
        Resp {
            status,
            headers,
            body,
        }
    }

    /// Запрос по токену `token` с дополнительными заголовками.
    pub async fn tok_h(
        &self,
        token: &str,
        m: Method,
        uri: &str,
        body: Option<Value>,
        extra: &[(&str, &str)],
    ) -> Resp {
        let mut b =
            req(m, uri, body.as_ref()).header(header::AUTHORIZATION, format!("Bearer {token}"));
        for (k, v) in extra {
            b = b.header(*k, *v);
        }
        self.send(b.body(body_of(body.as_ref())).unwrap()).await
    }

    pub async fn tok(&self, token: &str, m: Method, uri: &str, body: Option<Value>) -> Resp {
        self.tok_h(token, m, uri, body, &[]).await
    }

    /// Запрос по первому токену администратора.
    pub async fn t(&self, m: Method, uri: &str, body: Option<Value>) -> Resp {
        self.tok(&self.token, m, uri, body).await
    }

    /// Запрос администратора по cookie; `user` — `X-Zam-User`.
    pub async fn adm(&self, m: Method, uri: &str, body: Option<Value>, user: Option<&str>) -> Resp {
        let mut b = req(m, uri, body.as_ref())
            .header(header::COOKIE, &self.cookie)
            .header("x-csrf-token", &self.csrf);
        if let Some(u) = user {
            b = b.header("x-zam-user", u);
        }
        self.send(b.body(body_of(body.as_ref())).unwrap()).await
    }

    /// Новый пользователь: (id, токен `rw`).
    pub async fn new_user(&self, name: &str) -> (String, String) {
        let r = self
            .adm(
                Method::POST,
                "/api/v1/admin/users",
                Some(json!({ "name": name })),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        let j = r.json();
        (
            j["user"]["id"].as_str().unwrap().to_string(),
            j["token"]["plaintext"].as_str().unwrap().to_string(),
        )
    }

    /// Новый токен пользователя `user` с доступом `access`.
    pub async fn issue(&self, user: &str, access: &str) -> String {
        let r = self
            .adm(
                Method::POST,
                &format!("/api/v1/admin/users/{user}/tokens"),
                Some(json!({ "name": format!("t-{access}"), "access": access })),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        r.json()["plaintext"].as_str().unwrap().to_string()
    }

    /// Id пользователя администратора.
    pub fn admin_id(&self) -> String {
        self.st.admin_id.to_string()
    }

    /// Создаёт заметку первым токеном; возвращает узел.
    pub async fn note(&self, space: &str, path: &str, content: &str) -> Value {
        let r = self
            .t(
                Method::POST,
                "/api/v1/nodes",
                Some(json!({"space": space, "path": path, "kind": "note", "content": content})),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        r.json()
    }
}

/// Фильтр векторного индекса по всей базе пользователя `user` — одна область со всеми
/// пространствами базы (область без пространств фильтр не пропускает).
pub async fn vector_base(a: &Api, user: zam_core::Id) -> zam_index::VectorFilter {
    let db = a.st.users.get(user).await.unwrap();
    let spaces = zam_store::list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    zam_index::VectorFilter::scope(user, spaces)
}
