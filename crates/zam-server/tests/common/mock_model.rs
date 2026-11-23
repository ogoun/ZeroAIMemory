//! Подставной OpenAI-совместимый сервер модели (M7.3, спецификация §6.2 «Проверка»): `axum` на
//! `127.0.0.1` со своим потоком и рантаймом (годится и для синхронных тестов клиента `ureq`, и
//! для тестов REST сервера). Отвечает по очереди заданных ответов ([`MockModel::push`]; очередь
//! пуста — ответ по умолчанию [`MockModel::set_default`]) и запоминает каждый запрос — метод,
//! путь, заголовки, тело — на любом пути: перенаправление, если бы клиент ему последовал, тоже
//! попало бы в журнал.
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use serde_json::{Value, json};

/// Ответ подставной модели.
#[derive(Debug, Clone)]
pub struct MockReply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// Задержка перед ответом (таймаут клиента).
    pub delay: Duration,
}

impl MockReply {
    /// Ответ с кодом и телом как есть.
    pub fn raw(status: u16, body: &str) -> MockReply {
        MockReply {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.to_string(),
            delay: Duration::ZERO,
        }
    }

    /// Ответ с кодом и JSON-телом.
    pub fn json(status: u16, body: Value) -> MockReply {
        MockReply::raw(status, &body.to_string())
    }

    /// Обычный ответ Chat Completions: `content`, `finish_reason = stop`, `usage` 11/7.
    pub fn chat(content: &str) -> MockReply {
        MockReply::json(
            200,
            json!({
                "id": "chatcmpl-mock",
                "object": "chat.completion",
                "model": "mock",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": content},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18}
            }),
        )
    }

    /// Ответ без `usage` (часть локальных серверов).
    pub fn chat_no_usage(content: &str) -> MockReply {
        MockReply::json(
            200,
            json!({
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": content},
                    "finish_reason": "stop"
                }]
            }),
        )
    }

    /// Предел съеден размышлениями: `content = null`, `finish_reason = length`, непустое
    /// `reasoning_content` (DeepSeek, Qwen, llama.cpp).
    pub fn thinking() -> MockReply {
        MockReply::json(
            200,
            json!({
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "reasoning_content": "Сначала подумаю, что спрашивают…"
                    },
                    "finish_reason": "length"
                }],
                "usage": {"prompt_tokens": 20, "completion_tokens": 256}
            }),
        )
    }

    /// Перенаправление `302` на `location`.
    pub fn redirect(location: &str) -> MockReply {
        MockReply::raw(302, "").header("location", location)
    }

    pub fn header(mut self, name: &str, value: &str) -> MockReply {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn delay(mut self, d: Duration) -> MockReply {
        self.delay = d;
        self
    }
}

/// Запрос, который получила подставная модель.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    /// Заголовки, имена строчными.
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl Seen {
    /// Тело как JSON (иначе паника с телом).
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("не JSON ({e}): {}", self.body))
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

struct Shared {
    queue: Mutex<VecDeque<MockReply>>,
    default: Mutex<MockReply>,
    seen: Mutex<Vec<Seen>>,
}

/// Подставная модель; останавливается при сбросе.
pub struct MockModel {
    addr: SocketAddr,
    shared: Arc<Shared>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl MockModel {
    /// Поднимает сервер на `127.0.0.1:<свободный порт>`; ответ по умолчанию —
    /// [`MockReply::chat`] с `{"ok": true}`.
    pub fn start() -> MockModel {
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            default: Mutex::new(MockReply::chat(r#"{"ok": true}"#)),
            seen: Mutex::new(Vec::new()),
        });
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let st = shared.clone();
        std::thread::Builder::new()
            .name("mock-model".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async move {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    addr_tx.send(listener.local_addr().unwrap()).unwrap();
                    let app = axum::Router::new().fallback(handle).with_state(st);
                    let _ = axum::serve(listener, app)
                        .with_graceful_shutdown(async move {
                            let _ = stop_rx.await;
                        })
                        .await;
                });
            })
            .unwrap();
        let addr = addr_rx.recv().unwrap();
        MockModel {
            addr,
            shared,
            stop: Some(stop_tx),
        }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Базовый адрес подключения: `http://127.0.0.1:<порт>/v1`.
    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    /// Следующий ответ в очередь.
    pub fn push(&self, r: MockReply) {
        self.shared.queue.lock().unwrap().push_back(r);
    }

    /// Ответ, когда очередь пуста.
    pub fn set_default(&self, r: MockReply) {
        *self.shared.default.lock().unwrap() = r;
    }

    /// Все полученные запросы по порядку.
    pub fn requests(&self) -> Vec<Seen> {
        self.shared.seen.lock().unwrap().clone()
    }

    /// Число полученных запросов.
    pub fn count(&self) -> usize {
        self.shared.seen.lock().unwrap().len()
    }

    /// Последний запрос (нет — паника).
    pub fn last(&self) -> Seen {
        self.requests()
            .pop()
            .expect("подставная модель не получила запросов")
    }
}

impl Drop for MockModel {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
    }
}

async fn handle(State(st): State<Arc<Shared>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, 16 << 20)
        .await
        .unwrap_or_default();
    let headers = parts
        .headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_ascii_lowercase(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    st.seen.lock().unwrap().push(Seen {
        method: parts.method.to_string(),
        path: parts.uri.path().to_string(),
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    });
    let reply = {
        let next = st.queue.lock().unwrap().pop_front();
        next.unwrap_or_else(|| st.default.lock().unwrap().clone())
    };
    if !reply.delay.is_zero() {
        tokio::time::sleep(reply.delay).await;
    }
    let mut resp = Response::new(Body::from(reply.body));
    *resp.status_mut() = StatusCode::from_u16(reply.status).unwrap();
    for (k, v) in &reply.headers {
        resp.headers_mut().insert(
            HeaderName::from_bytes(k.as_bytes()).unwrap(),
            HeaderValue::from_str(v).unwrap(),
        );
    }
    resp
}
