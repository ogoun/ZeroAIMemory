//! `zam mcp` — stdio-прокси к `/mcp` (§13.1, §13.2) для агентов без HTTP-транспорта MCP.
//!
//! - Читает stdin построчно: строка — одно сообщение JSON-RPC (запрос, уведомление, ответ или
//!   пачка). Каждое уходит отдельным `POST /mcp` с `Authorization: Bearer`, `X-Zam-Solution`
//!   (из `solution.toml`), `X-Zam-Machine` и `X-Zam-Session` (как у остальных команд
//!   [`super::connect`]); `Origin` не шлётся.
//! - Ответ `application/json` пишется в stdout одной строкой; `text/event-stream` (SSE)
//!   разворачивается: каждое событие `data:` — отдельная строка. 202/204 — без вывода.
//! - После `initialize` прокси шлёт `MCP-Protocol-Version` с согласованной версией, а
//!   выданный сервером `Mcp-Session-Id` — во всех следующих запросах.
//! - HTTP-ошибка на запрос с `id` — ответ JSON-RPC `error` (код −32000, текст problem
//!   сервера); на уведомление — строка в stderr. Неразборчивая строка — `error` −32700.
//! - Сообщения обрабатываются параллельно (долгий инструмент не держит остальные); stdout
//!   пишется построчно под блокировкой. Конец stdin — выход с кодом 0 после всех ответов.
//!   Токен и тексты сообщений никуда, кроме сервера, не пишутся.
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::Ctx;
use crate::client::{CliError, Client};

/// Состояние сеанса MCP, общее для потоков запросов.
#[derive(Default)]
struct Session {
    protocol: Option<String>,
    session_id: Option<String>,
}

/// Предел тела ответа, байт.
const BODY_MAX: u64 = 64 * 1024 * 1024;

fn write_line(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out
        .write_all(line.as_bytes())
        .and_then(|_| out.write_all(b"\n"))
        .and_then(|_| out.flush());
}

/// JSON одной строкой (текст не JSON — как есть, без переводов строк).
fn compact(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    Some(match serde_json::from_str::<Value>(t) {
        Ok(v) => v.to_string(),
        Err(_) => t.replace(['\r', '\n'], " "),
    })
}

fn rpc_error(id: &Value, code: i64, msg: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}}).to_string()
}

/// События SSE: строки `data:` до пустой строки — одно сообщение.
fn sse(reader: impl Read, mut emit: impl FnMut(&str)) {
    let mut data: Vec<String> = Vec::new();
    for line in BufReader::new(reader).lines() {
        let Ok(line) = line else { break };
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if !data.is_empty() {
                emit(&data.join("\n"));
                data.clear();
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        }
    }
    if !data.is_empty() {
        emit(&data.join("\n"));
    }
}

/// Запомнить согласованную версию протокола из ответа `initialize`.
fn note_protocol(state: &Mutex<Session>, msg: &str, was_initialize: bool) {
    if !was_initialize {
        return;
    }
    if let Ok(v) = serde_json::from_str::<Value>(msg)
        && let Some(p) = v["result"]["protocolVersion"].as_str()
    {
        state.lock().unwrap_or_else(|e| e.into_inner()).protocol = Some(p.to_string());
    }
}

/// Одно сообщение: запрос к серверу и вывод ответа.
fn forward(client: &Client, state: &Mutex<Session>, line: &str) {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            write_line(&rpc_error(
                &Value::Null,
                -32700,
                &format!("parse error: {e}"),
            ));
            return;
        }
    };
    // id запроса (у уведомления и ответа клиента его нет или нет метода)
    let id = (msg.get("method").is_some())
        .then(|| msg.get("id").cloned())
        .flatten();
    let initialize = msg["method"] == "initialize";
    let (protocol, sid) = {
        let s = state.lock().unwrap_or_else(|e| e.into_inner());
        (s.protocol.clone(), s.session_id.clone())
    };
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(p) = protocol.as_deref().filter(|_| !initialize) {
        headers.push(("MCP-Protocol-Version", p));
    }
    if let Some(s) = sid.as_deref() {
        headers.push(("Mcp-Session-Id", s));
    }
    let body = msg.to_string();
    let mut resp = match client.post_mcp(body.as_bytes(), &headers) {
        Ok(r) => r,
        Err(e) => {
            match &id {
                Some(id) => write_line(&rpc_error(id, -32000, &e.msg)),
                None => eprintln!("zam mcp: {}", e.msg),
            }
            return;
        }
    };
    let status = resp.status().as_u16();
    if let Some(s) = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
    {
        state.lock().unwrap_or_else(|e| e.into_inner()).session_id = Some(s.to_string());
    }
    let sse_body = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.starts_with("text/event-stream"));
    if !(200..300).contains(&status) {
        let text = resp
            .body_mut()
            .with_config()
            .limit(BODY_MAX)
            .read_to_string()
            .unwrap_or_default();
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["detail"].as_str().map(String::from))
            .unwrap_or_else(|| text.chars().take(200).collect());
        let m = format!("HTTP {status}: {detail}");
        match &id {
            Some(id) => write_line(&rpc_error(id, -32000, &m)),
            None => eprintln!("zam mcp: {m}"),
        }
        return;
    }
    if sse_body {
        sse(resp.into_body().into_reader(), |data| {
            note_protocol(state, data, initialize);
            if let Some(l) = compact(data) {
                write_line(&l);
            }
        });
        return;
    }
    let text = resp
        .body_mut()
        .with_config()
        .limit(BODY_MAX)
        .read_to_string()
        .unwrap_or_default();
    note_protocol(state, &text, initialize);
    if let Some(l) = compact(&text) {
        write_line(&l);
    }
}

pub fn run(ctx: Ctx) -> Result<(), CliError> {
    let client = Arc::new(ctx.client);
    let state = Arc::new(Mutex::new(Session::default()));
    let mut workers = Vec::new();
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        // `initialize` — до остальных: от него зависят версия протокола и сеанс
        let first = serde_json::from_str::<Value>(&line)
            .is_ok_and(|v| v.get("method").and_then(Value::as_str) == Some("initialize"));
        let (c, s) = (client.clone(), state.clone());
        let h = std::thread::spawn(move || forward(&c, &s, &line));
        if first {
            let _ = h.join();
        } else {
            workers.push(h);
        }
        workers.retain(|w| !w.is_finished());
    }
    for w in workers {
        let _ = w.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_events_become_lines() {
        let body = "event: message\r\ndata: {\"a\":\r\ndata: 1}\r\n\r\n: comment\ndata: {\"b\": 2}\n\ndata: x";
        let mut got = Vec::new();
        sse(body.as_bytes(), |d| got.push(compact(d).unwrap()));
        assert_eq!(got, ["{\"a\":1}", "{\"b\":2}", "x"]);
    }

    #[test]
    fn compact_single_line() {
        assert_eq!(
            compact("{\n \"a\": [1,\n2]\n}").as_deref(),
            Some("{\"a\":[1,2]}")
        );
        assert_eq!(compact("  "), None);
    }
}
