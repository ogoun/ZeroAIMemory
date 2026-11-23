//! Клиент подключённой модели (M7.3, спецификация §6.2 пп. 1, 2, 5): OpenAI-совместимый Chat
//! Completions без потока и инструментов (решение координатора п. 36) — тело запроса, вызов
//! агентом `ureq`, разбор ответа и классы ошибок.
//!
//! - **Запрос** — `POST <url>/chat/completions`, `Authorization: Bearer <ключ>` (у `auth = "none"`
//!   — без него), тело `{model, messages, <предел>, stream: false}` и `body_extra` верхним уровнем;
//!   `temperature` и прочие параметры выборки не шлются. Предел при `token_field = "auto"` —
//!   `max_completion_tokens` у хоста `api.openai.com`, иначе `max_tokens` ([`request_body`]).
//! - **Ответ** — `choices[0].message.content` (строка или части с текстом); `finish_reason`:
//!   `stop` — итог, `length` — `max_tokens` (или `thinking`, если ответа нет, а рассуждение есть),
//!   `content_filter` — `refused`. `reasoning_content` (и `reasoning`) ответом не считается.
//!   Учёт — `usage`, без него — оценка счётчиком выдачи (`estimated`) ([`parse_response`]).
//! - **Агент `ureq`** ([`call_chat_blocking`]): `max_redirects(0)` — 3xx возвращается как есть и
//!   даёт `bad_response`; `http_status_as_error(false)` — тело 4xx/5xx читается (на нём держится
//!   `insufficient_quota`); соединение — только по адресам проверенного маршрута
//!   ([`super::egress::route`]); прокси окружения — только у маршрута вне LAN (`NO_PROXY` `ureq`
//!   учитывает сам), у петли и LAN — без прокси.
//!
//! Ключ поставщика в журнал, аудит и `tracing` не попадает: заголовок помечен чувствительным, а
//! `ureq` в отладочном журнале показывает только безопасные заголовки.

use std::time::Duration;

use serde_json::{Map, Value, json};

use super::egress::{BaseUrl, PinnedResolver, Route, endpoint};

/// Протокол подключения в M7.3 — один (§6.2 п. 1).
pub const PROTOCOL_OPENAI: &str = "openai";
/// Ключи тела, которые задаёт сервер: в `body_extra` — 422.
pub const RESERVED_BODY_KEYS: [&str; 5] = [
    "model",
    "messages",
    "stream",
    "max_tokens",
    "max_completion_tokens",
];
/// Предел `body_extra` (компактный JSON), байт.
pub const BODY_EXTRA_MAX_BYTES: usize = 4096;
/// Хост, у которого `token_field = "auto"` — `max_completion_tokens`.
pub const OPENAI_HOST: &str = "api.openai.com";
/// Срок соединения (§6.2 п. 7).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Предел тела ответа модели, байт.
const MAX_RESPONSE_BYTES: u64 = 4 << 20;

/// Класс исхода вызова модели (§6.2 п. 5; в журнале отправок и аудите — класс и код HTTP, без
/// текста ответа).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallClass {
    /// 401, 403 — пауза.
    Auth,
    /// 404 (модель или адрес) — пауза.
    NotFound,
    /// 400, 413, 422 — работа `failed`, ночь идёт дальше.
    BadRequest,
    /// 402, `insufficient_quota`, 429 без `retry-after` — стоп до следующей ночи.
    Quota,
    /// 429 с `retry-after`: не дольше 60 с — ожидание и один повтор, иначе стоп.
    Rate,
    /// 5xx, таймаут, отказ соединения, имя не разрешилось — два повтора; три за ночь — стоп.
    Unavailable,
    /// Предел съели размышления (`content` пуст, `reasoning_content` есть) — работа `failed`.
    Thinking,
    /// Ответ обрезан пределом `max_tokens` — работа `failed`.
    MaxTokens,
    /// Отказ модели (`content_filter`, `refusal`) — работа `failed`.
    Refused,
    /// 3xx, иной ответ (не Chat Completions) — работа `failed`.
    BadResponse,
    /// Ответ не JSON и после повтора — работа `failed`.
    BadJson,
    /// Находка сканера в постоянном тексте пакета (ошибка кода) — работа `failed`.
    SecretInPayload,
    /// Адрес запрещён политикой (§6.2 п. 6) — запрос не отправляется, пауза.
    Blocked,
    /// Запись Vault удалена, перенесена, Vault заблокирован, ключа нет — пауза.
    Vault,
    /// Адрес записи Vault не совпал с закреплённым — пауза до подтверждения человеком.
    VaultChanged,
    /// Не помещается в остаток суток или месяца — стоп до следующей ночи.
    Limit,
    /// Вызов оборван остановкой сервера.
    Aborted,
}

