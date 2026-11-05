//! MCP-сервер `/mcp` (§13.1; задача 9 M4) — тонкий адаптер над REST-ядром (принцип 5).
//!
//! - **Транспорт** — streamable HTTP `rmcp` без сессий: `legacy_session_mode = false`
//!   (MCP 2026-07-28 убрал сессии, SEP-2567; клиенты 2025-xx получают ответ `initialize`
//!   без `Mcp-Session-Id` и дальше работают так же), ответы — `application/json`
//!   (`json_response`), SSE — только если обработчик шлёт уведомления до ответа (у нас нет).
//!   Предел тела — как у JSON записи журнала ([`crate::api::entries_json_max`]: отчёт с файлами
//!   в base64 до `reports.mcp_max_bytes`) для всех инструментов; пределы заметок и записей
//!   проверяют маршруты REST. Тело больше `note_body_max` занимает разрешение
//!   [`crate::api::big_body`] (не больше двух одновременно, иначе 503 `busy`).
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
//! - **Сессия без заголовка** (решение контроллера R13, задача 10 M4): `.mcp.json` не знает
//!   `session_id` агента, поэтому запрос без `X-Zam-Session` идёт во внутренний REST с
//!   последней незавершённой сессией пользователя токена, к которой обращались не позже
//!   [`IMPLICIT_SESSION_HOURS`] ч назад (её создаёт хук `session-start`); с `X-Zam-Machine` —
//!   только сессией той же машины, с `X-Zam-Solution` — того же решения ([`implicit_session`]). Так запись через MCP попадает в
//!   `memory_writes` сессии хуков и `checkpoint` не напоминает зря. Подходящей нет — без сессии.
//! - **Явная сессия или никакая** (M6 §7.1): неявная сессия годится только счётчику записей.
//!   Внутренние запросы несут расширение [`McpOrigin`] (`implicit` — сессию подставил сервер);
//!   поиск, `/read` и `/recall` с неявной сессией не пишут `session_seen` и не помечают
//!   выданное. Учёт выданного — один на вызов инструмента, событием `mcp` ([`account`]): с явной
//!   сессией — в `session_tokens`, иначе — в `tokens_unattributed` пользователя (`GET
//!   /sessions/unattributed`). Явную сессию задаёт `zam mcp` (`ZAM_SESSION_ID` или
//!   файл привязки хука `session-start`; `.mcp.json` Claude Code с M6 — stdio `zam mcp`). В
//!   DEBUG — `implicit` и `no_session`; учёт пишется в фоне после ответа.
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

pub use crate::api::sessions::McpOrigin;
use crate::app::AppState;
use crate::attrib::{AREA_HEADER, MACHINE_HEADER, SESSION_HEADER};
use crate::auth::Principal;
use crate::error::ApiError;

/// Событие учёта выданного вызовом инструмента MCP (M6 §7.1).
pub const MCP_TOKEN_EVENT: &str = "mcp";

/// Окно поиска сессии для MCP без `X-Zam-Session`, часов (R13).
pub const IMPLICIT_SESSION_HOURS: i64 = 12;

/// Путь MCP (под `server.base_path`, как и REST).
pub const MCP_PATH: &str = "/mcp";

/// Заголовки исходного запроса, которые уходят во внутренние вызовы REST. Заголовки прокси —
/// чтобы IP клиента в лимитах и аудите был тем же, что у REST; доверять ли им, решает
/// `trusted_peer` по `ConnectInfo` (`server.trusted_proxies`), как и для REST. `Host` — для
/// адреса WebUI в ответе (`url` отчёта при пустом `server.public_url`, M5.5).
const FORWARDED: [&str; 9] = [
    "host",
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
перечитать). Задачи, вопросы человеку, словарь — memory_log kind=task|question|term; итог \
для пользователя — kind=report (большой — zam report); после \
сжатия контекста — memory_recall event=recovery, задание субагенту — event=brief; карта кода — \
memory_code (map, symbol, refs, impact). Протокол — zam guide (GET /api/v1/agent-guide). Ответы \
— Markdown, последняя строка ~N tok.";

