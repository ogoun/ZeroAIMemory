//! Аутентификация: bearer-токены, cookie-сессии WebUI (администратор и пользователи, §11.4)
//! с CSRF, учёт прокси.
pub mod admin;
pub mod rate;
pub mod user;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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

/// Имя cookie веб-сессии WebUI (администратор и пользователи).
pub const SESSION_COOKIE: &str = "zam_session";
/// Заголовок CSRF для изменяющих запросов по cookie.
pub const CSRF_HEADER: &str = "x-csrf-token";
/// Заголовок выбора пользователя администратором.
pub const USER_HEADER: &str = "x-zam-user";

/// Состояние аутентификации процесса (лимиты, фиктивные хэши).
pub struct AuthState {
    /// Каждая попытка входа в WebUI — администратора и пользователей (общий лимит, §11.5).
    pub login: RateLimiter,
    /// Только неудачные проверки токена.
    pub bad_token: RateLimiter,
    /// Фиктивный хэш для неизвестного логина: с параметрами хэша администратора, вычисляется
    /// при старте (пароли пользователей — параметры по умолчанию, как у `hash-password`).
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
    Admin {
        session_id: String,
    },
    /// Пользователь в WebUI (§11.4): доступ как у `rw`-токена этого пользователя, плюс свой
    /// Human; пишет как человек (`author_kind = human`); `X-Zam-User` другого — 403,
    /// администраторские маршруты — 403.
    User {
        session_id: String,
        user_id: Id,
    },
}

impl Principal {
    /// Администратор (cookie-сессия из конфига): управляет сервисом, видит все базы.
    pub fn is_admin(&self) -> bool {
        matches!(self, Principal::Admin { .. })
    }

    /// Человек — сессия WebUI (администратор или пользователь); токен — всегда агент.
    pub fn is_human(&self) -> bool {
        !matches!(self, Principal::Token(_))
    }

    /// Id токена запроса (у сессий WebUI — нет).
    pub fn token_id(&self) -> Option<Id> {
        match self {
            Principal::Token(t) => Some(t.token_id),
            _ => None,
        }
    }

    /// Пользователь, к которому привязан принципал: владелец токена или пользователь
    /// сессии; у администратора — нет (он выбирает пользователя `X-Zam-User`).
    pub fn bound_user(&self) -> Option<Id> {
        match self {
            Principal::Token(t) => Some(t.user_id),
            Principal::User { user_id, .. } => Some(*user_id),
            Principal::Admin { .. } => None,
        }
    }

    /// Актор аудита: `admin`, `user` (сессия пользователя; кто — в `audit.user_id`) или id
    /// токена.
    pub fn actor(&self) -> String {
        match self {
            Principal::Token(t) => t.token_id.to_string(),
            Principal::Admin { .. } => "admin".into(),
            Principal::User { .. } => "user".into(),
        }
    }
}

/// Ячейка принципала запроса: слой, которому нужен принципал после обработчика (счётчик
/// записей в память сессии, [`crate::api::sessions::count_memory_writes`]), кладёт её в
/// расширения запроса, а извлечение [`Principal`] заполняет — без повторной проверки токена.
#[derive(Clone, Default)]
pub struct PrincipalSlot(Arc<Mutex<Option<Principal>>>);

impl PrincipalSlot {
    fn put(&self, p: &Principal) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(p.clone());
    }

    /// Принципал, если обработчик его извлёк.
    pub fn take(&self) -> Option<Principal> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
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
            Principal::User {
                session_id,
                user_id,
            } => f
                .debug_struct("User")
                .field("session_id", &mask_sid(session_id))
                .field("user_id", user_id)
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
pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
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

    /// Сессия пользователя с обязательной сменой пароля — 403 `password-change-required`
    /// (до смены доступны только [`Session`]-маршруты: `/auth/me`, `/auth/logout`,
    /// `/auth/password`).
    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, ApiError> {
        let a = authenticate(parts, st).await?;
        if a.must_change_password {
            return Err(ApiError::PasswordChangeRequired);
        }
        if let Some(slot) = parts.extensions.get::<PrincipalSlot>() {
            slot.put(&a.principal);
        }
        Ok(a.principal)
    }
}