const ALL_CLASSES: [CallClass; 17] = [
    CallClass::Auth,
    CallClass::NotFound,
    CallClass::BadRequest,
    CallClass::Quota,
    CallClass::Rate,
    CallClass::Unavailable,
    CallClass::Thinking,
    CallClass::MaxTokens,
    CallClass::Refused,
    CallClass::BadResponse,
    CallClass::BadJson,
    CallClass::SecretInPayload,
    CallClass::Blocked,
    CallClass::Vault,
    CallClass::VaultChanged,
    CallClass::Limit,
    CallClass::Aborted,
];

impl CallClass {
    pub fn as_str(self) -> &'static str {
        match self {
            CallClass::Auth => "auth",
            CallClass::NotFound => "not_found",
            CallClass::BadRequest => "bad_request",
            CallClass::Quota => "quota",
            CallClass::Rate => "rate",
            CallClass::Unavailable => "unavailable",
            CallClass::Thinking => "thinking",
            CallClass::MaxTokens => "max_tokens",
            CallClass::Refused => "refused",
            CallClass::BadResponse => "bad_response",
            CallClass::BadJson => "bad_json",
            CallClass::SecretInPayload => "secret_in_payload",
            CallClass::Blocked => "blocked",
            CallClass::Vault => "vault",
            CallClass::VaultChanged => "vault_changed",
            CallClass::Limit => "limit",
            CallClass::Aborted => "aborted",
        }
    }

    /// Обратное [`CallClass::as_str`] (строки журнала отправок).
    pub fn parse(s: &str) -> Option<CallClass> {
        ALL_CLASSES.into_iter().find(|c| c.as_str() == s)
    }

    /// Подсказка человеку (проверка подключения, WebUI); у классов без совета — `None`.
    pub fn hint(self) -> Option<&'static str> {
        Some(match self {
            CallClass::Auth => "ключ не принят: проверьте значение записи Vault подключения",
            CallClass::NotFound => {
                "модель или адрес не найдены: проверьте `model` и `data.url` записи Vault (с /v1)"
            }
            CallClass::BadRequest => {
                "поставщик отверг запрос: обычно это поле `body_extra`, которого он не понимает"
            }
            CallClass::Quota => "у поставщика исчерпана квота или нет средств",
            CallClass::Rate => "поставщик ограничил частоту запросов",
            CallClass::Unavailable => "сервер модели недоступен или не ответил в срок",
            CallClass::Thinking => {
                "предел ответа съели размышления модели: выключите их в `body_extra` (например, \
                 {\"chat_template_kwargs\": {\"enable_thinking\": false}}) или увеличьте \
                 `max_tokens`"
            }
            CallClass::MaxTokens => {
                "ответ обрезан пределом: увеличьте `max_tokens` или выключите размышления"
            }
            CallClass::Refused => "модель отказалась отвечать",
            CallClass::BadResponse => {
                "ответ не по протоколу Chat Completions (или перенаправление 3xx — редиректы не \
                 выполняются)"
            }
            CallClass::BadJson => "ответ модели не JSON и после повтора",
            _ => return None,
        })
    }
}

/// Учёт токенов вызова: из `usage` ответа или оценкой (`estimated`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated: bool,
}

/// Неудача вызова: класс, код HTTP (если ответ был), `retry-after` в секундах (у `rate`) и учёт
/// токенов, если ответ их израсходовал (обрезанный пределом, отказ) — расход идёт в журнал
/// отправок и пределы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallError {
    pub class: CallClass,
    pub http_status: Option<u16>,
    pub retry_after: Option<u64>,
    pub usage: Option<Usage>,
}

impl CallError {
    /// Неудача без ответа HTTP.
    pub fn new(class: CallClass) -> CallError {
        CallError {
            class,
            http_status: None,
            retry_after: None,
            usage: None,
        }
    }