/// Маршрут `/mcp` с проверкой bearer-токена. Внутренний роутер инструментов собирается
/// здесь же из [`crate::api::router`] — без `/mcp`.
pub fn router(state: &AppState) -> Router<AppState> {
    let server = McpServer {
        rest: crate::api::router(state).with_state(state.clone()),
        state: state.clone(),
    };
    let cfg = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .disable_allowed_hosts()
        .with_max_request_body_bytes(crate::api::entries_json_max(&state.cfg));
    let svc = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(NeverSessionManager::default()),
        cfg,
    );
    Router::new()
        .route_service(MCP_PATH, svc)
        // тело — после аутентификации: разрешения на большие тела берут только токены
        .route_layer(from_fn_with_state(
            state.clone(),
            crate::api::big_body::limit_mcp,
        ))
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
        Ok(p) => {
            // принципал — для выбора сессии без заголовка (R13), токен второй раз не проверяется
            parts.extensions.insert(p);
            next.run(Request::from_parts(parts, body)).await
        }
        Err(e) => e.into_response(),
    }
}

/// Обработчик MCP: список инструментов и их вызов поверх REST.
#[derive(Clone)]
pub struct McpServer {
    rest: Router,
    state: AppState,
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
        let mut rest = Rest::new(self.rest.clone(), parts);
        rest.cancel = Some(context.ct.clone());
        // явная сессия — заголовок запроса (`zam mcp` с привязкой, M6 §7.1); иначе — неявная
        // сессия R13 только для счётчика записей
        let explicit = rest.header(SESSION_HEADER).is_some();
        if !explicit
            && let Some(id) = implicit_session(&self.state, parts).await
            && let Ok(v) = HeaderValue::from_str(&id)
        {
            rest.headers
                .insert(HeaderName::from_static(SESSION_HEADER), v);
            rest.origin.implicit = true;
        }
        let t = Instant::now();
        let out = tools::call(tool, &rest, request.arguments.unwrap_or_default()).await;
        tracing::debug!(
            tool = tool.as_str(),
            is_error = out.is_error,
            implicit = rest.origin.implicit,
            no_session = !explicit && !rest.origin.implicit,
            ms = t.elapsed().as_millis() as u64,
            "инструмент MCP"
        );
        // учёт — в фоне: ответ агенту его не ждёт (ревью задачи 3, M2)
        if let Some(p) = parts.extensions.get::<Principal>().cloned() {
            let (st, headers, tokens) = (self.state.clone(), parts.headers.clone(), out.tokens());
            tokio::spawn(async move { account(&st, &p, &headers, explicit, tokens).await });
        }
        Ok(out.into_result().into())
    }
}

/// Учёт выданного вызовом инструмента (M6 §7.1) — событие `mcp`: с явной сессией — в
/// `session_tokens` этой сессии, без неё — в `tokens_unattributed` пользователя (сутки UTC).
/// Вложенные `/search`, `/read`, `/recall` своих событий не пишут (`McpOrigin`). Идёт в фоне
/// после ответа; сбой учёта ответ не портит (WARN без id и текста).
async fn account(st: &AppState, p: &Principal, headers: &HeaderMap, explicit: bool, tokens: u64) {
    // некорректные заголовки отклонил сам маршрут REST — учёта нет
    let Ok(user) = crate::auth::target_user_id(st, p, headers) else {
        return;
    };
    let db = match st.users.get(user).await {
        Ok(db) => db,
        Err(e) => {
            tracing::warn!(error = %e, "MCP: учёт выданного — база пользователя недоступна");
            return;
        }
    };
    let r = if explicit {
        match crate::attrib::session_touch(p, headers, None) {
            Ok(Some(touch)) => zam_store::add_tokens(&db, &touch, MCP_TOKEN_EVENT, tokens).await,
            _ => return,
        }
    } else {
        let day = jiff::Timestamp::now()
            .to_zoned(jiff::tz::TimeZone::UTC)
            .date();
        zam_store::add_unattributed(&db, day, MCP_TOKEN_EVENT, 1, tokens).await
    };
    if let Err(e) = r {
        tracing::warn!(error = %e, "MCP: учёт выданного не записан");
    }
}

