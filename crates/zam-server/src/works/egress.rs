//! Куда сервер может обращаться к подключённой модели (M7.3, спецификация §6.2 пп. 1, 6; решения
//! координатора пп. 38, 78): разбор базового адреса подключения, класс каждого адреса имени
//! (петля, сам узел, link-local, LAN, вне LAN), проверка схемы и режима доступа и привязка
//! соединения к проверенным адресам.
//!
//! - **Адрес** — `data.url` записи Vault: схема `http`/`https`, хост, порт и путь (с `/v1`, как его
//!   дают поставщики); `userinfo`, запрос `?…` и фрагмент `#…` отвергаются ([`parse_base_url`]).
//! - **Класс адреса** ([`classify_ip`]): петля — `127/8`, `0.0.0.0/8`, `::1`, `::` и имя
//!   `localhost`; сам узел — адрес, который маршрут выбирает исходящим (UDP `connect` без
//!   отправки пакетов, `local_addr() == адрес`; `std`, без списка интерфейсов); link-local —
//!   `169.254/16`, `fe80::/10`; LAN — `10/8`, `172.16/12`, `192.168/16`, `100.64/10`, `fc00::/7`;
//!   прочее — вне LAN. IPv4-в-IPv6 (`::ffff:0:0/96`) — как IPv4.
//! - **Проверка** ([`check`], [`allowed`]): петля, сам узел и link-local — только при `[works]
//!   allow_loopback = true` (администратор); вне LAN — не при `model_access = "lan"` и не по
//!   `http://`; `model_access = "off"` — обращений нет. Проверяется **каждый** адрес имени.
//! - **Привязка** ([`route`], [`PinnedResolver`]): имя разрешается один раз за вызов, агент
//!   `ureq` соединяется только с адресами, прошедшими проверку (резолвер агента —
//!   `Agent::with_parts`, раздел `unversioned` `ureq` 3.4.2: при обновлении `ureq` — сверить).
//!   Через прокси окружения (только вне LAN) имя цели разрешает прокси — его выбирает
//!   администратор.
//!
//! Подмена резолвера для тестов — [`test_hooks`] (фича `test-hooks`): действует и на проверку, и на
//! соединение, поэтому тест хоста `api.openai.com` идёт без сети.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

use super::model::CallClass;

/// Сколько адресов имени берётся для соединения (предел `ureq` 3 — `MAX_ADDRS`); класс
/// считается по всем адресам.
const MAX_CONNECT_ADDRS: usize = 16;
/// Срок разрешения имени системой.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);
/// Предел длины базового адреса.
const BASE_URL_MAX: usize = 2048;

/// Базовый адрес подключения: схема (`http`/`https`, строчными), хост (строчными; IPv6 — без
/// скобок), порт и путь без завершающего `/` (`""` или `/v1`).
///
/// **Этап 1 задачи 2:** временный тип до слияния с задачей 1 — там `works::settings::BaseUrl`
/// (те же поля, serde) и `PinnedAddr = BaseUrl`; после перебазирования здесь остаётся `use`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BaseUrl {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// Режим доступа к модели `[works] model_access`.
///
/// **Этап 1 задачи 2:** временный тип до слияния с задачей 1 (`crate::config::ModelAccess`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelAccess {
    /// Любой адрес (вне LAN — с предупреждением).
    Any,
    /// Только LAN (и петля при `allow_loopback`).
    Lan,
    /// Режима `model` нет.
    Off,
}

/// Класс адреса (§6.2 п. 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    /// `127/8`, `0.0.0.0/8`, `::1`, `::`, имя `localhost`.
    Loopback,
    /// Собственный адрес сервера (в том числе его адрес LAN).
    SelfNode,
    /// `169.254/16` (там и адрес метаданных облака), `fe80::/10`.
    LinkLocal,
    /// `10/8`, `172.16/12`, `192.168/16`, `100.64/10`, `fc00::/7`.
    Lan,
    /// Всё остальное.
    External,
}