    fn http(class: CallClass, status: u16) -> CallError {
        CallError {
            http_status: Some(status),
            ..CallError::new(class)
        }
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.class.as_str())?;
        if let Some(s) = self.http_status {
            write!(f, " (HTTP {s})")?;
        }
        Ok(())
    }
}

impl std::error::Error for CallError {}

/// Ответ модели: текст `content` и учёт; `reasoning` — в ответе было непустое рассуждение
/// (`reasoning_content`/`reasoning`): проверка подключения советует выключить размышления.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelReply {
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated: bool,
    pub reasoning: bool,
}

impl ModelReply {
    pub fn usage(&self) -> Usage {
        Usage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            estimated: self.estimated,
        }
    }
}

/// Заголовок авторизации подключения (`auth`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelAuth {
    /// `Authorization: Bearer <ключ>` — ключ из значения записи Vault.
    Key,
    /// Без заголовка (локальный сервер без ключа).
    None,
}

/// Поле предела ответа (`token_field`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenField {
    /// `max_completion_tokens` у `api.openai.com`, иначе `max_tokens`.
    Auto,
    MaxTokens,
    MaxCompletionTokens,
}

/// Параметры запроса подключения.
///
/// **Этап 1 задачи 2:** временный тип до слияния с задачей 1 — там `works::settings::ModelConn`
/// (`protocol`, `vault`, `model`, `auth`, `max_tokens`, `token_field`, `body_extra`, `pinned`);
/// после перебазирования [`call_blocking`] берёт `&ModelConn`.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRequest {
    pub model: String,
    pub auth: ModelAuth,
    pub max_tokens: u32,
    pub token_field: TokenField,
    pub body_extra: Map<String, Value>,
}

/// Роль сообщения беседы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// Сообщение беседы Chat Completions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn system(s: impl Into<String>) -> ChatMessage {
        ChatMessage {
            role: Role::System,
            content: s.into(),
        }
    }
    pub fn user(s: impl Into<String>) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: s.into(),
        }
    }
    pub fn assistant(s: impl Into<String>) -> ChatMessage {
        ChatMessage {
            role: Role::Assistant,
            content: s.into(),
        }
    }
}

/// Протокол подключения — пока только `openai` (§6.2 п. 1); ошибка — текст для 422.
pub fn check_protocol(p: &str) -> Result<(), String> {
    if p == PROTOCOL_OPENAI {
        Ok(())
    } else {
        Err(format!(
            "протокол «{p}» не поддерживается: в этой версии — только \"openai\" \
             (OpenAI-совместимый Chat Completions); родной протокол Anthropic появится в \
             0.0.19-m7.6"
        ))
    }
}

/// `body_extra` — JSON-объект до [`BODY_EXTRA_MAX_BYTES`] без ключей [`RESERVED_BODY_KEYS`]
/// (§6.2 п. 1); ошибка — текст для 422.
pub fn check_body_extra(extra: &Map<String, Value>) -> Result<(), String> {
    if let Some(k) = extra
        .keys()
        .find(|k| RESERVED_BODY_KEYS.contains(&k.as_str()))
    {
        return Err(format!(
            "ключ «{k}» в body_extra задаёт сервер (запрещены: {})",
            RESERVED_BODY_KEYS.join(", ")
        ));
    }
    let size = serde_json::to_vec(extra).map_or(usize::MAX, |v| v.len());
    if size > BODY_EXTRA_MAX_BYTES {
        return Err(format!(
            "body_extra — {size} байт, предел {BODY_EXTRA_MAX_BYTES} (4 КиБ)"
        ));
    }
    Ok(())
}

/// Имя поля предела ответа: явное или по хосту при `auto` (§6.2 п. 2).
pub fn limit_field(tf: TokenField, host: &str) -> &'static str {
    match tf {
        TokenField::MaxTokens => "max_tokens",
        TokenField::MaxCompletionTokens => "max_completion_tokens",
        TokenField::Auto if host.eq_ignore_ascii_case(OPENAI_HOST) => "max_completion_tokens",
        TokenField::Auto => "max_tokens",
    }
}

