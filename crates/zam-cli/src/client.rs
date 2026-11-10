//! HTTP-клиент REST API (ureq, без tokio): заголовки атрибуции, разбор problem+json
//! (RFC 9457) в [`CliError`] с кодом выхода. Токен нигде не печатается.
use std::fmt;
use std::io::Read;
use std::time::{Duration, Instant};

use serde_json::Value;
use ureq::http::Response;
use ureq::{Body, RequestBuilder};

/// Коды выхода `zam` (стабильны: на них опираются агенты).
pub const EXIT_CONFIG: u8 = 1;
pub const EXIT_UNREACHABLE: u8 = 2;
pub const EXIT_TOKEN: u8 = 3;
pub const EXIT_CONFLICT: u8 = 4;
pub const EXIT_NOT_FOUND: u8 = 5;
pub const EXIT_INPUT: u8 = 6;

/// Предел чтения тела ответа в память (JSON заметки до 5 МБ с экранированием, дифф).
const BODY_MAX: u64 = 64 * 1024 * 1024;

/// Ошибка команды: текст для stderr и код выхода; `problem` — тело problem+json сервера.
#[derive(Debug)]
pub struct CliError {
    pub code: u8,
    pub msg: String,
    pub problem: Option<Value>,
    /// Сетевая ошибка — истёк срок запроса, ответа сервера нет ([`CliError::is_timeout`]).
    /// Поле закрыто: признак ставит только [`net_error`], прочие ошибки строятся через
    /// [`CliError::new`] и [`CliError::with_problem`] — без него.
    timeout: bool,
    /// Запрос не начат: абсолютный срок клиента вышел до отправки ([`CliError::is_unsent`]).
    unsent: bool,
}

impl CliError {
    pub fn new(code: u8, msg: impl Into<String>) -> CliError {
        CliError::with_problem(code, msg, None)
    }

    /// Ошибка по ответу сервера: код выхода, текст и тело problem+json (`problem`).
    pub fn with_problem(code: u8, msg: impl Into<String>, problem: Option<Value>) -> CliError {
        CliError {
            code,
            msg: msg.into(),
            problem,
            timeout: false,
            unsent: false,
        }
    }

    /// Запрос не начат: абсолютный срок клиента ([`Timeouts::Deadline`]) вышел до отправки. Текст —
    /// [`zam_core::HOOK_BUDGET_SPENT`]: в `hooks.log` это та же причина, что и «клиента нет».
    fn unsent() -> CliError {
        CliError {
            unsent: true,
            ..CliError::new(EXIT_UNREACHABLE, zam_core::HOOK_BUDGET_SPENT)
        }
    }

    pub fn config(msg: impl Into<String>) -> CliError {
        CliError::new(EXIT_CONFIG, msg)
    }

    pub fn input(msg: impl Into<String>) -> CliError {
        CliError::new(EXIT_INPUT, msg)
    }

    /// HTTP-статус ответа сервера с ошибкой (`problem.status`); сеть, разбор — `None`.
    pub fn http_status(&self) -> Option<u16> {
        self.problem
            .as_ref()?
            .get("status")?
            .as_u64()
            .and_then(|s| u16::try_from(s).ok())
    }

    /// Запрос оборван сроком клиента (разрешение имени, соединение, ожидание ответа): сервер не
    /// ответил вовремя. Отказ соединения, обрыв и ответ сервера с ошибкой — не срок.
    pub fn is_timeout(&self) -> bool {
        self.timeout
    }

    /// Запрос не начат — абсолютный срок клиента вышел до отправки ([`Timeouts::Deadline`]):
    /// сервер запроса не видел. Это не [`CliError::is_timeout`] (там сервер не ответил вовремя).
    pub fn is_unsent(&self) -> bool {
        self.unsent
    }