impl Reach {
    pub fn as_str(self) -> &'static str {
        match self {
            Reach::Loopback => "loopback",
            Reach::SelfNode => "self_node",
            Reach::LinkLocal => "link_local",
            Reach::Lan => "lan",
            Reach::External => "external",
        }
    }

    /// Адрес машины сервера или её сегмента: только при `allow_loopback` администратора.
    pub fn local_only(self) -> bool {
        matches!(self, Reach::Loopback | Reach::SelfNode | Reach::LinkLocal)
    }

    /// Строгость класса для «худшего из адресов имени»: адреса, запрещённые без
    /// `allow_loopback`, строже внешних, внешние — строже LAN.
    fn severity(self) -> u8 {
        match self {
            Reach::Lan => 0,
            Reach::External => 1,
            Reach::LinkLocal => 2,
            Reach::SelfNode => 3,
            Reach::Loopback => 4,
        }
    }
}

/// Почему обращение по адресу запрещено (класс вызова — [`CallClass::Blocked`]); строка — для
/// ответа `422` и причины в WebUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    /// `model_access = "off"`.
    AccessOff,
    /// Петля, сам узел или link-local без `allow_loopback`.
    LocalNeedsAdmin(Reach),
    /// `model_access = "lan"`, адрес вне LAN.
    LanOnly,
    /// `http://` вне LAN: ключ во внешнюю сеть открытым текстом не уходит.
    HttpExternal,
}

impl BlockReason {
    pub fn as_str(self) -> &'static str {
        match self {
            BlockReason::AccessOff => "model_access_off",
            BlockReason::LocalNeedsAdmin(_) => "loopback_requires_admin",
            BlockReason::LanOnly => "model_access_lan",
            BlockReason::HttpExternal => "http_external",
        }
    }

    /// Пояснение человеку.
    pub fn message(self) -> String {
        match self {
            BlockReason::AccessOff => {
                "обращения к модели выключены администратором ([works] model_access = \"off\")"
                    .into()
            }
            BlockReason::LocalNeedsAdmin(r) => format!(
                "адрес на машине сервера или в её сегменте ({}): нужен [works] allow_loopback = \
                 true (администратор)",
                r.as_str()
            ),
            BlockReason::LanOnly => {
                "адрес вне LAN, а администратор разрешил только LAN ([works] model_access = \
                 \"lan\")"
                    .into()
            }
            BlockReason::HttpExternal => {
                "http:// — только для LAN: вне LAN нужен https://, ключ открытым текстом не уходит"
                    .into()
            }
        }
    }
}

/// Политика доступа сервера: `[works] allow_loopback` и `model_access`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub allow_loopback: bool,
    pub access: ModelAccess,
}

/// Почему маршрут к модели не построен.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteError {
    /// Имя не разрешилось (нет адресов, срок, ошибка резолвера).
    Unresolved,
    /// Адрес запрещён политикой.
    Blocked(BlockReason),
}

impl RouteError {
    /// Класс вызова: имя не разрешилось — `unavailable`, запрет — `blocked`.
    pub fn class(self) -> CallClass {
        match self {
            RouteError::Unresolved => CallClass::Unavailable,
            RouteError::Blocked(_) => CallClass::Blocked,
        }
    }
}

/// Адреса имени с классом каждого (порядок — как у резолвера).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub addrs: Vec<(SocketAddr, Reach)>,
}

impl Resolved {
    /// Худший класс из адресов имени ([`Reach::severity`]).
    pub fn worst(&self) -> Reach {
        self.addrs
            .iter()
            .map(|(_, r)| *r)
            .max_by_key(|r| r.severity())
            .unwrap_or(Reach::External)
    }

    /// Хотя бы один адрес вне LAN — «вне LAN» (предупреждение, `external` журнала отправок).
    pub fn external(&self) -> bool {
        self.addrs.iter().any(|(_, r)| *r == Reach::External)
    }
}