/// Тело запроса Chat Completions: `body_extra` верхним уровнем, поверх — поля сервера (`model`,
/// `messages`, предел, `stream: false`): даже непроверенный `body_extra` их не перекрывает.
pub fn request_body(req: &ModelRequest, base: &BaseUrl, messages: &[ChatMessage]) -> Value {
    let mut body: Map<String, Value> = req
        .body_extra
        .iter()
        .filter(|(k, _)| !RESERVED_BODY_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    body.insert("model".into(), Value::String(req.model.clone()));
    body.insert(
        "messages".into(),
        Value::Array(
            messages
                .iter()
                .map(|m| json!({"role": m.role.as_str(), "content": m.content}))
                .collect(),
        ),
    );
    body.insert(
        limit_field(req.token_field, &base.host).into(),
        json!(req.max_tokens),
    );
    body.insert("stream".into(), Value::Bool(false));
    Value::Object(body)
}

/// `retry-after`: секунды или HTTP-дата (RFC 9110) относительно `now`; иное — `None`.
pub fn parse_retry_after(v: &str, now: jiff::Timestamp) -> Option<u64> {
    let v = v.trim();
    if let Ok(n) = v.parse::<u64>() {
        return Some(n);
    }
    let at = jiff::fmt::rfc2822::DateTimeParser::new()
        .parse_timestamp(v)
        .ok()?;
    let secs = at.as_second().saturating_sub(now.as_second());
    Some(u64::try_from(secs).unwrap_or(0))
}

/// Признак `insufficient_quota` в теле ошибки (`error.code` или `error.type`).
fn insufficient_quota(v: &Value) -> bool {
    let e = &v["error"];
    ["code", "type"]
        .iter()
        .any(|k| e[k].as_str() == Some("insufficient_quota"))
}

/// Класс ответа HTTP не 2xx по таблице §6.2 п. 5 (тело уже прочитано).
fn status_class(status: u16, body: Option<&Value>, retry_after: Option<u64>) -> CallError {
    if (400..500).contains(&status) && body.is_some_and(insufficient_quota) {
        return CallError::http(CallClass::Quota, status);
    }
    let class = match status {
        300..=399 => CallClass::BadResponse,
        401 | 403 => CallClass::Auth,
        404 => CallClass::NotFound,
        400 | 413 | 422 => CallClass::BadRequest,
        402 => CallClass::Quota,
        429 if retry_after.is_some() => {
            return CallError {
                retry_after,
                ..CallError::http(CallClass::Rate, status)
            };
        }
        429 => CallClass::Quota,
        408 => CallClass::Unavailable,
        500..=599 => CallClass::Unavailable,
        _ => CallClass::BadResponse,
    };
    CallError::http(class, status)
}

/// Текст `content`: строка или массив частей (склеиваются текстовые).
fn content_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p["type"].as_str().is_none_or(|t| t == "text"))
            .filter_map(|p| p["text"].as_str().or_else(|| p.as_str()))
            .collect(),
        _ => String::new(),
    }
}

/// Оценка входа счётчиком выдачи: содержимое сообщений через перевод строки.
fn estimate_input(sent: &[ChatMessage]) -> u64 {
    let joined: Vec<&str> = sent.iter().map(|m| m.content.as_str()).collect();
    zam_store::count_tokens(&joined.join("\n")) as u64
}

/// Учёт ответа: `usage.prompt_tokens`/`completion_tokens`; недостающее — оценкой (выход —
/// ответ и рассуждение: оба оплачены).
fn usage_of(v: &Value, sent: &[ChatMessage], out_text: &str, reasoning: &str) -> Usage {
    let u = &v["usage"];
    let input = u["prompt_tokens"].as_u64();
    let output = u["completion_tokens"].as_u64();
    let count = |s: &str| {
        if s.is_empty() {
            0
        } else {
            zam_store::count_tokens(s) as u64
        }
    };
    Usage {
        input_tokens: input.unwrap_or_else(|| estimate_input(sent)),
        output_tokens: output.unwrap_or_else(|| count(out_text) + count(reasoning)),
        estimated: input.is_none() || output.is_none(),
    }
}

