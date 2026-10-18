//! Аутентификация: bearer-токены, cookie-сессия администратора с CSRF, учёт прокси.
pub mod admin;
pub mod rate;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, header};
use ipnet::IpNet;
use subtle::ConstantTimeEq;
use zam_core::{Id, looks_like_token, mask_token};
use zam_store::{AuthToken, User};

use crate::app::AppState;
use crate::error::ApiError;
use rate::{RateLimiter, retry_after_secs};

/// Имя cookie веб-сессии администратора.
pub const SESSION_COOKIE: &str = "zam_session";
/// Заголовок CSRF для изменяющих запросов по cookie.
pub const CSRF_HEADER: &str = "x-csrf-token";
/// Заголовок выбора пользователя администратором.
pub const USER_HEADER: &str = "x-zam-user";

/// Состояние аутентификации процесса (лимиты, фиктивный хэш).
pub struct AuthState {
    /// Каждая попытка входа администратора.
    pub login: RateLimiter,
    /// Только неудачные проверки токена.
    pub bad_token: RateLimiter,
    /// Фиктивный хэш для неверного логина: с параметрами настоящего, вычисляется при старте.
    pub(crate) dummy_hash: String,
    /// Предупреждение о недоверенном прокси уже выдано.
    proxy_warned: AtomicBool,
}

impl AuthState {
    /// Блокирующий вызов (argon2): выполнять в `spawn_blocking`.
    pub fn new(admin_hash: &str) -> Result<AuthState, String> {
        Ok(AuthState {
            login: RateLimiter::default(),
            bad_token: RateLimiter::default(),
            dummy_hash: admin::make_dummy_hash(admin_hash)?,
            proxy_warned: AtomicBool::new(false),
        })
    }
}

impl std::fmt::Debug for AuthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthState").finish_non_exhaustive()
    }
}

/// Кто выполняет запрос.
#[derive(Clone)]
pub enum Principal {
    Token(AuthToken),
    Admin { session_id: String },
}

/// Только администратор по cookie-сессии; токены — 403.
#[derive(Clone)]
pub struct AdminOnly(pub String /*session_id*/);

/// Id сессии в отладочном выводе: первые 4 символа.
fn mask_sid(sid: &str) -> String {
    format!("{}…", sid.chars().take(4).collect::<String>())
}

impl std::fmt::Debug for Principal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Principal::Token(t) => f.debug_tuple("Token").field(t).finish(),
            Principal::Admin { session_id } => f
                .debug_struct("Admin")
                .field("session_id", &mask_sid(session_id))
                .finish(),
        }
    }
}

impl std::fmt::Debug for AdminOnly {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AdminOnly")
            .field(&mask_sid(&self.0))
            .finish()
    }
}

/// Адрес клиента и схема с учётом доверенных прокси. Извлекается всегда.
#[derive(Debug, Clone, Copy)]
pub struct Client {
    pub ip: IpAddr,
    pub https: bool,
}

/// Peer нет (тестовый вызов без `ConnectInfo`) — недоверенный 0.0.0.0.
const NO_PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);

fn trusted_peer(peer: SocketAddr, trusted: &[IpNet]) -> bool {
    let ip = peer.ip().to_canonical();
    trusted.iter().any(|n| n.contains(&ip))
}

/// Значения заголовка списком через запятую (все экземпляры по порядку).
fn header_list<'a>(headers: &'a HeaderMap, name: &str) -> Vec<&'a str> {
    headers
        .get_all(name)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// IP клиента. `X-Forwarded-For` учитывается, только если peer — доверенный прокси:
/// берётся крайний правый адрес не из `trusted`; нераспознанный элемент обрывает цепочку.
pub fn client_ip(peer: SocketAddr, headers: &HeaderMap, trusted: &[IpNet]) -> IpAddr {
    let mut ip = peer.ip().to_canonical();
    if !trusted_peer(peer, trusted) {
        return ip;
    }
    for raw in header_list(headers, "x-forwarded-for").into_iter().rev() {
        let Ok(hop) = raw.parse::<IpAddr>().map(|a| a.to_canonical()) else {
            break;
        };
        ip = hop;
        if !trusted.iter().any(|n| n.contains(&hop)) {
            break;
        }
    }
    ip
}