/// Проверенный маршрут одного вызова: базовый адрес и адреса, с которыми только и можно
/// соединяться.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub base: BaseUrl,
    /// Адреса соединения (не больше [`MAX_CONNECT_ADDRS`]).
    pub addrs: Vec<SocketAddr>,
    /// Худший класс адресов имени.
    pub reach: Reach,
    /// Хотя бы один адрес вне LAN: соединение — через прокси окружения, если он задан.
    pub external: bool,
}

/// Разбирает базовый адрес подключения (§6.2 п. 1): `http`/`https`, без `userinfo`, `?…` и `#…`;
/// хост — IPv4, `[IPv6]` (без зоны) или имя ASCII (`xn--…` для IDN), порт 1..=65535 (по
/// умолчанию — схемы), путь без завершающего `/`. Ошибка — текст для `422`.
pub fn parse_base_url(s: &str) -> Result<BaseUrl, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("адрес подключения пуст".into());
    }
    if s.len() > BASE_URL_MAX {
        return Err(format!("адрес подключения длиннее {BASE_URL_MAX} символов"));
    }
    if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("в адресе подключения пробел или управляющий символ".into());
    }
    if s.contains('?') {
        return Err("в адресе подключения не должно быть запроса «?…»".into());
    }
    if s.contains('#') {
        return Err("в адресе подключения не должно быть фрагмента «#…»".into());
    }
    let Some((scheme, rest)) = s.split_once("://") else {
        return Err("адрес подключения — с схемой http:// или https://".into());
    };
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => return Err("схема адреса подключения — только http или https".into()),
    };
    let (authority, path) = rest.find('/').map_or((rest, ""), |i| rest.split_at(i));
    if authority.contains('@') {
        return Err(
            "в адресе подключения не должно быть имени пользователя и пароля («user@»): ключ — \
             значение записи Vault"
                .into(),
        );
    }
    if authority.is_empty() {
        return Err("в адресе подключения нет хоста".into());
    }
    let (host, port) = split_host_port(authority)?;
    let host = normalize_host(host)?;
    let port = port.unwrap_or(default_port);
    let path = path.trim_end_matches('/');
    check_path(path)?;
    Ok(BaseUrl {
        scheme,
        host,
        port,
        path: path.to_string(),
    })
}

/// Хост и порт из `authority` (`[v6]:p`, `host:p`, `host`).
fn split_host_port(authority: &str) -> Result<(&str, Option<u16>), String> {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let Some((h, after)) = rest.split_once(']') else {
            return Err("в адресе подключения не закрыта скобка IPv6".into());
        };
        let port = match after {
            "" => None,
            p => Some(
                p.strip_prefix(':')
                    .ok_or("после адреса IPv6 — только «:порт»")?,
            ),
        };
        if h.parse::<Ipv6Addr>().is_err() {
            return Err(format!("«{h}» — не адрес IPv6 (зона «%…» не допускается)"));
        }
        (h, port)
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    let port = match port {
        None => None,
        Some(p) => {
            let n = (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                .then(|| p.parse::<u16>().ok())
                .flatten()
                .filter(|&n| n != 0)
                .ok_or_else(|| format!("порт «{p}» — не число 1..=65535"))?;
            Some(n)
        }
    };
    Ok((host, port))
}

/// Хост строчными: IPv6 — как есть (без скобок), IPv4 — как есть, имя — метки `[a-z0-9_-]`
/// длиной 1..=63, всего ≤ 253; одна завершающая точка снимается.
fn normalize_host(host: &str) -> Result<String, String> {
    if host.parse::<Ipv6Addr>().is_ok() || host.parse::<Ipv4Addr>().is_ok() {
        return Ok(host.to_ascii_lowercase());
    }
    if !host.is_ascii() {
        return Err(format!(
            "хост «{host}» не ASCII: укажите имя в punycode (xn--…)"
        ));
    }
    let h = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    let label_ok = |l: &str| {
        (1..=63).contains(&l.len())
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    if h.is_empty() || h.len() > 253 || !h.split('.').all(label_ok) {
        return Err(format!("«{host}» — не имя хоста"));
    }
    Ok(h)
}

/// Путь — символы URI (`pchar` и `/`), `%` — только с двумя шестнадцатеричными цифрами.
fn check_path(path: &str) -> Result<(), String> {
    let b = path.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'%' {
            let hex = |j: usize| b.get(j).is_some_and(|x| x.is_ascii_hexdigit());
            if !(hex(i + 1) && hex(i + 2)) {
                return Err("в пути адреса подключения «%» без двух шестнадцатеричных цифр".into());
            }
            i += 3;
            continue;
        }
        let ok = c.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&c);
        if !ok {
            return Err(format!(
                "в пути адреса подключения недопустимый символ «{}»",
                path[i..].chars().next().unwrap_or('?')
            ));
        }
        i += 1;
    }
    Ok(())
}

