//! Минимальный HTTP/1.0 GET к Qdrant на 127.0.0.1 для проверки готовности.
//!
//! `/readyz` и `/` доступны без API-ключа (исследование 05 §6.1), поэтому ключ здесь не
//! нужен и в запросы не попадает. Отдельный HTTP-клиент ради двух запросов не тянем.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// Предел ответа: `/` и `/readyz` — десятки байт.
const MAX_BODY: u64 = 64 * 1024;
/// Соединение с 127.0.0.1.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);

/// `GET path` → (код, тело). `None` — нет соединения, таймаут или не HTTP.
pub(crate) async fn get(port: u16, path: &str, limit: Duration) -> Option<(u16, String)> {
    tokio::time::timeout(limit, async {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        // Windows повторяет SYN на закрытый порт петли ~2 с; слушающий порт отвечает сразу.
        let mut s = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
            .await
            .ok()?
            .ok()?;
        let req = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
        s.write_all(req.as_bytes()).await.ok()?;
        let mut buf = Vec::new();
        s.take(MAX_BODY).read_to_end(&mut buf).await.ok()?;
        let text = String::from_utf8_lossy(&buf);
        let (head, body) = text.split_once("\r\n\r\n")?;
        let code = head.split_whitespace().nth(1)?.parse().ok()?;
        Some((code, body.to_string()))
    })
    .await
    .ok()
    .flatten()
}

/// `/readyz` отвечает 200.
pub(crate) async fn readyz(port: u16) -> bool {
    matches!(
        get(port, "/readyz", Duration::from_secs(2)).await,
        Some((200, _))
    )
}

/// Версия из `GET /` (`{"title":"qdrant - vector search engine","version":"1.19.2",…}`).
pub(crate) async fn version(port: u16) -> Option<String> {
    let (code, body) = get(port, "/", Duration::from_secs(2)).await?;
    parse_version(code, &body)
}

/// Блокирующий вариант [`version`] для синхронного кода (`reset_storage` из CLI,
/// который может выполняться внутри runtime — там `block_on` недоступен).
pub(crate) fn version_blocking(port: u16) -> Option<String> {
    use std::io::{Read as _, Write as _};
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut s = std::net::TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    s.set_write_timeout(Some(Duration::from_secs(2))).ok()?;
    s.write_all(b"GET / HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
        .ok()?;
    let mut buf = Vec::new();
    // таймаут чтения после полученного ответа — не ошибка: разбираем то, что пришло
    let _ = s.take(MAX_BODY).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let (head, body) = text.split_once("\r\n\r\n")?;
    let code = head.split_whitespace().nth(1)?.parse().ok()?;
    parse_version(code, body)
}

fn parse_version(code: u16, body: &str) -> Option<String> {
    if code != 200 {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    if !v.get("title")?.as_str()?.contains("qdrant") {
        return None;
    }
    Some(v.get("version")?.as_str()?.to_string())
}