    /// Уточнение: «<ctx>: <текст>».
    pub fn context(mut self, ctx: impl fmt::Display) -> CliError {
        self.msg = format!("{ctx}: {}", self.msg);
        self
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

/// Хост из URL без схемы, userinfo и порта (IPv6 без скобок).
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
    if let Some(v6) = auth.strip_prefix('[') {
        return v6.split(']').next().unwrap_or("");
    }
    auth.split(':').next().unwrap_or("")
}

/// К loopback системный прокси не применяется; для остальных хостов работают
/// ALL_PROXY/HTTPS_PROXY/HTTP_PROXY/NO_PROXY (по умолчанию ureq).
fn bypass_proxy(url: &str) -> bool {
    matches!(host_of(url), "127.0.0.1" | "localhost" | "::1")
}

/// Профиль таймаутов агента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timeouts {
    /// `zam status`: быстрый ответ о доступности (10 с на всё).
    Probe,
    /// Команды заметок: тело (вложение до 200 МБ) может идти долго, общего предела нет.
    Transfer,
    /// Хуки агента (§10.3): абсолютный срок — момент, к которому кончаются все запросы клиента
    /// (старт хука + бюджет − резерв). Срок каждого запроса — остаток до этого момента в момент
    /// отправки (весь запрос, включая чтение ответа): последовательные запросы и параллельные
    /// потоки одного клиента делят один срок; без остатка запрос не начинается
    /// ([`CliError::is_unsent`]).
    Deadline(Instant),
    /// `zam mcp`: как `Transfer`, но ответ ждётся до [`MCP_RECV_RESPONSE`] — сервер (`json_response`)
    /// отдаёт заголовки только после конца инструмента.
    Mcp,
    /// `zam export`: заголовки — после снимка пространства (до [`MCP_RECV_RESPONSE`]), тело —
    /// архив любого размера потоком, без общего предела на его приём.
    Download,
}

/// Ожидание ответа `POST /mcp` (ревью задачи 3, m1). Сервер синхронно ждёт инструмент, а
/// своего предела на запрос у него нет; самые долгие пути — разбор вложения рабочим процессом
/// (`docs.timeout_ms`, 60 с по умолчанию, в очереди из `docs.workers` = 2), реранк на CPU и
/// карта кода. 10 мин покрывают несколько таких сроков подряд при умолчаниях; дольше — пусть
/// решает тайм-аут инструмента самого агента.
pub const MCP_RECV_RESPONSE: Duration = Duration::from_secs(10 * 60);
/// Ожидание ответа на `Expect: 100-continue` при импорте (по умолчанию ureq — 1 с: под нагрузкой
/// тело уходило до ответа сервера).
pub const AWAIT_100: Duration = Duration::from_secs(30);

/// Остаток до абсолютного срока, с которым запрос ещё начинается ([`Timeouts::Deadline`]):
/// меньше — ответа он не дождётся, только выйдет за срок на шаг таймера ОС.
pub const DEADLINE_MIN: Duration = Duration::from_millis(1);

/// Абсолютный срок профиля ([`Timeouts::Deadline`]); у прочих профилей его нет.
fn deadline_of(t: Timeouts) -> Option<Instant> {
    match t {
        Timeouts::Deadline(at) => Some(at),
        _ => None,
    }
}