/// Хост для URL: IPv6 — в скобках.
pub fn url_host(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

/// Полный адрес точки входа: `<схема>://<хост>[:<порт>]<путь><tail>` (порт схемы по умолчанию
/// опускается).
pub fn endpoint(base: &BaseUrl, tail: &str) -> String {
    let default_port = match base.scheme.as_str() {
        "https" => 443,
        _ => 80,
    };
    let port = if base.port == default_port {
        String::new()
    } else {
        format!(":{}", base.port)
    };
    format!(
        "{}://{}{port}{}{tail}",
        base.scheme,
        url_host(&base.host),
        base.path
    )
}

/// IPv4-в-IPv6 (`::ffff:a.b.c.d`) — как IPv4.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// Петля по §6.2 п. 6: `127/8`, `0.0.0.0/8`, `::1`, `::`.
fn is_loopback_range(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 127 || o[0] == 0
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
    }
}

/// Собственный адрес узла: маршрут к `ip` выбирает исходящим тот же адрес (UDP `connect` без
/// отправки пакетов). Нет маршрута или ошибка сокета — не свой.
fn is_own(ip: IpAddr) -> bool {
    let bind = match ip {
        IpAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        IpAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    };
    let Ok(sock) = UdpSocket::bind(bind) else {
        return false;
    };
    if sock.connect(SocketAddr::new(ip, 9)).is_err() {
        return false;
    }
    sock.local_addr().is_ok_and(|a| a.ip() == ip)
}

/// Класс адреса (§6.2 п. 6). Порядок проверок: петля → сам узел → link-local → LAN → вне LAN
/// (собственный адрес LAN сервера — «сам узел», а не LAN).
pub fn classify_ip(ip: IpAddr) -> Reach {
    let ip = canonical(ip);
    if is_loopback_range(ip) {
        return Reach::Loopback;
    }
    if is_own(ip) {
        return Reach::SelfNode;
    }
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            if o[0] == 169 && o[1] == 254 {
                Reach::LinkLocal
            } else if o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
                || (o[0] == 100 && (64..=127).contains(&o[1]))
            {
                Reach::Lan
            } else {
                Reach::External
            }
        }
        IpAddr::V6(v6) => {
            let s0 = v6.segments()[0];
            if s0 & 0xffc0 == 0xfe80 {
                Reach::LinkLocal
            } else if s0 & 0xfe00 == 0xfc00 {
                Reach::Lan
            } else {
                Reach::External
            }
        }
    }
}

/// Имя `localhost` и его поддомены (RFC 6761) — петля без обращения к резолверу.
fn is_localhost_name(host: &str) -> bool {
    let h = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    h == "localhost" || h.ends_with(".localhost")
}

/// Разрешает имя один раз и классифицирует каждый адрес: литерал IP — без резолвера, `localhost`
/// — `127.0.0.1` и `::1`, иначе — подмена `test-hooks` или системный резолвер (срок —
/// [`RESOLVE_TIMEOUT`]).
pub fn resolve(host: &str, port: u16) -> Result<Resolved, RouteError> {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    let (ips, forced): (Vec<IpAddr>, Option<Reach>) = if let Ok(ip) = h.parse::<IpAddr>() {
        (vec![ip], None)
    } else if is_localhost_name(h) {
        (
            vec![
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ],
            Some(Reach::Loopback),
        )
    } else {
        (lookup(h, port)?, None)
    };
    if ips.is_empty() {
        return Err(RouteError::Unresolved);
    }
    let addrs = ips
        .into_iter()
        .map(|ip| {
            let reach = forced.unwrap_or_else(|| classify_ip(ip));
            // IPv4-в-IPv6 соединяется как IPv4: сокет IPv6 Windows по умолчанию — только IPv6
            (SocketAddr::new(canonical(ip), port), reach)
        })
        .collect();
    Ok(Resolved { addrs })
}