/// Пришёл ли запрос по HTTPS: `X-Forwarded-Proto` от доверенного прокси (последнее значение).
pub fn is_https(peer: SocketAddr, headers: &HeaderMap, trusted: &[IpNet]) -> bool {
    trusted_peer(peer, trusted)
        && header_list(headers, "x-forwarded-proto")
            .last()
            .is_some_and(|p| p.eq_ignore_ascii_case("https"))
}

impl FromRequestParts<AppState> for Client {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let trusted = &st.cfg.server.trusted_proxies;
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0);
        if let Some(p) = peer
            && !trusted_peer(p, trusted)
            && parts.headers.contains_key("x-forwarded-for")
            && !st.auth.proxy_warned.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(
                peer = %p.ip(),
                "запросы идут через прокси, не указанный в server.trusted_proxies — \
                 лимиты и IP в аудите общие для всех клиентов"
            );
        }
        Ok(match peer {
            Some(p) => Client {
                ip: client_ip(p, &parts.headers, trusted),
                https: is_https(p, &parts.headers, trusted),
            },
            // Без peer заголовкам не доверяем вовсе (даже при trusted = 0.0.0.0/0).
            None => Client {
                ip: NO_PEER.ip(),
                https: false,
            },
        })
    }
}

/// Токен из `Authorization: Bearer …` (схема без учёта регистра).
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, rest) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| rest.trim())
}

/// Значение cookie по имени из всех заголовков `Cookie`.
pub(crate) fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(cookie::Cookie::split_parse)
        .filter_map(Result::ok)
        .find(|c| c.name() == name)
        .map(|c| c.value().to_string())
}

/// Сравнение секретов за время, не зависящее от содержимого.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

async fn authenticate_token(st: &AppState, ip: IpAddr, token: &str) -> Result<Principal, ApiError> {
    let limited = |d| ApiError::RateLimited {
        retry_after_s: retry_after_secs(d),
    };
    // Превышение не блокирует верные токены: за общим NAT/прокси чужой мусор
    // не должен отключать агентов. Перебор 40 символов base62 и так невозможен.
    let over = st.auth.bad_token.peek(ip).err();
    if looks_like_token(token) {
        let ip_s = ip.to_string();
        if let Some(t) = st.system.verify_token(token, Some(&ip_s)).await? {
            return Ok(Principal::Token(t));
        }
    }
    // При превышении — 429 без записи в лог (защита от наводнения журнала).
    if let Some(d) = over {
        return Err(limited(d));
    }
    tracing::warn!(token = %mask_token(token), ip = %ip, "отклонён неверный токен");
    st.auth.bad_token.check(ip).map_err(limited)?;
    Err(ApiError::Unauthorized)
}

impl FromRequestParts<AppState> for Principal {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, ApiError> {
        let Ok(client) = Client::from_request_parts(parts, st).await;
        if let Some(token) = bearer(&parts.headers) {
            return authenticate_token(st, client.ip, token).await;
        }
        let Some(sid) = cookie_value(&parts.headers, SESSION_COOKIE) else {
            return Err(ApiError::Unauthorized);
        };
        let Some(ws) = st.system.get_web_session(&sid).await? else {
            return Err(ApiError::Unauthorized);
        };
        if !matches!(parts.method, Method::GET | Method::HEAD) {
            let sent = parts
                .headers
                .get(CSRF_HEADER)
                .map(|v| v.as_bytes())
                .unwrap_or_default();
            if !ct_eq(sent, ws.csrf_token.as_bytes()) {
                tracing::warn!(ip = %client.ip, "запрос администратора без верного CSRF-токена");
                return Err(ApiError::Forbidden);
            }
        }
        Ok(Principal::Admin { session_id: sid })
    }
}

impl FromRequestParts<AppState> for AdminOnly {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, ApiError> {
        match Principal::from_request_parts(parts, st).await? {
            Principal::Admin { session_id } => Ok(AdminOnly(session_id)),
            Principal::Token(_) => Err(ApiError::Forbidden),
        }
    }
}