pub fn agent(url: &str, t: Timeouts) -> ureq::Agent {
    let mut b = ureq::Agent::config_builder()
        .timeout_resolve(Some(Duration::from_secs(2)))
        .timeout_connect(Some(Duration::from_secs(2)))
        .http_status_as_error(false);
    b = match t {
        Timeouts::Probe => b
            .timeout_global(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(5)))
            .timeout_recv_body(Some(Duration::from_secs(5))),
        Timeouts::Transfer => b
            .timeout_send_request(Some(Duration::from_secs(30)))
            .timeout_send_body(Some(Duration::from_secs(30 * 60)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .timeout_recv_body(Some(Duration::from_secs(5 * 60))),
        Timeouts::Mcp => b
            .timeout_send_request(Some(Duration::from_secs(30)))
            .timeout_send_body(Some(Duration::from_secs(30 * 60)))
            .timeout_recv_response(Some(MCP_RECV_RESPONSE))
            .timeout_recv_body(Some(Duration::from_secs(5 * 60))),
        Timeouts::Download => b
            .timeout_send_request(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(MCP_RECV_RESPONSE))
            .timeout_recv_body(None),
        // срок — у каждого запроса свой, по остатку до момента ([`Client::timed`]); у агента —
        // тот же остаток на случай запроса мимо клиента
        Timeouts::Deadline(at) => {
            let d = at.saturating_duration_since(Instant::now());
            b.timeout_resolve(Some(d))
                .timeout_connect(Some(d))
                .timeout_global(Some(d))
        }
    };
    if bypass_proxy(url) {
        b = b.proxy(None);
    }
    ureq::Agent::new_with_config(b.build())
}

/// Имя машины для `X-Zam-Machine` (финальное ревью M4, M-4): [`zam_core::machine_name`]
/// (`ZAM_MACHINE`, `COMPUTERNAME`, `HOSTNAME`), на Linux без них — `/etc/hostname` (файл, не
/// внешний вызов). Тот же порядок, что у `.mcp.json` (`${ZAM_MACHINE:-}`), когда `ZAM_MACHINE`
/// задана, — команда установки `zam` её задаёт.
pub fn machine_from(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    zam_core::machine_name(get).or_else(os_hostname)
}

/// Имя машины из ОС без переменных окружения: на Linux — `/etc/hostname`.
#[cfg(unix)]
fn os_hostname() -> Option<String> {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .and_then(|s| s.lines().next().map(str::to_string))
        .and_then(zam_core::valid_machine)
}

#[cfg(not(unix))]
fn os_hostname() -> Option<String> {
    None
}

/// Клиент API одного сервера.
pub struct Client {
    agent: ureq::Agent,
    base: String,
    token: String,
    machine: Option<String>,
    solution: Option<String>,
    session: Option<String>,
    /// Абсолютный срок запросов ([`Timeouts::Deadline`]); прочие профили — `None`.
    deadline: Option<Instant>,
}

/// В Debug токен — только маска.
impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .field("token", &zam_core::mask_token(&self.token))
            .field("machine", &self.machine)
            .field("solution", &self.solution)
            .field("session", &self.session)
            .finish()
    }
}

impl Client {
    /// `base` — без замыкающего `/`; `solution` — GUID для `X-Zam-Solution`.
    pub fn new(base: &str, token: &str, solution: Option<String>) -> Client {
        Client::with_timeouts(base, token, solution, Timeouts::Transfer)
    }

    /// Клиент с профилем таймаутов (`Deadline` — хуки агента).
    pub fn with_timeouts(base: &str, token: &str, solution: Option<String>, t: Timeouts) -> Client {
        Client {
            agent: agent(base, t),
            base: base.to_string(),
            token: token.to_string(),
            machine: machine_from(|k| std::env::var(k).ok()),
            solution,
            session: None,
            deadline: deadline_of(t),
        }
    }

    /// Сменить профиль таймаутов (`zam mcp` — [`Timeouts::Mcp`]).
    pub fn set_timeouts(&mut self, t: Timeouts) {
        self.agent = agent(&self.base, t);
        self.deadline = deadline_of(t);
    }

    /// Остаток до абсолютного срока клиента ([`Timeouts::Deadline`]): `None` — срока нет (прочие
    /// профили); ноль — срок вышел, запрос не начнётся.
    pub fn deadline_left(&self) -> Option<Duration> {
        self.deadline
            .map(|at| at.saturating_duration_since(Instant::now()))
    }