/// Адреса имени: подмена `test-hooks` (если задана для имени), иначе системный резолвер в
/// отдельном потоке со сроком.
fn lookup(host: &str, port: u16) -> Result<Vec<IpAddr>, RouteError> {
    #[cfg(feature = "test-hooks")]
    if let Some(ips) = test_hooks::lookup(host) {
        return Ok(ips);
    }
    let name = format!("{}:{port}", url_host(host));
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("works-resolve".into())
        .spawn(move || {
            let r = name
                .to_socket_addrs()
                .map(|it| it.map(|a| a.ip()).collect::<Vec<_>>());
            let _ = tx.send(r);
        })
        .map_err(|_| RouteError::Unresolved)?;
    match rx.recv_timeout(RESOLVE_TIMEOUT) {
        Ok(Ok(ips)) => {
            let mut out: Vec<IpAddr> = Vec::with_capacity(ips.len());
            for ip in ips {
                if !out.contains(&ip) {
                    out.push(ip);
                }
            }
            Ok(out)
        }
        _ => Err(RouteError::Unresolved),
    }
}

/// Худший класс адресов имени (§6.2 п. 6); имя не разрешилось — `unavailable`.
pub fn resolve_reach(host: &str, port: u16) -> Result<Reach, CallClass> {
    resolve(host, port)
        .map(|r| r.worst())
        .map_err(RouteError::class)
}

/// Можно ли обращаться по адресу класса `reach` по схеме `scheme` (§6.2 п. 6) — с причиной
/// запрета.
pub fn check(reach: Reach, scheme: &str, policy: Policy) -> Result<(), BlockReason> {
    if policy.access == ModelAccess::Off {
        return Err(BlockReason::AccessOff);
    }
    if reach.local_only() {
        return if policy.allow_loopback {
            Ok(())
        } else {
            Err(BlockReason::LocalNeedsAdmin(reach))
        };
    }
    if reach == Reach::External {
        if policy.access == ModelAccess::Lan {
            return Err(BlockReason::LanOnly);
        }
        if !scheme.eq_ignore_ascii_case("https") {
            return Err(BlockReason::HttpExternal);
        }
    }
    Ok(())
}

/// [`check`] с классом вызова (`blocked`) вместо причины.
pub fn allowed(
    reach: Reach,
    scheme: &str,
    allow_loopback: bool,
    access: ModelAccess,
) -> Result<(), CallClass> {
    check(
        reach,
        scheme,
        Policy {
            allow_loopback,
            access,
        },
    )
    .map_err(|_| CallClass::Blocked)
}

/// Маршрут одного вызова: имя разрешается **один раз**, каждый адрес проходит [`check`];
/// соединение — только по этим адресам ([`PinnedResolver`]). `model_access = "off"` — без
/// разрешения имени.
pub fn route(base: &BaseUrl, policy: Policy) -> Result<Route, RouteError> {
    if policy.access == ModelAccess::Off {
        return Err(RouteError::Blocked(BlockReason::AccessOff));
    }
    let resolved = resolve(&base.host, base.port)?;
    for (_, reach) in &resolved.addrs {
        check(*reach, &base.scheme, policy).map_err(RouteError::Blocked)?;
    }
    Ok(Route {
        base: base.clone(),
        addrs: resolved
            .addrs
            .iter()
            .take(MAX_CONNECT_ADDRS)
            .map(|(a, _)| *a)
            .collect(),
        reach: resolved.worst(),
        external: resolved.external(),
    })
}

