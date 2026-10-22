//! HTTP-клиент REST API (ureq, без tokio): заголовки атрибуции, разбор problem+json
//! (RFC 9457) в [`CliError`] с кодом выхода. Токен нигде не печатается.
use std::fmt;
use std::io::Read;
use std::time::Duration;

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
/// Предел длины `X-Zam-Machine`, символов (как на сервере).
const MACHINE_MAX_CHARS: usize = 128;

/// Ошибка команды: текст для stderr и код выхода; `problem` — тело problem+json сервера.
#[derive(Debug)]
pub struct CliError {
    pub code: u8,
    pub msg: String,
    pub problem: Option<Value>,
}

impl CliError {
    pub fn new(code: u8, msg: impl Into<String>) -> CliError {
        CliError {
            code,
            msg: msg.into(),
            problem: None,
        }
    }

    pub fn config(msg: impl Into<String>) -> CliError {
        CliError::new(EXIT_CONFIG, msg)
    }

    pub fn input(msg: impl Into<String>) -> CliError {
        CliError::new(EXIT_INPUT, msg)
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
    };
    if bypass_proxy(url) {
        b = b.proxy(None);
    }
    ureq::Agent::new_with_config(b.build())
}

/// Имя машины для `X-Zam-Machine`: `COMPUTERNAME` (Windows) или `HOSTNAME`; без
/// внешних вызовов. Пустое, длинное или с управляющими символами — не отправляется.
pub fn machine_from(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .filter_map(|k| get(k))
        .map(|v| v.trim().to_string())
        .find(|v| !v.is_empty())
        .filter(|v| v.chars().count() <= MACHINE_MAX_CHARS && !v.chars().any(char::is_control))
}

/// Клиент API одного сервера.
pub struct Client {
    agent: ureq::Agent,
    base: String,
    token: String,
    machine: Option<String>,
    solution: Option<String>,
}

/// В Debug токен — только маска.
impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .field("token", &zam_core::mask_token(&self.token))
            .field("machine", &self.machine)
            .field("solution", &self.solution)
            .finish()
    }
}

impl Client {
    /// `base` — без замыкающего `/`; `solution` — GUID для `X-Zam-Solution`.
    pub fn new(base: &str, token: &str, solution: Option<String>) -> Client {
        Client {
            agent: agent(base, Timeouts::Transfer),
            base: base.to_string(),
            token: token.to_string(),
            machine: machine_from(|k| std::env::var(k).ok()),
            solution,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.base)
    }

    fn prepare<B>(&self, mut rb: RequestBuilder<B>) -> RequestBuilder<B> {
        rb = rb.header("Authorization", format!("Bearer {}", self.token));
        if let Some(m) = &self.machine {
            rb = rb.header("X-Zam-Machine", m.as_str());
        }
        if let Some(s) = &self.solution {
            rb = rb.header("X-Zam-Solution", s.as_str());
        }
        rb
    }

    /// GET `/api/v1{path}` с параметрами запроса (кодируются ureq).
    pub fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Response<Body>, CliError> {
        let mut rb = self.prepare(self.agent.get(&self.url(path)));
        for (k, v) in query {
            rb = rb.query(*k, *v);
        }
        self.check(rb.call())
    }

    pub fn post_json(&self, path: &str, body: &Value) -> Result<Response<Body>, CliError> {
        let rb = self.prepare(self.agent.post(&self.url(path)));
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
        let mut rb = self.prepare(self.agent.post(&self.url(path)));
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
        self.check(self.prepare(self.agent.delete(&self.url(path))).call())
    }

    /// PATCH JSON.
    pub fn patch_json(&self, path: &str, body: &Value) -> Result<Response<Body>, CliError> {
        let rb = self.prepare(self.agent.patch(&self.url(path)));
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
            .prepare(self.agent.put(&self.url(path)))
            .header("If-Match", format!("\"{if_match}\""));
        self.check(
            rb.content_type("application/json")
                .send(body.to_string().as_bytes()),
        )
    }

    /// POST тела потоком известной длины (`Content-Length`, без chunked).
    pub fn send_multipart(
        &self,
        path: &str,
        content_type: &str,
        len: u64,
        reader: impl Read + 'static,
    ) -> Result<Response<Body>, CliError> {
        let rb = self
            .prepare(self.agent.post(&self.url(path)))
            .header("Content-Type", content_type)
            .header("Content-Length", len.to_string());
        self.check(rb.send(ureq::SendBody::from_owned_reader(reader)))
    }

    /// 2xx → ответ; иначе ошибка с кодом выхода по статусу.
    fn check(&self, r: Result<Response<Body>, ureq::Error>) -> Result<Response<Body>, CliError> {
        let resp = r.map_err(|e| {
            CliError::new(
                EXIT_UNREACHABLE,
                format!("сервер {} недоступен: {e}", self.base),
            )
        })?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        Err(problem(resp))
    }
}

/// Ответ с ошибкой → [`CliError`]: текст из `detail` problem+json.
fn problem(mut resp: Response<Body>) -> CliError {
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .with_config()
        .limit(BODY_MAX)
        .read_to_string()
        .unwrap_or_default();
    let v: Option<Value> = serde_json::from_str(&text).ok();
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
        _ => format!("{detail} (HTTP {status})"),
    };
    CliError {
        code,
        msg,
        problem: v,
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
    fn machine_name_rules() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            machine_from(env(&[("COMPUTERNAME", "WIN"), ("HOSTNAME", "lin")])).as_deref(),
            Some("WIN")
        );
        assert_eq!(
            machine_from(env(&[("COMPUTERNAME", " "), ("HOSTNAME", "lin")])).as_deref(),
            Some("lin")
        );
        assert_eq!(machine_from(env(&[])), None);
        assert_eq!(machine_from(env(&[("HOSTNAME", "a\tb")])), None);
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