    /// Срок запроса по абсолютному сроку клиента: остаток в момент отправки — на разрешение
    /// имени, соединение и весь запрос с чтением ответа. Остатка нет (меньше [`DEADLINE_MIN`]) —
    /// запрос не начинается: ошибка [`CliError::is_unsent`]. Профили без срока — без изменений.
    fn timed<B>(&self, rb: RequestBuilder<B>) -> Result<RequestBuilder<B>, CliError> {
        let Some(left) = self.deadline_left() else {
            return Ok(rb);
        };
        if left < DEADLINE_MIN {
            return Err(CliError::unsent());
        }
        Ok(rb
            .config()
            .timeout_resolve(Some(left))
            .timeout_connect(Some(left))
            .timeout_global(Some(left))
            .build())
    }

    /// Сессия агента для `X-Zam-Session` (запись в память учитывается в её `memory_writes`).
    pub fn set_session(&mut self, id: Option<String>) {
        self.session = id;
    }

    /// Адрес сервера (без `/api/v1`).
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Имя машины (`X-Zam-Machine`), если известно.
    pub fn machine(&self) -> Option<&str> {
        self.machine.as_deref()
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.base)
    }

    /// Заголовки атрибуции и срок запроса ([`Client::timed`]).
    fn prepare<B>(&self, rb: RequestBuilder<B>) -> Result<RequestBuilder<B>, CliError> {
        let mut rb = self.timed(rb)?;
        rb = rb.header("Authorization", format!("Bearer {}", self.token));
        if let Some(m) = &self.machine {
            rb = rb.header("X-Zam-Machine", m.as_str());
        }
        if let Some(s) = &self.solution {
            rb = rb.header("X-Zam-Solution", s.as_str());
        }
        if let Some(s) = &self.session {
            rb = rb.header("X-Zam-Session", s.as_str());
        }
        Ok(rb)
    }

    /// GET `/api/v1{path}` с параметрами запроса (кодируются ureq).
    pub fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Response<Body>, CliError> {
        let mut rb = self.prepare(self.agent.get(&self.url(path)))?;
        for (k, v) in query {
            rb = rb.query(*k, *v);
        }
        self.check(rb.call())
    }

    /// GET с дополнительными заголовками (`If-None-Match`): 304 — тоже ответ, а не ошибка.
    pub fn get_with(
        &self,
        path: &str,
        query: &[(&str, &str)],
        headers: &[(&str, &str)],
    ) -> Result<Response<Body>, CliError> {
        let mut rb = self.prepare(self.agent.get(&self.url(path)))?;
        for (k, v) in query {
            rb = rb.query(*k, *v);
        }
        for (k, v) in headers {
            rb = rb.header(*k, *v);
        }
        self.check(rb.call())
    }

    pub fn post_json(&self, path: &str, body: &Value) -> Result<Response<Body>, CliError> {
        let rb = self.prepare(self.agent.post(&self.url(path)))?;
        self.check(
            rb.content_type("application/json")
                .send(body.to_string().as_bytes()),
        )
    }

    /// POST JSON с дополнительными заголовками (`Idempotency-Key`, `X-Zam-Area`).
    pub fn post_json_with(
        &self,
        path: &str,
        body: &Value,
        headers: &[(&str, &str)],
    ) -> Result<Response<Body>, CliError> {
        let mut rb = self.prepare(self.agent.post(&self.url(path)))?;
        for (k, v) in headers {
            rb = rb.header(*k, *v);
        }
        self.check(
            rb.content_type("application/json")
                .send(body.to_string().as_bytes()),
        )
    }

    /// DELETE `/api/v1{path}`.
    pub fn delete(&self, path: &str) -> Result<Response<Body>, CliError> {
        self.check(self.prepare(self.agent.delete(&self.url(path)))?.call())
    }

    /// PATCH JSON без `If-Match`.
    pub fn patch_json(&self, path: &str, body: &Value) -> Result<Response<Body>, CliError> {
        let rb = self.prepare(self.agent.patch(&self.url(path)))?;
        self.check(
            rb.content_type("application/json")
                .send(body.to_string().as_bytes()),
        )
    }

    /// PATCH JSON с `If-Match: "<rev>"`.
    pub fn patch_json_if(
        &self,
        path: &str,
        body: &Value,
        if_match: i64,
    ) -> Result<Response<Body>, CliError> {
        let rb = self
            .prepare(self.agent.patch(&self.url(path)))?
            .header("If-Match", format!("\"{if_match}\""));
        self.check(
            rb.content_type("application/json")
                .send(body.to_string().as_bytes()),
        )
    }

    /// PUT с `If-Match: "<rev>"`.
    pub fn put_json(
        &self,
        path: &str,
        body: &Value,
        if_match: i64,
    ) -> Result<Response<Body>, CliError> {
        let rb = self
            .prepare(self.agent.put(&self.url(path)))?
            .header("If-Match", format!("\"{if_match}\""));
        self.check(
            rb.content_type("application/json")
                .send(body.to_string().as_bytes()),
        )
    }

    /// PUT JSON без `If-Match` (готовое тело: размер пачки считает вызывающий).
    pub fn put_json_body(&self, path: &str, body: Vec<u8>) -> Result<Response<Body>, CliError> {
        let rb = self.prepare(self.agent.put(&self.url(path)))?;
        self.check(rb.content_type("application/json").send(&body[..]))
    }

    /// POST тела потоком известной длины (`Content-Length`, без chunked).
    pub fn send_multipart(
        &self,
        path: &str,
        content_type: &str,
        len: u64,
        reader: impl Read + 'static,
    ) -> Result<Response<Body>, CliError> {
        self.send_multipart_with(path, content_type, len, reader, &[])
    }

    /// POST тела импорта: `.zip` — `len` (`Content-Length`), каталог — потоком (chunked, архив
    /// собирается на лету) с оценкой объёма `expected` (`X-Zam-Expected-Bytes`: резерв места и
    /// предел тела на сервере). `Expect: 100-continue`: ureq читает ответ сервера только после
    /// всего тела, а так отказ до приёма (413, 503, 507, 404 старого сервера) приходит до тела —
    /// тело не уходит, и ответ не теряется в обрыве записи. Ответ ждётся до [`AWAIT_100`];
    /// сервер (прокси) молчит дольше — тело уходит как обычно. `Connection: close`: ureq 3.4.2
    /// дочитывает ранний ответ на `Expect` только до закрытия соединения сервером (после
    /// Await100 ждёт новых данных), а прокси с keep-alive, ответивший до тела, держал бы CLI до
    /// срока ответа (10 мин); с `Connection: close` сервер обязан закрыть соединение после
    /// ответа (RFC 9112 §9.6).
    pub fn send_import(
        &self,
        path: &str,
        content_type: &str,
        len: Option<u64>,
        expected: Option<u64>,
        reader: impl Read + 'static,
    ) -> Result<Response<Body>, CliError> {
        let mut rb = self
            .prepare(self.agent.post(&self.url(path)))?
            .header("Content-Type", content_type)
            .header("Expect", "100-continue")
            .header("Connection", "close");
        if let Some(n) = len {
            rb = rb.header("Content-Length", n.to_string());
        }
        if let Some(n) = expected {
            rb = rb.header("X-Zam-Expected-Bytes", n.to_string());
        }
        let rb = rb.config().timeout_await_100(Some(AWAIT_100)).build();
        self.check(rb.send(ureq::SendBody::from_owned_reader(reader)))
    }

    /// POST тела потоком с дополнительными заголовками (`Idempotency-Key`).
    pub fn send_multipart_with(
        &self,
        path: &str,
        content_type: &str,
        len: u64,
        reader: impl Read + 'static,
        headers: &[(&str, &str)],
    ) -> Result<Response<Body>, CliError> {
        let mut rb = self
            .prepare(self.agent.post(&self.url(path)))?
            .header("Content-Type", content_type)
            .header("Content-Length", len.to_string());
        for (k, v) in headers {
            rb = rb.header(*k, *v);
        }
        self.check(rb.send(ureq::SendBody::from_owned_reader(reader)))
    }

    /// POST сообщения JSON-RPC на `{base}/mcp` (stdio-прокси `zam mcp`, §13.2): ответ любого
    /// статуса — разбирает вызывающий; ошибка — только сетевая.
    pub fn post_mcp(
        &self,
        body: &[u8],
        headers: &[(&str, &str)],
    ) -> Result<Response<Body>, CliError> {
        let mut rb = self
            .prepare(self.agent.post(&format!("{}/mcp", self.base)))?
            .header("Accept", "application/json, text/event-stream");
        for (k, v) in headers {
            rb = rb.header(*k, *v);
        }
        rb.content_type("application/json")
            .send(body)
            .map_err(|e| net_error(&self.base, e))
    }

    /// 2xx и 304 (ответ на `If-None-Match`) → ответ; иначе ошибка с кодом выхода по статусу.
    fn check(&self, r: Result<Response<Body>, ureq::Error>) -> Result<Response<Body>, CliError> {
        let resp = r.map_err(|e| net_error(&self.base, e))?;
        if resp.status().is_success() || resp.status().as_u16() == 304 {
            return Ok(resp);
        }
        Err(problem(resp))
    }
}

