//! Атрибуция ревизий (§11.3, §12): кто пишет, с какой машины и в какой сессии.
//!
//! - `author_kind`: токен — `agent`, человеческий токен (`human`, §7.5) и администратор
//!   (cookie WebUI) — `human`;
//! - `token_id` — id токена запроса, `user_id` — его пользователь (автор ревизии в базе
//!   владельца общего решения, §11.6); у администратора — нет;
//! - `X-Zam-Session`, `X-Zam-Machine` — до 128 символов UTF-8 без управляющих,
//!   иначе 400; пустое значение равно отсутствию заголовка;
//! - `X-Zam-Area` (§11.2) — область по умолчанию для новых узлов и записей журналов, те же
//!   правила;
//! - `message` — из тела запроса.
use axum::http::HeaderMap;
use zam_store::{Attribution, AuthorKind};

use crate::auth::Principal;
use crate::error::ApiError;

/// Заголовок id сессии агента.
pub const SESSION_HEADER: &str = "x-zam-session";
/// Заголовок имени машины.
pub const MACHINE_HEADER: &str = "x-zam-machine";
/// Заголовок текущей области (монорепозиторий, §11.2).
pub const AREA_HEADER: &str = "x-zam-area";
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
    let (author_kind, token_id, user_id) = match p {
        Principal::Token(t) => {
            let kind = if t.human {
                AuthorKind::Human
            } else {
                AuthorKind::Agent
            };
            (kind, Some(t.token_id), Some(t.user_id))
        }
        Principal::Admin { .. } => (AuthorKind::Human, None, None),
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
        user_id,
        area: header_value(headers, AREA_HEADER)?,
    })
}

/// Значение заголовка атрибуции: UTF-8, ≤ 128 символов, только печатные.
pub fn header_value(headers: &HeaderMap, name: &str) -> Result<Option<String>, ApiError> {
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
        assert_eq!(a.area, None);
        h.insert(AREA_HEADER, HeaderValue::from_static(" services/parser "));
        let a = attribution(&admin(), &h, None).unwrap();
        assert_eq!(a.area.as_deref(), Some("services/parser"));
    }

    #[test]
    fn token_author_kind_by_human_flag() {
        let tok = |human| {
            Principal::Token(zam_store::AuthToken {
                token_id: zam_core::Id::new(),
                user_id: zam_core::Id::new(),
                access: zam_store::Access::Rw,
                human,
            })
        };
        let h = HeaderMap::new();
        assert_eq!(
            attribution(&tok(false), &h, None).unwrap().author_kind,
            AuthorKind::Agent
        );
        let a = attribution(&tok(true), &h, None).unwrap();
        assert_eq!(a.author_kind, AuthorKind::Human);
        assert!(a.token_id.is_some() && a.user_id.is_some());
    }

    #[test]
    fn bad_headers_rejected() {
        for bad in ["a\tb".to_string(), "x".repeat(129)] {
            for name in [MACHINE_HEADER, AREA_HEADER] {
                let mut h = HeaderMap::new();
                h.insert(name, HeaderValue::from_str(&bad).unwrap());
                assert!(attribution(&admin(), &h, None).is_err(), "{name}: {bad}");
            }
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