/// Сессия для запроса MCP без `X-Zam-Session` (R13): последняя незавершённая сессия
/// пользователя токена за [`IMPLICIT_SESSION_HOURS`] ч, с `X-Zam-Machine` — той же машины, с
/// `X-Zam-Solution` — того же решения.
/// Сбой поиска — без сессии (WARN без id).
async fn implicit_session(st: &AppState, parts: &Parts) -> Option<String> {
    let p = parts.extensions.get::<Principal>()?;
    let user = crate::auth::target_user_id(st, p, &parts.headers).ok()?;
    // некорректный X-Zam-Machine отклонит сам маршрут REST
    let machine = crate::attrib::header_value(&parts.headers, MACHINE_HEADER).ok()?;
    // с решением — только сессии этого решения; некорректный X-Zam-Solution — как без него
    let solution: Option<zam_core::SolutionGuid> =
        crate::attrib::header_value(&parts.headers, crate::api::search::SOLUTION_HEADER)
            .ok()
            .flatten()
            .and_then(|g| g.trim().parse().ok());
    let since = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(IMPLICIT_SESSION_HOURS);
    let found = match st.users.get(user).await {
        Ok(db) => {
            zam_store::latest_session(&db, machine.as_deref(), solution.as_ref(), since).await
        }
        Err(e) => Err(e),
    };
    match found {
        Ok(s) => s.map(|s| s.id),
        Err(e) => {
            tracing::warn!(error = %e, "MCP: сессия пользователя не найдена");
            None
        }
    }
}

/// Внутренний клиент REST: запросы к роутеру `/api/v1` с заголовками исходного запроса MCP.
pub struct Rest {
    router: Router,
    headers: HeaderMap,
    connect: Option<ConnectInfo<SocketAddr>>,
    /// Разрешение большого тела внешнего `/mcp` ([`crate::api::big_body::OuterPermit`]): копия
    /// держит его до конца вызова инструмента, даже если клиент оборвал соединение.
    outer_permit: Option<crate::api::big_body::OuterPermit>,
    /// Отмена вызова (`RequestContext::ct`): клиент прислал `notifications/cancelled` или оборвал
    /// соединение — дальше внутренних запросов нет.
    cancel: Option<tokio_util::sync::CancellationToken>,
    /// Признак MCP внутренних запросов (M6 §7.1): расширение запроса, снаружи не подделать.
    origin: McpOrigin,
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
        // HTTP/2 без `Host` — адрес из `:authority`
        if !headers.contains_key(header::HOST)
            && let Some(a) = parts.uri.authority()
            && let Ok(v) = HeaderValue::from_str(a.as_str())
        {
            headers.insert(header::HOST, v);
        }
        Rest {
            router,
            headers,
            connect: parts.extensions.get::<ConnectInfo<SocketAddr>>().cloned(),
            outer_permit: parts
                .extensions
                .get::<crate::api::big_body::OuterPermit>()
                .cloned(),
            cancel: None,
            origin: McpOrigin::default(),
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
        if self.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
            return Err("вызов отменён клиентом".into());
        }
        let mut req = b.body(body).map_err(|e| format!("запрос REST: {e}"))?;
        // внешний `/mcp` с большим телом уже держит разрешение (его копия живёт в `self`, пока
        // идёт вызов) — внутреннее его не берёт; без него большое внутреннее тело берёт своё
        req.extensions_mut()
            .insert(crate::api::big_body::InnerCall {
                outer_permit: self.outer_permit.is_some(),
            });
        if let Some(c) = self.connect {
            req.extensions_mut().insert(c);
        }
        req.extensions_mut().insert(self.origin);
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

    /// Ревью задачи 5, I1: внешний future `/mcp` уничтожен (обрыв клиента), а инструмент ещё
    /// работает — разрешение держит копия в [`super::Rest`] до конца вызова.
    #[tokio::test]
    async fn rest_keeps_outer_permit_until_dropped() {
        use crate::api::big_body::{BigBodies, OuterPermit};
        use std::sync::Arc;
        let b = BigBodies::new(2, 10);
        let permit = Arc::new(b.admit().await.unwrap());
        let (mut parts, ()) = axum::http::Request::new(()).into_parts();
        parts.extensions.insert(OuterPermit(permit.clone()));
        let rest = super::Rest::new(axum::Router::new(), &parts);
        // слой `limit_mcp` и запрос rmcp уничтожены
        drop((permit, parts));
        assert_eq!(b.available(), 1, "разрешение держит вызов инструмента");
        assert!(rest.outer_permit.is_some());
        drop(rest);
        assert_eq!(b.available(), 2);
    }

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
