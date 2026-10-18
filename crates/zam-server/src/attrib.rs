//! Атрибуция ревизий (§11.3, §12): кто пишет, с какой машины и в какой сессии.
//!
//! - `author_kind`: токен — `agent`, администратор (cookie WebUI) — `human`;
//! - `token_id` — id токена запроса;
//! - `X-Zam-Session`, `X-Zam-Machine` — до 128 символов UTF-8 без управляющих,
//!   иначе 400; пустое значение равно отсутствию заголовка;
//! - `message` — из тела запроса.
use axum::http::HeaderMap;
use zam_store::{Attribution, AuthorKind};

use crate::auth::Principal;
use crate::error::ApiError;

/// Заголовок id сессии агента.
pub const SESSION_HEADER: &str = "x-zam-session";
/// Заголовок имени машины.
pub const MACHINE_HEADER: &str = "x-zam-machine";
/// Предел длины значений заголовков атрибуции, символов.
pub const ATTRIB_MAX_CHARS: usize = 128;
/// Предел длины сообщения ревизии, символов.
pub const MESSAGE_MAX_CHARS: usize = 1000;

/// Атрибуция записи по принципалу, заголовкам и сообщению из тела.
pub fn attribution(
    p: &Principal,
    headers: &HeaderMap,
    message: Option<String>,
) -> Result<Attribution, ApiError> {
    let (author_kind, token_id) = match p {
        Principal::Token(t) => (AuthorKind::Agent, Some(t.token_id)),
        Principal::Admin { .. } => (AuthorKind::Human, None),
    };
    let message = message.filter(|m| !m.trim().is_empty());
    if message
        .as_ref()
        .is_some_and(|m| m.chars().count() > MESSAGE_MAX_CHARS)
    {
        return Err(ApiError::BadRequest(format!(
            "message длиннее {MESSAGE_MAX_CHARS} символов"
        )));
    }
    Ok(Attribution {
        author_kind,
        token_id,
        session_id: header_value(headers, SESSION_HEADER)?,
        machine: header_value(headers, MACHINE_HEADER)?,
        message,
    })
}

/// Значение заголовка атрибуции: UTF-8, ≤ 128 символов, только печатные.
fn header_value(headers: &HeaderMap, name: &str) -> Result<Option<String>, ApiError> {
    let Some(v) = headers.get(name) else {
        return Ok(None);
    };
    let bad = || {
        ApiError::BadRequest(format!(
            "заголовок {name}: до {ATTRIB_MAX_CHARS} печатных символов UTF-8"
        ))
    };
    let s = std::str::from_utf8(v.as_bytes()).map_err(|_| bad())?.trim();
    if s.chars().count() > ATTRIB_MAX_CHARS || s.chars().any(char::is_control) {
        return Err(bad());
    }
    Ok((!s.is_empty()).then(|| s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn admin() -> Principal {
        Principal::Admin {
            session_id: "s".into(),
        }
    }

    #[test]
    fn admin_is_human_and_headers_parsed() {
        let mut h = HeaderMap::new();
        h.insert(MACHINE_HEADER, HeaderValue::from_static(" box "));
        h.insert(
            SESSION_HEADER,
            HeaderValue::from_bytes("сессия".as_bytes()).unwrap(),
        );
        let a = attribution(&admin(), &h, Some("m".into())).unwrap();
        assert_eq!(a.author_kind, AuthorKind::Human);
        assert_eq!(a.token_id, None);
        assert_eq!(a.machine.as_deref(), Some("box"));
        assert_eq!(a.session_id.as_deref(), Some("сессия"));
        assert_eq!(a.message.as_deref(), Some("m"));
    }

    #[test]
    fn bad_headers_rejected() {
        for bad in ["a\tb".to_string(), "x".repeat(129)] {
            let mut h = HeaderMap::new();
            h.insert(MACHINE_HEADER, HeaderValue::from_str(&bad).unwrap());
            assert!(attribution(&admin(), &h, None).is_err(), "{bad}");
        }
        let mut h = HeaderMap::new();
        h.insert(
            MACHINE_HEADER,
            HeaderValue::from_str(&"я".repeat(128)).unwrap(),
        );
        assert!(attribution(&admin(), &h, None).is_ok());
        let mut h = HeaderMap::new();
        h.insert(
            SESSION_HEADER,
            HeaderValue::from_bytes(&[0xff, 0x41]).unwrap(),
        );
        assert!(attribution(&admin(), &h, None).is_err());
        assert!(attribution(&admin(), &HeaderMap::new(), Some("я".repeat(1001))).is_err());
    }
}
