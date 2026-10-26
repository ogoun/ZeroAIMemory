//! Настоящий `zam-server serve` как дочерний процесс теста: конфиг TOML, первый токен
//! из stderr, жёсткое завершение и минимальный HTTP/1.1-клиент без внешних зависимостей.
//!
//! Нужен там, где проверяется сам процесс (привязка Qdrant к родителю, перезапуск
//! сервера); остальные тесты собирают сервер в процессе теста (`common::api`).

use std::collections::VecDeque;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

/// Пароль администратора первого старта (`ZAM_ADMIN_PASSWORD`).
const PASSWORD: &str = "pw";
/// Строка stderr с первым токеном (`zam_server::print_initial_token`).
const TOKEN_PREFIX: &str = "ZeroAIMemoryToken=";
/// Сколько последних строк stderr хранить для сообщения об ошибке.
const TAIL: usize = 60;

/// Команда `zam-server` без унаследованных переменных `ZAM_*` окружения теста
/// (в CI задана `ZAM_REQUIRE_ML`, а сервер читает `ZAM_*` как ключи конфигурации).
pub fn command() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_zam-server"));
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("ZAM_")) {
        cmd.env_remove(k);
    }
    cmd
}

/// Сессия администратора WebUI: cookie `zam_session=…` и CSRF-токен.
pub struct AdminWeb {
    pub cookie: String,
    pub csrf: String,
}

/// Дочерний `zam-server serve`; в `Drop` завершается жёстко.
pub struct ServerProc {
    child: Child,
    pub addr: SocketAddr,
    tail: Arc<Mutex<VecDeque<String>>>,
    token_rx: Receiver<String>,
}