/// Ответ с ошибкой → [`CliError`]: текст из `detail` problem+json.
fn problem(mut resp: Response<Body>) -> CliError {
    let status = resp.status().as_u16();
    // занято, слишком велико, мало места: когда повторить — из `Retry-After`
    let retry = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .map(|v| format!("; повторите через {} с", v.trim()))
        .unwrap_or_default();
    let text = resp
        .body_mut()
        .with_config()
        .limit(BODY_MAX)
        .read_to_string()
        .unwrap_or_default();
    let mut v: Option<Value> = serde_json::from_str(&text).ok();
    let detail = v
        .as_ref()
        .and_then(|v| v.get("detail"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| text.chars().take(200).collect());
    let code = match status {
        401 | 403 => EXIT_TOKEN,
        404 => EXIT_NOT_FOUND,
        409 | 412 | 428 => EXIT_CONFLICT,
        400 | 413 | 415 | 422 => EXIT_INPUT,
        _ => EXIT_UNREACHABLE,
    };
    let msg = match status {
        401 => "токен отклонён сервером".to_string(),
        _ => format!("{detail} (HTTP {status}{retry})"),
    };
    // статус — всегда в `problem` ([`CliError::http_status`]): у тела не problem+json тоже
    match v.as_mut() {
        Some(Value::Object(o)) => {
            o.entry("status").or_insert(Value::from(status));
        }
        _ => v = Some(serde_json::json!({ "status": status })),
    }
    CliError::with_problem(code, msg, v)
}

/// Сетевая ошибка запроса к серверу `base` (ответа нет): «сервер … недоступен» с кодом
/// [`EXIT_UNREACHABLE`]. Срок клиента — по типу ошибки, а не по её тексту ([`timed_out`]) —
/// [`CliError::is_timeout`].
fn net_error(base: &str, e: ureq::Error) -> CliError {
    CliError {
        code: EXIT_UNREACHABLE,
        msg: format!("сервер {base} недоступен: {e}"),
        problem: None,
        timeout: timed_out(&e),
        unsent: false,
    }
}

/// Ошибка клиента — истёкший срок: `Timeout` ureq или ввод-вывод сокета с `TimedOut` либо
/// `WouldBlock`. Срок чтения и записи сокета (`SO_RCVTIMEO`/`SO_SNDTIMEO`) Windows отдаёт как
/// `TimedOut`, unix — как `WouldBlock` (`EAGAIN`); сокеты клиента блокирующие, поэтому
/// `WouldBlock` у них бывает только по сроку. ureq сам сводит его к `Timeout`, но не на каждом
/// пути (ошибка ввода-вывода из чтения тела приходит как `Io`).
fn timed_out(e: &ureq::Error) -> bool {
    use std::io::ErrorKind;
    match e {
        ureq::Error::Timeout(_) => true,
        ureq::Error::Io(io) => matches!(io.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock),
        _ => false,
    }
}

/// Тело ответа как JSON.
pub fn read_json(mut resp: Response<Body>) -> Result<Value, CliError> {
    let text = read_text_mut(&mut resp)?;
    serde_json::from_str(&text)
        .map_err(|e| CliError::new(EXIT_UNREACHABLE, format!("ответ сервера не JSON: {e}")))
}

/// Тело ответа как текст UTF-8.
pub fn read_text(mut resp: Response<Body>) -> Result<String, CliError> {
    read_text_mut(&mut resp)
}

fn read_text_mut(resp: &mut Response<Body>) -> Result<String, CliError> {
    resp.body_mut()
        .with_config()
        .limit(BODY_MAX)
        .read_to_string()
        .map_err(|e| CliError::new(EXIT_UNREACHABLE, format!("чтение ответа: {e}")))
}

/// Тело ответа потоком (без предела: заметка до 5 МБ, сервер свой).
pub fn into_reader(resp: Response<Body>) -> impl Read {
    resp.into_body().into_reader()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// m1: `zam mcp` ждёт ответа инструмента до 10 мин, команды заметок — по-прежнему 60 с.
    #[test]
    fn mcp_profile_waits_long_for_response() {
        let t = |p| agent("http://127.0.0.1:1", p).config().timeouts();
        assert_eq!(t(Timeouts::Mcp).recv_response, Some(MCP_RECV_RESPONSE));
        assert!(MCP_RECV_RESPONSE >= Duration::from_secs(10 * 60));
        assert_eq!(
            t(Timeouts::Transfer).recv_response,
            Some(Duration::from_secs(60))
        );
        let mut c = Client::new("http://127.0.0.1:1", "t", None);
        c.set_timeouts(Timeouts::Mcp);
        assert_eq!(
            c.agent.config().timeouts().recv_response,
            Some(MCP_RECV_RESPONSE)
        );
    }

    /// Повторное ревью 76458e3 (17b), I1: срок клиента — типизированный признак ошибки.
    /// Сервер принял соединение и молчит — `is_timeout()`; отказ соединения — нет.
    #[test]
    fn timeout_is_typed_not_parsed_from_text() {
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", silent.local_addr().unwrap());
        let c = Client::with_timeouts(
            &url,
            "t",
            None,
            Timeouts::Deadline(Instant::now() + Duration::from_millis(50)),
        );
        let e = c.put_json_body("/x", b"{}".to_vec()).unwrap_err();
        assert!(e.is_timeout(), "{e}");
        assert_eq!(e.code, EXIT_UNREACHABLE);
        assert_eq!(e.http_status(), None);
        drop(silent);

        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let c = Client::with_timeouts(
            &url,
            "t",
            None,
            Timeouts::Deadline(Instant::now() + Duration::from_secs(30)),
        );
        let e = c.put_json_body("/x", b"{}".to_vec()).unwrap_err();
        assert!(!e.is_timeout(), "{e}");
        assert_eq!(e.code, EXIT_UNREACHABLE);
    }

    /// Третье ревью 599c933 (17b), M2: срок сокета на unix приходит как `WouldBlock`; отказ и
    /// обрыв соединения — не срок.
    #[test]
    fn socket_timeout_kinds_are_timeouts() {
        use std::io::{Error, ErrorKind};
        let io = |k: ErrorKind| ureq::Error::Io(Error::from(k));
        assert!(timed_out(&io(ErrorKind::TimedOut)));
        assert!(timed_out(&io(ErrorKind::WouldBlock)));
        assert!(!timed_out(&io(ErrorKind::ConnectionRefused)));
        assert!(!timed_out(&io(ErrorKind::ConnectionReset)));
        assert!(net_error("http://x", io(ErrorKind::WouldBlock)).is_timeout());
        assert!(!net_error("http://x", io(ErrorKind::ConnectionReset)).is_timeout());
        assert!(!CliError::new(EXIT_UNREACHABLE, "timeout: global").is_timeout());
        assert!(!CliError::with_problem(EXIT_CONFLICT, "x", None).is_timeout());
    }

    /// Задача 17b-http: срок клиента хуков — абсолютный, один на все запросы. Сервер принял
    /// соединение и молчит: первый запрос оборван сроком, второй (тем же клиентом, срок вышел) не
    /// начинается вовсе — оба вместе идут один срок, а не два. Запас — возврат ожидания сокета
    /// (шаг таймера ОС) и занятая машина.
    #[test]
    fn deadline_is_absolute_and_shared_by_requests() {
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", silent.local_addr().unwrap());
        let ms = 150;
        let t0 = Instant::now();
        let c = Client::with_timeouts(
            &url,
            "t",
            None,
            Timeouts::Deadline(t0 + Duration::from_millis(ms)),
        );
        let e = c.post_json("/x", &serde_json::json!({})).unwrap_err();
        assert!(e.is_timeout() && !e.is_unsent(), "{e}");
        let first = t0.elapsed();
        let e = c.get("/x", &[]).unwrap_err();
        assert!(e.is_unsent() && !e.is_timeout(), "{e}");
        assert_eq!(e.code, EXIT_UNREACHABLE);
        assert_eq!(e.to_string(), zam_core::HOOK_BUDGET_SPENT);
        let both = t0.elapsed();
        assert!(first >= Duration::from_millis(ms - 20), "{first:?}");
        assert!(
            both < Duration::from_millis(2 * ms - 20),
            "два запроса — один срок {ms} мс, а не два: {both:?}"
        );
        assert_eq!(c.deadline_left(), Some(Duration::ZERO));
        // запрос, начатый позже, получает остаток, а не полный срок
        let t0 = Instant::now();
        let c = Client::with_timeouts(
            &url,
            "t",
            None,
            Timeouts::Deadline(t0 + Duration::from_millis(ms)),
        );
        std::thread::sleep(Duration::from_millis(100));
        let e = c.get("/x", &[]).unwrap_err();
        assert!(e.is_timeout(), "{e}");
        let took = t0.elapsed();
        assert!(
            took < Duration::from_millis(ms + 100 - 20),
            "срок от создания клиента, а не от начала запроса: {took:?}"
        );
        // профили без срока — без изменений
        assert_eq!(Client::new(&url, "t", None).deadline_left(), None);
    }

    #[test]
    fn loopback_hosts_bypass_proxy() {
        for u in [
            "http://127.0.0.1:7700",
            "http://localhost",
            "https://localhost:1/x",
            "http://[::1]:7700",
            "http://user:pw@127.0.0.1/",
        ] {
            assert!(bypass_proxy(u), "{u}");
        }
        for u in [
            "http://192.168.1.5:7700",
            "http://zam.lan",
            "http://localhost.evil.com",
            "http://127.0.0.10",
        ] {
            assert!(!bypass_proxy(u), "{u}");
        }
    }

    #[test]
    fn machine_name_prefers_env() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            machine_from(env(&[("ZAM_MACHINE", "m"), ("COMPUTERNAME", "WIN")])).as_deref(),
            Some("m")
        );
        assert_eq!(
            machine_from(env(&[("COMPUTERNAME", "WIN"), ("HOSTNAME", "lin")])).as_deref(),
            Some("WIN")
        );
        // без переменных — имя из ОС (Linux) или ничего (Windows без COMPUTERNAME)
        assert_eq!(machine_from(env(&[])), os_hostname());
    }

    #[test]
    fn debug_masks_token() {
        let t = "zam_abcdefghijklmnopqrstuvwxyz0123456789ABCD";
        let c = Client::new("http://127.0.0.1:1", t, None);
        let d = format!("{c:?}");
        assert!(!d.contains(t), "{d}");
        assert!(d.contains("zam_abcdefgh"), "{d}");
    }
}