/// Любая cookie-сессия WebUI (администратор или пользователь), в том числе с обязательной
/// сменой пароля; токен — 403. Для `/auth/me`, `/auth/logout`, `/auth/password`.
#[derive(Debug, Clone)]
pub struct Session {
    pub principal: Principal,
    pub must_change_password: bool,
}

impl FromRequestParts<AppState> for Session {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, ApiError> {
        let a = authenticate(parts, st).await?;
        if !a.principal.is_human() {
            return Err(ApiError::Forbidden);
        }
        Ok(Session {
            principal: a.principal,
            must_change_password: a.must_change_password,
        })
    }
}

/// Итог аутентификации запроса.
struct Authed {
    principal: Principal,
    /// Сессия пользователя, которому администратор задал пароль: до смены — только
    /// [`Session`]-маршруты.
    must_change_password: bool,
}

/// Принципал по bearer-токену или cookie-сессии (с CSRF на изменяющих запросах). Сессия
/// пользователя проверяется на каждом запросе: отключённый пользователь или закрытый вход
/// (пароль стёрт) — 401.
async fn authenticate(parts: &mut Parts, st: &AppState) -> Result<Authed, ApiError> {
    let Ok(client) = Client::from_request_parts(parts, st).await;
    if let Some(token) = bearer(&parts.headers) {
        return Ok(Authed {
            principal: authenticate_token(st, client.ip, token).await?,
            must_change_password: false,
        });
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
            tracing::warn!(ip = %client.ip, "запрос по cookie-сессии без верного CSRF-токена");
            return Err(ApiError::Forbidden);
        }
    }
    let Some(user_id) = ws.user_id else {
        return Ok(Authed {
            principal: Principal::Admin { session_id: sid },
            must_change_password: false,
        });
    };
    let user = st.system.get_user(user_id).await?;
    let Some(user) = user.filter(|u| u.disabled_at.is_none() && u.has_password) else {
        // пользователь отключён или вход закрыт после создания сессии
        st.system.delete_web_session(&sid).await?;
        return Err(ApiError::Unauthorized);
    };
    Ok(Authed {
        principal: Principal::User {
            session_id: sid,
            user_id,
        },
        must_change_password: user.must_change_password,
    })
}

impl FromRequestParts<AppState> for AdminOnly {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, ApiError> {
        match Principal::from_request_parts(parts, st).await? {
            Principal::Admin { session_id } => Ok(AdminOnly(session_id)),
            Principal::Token(_) | Principal::User { .. } => Err(ApiError::Forbidden),
        }
    }
}

/// Id пользователя, от имени которого действует запрос, без проверки, что он есть: для
/// токена — его владелец (`X-Zam-User` игнорируется); для сессии пользователя — он сам
/// (`X-Zam-User` другого пользователя — 403); для администратора — `X-Zam-User` или он сам.
pub fn target_user_id(st: &AppState, p: &Principal, headers: &HeaderMap) -> Result<Id, ApiError> {
    let header = || -> Result<Option<Id>, ApiError> {
        headers
            .get(USER_HEADER)
            .map(|v| {
                v.to_str()
                    .ok()
                    .and_then(|s| s.trim().parse::<Id>().ok())
                    .ok_or_else(|| ApiError::BadRequest("некорректный X-Zam-User".into()))
            })
            .transpose()
    };
    Ok(match p {
        Principal::Token(t) => t.user_id,
        Principal::User { user_id, .. } => match header() {
            Ok(None) => *user_id,
            Ok(Some(u)) if u == *user_id => *user_id,
            // чужой или нечитаемый X-Zam-User: сессия пользователя действует только за себя
            _ => return Err(ApiError::Forbidden),
        },
        Principal::Admin { .. } => header()?.unwrap_or(st.admin_id),
    })
}

/// Пользователь, от имени которого действует запрос ([`target_user_id`]).
/// Несуществующий пользователь — 404 (база для него не создаётся).
pub async fn target_user(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
) -> Result<User, ApiError> {
    let id = target_user_id(st, p, headers)?;
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