impl ServerProc {
    /// Запускает `zam-server serve --config <conf>`; `addr` — `server.bind` конфига.
    pub fn start(conf: &Path, addr: SocketAddr) -> ServerProc {
        let mut child = command()
            .arg("serve")
            .arg("--config")
            .arg(conf)
            .env("ZAM_ADMIN_PASSWORD", PASSWORD)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("запуск zam-server");
        let stderr = child.stderr.take().expect("stderr сервера");
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let (tx, token_rx) = channel();
        let t = Arc::clone(&tail);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(tok) = line.strip_prefix(TOKEN_PREFIX) {
                    let _ = tx.send(tok.trim().to_string());
                    continue;
                }
                let mut l = t.lock().unwrap_or_else(|p| p.into_inner());
                if l.len() == TAIL {
                    l.pop_front();
                }
                l.push_back(line);
            }
        });
        ServerProc {
            child,
            addr,
            tail,
            token_rx,
        }
    }

    /// Первый токен администратора (печатается только при первом старте каталога).
    pub fn initial_token(&self, limit: Duration) -> String {
        self.token_rx
            .recv_timeout(limit)
            .unwrap_or_else(|e| panic!("нет первого токена ({e}), stderr:\n{}", self.tail()))
    }

    /// pid процесса сервера.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Последние строки stderr (без строки с токеном).
    pub fn tail(&self) -> String {
        let l = self.tail.lock().unwrap_or_else(|p| p.into_inner());
        l.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// Процесс ещё работает.
    pub fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Жёсткое завершение: `TerminateProcess` на Windows, SIGKILL на Linux — штатная
    /// остановка супервизора не выполняется.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Штатная остановка на Linux (SIGTERM, ожидание до `limit`), код выхода — успех.
    #[cfg(unix)]
    pub fn terminate(&mut self, limit: Duration) {
        let sent = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status()
            .expect("kill -TERM");
        assert!(sent.success(), "kill -TERM: {sent}");
        let t = std::time::Instant::now();
        loop {
            if let Some(st) = self.child.try_wait().expect("ожидание сервера") {
                assert!(st.success(), "сервер завершился с {st}:\n{}", self.tail());
                return;
            }
            assert!(
                t.elapsed() < limit,
                "сервер не завершился по SIGTERM за {limit:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Запрос с токеном; ответ — (код, JSON или `Null` для пустого тела).
    pub fn call(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<&Value>,
    ) -> (u16, Value) {
        self.try_call(method, path, token, body)
            .unwrap_or_else(|e| panic!("{method} {path}: {e}\nstderr:\n{}", self.tail()))
    }

    /// То же, что [`ServerProc::call`], но ошибка соединения — `Err` (сервер ещё не
    /// слушает порт: bind идёт после открытия баз и загрузки модели).
    pub fn try_call(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<&Value>,
    ) -> std::io::Result<(u16, Value)> {
        http(self.addr, method, path, Some(token), body)
    }

    /// Вход администратора (пароль первого старта): значение cookie `zam_session`.
    pub fn admin_session(&self) -> String {
        let web = self.admin_login();
        web.cookie
            .strip_prefix("zam_session=")
            .unwrap_or(&web.cookie)
            .to_string()
    }

    /// Вход администратора (пароль первого старта): cookie `zam_session=…` и CSRF-токен
    /// для изменяющих запросов WebUI.
    pub fn admin_login(&self) -> AdminWeb {
        let (code, head, v) = http_with(
            self.addr,
            "POST",
            "/api/v1/auth/login",
            &[],
            Some(&serde_json::json!({"login": "admin", "password": PASSWORD})),
        )
        .unwrap_or_else(|e| {
            panic!(
                "вход администратора: {e}
stderr:
{}",
                self.tail()
            )
        });
        assert_eq!(code, 200, "вход администратора: {v}");
        let cookie = head
            .lines()
            .filter_map(|l| l.split_once(':'))
            .filter(|(k, _)| k.trim().eq_ignore_ascii_case("set-cookie"))
            .find_map(|(_, c)| {
                c.trim()
                    .split(';')
                    .next()
                    .filter(|kv| kv.trim().starts_with("zam_session="))
                    .map(|kv| kv.trim().to_string())
            })
            .unwrap_or_else(|| panic!("нет cookie zam_session: {head}"));
        let csrf = v["csrf"]
            .as_str()
            .unwrap_or_else(|| panic!("нет csrf: {v}"))
            .to_string();
        AdminWeb { cookie, csrf }
    }

    /// Запрос администратора WebUI (cookie и CSRF); `user` — `X-Zam-User`.
    pub fn admin_call(
        &self,
        web: &AdminWeb,
        method: &str,
        path: &str,
        body: Option<&Value>,
        user: Option<&str>,
    ) -> (u16, Value) {
        let mut h = vec![
            ("Cookie", web.cookie.as_str()),
            ("X-CSRF-Token", web.csrf.as_str()),
        ];
        if let Some(u) = user {
            h.push(("X-Zam-User", u));
        }
        http_with(self.addr, method, path, &h, body)
            .map(|(code, _, v)| (code, v))
            .unwrap_or_else(|e| {
                panic!(
                    "{method} {path}: {e}
stderr:
{}",
                    self.tail()
                )
            })
    }

    /// Запрос с токеном и дополнительными заголовками (`X-Zam-Session`, `If-Match` …).
    pub fn call_h(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<&Value>,
        extra: &[(&str, &str)],
    ) -> (u16, Value) {
        let auth = format!("Bearer {token}");
        let mut h = vec![("Authorization", auth.as_str())];
        h.extend_from_slice(extra);
        http_with(self.addr, method, path, &h, body)
            .map(|(code, _, v)| (code, v))
            .unwrap_or_else(|e| {
                panic!(
                    "{method} {path}: {e}
stderr:
{}",
                    self.tail()
                )
            })
    }

    /// Запрос с токеном и сырым телом (`multipart/form-data`); ответ — сырые байты.
    pub fn call_bytes(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<(&str, &[u8])>,
    ) -> (u16, Vec<u8>) {
        let auth = format!("Bearer {token}");
        http_bytes(self.addr, method, path, &[("Authorization", &auth)], body)
            .map(|(code, _, b)| (code, b))
            .unwrap_or_else(|e| {
                panic!(
                    "{method} {path}: {e}
stderr:
{}",
                    self.tail()
                )
            })
    }

    /// GET в сессии администратора (`session` — из [`ServerProc::admin_session`]).
    pub fn admin_get(&self, path: &str, session: &str) -> std::io::Result<(u16, Value)> {
        let cookie = format!("zam_session={session}");
        http_with(self.addr, "GET", path, &[("Cookie", &cookie)], None)
            .map(|(code, _, v)| (code, v))
    }

    /// Процесс жив; умер — паника с кодом выхода и хвостом stderr (без ожидания
    /// таймаута вызывающего).
    pub fn assert_running(&mut self, what: &str) {
        if let Ok(Some(st)) = self.child.try_wait() {
            // поток чтения stderr дочитывает вывод умершего процесса
            std::thread::sleep(Duration::from_millis(200));
            panic!(
                "zam-server завершился ({st}) до «{what}», stderr:\n{}",
                self.tail()
            );
        }
    }
}

impl Drop for ServerProc {
    fn drop(&mut self) {
        if self.running() {
            self.kill();
        }
        if std::thread::panicking() {
            eprintln!("stderr zam-server:\n{}", self.tail());
        }
    }
}

/// Один HTTP/1.1-запрос с `Connection: close`; тело ответа — JSON (`Null`, если пусто).
pub fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&Value>,
) -> std::io::Result<(u16, Value)> {
    let auth = token.map(|t| format!("Bearer {t}"));
    let headers: Vec<(&str, &str)> = auth.iter().map(|a| ("Authorization", a.as_str())).collect();
    http_with(addr, method, path, &headers, body).map(|(code, _, v)| (code, v))
}

/// То же, что [`http`], с произвольными заголовками запроса; ответ — (код, заголовки
/// ответа как есть, тело JSON).
pub fn http_with(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&Value>,
) -> std::io::Result<(u16, String, Value)> {
    let payload = body.map(Value::to_string);
    let raw = payload
        .as_deref()
        .map(|p| ("application/json", p.as_bytes()));
    let (code, head, body) = http_bytes(addr, method, path, headers, raw)?;
    let v = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).map_err(|e| {
            std::io::Error::other(format!(
                "не JSON ({e}), код {code}: {}",
                String::from_utf8_lossy(&body)
            ))
        })?
    };
    Ok((code, head, v))
}

