//! Синхронный клиент MCP (`POST /mcp`, JSON-RPC) симулированного агента ТС-1 (спецификация M6
//! §3.1; решение составителя плана 4).
//!
//! Без `tokio`: `ureq`, как остальной xtask. [`McpClient::connect`] шлёт `initialize`; если
//! сервер выдал `Mcp-Session-Id` (транспорт с сессиями у старых сборок), id передаётся в каждом
//! следующем запросе и отправляется `notifications/initialized`. Нынешний сервер работает без
//! сессий (`legacy_session_mode = false`), и `initialize` ему не обязателен.
//!
//! Заголовки — как у агента: `Authorization: Bearer`, `X-Zam-Solution` (решение набора) и, если
//! задана, `X-Zam-Session` (сессия сценария: явная привязка, §7.1). Ответ — `application/json`
//! или `text/event-stream` (берётся событие с `id` запроса). [`McpClient::call`] возвращает текст
//! `result.content` — то, что видит агент; ошибка JSON-RPC или `isError` — ошибка вызова.

use std::cell::Cell;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

/// Предел одного вызова (реранк на CPU, первая загрузка реранкера).
const CALL_TIMEOUT: Duration = Duration::from_secs(300);
/// Версия протокола в `initialize`.
const PROTOCOL: &str = "2025-06-18";
const SESSION_ID: &str = "Mcp-Session-Id";

/// Клиент MCP одной сессии агента.
pub struct McpClient {
    agent: ureq::Agent,
    url: String,
    token: String,
    solution: String,
    session: Option<String>,
    mcp_session_id: Option<String>,
    next_id: Cell<u64>,
}

impl McpClient {
    /// `initialize` к `<base>/mcp`.
    pub fn connect(base: &str, token: &str, solution: &str, session: Option<&str>) -> Result<Self> {
        let mut c = McpClient {
            agent: crate::smoke::agent_with(CALL_TIMEOUT),
            url: format!("{}/mcp", base.trim_end_matches('/')),
            token: token.to_string(),
            solution: solution.to_string(),
            session: session.map(str::to_string),
            mcp_session_id: None,
            next_id: Cell::new(1),
        };
        let params = json!({
            "protocolVersion": PROTOCOL,
            "capabilities": {},
            "clientInfo": { "name": "zam-xtask-eval", "version": env!("CARGO_PKG_VERSION") },
        });
        let (sid, _) = c.rpc("initialize", params, true)?;
        if let Some(sid) = sid {
            c.mcp_session_id = Some(sid);
            c.notify("notifications/initialized")?;
        }
        Ok(c)
    }

    /// `tools/call`: текст `result.content` (части `text` через перевод строки).
    pub fn call(&self, tool: &str, args: Value) -> Result<String> {
        let (_, r) = self.rpc(
            "tools/call",
            json!({ "name": tool, "arguments": args }),
            false,
        )?;
        let text = content_text(&r);
        if r["isError"].as_bool() == Some(true) {
            bail!("{tool}: ошибка инструмента: {text}");
        }
        Ok(text)
    }

    fn request(&self) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        let mut req = self
            .agent
            .post(&self.url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/json, text/event-stream")
            .header("X-Zam-Solution", &self.solution)
            .content_type("application/json");
        if let Some(s) = &self.session {
            req = req.header("X-Zam-Session", s);
        }
        if let Some(s) = &self.mcp_session_id {
            req = req.header(SESSION_ID, s);
        }
        req
    }

    fn notify(&self, method: &str) -> Result<()> {
        let body = json!({ "jsonrpc": "2.0", "method": method });
        let r = self.request().send(body.to_string())?;
        let st = r.status().as_u16();
        if !(200..300).contains(&st) {
            bail!("MCP {method}: HTTP {st}");
        }
        Ok(())
    }

    /// Запрос JSON-RPC: (`Mcp-Session-Id` ответа, если `want_sid`; `result`).
    fn rpc(&self, method: &str, params: Value, want_sid: bool) -> Result<(Option<String>, Value)> {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let mut r = self
            .request()
            .send(body.to_string())
            .with_context(|| format!("POST {} {method}", self.url))?;
        let st = r.status().as_u16();
        let sid = want_sid
            .then(|| {
                r.headers()
                    .get(SESSION_ID)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            })
            .flatten();
        let ctype = r
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = r.body_mut().read_to_string()?;
        if st != 200 {
            bail!("MCP {method}: HTTP {st}: {text}");
        }
        let v = parse_reply(&ctype, &text, id)?;
        if let Some(e) = v.get("error") {
            bail!("MCP {method}: ошибка JSON-RPC: {e}");
        }
        Ok((sid, v["result"].clone()))
    }
}

/// Тело ответа → сообщение JSON-RPC с `id`: JSON или поток SSE (`data:` событий).
pub fn parse_reply(content_type: &str, text: &str, id: u64) -> Result<Value> {
    if !content_type.starts_with("text/event-stream") {
        return serde_json::from_str(text).map_err(|e| anyhow!("ответ MCP не JSON ({e}): {text}"));
    }
    let mut data = String::new();
    let mut events = Vec::new();
    for line in text.lines().chain(std::iter::once("")) {
        if let Some(d) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(d.strip_prefix(' ').unwrap_or(d));
        } else if line.is_empty() && !data.is_empty() {
            events.push(std::mem::take(&mut data));
        }
    }
    events
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(e).ok())
        .find(|v| v["id"].as_u64() == Some(id))
        .ok_or_else(|| anyhow!("в потоке SSE нет ответа на запрос {id}: {text}"))
}

/// Текст `result.content`: части `type: text` через перевод строки.
pub fn content_text(result: &Value) -> String {
    result["content"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|c| c["type"] == "text")
                .filter_map(|c| c["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_json_and_sse() {
        let j =
            r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"a\n~2 tok"}]}}"#;
        let v = parse_reply("application/json", j, 3).unwrap();
        assert_eq!(content_text(&v["result"]), "a\n~2 tok");
        let sse = format!(
            "event: message\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}}\n\n\
             event: message\ndata: {j}\n\n"
        );
        let v = parse_reply("text/event-stream", &sse, 3).unwrap();
        assert_eq!(v["id"], 3);
        assert!(parse_reply("text/event-stream", &sse, 4).is_err());
        assert!(parse_reply("application/json", "<html>", 1).is_err());
        let two = json!({"content": [{"type": "text", "text": "x"}, {"type": "image"}, {"type": "text", "text": "y"}]});
        assert_eq!(content_text(&two), "x\ny");
    }
}