/// Резолвер агента `ureq` для одного вызова: хост и порт маршрута — только проверенные адреса
/// ([`route`]); прочие имена (прокси окружения у вызова вне LAN) — резолвер `ureq` по умолчанию.
#[derive(Debug)]
pub(crate) struct PinnedResolver {
    host: String,
    port: u16,
    addrs: Vec<SocketAddr>,
}

impl PinnedResolver {
    pub(crate) fn new(route: &Route) -> PinnedResolver {
        PinnedResolver {
            host: route.base.host.clone(),
            port: route.base.port,
            addrs: route.addrs.clone(),
        }
    }
}

impl ureq::unversioned::resolver::Resolver for PinnedResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let host = uri
            .host()
            .unwrap_or("")
            .trim_start_matches('[')
            .trim_end_matches(']');
        let port = uri.port_u16().or_else(|| match uri.scheme_str() {
            Some("https") => Some(443),
            Some("http") => Some(80),
            _ => None,
        });
        if host.eq_ignore_ascii_case(&self.host) && port == Some(self.port) {
            let mut out = self.empty();
            for a in &self.addrs {
                if out.try_push(*a).is_err() {
                    break;
                }
            }
            return if out.is_empty() {
                Err(ureq::Error::HostNotFound)
            } else {
                Ok(out)
            };
        }
        ureq::unversioned::resolver::DefaultResolver::default().resolve(uri, config, timeout)
    }
}

