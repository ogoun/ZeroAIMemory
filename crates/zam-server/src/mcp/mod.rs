//! MCP-сервер `/mcp` (§13.1; задача 9 M4) — тонкий адаптер над REST-ядром (принцип 5).
//!
//! - **Транспорт** — streamable HTTP `rmcp` без сессий: `legacy_session_mode = false`
//!   (MCP 2026-07-28 убрал сессии, SEP-2567; клиенты 2025-xx получают ответ `initialize`
//!   без `Mcp-Session-Id` и дальше работают так же), ответы — `application/json`
//!   (`json_response`), SSE — только если обработчик шлёт уведомления до ответа (у нас нет).
//!   Предел тела — как у записи заметки ([`crate::api::note_body_max`]).
//! - **Аутентификация** — `Authorization: Bearer` обязателен ([`require_bearer`]): токен
//!   проверяется тем же извлекателем [`Principal`], что у REST (лимит неудачных попыток по
//!   IP, отзыв, срок); без заголовка или с неверным токеном — 401 problem+json до `rmcp`.
//!   Cookie WebUI здесь не действует.
//! - **Инструменты** ([`tools`]) вызывают маршруты `/api/v1/…` через внутренний `Router`
//!   ([`Rest`], `tower::ServiceExt::oneshot`; решение контроллера R12) с заголовками
//!   исходного запроса — `Authorization`, `X-Zam-Solution`, `X-Zam-Session`, `X-Zam-Machine`,
//!   `X-Zam-Area` — и адресом клиента (`ConnectInfo`): сами срабатывают проверка токена и
//!   прав, лимиты, счётчик `memory_writes` (запись идёт через `memory_routes`), атрибуция
//!   ревизий, предупреждения. Внутренний роутер — [`crate::api::router`] без `/mcp` и WebUI:
//!   рекурсии нет.
//! - **Host** не проверяется (`disable_allowed_hosts`) **[решение исполнителя]**: сервер
//!   слушает LAN под своим именем (`memory.lan`), а защита от DNS rebinding не нужна —
//!   без bearer-токена, который браузер сам не подставит, `/mcp` отвечает 401.
//! - **Логи** — имя инструмента, признак ошибки и длительность (DEBUG); аргументы и ответы
//!   (тексты заметок, значения Vault, промпты) в журнал не попадают; сам `rmcp` пишет JSON-RPC
//!   на debug/trace — его уровень ограничен `warn` в [`crate::logging`].
//! - **Origin** — если есть, должен совпадать с `Host` ([`same_origin`]), иначе 403.
pub mod tools;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::Value;
use tower::ServiceExt;

use crate::app::AppState;
use crate::attrib::{AREA_HEADER, MACHINE_HEADER, SESSION_HEADER};
use crate::auth::Principal;
use crate::error::ApiError;

/// Путь MCP (под `server.base_path`, как и REST).
pub const MCP_PATH: &str = "/mcp";

/// Заголовки исходного запроса, которые уходят во внутренние вызовы REST. Заголовки прокси —
/// чтобы IP клиента в лимитах и аудите был тем же, что у REST; доверять ли им, решает
/// `trusted_peer` по `ConnectInfo` (`server.trusted_proxies`), как и для REST.
const FORWARDED: [&str; 8] = [
    "authorization",
    crate::api::search::SOLUTION_HEADER,
    SESSION_HEADER,
    MACHINE_HEADER,
    AREA_HEADER,
    "x-forwarded-for",
    "x-forwarded-proto",
    "forwarded",
];

/// Инструкция клиенту в ответе `initialize`.
const INSTRUCTIONS: &str = "ZeroAIMemory — долговременная память агента. Порядок: memory_search \
(карточки, шаг 0 — кэш выжимок) → memory_read нужного → memory_cache_put выжимки. Запись — \
memory_write, memory_log, memory_update_entry; ревизии обязательны при правке (409/428 — \
перечитать). Ответы — Markdown, последняя строка ~N tok.";

/// Маршрут `/mcp` с проверкой bearer-токена. Внутренний роутер инструментов собирается
/// здесь же из [`crate::api::router`] — без `/mcp`.
pub fn router(state: &AppState) -> Router<AppState> {
    let server = McpServer {
        rest: crate::api::router(state).with_state(state.clone()),
    };
    let cfg = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .disable_allowed_hosts()
        .with_max_request_body_bytes(crate::api::note_body_max(&state.cfg.limits));
    let svc = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(NeverSessionManager::default()),
        cfg,
    );
    Router::new()
        .route_service(MCP_PATH, svc)
        .route_layer(from_fn_with_state(state.clone(), require_bearer))
        .route_layer(axum::middleware::from_fn(same_origin))
}

/// `Origin` (если есть) должен совпадать с `Host` — хостом и портом (порт по умолчанию — по
/// схеме `Origin`); иначе 403 problem. Запрос без `Origin` (агенты, CLI) проходит. Защита от
/// страниц в браузере, которые шлют запросы на `/mcp` с чужого сайта.
async fn same_origin(req: Request, next: Next) -> Response {
    if let Some(origin) = req.headers().get(header::ORIGIN) {
        let host = req
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .map(str::to_string)
            .or_else(|| req.uri().authority().map(|a| a.to_string()));
        let ok = origin
            .to_str()
            .ok()
            .zip(host.as_deref())
            .is_some_and(|(o, h)| origin_matches_host(o, h));
        if !ok {
            return ApiError::Denied("Origin не совпадает с Host: запрос с чужой страницы".into())
                .into_response();
        }
    }
    next.run(req).await
}