/// Разбор ответа HTTP по §6.2 пп. 2, 5: `status` и `retry-after` ответа, тело, отправленная
/// беседа (оценка входа без `usage`).
pub fn parse_response(
    status: u16,
    retry_after: Option<&str>,
    body: &[u8],
    sent: &[ChatMessage],
) -> Result<ModelReply, CallError> {
    let json: Option<Value> = serde_json::from_slice(body).ok();
    if !(200..300).contains(&status) {
        let ra = retry_after.and_then(|v| parse_retry_after(v, zam_store::time::now()));
        return Err(status_class(status, json.as_ref(), ra));
    }
    let bad = || CallError::http(CallClass::BadResponse, status);
    let Some(v) = json else {
        return Err(bad());
    };
    let Some(choice) = v["choices"].get(0) else {
        if insufficient_quota(&v) {
            return Err(CallError::http(CallClass::Quota, status));
        }
        return Err(bad());
    };
    let msg = &choice["message"];
    let text = content_text(&msg["content"]);
    let reasoning = msg["reasoning_content"]
        .as_str()
        .or_else(|| msg["reasoning"].as_str())
        .unwrap_or("");
    let has_reasoning = !reasoning.trim().is_empty();
    let usage = usage_of(&v, sent, &text, reasoning);
    let fail = |class: CallClass| CallError {
        usage: Some(usage),
        ..CallError::http(class, status)
    };
    let refusal = msg["refusal"]
        .as_str()
        .is_some_and(|r| !r.trim().is_empty());
    let empty = text.trim().is_empty();
    match choice["finish_reason"].as_str() {
        Some("content_filter") => return Err(fail(CallClass::Refused)),
        _ if refusal => return Err(fail(CallClass::Refused)),
        Some("length") if empty && has_reasoning => return Err(fail(CallClass::Thinking)),
        Some("length") => return Err(fail(CallClass::MaxTokens)),
        Some("tool_calls" | "function_call") => return Err(fail(CallClass::BadResponse)),
        _ if empty && has_reasoning => return Err(fail(CallClass::Thinking)),
        _ if empty => return Err(fail(CallClass::BadResponse)),
        _ => {}
    }
    Ok(ModelReply {
        text,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        estimated: usage.estimated,
        reasoning: has_reasoning,
    })
}

/// Класс ошибки транспорта `ureq` (ответа HTTP нет).
fn transport_class(e: &ureq::Error) -> CallClass {
    match e {
        ureq::Error::Timeout(_)
        | ureq::Error::Io(_)
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::ConnectProxyFailed(_)
        | ureq::Error::Tls(_)
        | ureq::Error::Rustls(_) => CallClass::Unavailable,
        _ => CallClass::BadResponse,
    }
}

/// Вызов с системным и пользовательским сообщением (§6.2 п. 2) — [`call_chat_blocking`].
pub fn call_blocking(
    req: &ModelRequest,
    route: &Route,
    key: Option<&str>,
    system: &str,
    user: &str,
    timeout: Duration,
) -> Result<ModelReply, CallError> {
    call_chat_blocking(
        req,
        route,
        key,
        &[ChatMessage::system(system), ChatMessage::user(user)],
        timeout,
    )
}