/// Один HTTP/1.1-запрос с `Connection: close` и сырым телом `(Content-Type, байты)`;
/// ответ — (код, заголовки ответа как есть, тело байтами).
pub fn http_bytes(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<(&str, &[u8])>,
) -> std::io::Result<(u16, String, Vec<u8>)> {
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    s.set_read_timeout(Some(Duration::from_secs(60)))?;
    let payload: &[u8] = body.map_or(&[], |(_, b)| b);
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some((ct, _)) = body {
        req.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n", payload.len()));
    s.write_all(req.as_bytes())?;
    s.write_all(payload)?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw)?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("ответ без заголовков"))?;
    let head_raw = String::from_utf8_lossy(&raw[..split]).into_owned();
    let head = head_raw.to_ascii_lowercase();
    let code: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| std::io::Error::other(format!("строка статуса: {head}")))?;
    let mut rest = &raw[split + 4..];
    let mut body = Vec::new();
    if head.contains("transfer-encoding: chunked") {
        // <hex-размер>\r\n<данные>\r\n … 0\r\n\r\n
        loop {
            let eol = rest
                .windows(2)
                .position(|w| w == b"\r\n")
                .ok_or_else(|| std::io::Error::other("обрыв chunked"))?;
            let size = usize::from_str_radix(
                String::from_utf8_lossy(&rest[..eol])
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim(),
                16,
            )
            .map_err(|e| std::io::Error::other(format!("размер chunk: {e}")))?;
            if size == 0 {
                break;
            }
            // усечённый ответ (сервер умер посреди тела) — ошибка, а не паника по срезу
            let start = eol + 2;
            let chunk = start
                .checked_add(size)
                .and_then(|end| rest.get(start..end))
                .ok_or_else(|| std::io::Error::other("усечённый chunk"))?;
            body.extend_from_slice(chunk);
            rest = rest
                .get(start + size..)
                .and_then(|r| r.strip_prefix(b"\r\n"))
                .ok_or_else(|| std::io::Error::other("chunk без завершающего CRLF"))?;
        }
    } else {
        body.extend_from_slice(rest);
    }
    Ok((code, head_raw, body))
}