/// `scheme://host[:port]` против `host[:port]`: хост без учёта регистра, порт — явный или
/// по умолчанию для схемы `Origin` (http — 80, https — 443).
fn origin_matches_host(origin: &str, host: &str) -> bool {
    let Some((scheme, rest)) = origin.trim().split_once("://") else {
        return false;
    };
    let default = match scheme.to_ascii_lowercase().as_str() {
        "http" => 80,
        "https" => 443,
        _ => return false,
    };
    let split = |s: &str| -> Option<(String, u16)> {
        let a: axum::http::uri::Authority = s.parse().ok()?;
        Some((
            a.host().to_ascii_lowercase(),
            a.port_u16().unwrap_or(default),
        ))
    };
    match (split(rest.trim_end_matches('/')), split(host.trim())) {
        (Some(o), Some(h)) => o == h,
        _ => false,
    }
}

/// `/mcp` — только с `Authorization: Bearer` действующего токена (тот же [`Principal`], что у
/// REST); иначе 401 problem+json.
async fn require_bearer(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    if crate::auth::bearer(&parts.headers).is_none() {
        return ApiError::Unauthorized.into_response();
    }
    match Principal::from_request_parts(&mut parts, &st).await {
        Ok(_) => next.run(Request::from_parts(parts, body)).await,
        Err(e) => e.into_response(),
    }
}

/// Обработчик MCP: список инструментов и их вызов поверх REST.
#[derive(Clone)]
pub struct McpServer {
    rest: Router,
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("zeroaimemory", env!("CARGO_PKG_VERSION"))
                    .with_title("ZeroAIMemory"),
            )
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            tools::definitions().to_vec(),
        ))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools::definitions()
            .iter()
            .find(|t| t.name == name)
            .cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let Some(tool) = tools::ToolName::parse(&request.name) else {
            return Err(ErrorData::invalid_params(
                format!("неизвестный инструмент: {}", request.name),
                None,
            ));
        };
        let Some(parts) = context.extensions.get::<Parts>() else {
            return Err(ErrorData::internal_error(
                "нет HTTP-запроса MCP".to_string(),
                None,
            ));
        };
        let rest = Rest::new(self.rest.clone(), parts);
        let t = Instant::now();
        let out = tools::call(tool, &rest, request.arguments.unwrap_or_default()).await;
        tracing::debug!(
            tool = tool.as_str(),
            is_error = out.is_error,
            ms = t.elapsed().as_millis() as u64,
            "инструмент MCP"
        );
        Ok(out.into_result().into())
    }
}

/// Внутренний клиент REST: запросы к роутеру `/api/v1` с заголовками исходного запроса MCP.
pub struct Rest {
    router: Router,
    headers: HeaderMap,
    connect: Option<ConnectInfo<SocketAddr>>,
}

/// Ответ внутреннего вызова REST.
pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Reply {
    /// Тело как JSON (`Null` — пустое или не JSON).
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// Тело как текст UTF-8.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

impl Rest {
    fn new(router: Router, parts: &Parts) -> Self {
        let mut headers = HeaderMap::new();
        for name in FORWARDED {
            if let Some(v) = parts.headers.get(name) {
                headers.insert(HeaderName::from_static(name), v.clone());
            }
        }
        Rest {
            router,
            headers,
            connect: parts.extensions.get::<ConnectInfo<SocketAddr>>().cloned(),
        }
    }

    /// Значение заголовка исходного запроса (из пересылаемых), без пустых.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// Запрос к REST: `uri` — путь с query (`/api/v1/…`), `body` — JSON, `extra` —
    /// дополнительные заголовки (`If-Match`, `Idempotency-Key`).
    pub async fn send(
        &self,
        method: Method,
        uri: &str,
        body: Option<&Value>,
        extra: &[(HeaderName, String)],
    ) -> Result<Reply, String> {
        let mut b = Request::builder().method(method).uri(uri);
        for (k, v) in &self.headers {
            b = b.header(k, v);
        }
        for (k, v) in extra {
            let v = HeaderValue::from_str(v)
                .map_err(|_| format!("заголовок {k}: недопустимое значение"))?;
            b = b.header(k, v);
        }
        let body = match body {
            Some(v) => {
                b = b.header(header::CONTENT_TYPE, "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let mut req = b.body(body).map_err(|e| format!("запрос REST: {e}"))?;
        if let Some(c) = self.connect {
            req.extensions_mut().insert(c);
        }
        let Ok(r) = self.router.clone().oneshot(req).await;
        let (status, headers) = (r.status(), r.headers().clone());
        let body = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .map_err(|e| format!("ответ REST: {e}"))?;
        Ok(Reply {
            status,
            headers,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::origin_matches_host;

    #[test]
    fn origin_vs_host() {
        assert!(origin_matches_host(
            "http://memory.lan:7700",
            "memory.lan:7700"
        ));
        assert!(origin_matches_host("http://Memory.LAN", "memory.lan:80"));
        assert!(origin_matches_host("https://memory.lan", "memory.lan"));
        assert!(!origin_matches_host("https://memory.lan", "memory.lan:80"));
        assert!(!origin_matches_host(
            "http://evil.example",
            "memory.lan:7700"
        ));
        assert!(!origin_matches_host(
            "http://memory.lan:7701",
            "memory.lan:7700"
        ));
        assert!(!origin_matches_host("null", "memory.lan:7700"));
    }
}