/// Пользователь, от имени которого действует запрос: для токена — его владелец
/// (`X-Zam-User` игнорируется); для администратора — `X-Zam-User` или он сам.
/// Несуществующий пользователь — 404 (база для него не создаётся).
pub async fn target_user(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
) -> Result<User, ApiError> {
    let id = match p {
        Principal::Token(t) => t.user_id,
        Principal::Admin { .. } => match headers.get(USER_HEADER) {
            Some(v) => v
                .to_str()
                .ok()
                .and_then(|s| s.trim().parse::<Id>().ok())
                .ok_or_else(|| ApiError::BadRequest("некорректный X-Zam-User".into()))?,
            None => st.admin_id,
        },
    };
    st.system.get_user(id).await?.ok_or(ApiError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn sa(s: &str) -> SocketAddr {
        SocketAddr::new(s.parse().unwrap(), 1)
    }
    fn nets(v: &[&str]) -> Vec<IpNet> {
        v.iter().map(|s| s.parse().unwrap()).collect()
    }
    fn xff(v: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for x in v {
            h.append("x-forwarded-for", HeaderValue::from_str(x).unwrap());
        }
        h
    }

    #[test]
    fn xff_ignored_from_untrusted_peer() {
        let t = nets(&["127.0.0.1/32"]);
        let ip = client_ip(sa("10.1.1.1"), &xff(&["1.2.3.4"]), &t);
        assert_eq!(ip.to_string(), "10.1.1.1");
    }

    #[test]
    fn rightmost_untrusted_hop() {
        let t = nets(&["127.0.0.1/32", "10.0.0.0/8"]);
        // клиент мог подделать левую часть; правее — добавленное прокси
        let h = xff(&["6.6.6.6, 203.0.113.7", "10.0.0.2"]);
        assert_eq!(
            client_ip(sa("127.0.0.1"), &h, &t).to_string(),
            "203.0.113.7"
        );
        // все звенья доверенные — крайнее левое
        let h = xff(&["10.0.0.3, 10.0.0.2"]);
        assert_eq!(client_ip(sa("127.0.0.1"), &h, &t).to_string(), "10.0.0.3");
        // нет заголовка — peer
        assert_eq!(
            client_ip(sa("127.0.0.1"), &HeaderMap::new(), &t).to_string(),
            "127.0.0.1"
        );
    }

    #[test]
    fn garbage_hop_stops_chain() {
        let t = nets(&["127.0.0.1/32"]);
        let h = xff(&["1.2.3.4, unknown"]);
        assert_eq!(client_ip(sa("127.0.0.1"), &h, &t).to_string(), "127.0.0.1");
    }

    #[test]
    fn mapped_ipv4_peer_is_trusted() {
        let t = nets(&["127.0.0.1/32"]);
        let peer = sa("::ffff:127.0.0.1");
        assert_eq!(
            client_ip(peer, &xff(&["1.2.3.4"]), &t).to_string(),
            "1.2.3.4"
        );
        assert_eq!(
            client_ip(peer, &HeaderMap::new(), &[]).to_string(),
            "127.0.0.1"
        );
    }

    #[test]
    fn https_only_via_trusted_proxy() {
        let t = nets(&["127.0.0.1/32"]);
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-proto", HeaderValue::from_static("HTTPS"));
        assert!(is_https(sa("127.0.0.1"), &h, &t));
        assert!(!is_https(sa("10.1.1.1"), &h, &t));
        h.insert("x-forwarded-proto", HeaderValue::from_static("https, http"));
        assert!(!is_https(sa("127.0.0.1"), &h, &t));
    }

    #[test]
    fn bearer_scheme_case_insensitive() {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("bearer  zam_x"),
        );
        assert_eq!(bearer(&h), Some("zam_x"));
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic abc"));
        assert_eq!(bearer(&h), None);
    }

    #[test]
    fn cookie_lookup_across_headers() {
        let mut h = HeaderMap::new();
        h.append(header::COOKIE, HeaderValue::from_static("a=1; b=2"));
        h.append(header::COOKIE, HeaderValue::from_static("zam_session=xyz"));
        assert_eq!(cookie_value(&h, SESSION_COOKIE).as_deref(), Some("xyz"));
        assert_eq!(cookie_value(&h, "c"), None);
    }
}