/// Тестовые точки: подмена адресов имени для [`resolve`] (и тем самым для соединения) и счётчик
/// разрешений имени (фича `test-hooks`).
#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub mod test_hooks {
    use std::collections::HashMap;
    use std::net::IpAddr;
    use std::sync::{Mutex, PoisonError};

    #[derive(Default)]
    struct Hosts {
        map: HashMap<String, Vec<IpAddr>>,
        lookups: HashMap<String, usize>,
    }

    static HOSTS: Mutex<Option<Hosts>> = Mutex::new(None);

    fn with<R>(f: impl FnOnce(&mut Hosts) -> R) -> R {
        let mut g = HOSTS.lock().unwrap_or_else(PoisonError::into_inner);
        f(g.get_or_insert_with(Hosts::default))
    }

    /// Имя `host` (без учёта регистра) разрешается в `ips` — и при проверке, и при соединении.
    pub fn set_host(host: &str, ips: &[IpAddr]) {
        with(|h| {
            h.map.insert(host.to_ascii_lowercase(), ips.to_vec());
        });
    }

    /// Снимает подмену имени.
    pub fn clear_host(host: &str) {
        with(|h| {
            h.map.remove(&host.to_ascii_lowercase());
        });
    }

    /// Сколько раз подменённое имя разрешалось.
    pub fn lookups(host: &str) -> usize {
        with(|h| {
            h.lookups
                .get(&host.to_ascii_lowercase())
                .copied()
                .unwrap_or(0)
        })
    }

    pub(super) fn lookup(host: &str) -> Option<Vec<IpAddr>> {
        let key = host.to_ascii_lowercase();
        with(|h| {
            let ips = h.map.get(&key).cloned()?;
            *h.lookups.entry(key).or_default() += 1;
            Some(ips)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    const ANY: Policy = Policy {
        allow_loopback: false,
        access: ModelAccess::Any,
    };
    const ADMIN: Policy = Policy {
        allow_loopback: true,
        access: ModelAccess::Any,
    };
    const LAN: Policy = Policy {
        allow_loopback: false,
        access: ModelAccess::Lan,
    };

    /// Диапазоны §6.2 п. 6 и их границы; IPv4-в-IPv6 — как IPv4. Адреса — из
    /// документационных диапазонов и частных сетей, которых нет у машин CI.
    #[test]
    fn classify_ip_ranges() {
        let cases = [
            ("127.0.0.1", Reach::Loopback),
            ("127.255.255.254", Reach::Loopback),
            ("0.0.0.0", Reach::Loopback),
            ("0.1.2.3", Reach::Loopback),
            ("::1", Reach::Loopback),
            ("::", Reach::Loopback),
            ("::ffff:127.0.0.1", Reach::Loopback),
            ("169.254.169.254", Reach::LinkLocal),
            ("::ffff:169.254.0.1", Reach::LinkLocal),
            ("fe80::1", Reach::LinkLocal),
            ("febf::1", Reach::LinkLocal),
            ("10.203.113.7", Reach::Lan),
            ("172.16.203.9", Reach::Lan),
            ("172.31.203.9", Reach::Lan),
            ("192.168.203.11", Reach::Lan),
            ("100.64.203.13", Reach::Lan),
            ("100.127.203.13", Reach::Lan),
            ("::ffff:10.203.113.7", Reach::Lan),
            ("fc00::5", Reach::Lan),
            ("fd12:3456::1", Reach::Lan),
            ("172.15.255.255", Reach::External),
            ("172.32.0.1", Reach::External),
            ("100.63.255.255", Reach::External),
            ("100.128.0.1", Reach::External),
            ("192.169.0.1", Reach::External),
            ("169.255.0.1", Reach::External),
            ("203.0.113.10", Reach::External),
            ("198.51.100.7", Reach::External),
            ("2001:db8::10", Reach::External),
            ("fec0::1", Reach::External),
        ];
        for (s, want) in cases {
            assert_eq!(classify_ip(ip(s)), want, "{s}");
        }
    }

    /// Литералы и `localhost` разрешаются без резолвера; худший класс и «вне LAN».
    #[test]
    fn resolve_literals_and_localhost() {
        let r = resolve("203.0.113.10", 443).unwrap();
        assert_eq!(
            r.addrs,
            vec![("203.0.113.10:443".parse().unwrap(), Reach::External)]
        );
        assert!(r.external());
        let r = resolve("::ffff:127.0.0.1", 8080).unwrap();
        assert_eq!(r.worst(), Reach::Loopback);
        for h in ["localhost", "LOCALHOST", "localhost.", "model.localhost"] {
            let r = resolve(h, 11434).unwrap();
            assert_eq!(r.worst(), Reach::Loopback, "{h}");
            assert!(r.addrs.iter().all(|(a, re)| a.port() == 11434
                && *re == Reach::Loopback
                && a.ip().is_loopback()));
        }
        assert_eq!(resolve_reach("10.203.113.7", 80), Ok(Reach::Lan));
        let mixed = Resolved {
            addrs: vec![
                ("10.203.113.7:80".parse().unwrap(), Reach::Lan),
                ("203.0.113.10:80".parse().unwrap(), Reach::External),
            ],
        };
        assert_eq!(mixed.worst(), Reach::External);
        assert!(mixed.external());
        let with_local = Resolved {
            addrs: vec![
                ("203.0.113.10:80".parse().unwrap(), Reach::External),
                ("127.0.0.1:80".parse().unwrap(), Reach::Loopback),
            ],
        };
        assert_eq!(with_local.worst(), Reach::Loopback);
    }

    /// Матрица проверки: класс × схема × политика.
    #[test]
    fn check_matrix() {
        for r in [Reach::Loopback, Reach::SelfNode, Reach::LinkLocal] {
            assert_eq!(check(r, "http", ANY), Err(BlockReason::LocalNeedsAdmin(r)));
            assert_eq!(check(r, "https", LAN), Err(BlockReason::LocalNeedsAdmin(r)));
            assert_eq!(check(r, "http", ADMIN), Ok(()));
            assert_eq!(
                allowed(r, "http", false, ModelAccess::Any),
                Err(CallClass::Blocked)
            );
            assert_eq!(allowed(r, "http", true, ModelAccess::Lan), Ok(()));
        }
        assert_eq!(check(Reach::Lan, "http", ANY), Ok(()));
        assert_eq!(check(Reach::Lan, "http", LAN), Ok(()));
        assert_eq!(check(Reach::External, "https", ANY), Ok(()));
        assert_eq!(
            check(Reach::External, "http", ANY),
            Err(BlockReason::HttpExternal)
        );
        assert_eq!(
            check(Reach::External, "https", LAN),
            Err(BlockReason::LanOnly)
        );
        let off = Policy {
            allow_loopback: true,
            access: ModelAccess::Off,
        };
        for r in [Reach::Loopback, Reach::Lan, Reach::External] {
            assert_eq!(check(r, "https", off), Err(BlockReason::AccessOff));
        }
    }

    /// Маршрут проверяет каждый адрес; `off` — без разрешения имени.
    #[test]
    fn route_checks_and_pins() {
        let base = parse_base_url("https://203.0.113.10/v1").unwrap();
        let r = route(&base, ANY).unwrap();
        assert_eq!(r.addrs, vec!["203.0.113.10:443".parse().unwrap()]);
        assert!(r.external);
        assert_eq!(r.reach, Reach::External);
        assert_eq!(
            route(&base, LAN),
            Err(RouteError::Blocked(BlockReason::LanOnly))
        );
        let http = parse_base_url("http://203.0.113.10/v1").unwrap();
        assert_eq!(
            route(&http, ANY).map_err(RouteError::class),
            Err(CallClass::Blocked)
        );
        let local = parse_base_url("http://localhost:11434/v1").unwrap();
        assert!(route(&local, ANY).is_err());
        let r = route(&local, ADMIN).unwrap();
        assert!(!r.external && r.reach == Reach::Loopback);
        let off = Policy {
            allow_loopback: true,
            access: ModelAccess::Off,
        };
        // имя, которого нет, не разрешается: `off` отвечает раньше резолвера
        let missing = parse_base_url("https://nonexistent.invalid/v1").unwrap();
        assert_eq!(
            route(&missing, off),
            Err(RouteError::Blocked(BlockReason::AccessOff))
        );
    }

    /// Разбор адреса: нормализация и отказы (`userinfo`, `?`, `#`, схема, порт, хост, путь).
    #[test]
    fn parse_base_url_cases() {
        let b = parse_base_url("  HTTPS://API.OpenAI.com/v1/  ").unwrap();
        assert_eq!(
            b,
            BaseUrl {
                scheme: "https".into(),
                host: "api.openai.com".into(),
                port: 443,
                path: "/v1".into()
            }
        );
        assert_eq!(
            endpoint(&b, "/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        let b = parse_base_url("http://[fd12::7]:8080/v1").unwrap();
        assert_eq!((b.host.as_str(), b.port), ("fd12::7", 8080));
        assert_eq!(endpoint(&b, "/x"), "http://[fd12::7]:8080/v1/x");
        let b = parse_base_url("http://192.0.2.5:11434").unwrap();
        assert_eq!((b.port, b.path.as_str()), (11434, ""));
        assert_eq!(
            endpoint(&b, "/chat/completions"),
            "http://192.0.2.5:11434/chat/completions"
        );
        let b = parse_base_url("https://model.example.com./compatible-mode/v1").unwrap();
        assert_eq!(b.host, "model.example.com");
        for bad in [
            "",
            "api.example.com/v1",
            "ftp://example.com/v1",
            "https://user:pw@example.com/v1",
            "https://key@example.com/v1",
            "https://example.com/v1?key=1",
            "https://example.com/v1#frag",
            "https://example.com:0/v1",
            "https://example.com:70000/v1",
            "https://example.com:/v1",
            "https://example.com:8a/v1",
            "https:///v1",
            "https://exa mple.com/v1",
            "https://пример.рф/v1",
            "https://-bad.example.com/v1",
            "https://[fe80::1%25eth0]/v1",
            "https://[::1/v1",
            "https://ex\\ample.com/v1",
            "https://example.com/v1%zz",
            "https://example.com/v1\\x",
        ] {
            assert!(parse_base_url(bad).is_err(), "{bad:?}");
        }
        let e = parse_base_url("https://u:p@example.com/v1").unwrap_err();
        assert!(e.contains("user@"), "{e}");
        let e = parse_base_url("https://example.com/v1?x").unwrap_err();
        assert!(e.contains('?'), "{e}");
    }
}