/// Один вызов Chat Completions с беседой `messages` (повтор JSON — с ответом и просьбой
/// исправить, [`super::exec::json_step`]). Блокирующий: зовётся из `spawn_blocking`; срок вызова
/// — `timeout`, соединения — не дольше [`CONNECT_TIMEOUT`]. `auth = "key"` без ключа — `vault`.
pub fn call_chat_blocking(
    req: &ModelRequest,
    route: &Route,
    key: Option<&str>,
    messages: &[ChatMessage],
    timeout: Duration,
) -> Result<ModelReply, CallError> {
    let auth = match (req.auth, key) {
        (ModelAuth::None, _) => None,
        (ModelAuth::Key, Some(k)) => {
            let mut v = ureq::http::HeaderValue::try_from(format!("Bearer {k}"))
                .map_err(|_| CallError::new(CallClass::Vault))?;
            v.set_sensitive(true);
            Some(v)
        }
        (ModelAuth::Key, None) => return Err(CallError::new(CallClass::Vault)),
    };
    let body = serde_json::to_vec(&request_body(req, &route.base, messages))
        .map_err(|_| CallError::new(CallClass::BadRequest))?;
    let mut cfg = ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .timeout_connect(Some(timeout.min(CONNECT_TIMEOUT)))
        .user_agent(concat!("zam-server/", env!("CARGO_PKG_VERSION")));
    if !route.external {
        cfg = cfg.proxy(None);
    }
    let agent = ureq::Agent::with_parts(
        cfg.build(),
        ureq::unversioned::transport::DefaultConnector::default(),
        PinnedResolver::new(route),
    );
    let mut rb = agent
        .post(endpoint(&route.base, "/chat/completions"))
        .header("content-type", "application/json")
        .header("accept", "application/json");
    if let Some(v) = auth {
        rb = rb.header("authorization", v);
    }
    let resp = rb
        .send(&body[..])
        .map_err(|e| CallError::new(transport_class(&e)))?;
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = resp
        .into_body()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_to_vec()
        .map_err(|e| {
            let class = match e {
                ureq::Error::BodyExceedsLimit(_) => CallClass::BadResponse,
                e => transport_class(&e),
            };
            CallError {
                http_status: Some(status),
                ..CallError::new(class)
            }
        })?;
    parse_response(status, retry_after.as_deref(), &bytes, messages)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(host: &str) -> BaseUrl {
        BaseUrl {
            scheme: "https".into(),
            host: host.into(),
            port: 443,
            path: "/v1".into(),
        }
    }

    fn req() -> ModelRequest {
        ModelRequest {
            model: "m".into(),
            auth: ModelAuth::Key,
            max_tokens: 4000,
            token_field: TokenField::Auto,
            body_extra: Map::new(),
        }
    }

    fn sent() -> Vec<ChatMessage> {
        vec![ChatMessage::system("с"), ChatMessage::user("п")]
    }

    fn ok(body: Value) -> Result<ModelReply, CallError> {
        parse_response(200, None, body.to_string().as_bytes(), &sent())
    }

    /// Семнадцать классов таблицы §6.2 п. 5: имена уникальны, `parse` — обратное `as_str`.
    #[test]
    fn call_class_names_roundtrip() {
        let mut names: Vec<&str> = ALL_CLASSES.iter().map(|c| c.as_str()).collect();
        for c in ALL_CLASSES {
            assert_eq!(CallClass::parse(c.as_str()), Some(c));
        }
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 17);
        assert_eq!(CallClass::parse("ok"), None);
        assert!(CallClass::Thinking.hint().unwrap().contains("body_extra"));
        assert!(CallClass::Limit.hint().is_none());
    }

    /// Поле предела: `auto` — по хосту (регистр не важен), явный выбор — как есть.
    #[test]
    fn request_body_limit_field_by_host() {
        let b = request_body(&req(), &base("API.OpenAI.com"), &sent());
        assert_eq!(b["max_completion_tokens"], 4000);
        assert!(b.get("max_tokens").is_none());
        let b = request_body(&req(), &base("api.deepseek.com"), &sent());
        assert_eq!(b["max_tokens"], 4000);
        let b = request_body(&req(), &base("openai.example.com"), &sent());
        assert_eq!(b["max_tokens"], 4000);
        let mut r = req();
        r.token_field = TokenField::MaxCompletionTokens;
        let b = request_body(&r, &base("localhost"), &sent());
        assert_eq!(b["max_completion_tokens"], 4000);
        assert_eq!(b["stream"], false);
        assert_eq!(b["messages"][0], json!({"role": "system", "content": "с"}));
        assert_eq!(b["messages"][1], json!({"role": "user", "content": "п"}));
    }

    /// `body_extra`: ключи сервера и размер; граница 4 КиБ включительно.
    #[test]
    fn check_body_extra_bounds() {
        let mut m = Map::new();
        // {"x":"…"} — 8 байт обвязки
        m.insert(
            "x".into(),
            Value::String("a".repeat(BODY_EXTRA_MAX_BYTES - 8)),
        );
        assert_eq!(serde_json::to_vec(&m).unwrap().len(), BODY_EXTRA_MAX_BYTES);
        assert!(check_body_extra(&m).is_ok());
        m.insert(
            "x".into(),
            Value::String("a".repeat(BODY_EXTRA_MAX_BYTES - 7)),
        );
        assert!(check_body_extra(&m).is_err());
        let mut m = Map::new();
        m.insert("max_completion_tokens".into(), json!(5));
        assert!(
            check_body_extra(&m)
                .unwrap_err()
                .contains("max_completion_tokens")
        );
    }

    /// Разбор ответа: части `content`, отказ, `tool_calls`, пустой ответ, `finish_reason` без
    /// значения у локальных серверов.
    #[test]
    fn parse_response_shapes() {
        let r = ok(json!({"choices": [{"message": {"content": [
            {"type": "text", "text": "{\"a\":"}, {"type": "image_url", "image_url": {}},
            {"type": "text", "text": "1}"}]}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 2}}))
        .unwrap();
        assert_eq!(r.text, "{\"a\":1}");
        assert!(!r.estimated && !r.reasoning);
        // finish_reason нет (часть локальных серверов) — ответ принимается
        let r = ok(json!({"choices": [{"message": {"content": "ок"}}],
                          "usage": {"prompt_tokens": 1, "completion_tokens": 1}}))
        .unwrap();
        assert_eq!(r.text, "ок");
        let class = |v: Value| ok(v).unwrap_err().class;
        assert_eq!(
            class(
                json!({"choices": [{"message": {"content": "x"}, "finish_reason": "content_filter"}]})
            ),
            CallClass::Refused
        );
        assert_eq!(
            class(
                json!({"choices": [{"message": {"content": null, "refusal": "не могу"},
                                      "finish_reason": "stop"}]})
            ),
            CallClass::Refused
        );
        assert_eq!(
            class(
                json!({"choices": [{"message": {"content": null, "tool_calls": []},
                                      "finish_reason": "tool_calls"}]})
            ),
            CallClass::BadResponse
        );
        assert_eq!(
            class(json!({"choices": [{"message": {"content": "  "}, "finish_reason": "stop"}]})),
            CallClass::BadResponse
        );
        assert_eq!(
            class(
                json!({"choices": [{"message": {"content": null, "reasoning_content": "мысль"},
                                      "finish_reason": "stop"}]})
            ),
            CallClass::Thinking
        );
        assert_eq!(class(json!({"choices": []})), CallClass::BadResponse);
        assert_eq!(
            class(json!({"error": {"code": "insufficient_quota"}})),
            CallClass::Quota
        );
        // 1xx/иные коды — иной ответ; 408 — недоступен
        let e = parse_response(418, None, b"{}", &sent()).unwrap_err();
        assert_eq!(
            (e.class, e.http_status),
            (CallClass::BadResponse, Some(418))
        );
        let e = parse_response(408, None, b"", &sent()).unwrap_err();
        assert_eq!(e.class, CallClass::Unavailable);
        // 429 с непонятным retry-after — как без него
        let e = parse_response(429, Some("скоро"), b"", &sent()).unwrap_err();
        assert_eq!((e.class, e.retry_after), (CallClass::Quota, None));
    }

    /// `retry-after` секундами и HTTP-датой; дата в прошлом — 0.
    #[test]
    fn retry_after_forms() {
        let now: jiff::Timestamp = "2026-11-23T10:00:00Z".parse().unwrap();
        assert_eq!(parse_retry_after("30", now), Some(30));
        assert_eq!(parse_retry_after(" 0 ", now), Some(0));
        assert_eq!(
            parse_retry_after("Mon, 23 Nov 2026 10:00:45 GMT", now),
            Some(45)
        );
        assert_eq!(
            parse_retry_after("Mon, 23 Nov 2026 09:59:00 GMT", now),
            Some(0)
        );
        assert_eq!(parse_retry_after("-5", now), None);
        assert_eq!(parse_retry_after("потом", now), None);
    }

    /// Учёт без `usage` — оценка, с рассуждением в выходе.
    #[test]
    fn usage_estimate_includes_reasoning() {
        let r = ok(
            json!({"choices": [{"message": {"content": "ответ", "reasoning_content": "рассуждение"},
                                       "finish_reason": "stop"}]}),
        )
        .unwrap();
        assert!(r.estimated && r.reasoning);
        assert_eq!(
            r.output_tokens,
            (zam_store::count_tokens("ответ") + zam_store::count_tokens("рассуждение")) as u64
        );
        assert_eq!(r.input_tokens, zam_store::count_tokens("с\nп") as u64);
    }
}
