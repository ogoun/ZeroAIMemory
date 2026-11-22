//! Поиск настроек подключения (§11.4): флаги → env → CLAUDE.md/AGENTS.md вверх от cwd → config.toml в home.
//! URL и токен разрешаются независимо; токен из config.toml в home — только для найденного адреса
//! (`[servers."<адрес>"]`, M7 §4.4).
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

/// Предки `start` (с ним самим) для поиска файлов проекта вверх: `CLAUDE.md`/`AGENTS.md`,
/// `.zeroaimemory/`, `solution.toml`. Как `GIT_CEILING_DIRECTORIES` у git: если `start` лежит
/// внутри каталога из `ZAM_CEILING_DIRECTORIES` (список путей, как `PATH`), поиск не смотрит сам
/// этот каталог и всё выше. Тесты workspace ставят потолок `target/`
/// (`.cargo/config.toml`): иначе из их каталогов поиск доходил до настоящих `CLAUDE.md` и
/// `.zeroaimemory/` репозитория, подключённого к памяти (обкатка 0.0.11-m6).
pub fn ancestors(start: &Path) -> impl Iterator<Item = &Path> {
    ancestors_below(start, ceilings())
}

fn ancestors_below<'a>(
    start: &'a Path,
    ceilings: &[Vec<String>],
) -> impl Iterator<Item = &'a Path> {
    let s = path_key(start);
    let active: Vec<Vec<String>> = ceilings
        .iter()
        .filter(|c| s.len() > c.len() && s.starts_with(c))
        .cloned()
        .collect();
    start.ancestors().take_while(move |d| {
        let d = path_key(d);
        active.iter().all(|c| d.len() > c.len() && d.starts_with(c))
    })
}

fn ceilings() -> &'static [Vec<String>] {
    static C: OnceLock<Vec<Vec<String>>> = OnceLock::new();
    C.get_or_init(|| {
        std::env::var_os(zam_core::CEILING_ENV)
            .map(|v| {
                std::env::split_paths(&v)
                    .filter(|p| p.is_absolute())
                    .map(|p| path_key(&p))
                    .filter(|k| k.len() > 1)
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Путь для сравнения: компоненты после лексической нормализации (`.` и `..`), префикс диска
/// без `\\?\`; на Windows — без учёта регистра.
fn path_key(p: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // префикс диска и корень `..` не снимает
    let mut base = 0;
    for c in p.components() {
        let s = match c {
            Component::CurDir => continue,
            Component::ParentDir => {
                if out.len() > base {
                    out.pop();
                }
                continue;
            }
            Component::Prefix(x) => {
                base += 1;
                let s = x.as_os_str().to_string_lossy();
                s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
            }
            Component::RootDir => {
                base += 1;
                "/".to_string()
            }
            Component::Normal(n) => n.to_string_lossy().into_owned(),
        };
        out.push(if cfg!(windows) { s.to_lowercase() } else { s });
    }
    out
}

/// Значения из флагов командной строки.
#[derive(Debug, Default, Clone)]
pub struct Flags {
    pub url: Option<String>,
    pub token: Option<String>,
}

/// Откуда взято значение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Flag,
    Env(&'static str),
    File(PathBuf),
    Home(PathBuf),
    /// Ключ домашнего конфига для адреса: `[servers."<адрес>"]` (адрес — ключ таблицы в файле).
    HomeServer(PathBuf, String),
}

impl Source {
    /// Текст для вывода пользователю.
    pub fn describe(&self) -> String {
        match self {
            Source::Flag => "flag".into(),
            Source::Env(n) => format!("env {n}"),
            Source::File(p) => format!("файл {}", p.display()),
            Source::Home(_) => "~/.zeroaimemory/config.toml".into(),
            Source::HomeServer(_, url) => {
                format!("~/.zeroaimemory/config.toml [servers.\"{url}\"]")
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Found {
    pub value: String,
    pub source: Source,
}

/// В Debug токен выводится маской (URL секретом не считается).
impl std::fmt::Debug for Found {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shown = if self.value.starts_with("zam_") {
            zam_core::mask_token(&self.value)
        } else {
            self.value.clone()
        };
        f.debug_struct("Found")
            .field("value", &shown)
            .field("source", &self.source)
            .finish()
    }
}

#[derive(Debug, Default, Clone)]
pub struct Resolved {
    pub url: Option<Found>,
    pub token: Option<Found>,
    /// Токена для найденного адреса нет, а в домашнем конфиге есть токены других адресов (или
    /// ключ без адреса при записях `[servers]`): на этот сервер они не отправляются (M7 §4.4).
    pub other_server_keys: bool,
    /// В домашнем конфиге, к которому обращался поиск токена, — ключ без адреса.
    pub unbound_token: Option<UnboundToken>,
}

/// Ключ без адреса в домашнем конфиге: `token` верхнего уровня без `url` (так писал
/// `zam init --url-to local` 0.0.13-m6.6; M7 §4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnboundToken {
    /// Маска ключа (`zam_XXXXXXXX…`), сам ключ сюда не попадает.
    pub mask: String,
    /// Он и есть найденный токен: записей `[servers]` с токеном нет — как в 0.0.13-m6.6, он
    /// уходит на любой адрес.
    pub used: bool,
}

/// Файлы настроек больше этого размера пропускаются.
const MAX_FILE: u64 = 1024 * 1024;

/// Результат чтения файла настроек.
enum Read {
    /// Нет файла, это не файл или он слишком велик.
    Skip,
    Text(String),
    /// Файл есть, но не прочитан.
    Failed(String),
}

fn read_limited(path: &Path) -> Read {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Read::Skip,
        Err(e) => return Read::Failed(e.to_string()),
    };
    if !meta.is_file() || meta.len() > MAX_FILE {
        return Read::Skip;
    }
    match std::fs::read(path) {
        Ok(b) if b.len() as u64 <= MAX_FILE => match String::from_utf8(b) {
            Ok(t) => Read::Text(t),
            Err(e) => Read::Failed(e.to_string()),
        },
        Ok(_) => Read::Skip,
        Err(e) => Read::Failed(e.to_string()),
    }
}

/// Адрес сервера должен быть http(s)://.
pub fn check_url(url: &str) -> Result<(), String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .filter(|r| !r.is_empty() && !r.starts_with('/'));
    match rest {
        Some(_) => Ok(()),
        None => Err(format!(
            "адрес сервера должен начинаться с http:// или https:// (получено: {url})"
        )),
    }
}

/// Порядок внутри одного каталога: локальные настройки важнее общих.
const FILES: [&str; 3] = ["CLAUDE.local.md", "CLAUDE.md", "AGENTS.md"];
const BEGIN: &str = "<!-- zeroaimemory:begin";
const END: &str = "<!-- zeroaimemory:end";

/// Значения из одного файла: (url, token); значения внутри блока важнее внешних.
fn parse_md(text: &str) -> (Option<String>, Option<String>) {
    let (inside, outside) = parse_md_parts(text);
    (inside.0.or(outside.0), inside.1.or(outside.1))
}

/// Токен внутри блока ZeroAIMemory (прежний блок v1 держал `ZeroAIMemoryToken=`): его
/// переносит `zam init`, заменяя блок (fix round 1, I-6).
pub fn block_token(text: &str) -> Option<String> {
    parse_md_parts(text).0.1
}

/// Адрес из своей строки `ZeroAIMemoryUrl=` вне блока ZeroAIMemory: `zam init --url-to
/// home|local` её не трогает (текст пользователя), а предупреждает — файл коммитится (M6.6).
pub fn outside_url(text: &str) -> Option<String> {
    parse_md_parts(text).1.0
}

type Pair = (Option<String>, Option<String>);

/// Ключ строки адреса сервера в CLAUDE.md/AGENTS.md/CLAUDE.local.md.
pub const URL_KEY: &str = "ZeroAIMemoryUrl";

/// Ключ и значение строки настроек `Ключ=значение` (допускаются `- `, `*`, обратные кавычки
/// вокруг и пробелы у `=`; значение без кавычек, может быть пустым). Одно правило для поиска
/// настроек и для `zam init`, который вырезает и пишет строку адреса (M6.6 §10.1).
pub fn md_setting(line: &str) -> Option<(&str, &str)> {
    let l = line
        .trim()
        .trim_start_matches(['-', '*', ' '])
        .trim_matches('`');
    let (k, v) = l.split_once('=')?;
    Some((k.trim(), v.trim().trim_matches(['`', '"', '\'']).trim()))
}

/// (url, token) внутри блока и вне его. BOM в начале файла пропускается (fix round 1, I-1).
fn parse_md_parts(text: &str) -> (Pair, Pair) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let (mut in_block, mut inside, mut outside) = (false, (None, None), (None, None));
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with(BEGIN) {
            in_block = true;
            continue;
        }
        if l.starts_with(END) {
            in_block = false;
            continue;
        }
        let Some((k, v)) = md_setting(l).filter(|(_, v)| !v.is_empty()) else {
            continue;
        };
        let slot = if in_block { &mut inside } else { &mut outside };
        match k {
            URL_KEY => slot.0 = Some(v.to_string()),
            "ZeroAIMemoryToken" => slot.1 = Some(v.to_string()),
            _ => {}
        }
    }
    (inside, outside)
}

/// `~/.zeroaimemory/config.toml` для поиска настроек (M7 §4.4): `url` — адрес для каталогов без
/// своего; токены — по адресу сервера `[servers."<адрес>"]`. Прежний `token` верхнего уровня
/// принадлежит `url` верхнего уровня, а без `url` (ключ без адреса) — любому адресу, пока записей
/// `[servers]` с токеном нет (как в 0.0.13-m6.6). Прочие секции файла (`[hooks]`, `[code]`) читают
/// свои модули.
#[derive(serde::Deserialize, Default)]
struct HomeConfig {
    url: Option<String>,
    token: Option<String>,
    #[serde(default)]
    servers: BTreeMap<String, ServerEntry>,
}

/// Запись `[servers."<адрес>"]`.
#[derive(serde::Deserialize, Default)]
struct ServerEntry {
    token: Option<String>,
}

impl HomeConfig {
    /// Токен адреса `url` (адреса сравниваются после [`normalize_url`]): `[servers."<адрес>"]`,
    /// иначе прежний `token` верхнего уровня — если `url` верхнего уровня этот адрес или если
    /// `url` нет, а записей `[servers]` с токеном нет (ключ без адреса, как в 0.0.13-m6.6). Токен
    /// другого адреса не берётся.
    fn token_for(&self, url: &str, path: &Path) -> Option<Found> {
        let want = normalize_url(url);
        if let Some((k, value)) = self.server_token(&want) {
            return Some(Found {
                value,
                source: Source::HomeServer(path.to_path_buf(), k.to_string()),
            });
        }
        let own = match nonempty(self.url.as_deref()) {
            Some(u) => normalize_url(&u) == want,
            None => !self.has_server_tokens(),
        };
        let value = nonempty(self.token.as_deref()).filter(|_| own)?;
        Some(Found {
            value,
            source: Source::Home(path.to_path_buf()),
        })
    }

    /// Ключ без адреса: `token` верхнего уровня без `url`.
    fn unbound_token(&self) -> Option<String> {
        nonempty(self.token.as_deref()).filter(|_| nonempty(self.url.as_deref()).is_none())
    }

    /// Есть ли записи `[servers]` с токеном.
    fn has_server_tokens(&self) -> bool {
        self.servers
            .values()
            .any(|e| nonempty(e.token.as_deref()).is_some())
    }

    /// Ключ записи `[servers."<адрес>"]` нормализованного адреса `want` и её токен (ключи таблицы
    /// сравниваются после [`normalize_url`]; запись без токена не в счёт).
    fn server_token(&self, want: &str) -> Option<(&str, String)> {
        self.servers
            .iter()
            .filter(|(k, _)| normalize_url(k) == want)
            .find_map(|(k, e)| Some((k.as_str(), nonempty(e.token.as_deref())?)))
    }

    /// Есть ли в файле токены (любых адресов, прежний `token` — тоже).
    fn has_tokens(&self) -> bool {
        nonempty(self.token.as_deref()).is_some() || self.has_server_tokens()
    }
}

/// Адрес сервера для сравнения и ключа `[servers."<адрес>"]` домашнего конфига (M7 §4.4): без
/// пробелов по краям и `/` в конце, схема и хост — в нижнем регистре; userinfo, порт, путь и
/// запрос — как есть. Строка без `://` — только без пробелов и `/` в конце.
pub fn normalize_url(u: &str) -> String {
    let u = u.trim().trim_end_matches('/');
    let Some((scheme, rest)) = u.split_once("://") else {
        return u.to_string();
    };
    let (authority, tail) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    let (user, host) = authority.split_at(authority.rfind('@').map_or(0, |i| i + 1));
    format!(
        "{}://{user}{}{tail}",
        scheme.to_ascii_lowercase(),
        host.to_lowercase()
    )
}

fn nonempty(v: Option<&str>) -> Option<String> {
    v.map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

pub fn resolve(
    flags: &Flags,
    env: &[(String, String)],
    cwd: &Path,
    home: &Path,
) -> Result<Resolved, String> {
    let mut r = Resolved::default();
    let get_env = |n: &str| nonempty(env.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str()));
    // 1–2. флаги и окружение
    if let Some(v) = nonempty(flags.url.as_deref()) {
        r.url = Some(Found {
            value: v,
            source: Source::Flag,
        });
    } else if let Some(v) = get_env("ZAM_URL") {
        r.url = Some(Found {
            value: v,
            source: Source::Env("ZAM_URL"),
        });
    }
    if let Some(v) = nonempty(flags.token.as_deref()) {
        r.token = Some(Found {
            value: v,
            source: Source::Flag,
        });
    } else if let Some(v) = get_env("ZAM_TOKEN") {
        r.token = Some(Found {
            value: v,
            source: Source::Env("ZAM_TOKEN"),
        });
    }
    // 3. файлы вверх от cwd: выигрывает ближайший каталог, где нашлось значение
    for dir in ancestors(cwd) {
        if r.url.is_some() && r.token.is_some() {
            break;
        }
        for name in FILES {
            let path = dir.join(name);
            // нечитаемый CLAUDE.md пропускаем: это не наш файл настроек
            let Read::Text(text) = read_limited(&path) else {
                continue;
            };
            let (u, t) = parse_md(&text);
            if r.url.is_none()
                && let Some(value) = u
            {
                r.url = Some(Found {
                    value,
                    source: Source::File(path.clone()),
                });
            }
            if r.token.is_none()
                && let Some(value) = t
            {
                r.token = Some(Found {
                    value,
                    source: Source::File(path),
                });
            }
        }
    }
    // 4. ~/.zeroaimemory/config.toml: адрес — `url`; токен — только найденного адреса (M7 §4.4),
    //    токен другого сервера на этот не отправляется
    if r.url.is_none() || r.token.is_none() {
        let path = home.join(".zeroaimemory").join("config.toml");
        let cfg: HomeConfig = match read_limited(&path) {
            Read::Skip => HomeConfig::default(),
            Read::Failed(e) => {
                return Err(format!("не удалось прочитать {}: {e}", path.display()));
            }
            // BOM в начале файла (fix round 3)
            Read::Text(t) => toml::from_str(t.strip_prefix('\u{feff}').unwrap_or(&t))
                .map_err(|e| format!("не удалось разобрать {}: {}", path.display(), e.message()))?,
        };
        if r.url.is_none()
            && let Some(value) = nonempty(cfg.url.as_deref())
        {
            r.url = Some(Found {
                value,
                source: Source::Home(path.clone()),
            });
        }
        if r.token.is_none()
            && let Some(url) = &r.url
        {
            r.token = cfg.token_for(&url.value, &path);
            r.other_server_keys = r.token.is_none() && cfg.has_tokens();
            // без `url` токен `Home` — только ключ без адреса (с `url` его бы не было)
            r.unbound_token = cfg.unbound_token().map(|t| UnboundToken {
                mask: zam_core::mask_token(&t),
                used: r
                    .token
                    .as_ref()
                    .is_some_and(|f| matches!(f.source, Source::Home(_))),
            });
        }
    }
    if let Some(u) = r.url.as_mut() {
        u.value = normalize_url(&u.value);
    }
    Ok(r)
}

/// Что `zam init` делает с `~/.zeroaimemory/config.toml` (§14.1; fix round 1, I-6; M7 §4.4).
#[derive(Debug, PartialEq, Eq)]
pub enum HomePlan {
    /// Там уже этот токен этого адреса (и адрес верхнего уровня, если он нужен).
    Unchanged,
    /// Записать `text` в `path`; `replaced` — заменяется другой токен этого адреса;
    /// `unbound_left` — маска ключа без адреса, который остаётся в файле и после записи больше не
    /// используется (ключи теперь по адресам).
    Write {
        path: PathBuf,
        text: String,
        replaced: bool,
        unbound_left: Option<String>,
    },
    /// Там другой токен этого адреса, а `--token` не задан: не трогать, предупредить.
    Conflict(PathBuf),
}

/// Что [`plan_home_aliases`] делает с `url` верхнего уровня — адресом для каталогов без своего
/// (M7 §4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopUrl {
    /// Не трогать (`--url-to local`).
    Keep,
    /// Записать, если его нет (`--url-to block`; 0.0.13-m6.6 перезаписывал его адресом каждого
    /// `zam init`); другой адрес не заменяется. Не пишется и тогда, когда в файле остаётся ключ
    /// без адреса: рядом с `url` он стал бы ключом этого адреса.
    IfAbsent,
    /// Записать, если его нет (`--url-to home`; другой адрес там `zam init` отклоняет раньше).
    /// Ключ без адреса рядом с `url` стал бы ключом этого адреса: другой токен — отказ.
    Set,
}

/// `~/.zeroaimemory/config.toml` таблицей TOML (нет файла — пустая) и для поиска настроек:
/// битый файл или не та форма (`servers` не таблица) — ошибка, а не тихая чистка.
fn home_doc(home: &Path) -> Result<(PathBuf, toml::Table, HomeConfig), String> {
    let path = home.join(".zeroaimemory").join("config.toml");
    let t: toml::Table = match read_limited(&path) {
        Read::Skip => toml::Table::new(),
        Read::Failed(e) => return Err(format!("не удалось прочитать {}: {e}", path.display())),
        Read::Text(s) => toml::from_str(s.strip_prefix('\u{feff}').unwrap_or(&s))
            .map_err(|e| format!("не удалось разобрать {}: {}", path.display(), e.message()))?,
    };
    let cfg = toml::Value::Table(t.clone())
        .try_into()
        .map_err(|e| format!("не удалось разобрать {}: {e}", path.display()))?;
    Ok((path, t, cfg))
}

/// Таблица `[servers]` из `t` (её нет — пустая; форму проверил [`home_doc`]).
fn take_servers(t: &mut toml::Table) -> toml::Table {
    match t.remove("servers") {
        Some(toml::Value::Table(s)) => s,
        _ => toml::Table::new(),
    }
}

/// Отказ `--url-to home` (M7 §4.4): ключ без адреса не становится ключом этого адреса молча и не
/// удаляется.
fn unbound_refusal(path: &Path, unbound: &str) -> String {
    format!(
        "{}: в домашнем конфиге есть ключ без адреса ({}) — привяжите его к его серверу: zam init \
         --url <адрес> --token … — или удалите строку token, затем повторите",
        path.display(),
        zam_core::mask_token(unbound)
    )
}

/// Маска ключа без адреса, который остаётся в домашнем конфиге и не используется — есть записи
/// `[servers]` с токеном — и не равен `token` (M7 §4.4); битый файл — ошибка.
pub fn unused_unbound_token(home: &Path, token: &str) -> Result<Option<String>, String> {
    let (_, _, cfg) = home_doc(home)?;
    Ok(cfg
        .unbound_token()
        .filter(|u| u != token && cfg.has_server_tokens())
        .map(|u| zam_core::mask_token(&u)))
}

/// [`plan_home_aliases`] без псевдонимов адреса — обёртка модульных тестов.
#[cfg(test)]
pub fn plan_home(
    home: &Path,
    url: &str,
    token: &str,
    force: bool,
    top: TopUrl,
) -> Result<HomePlan, String> {
    plan_home_aliases(home, url, &[], token, force, top)
}

/// План записи токена `token` адреса `url` в `~/.zeroaimemory/config.toml` (M7 §4.4): ключом
/// `[servers."<адрес>"]` (адрес — [`normalize_url`]); токены других адресов не трогаются,
/// `url` верхнего уровня — по `top` ([`TopUrl`]). Прежний формат переводится в новый без потери
/// ключа: `token` верхнего уровня уходит в `[servers."<url верхнего уровня>"]`, если там своего
/// нет (тогда действует свой, прежний и так не читался), а `url` остаётся. Ключ без адреса
/// (`token` без `url`), равный записываемому, становится ключом этого адреса; другой остаётся как
/// есть — `unbound_left` у [`HomePlan::Write`], а у [`TopUrl::Set`] — отказ. Остальные ключи
/// сохраняются (комментарии — нет: файл пишет `zam`). Другой токен этого адреса заменяется, только
/// если `force` (`--token`), иначе [`HomePlan::Conflict`]. Битый файл — ошибка.
///
/// Тот же токен пишется и под адресами `aliases` — другими адресами того же сервера (`zam init` в
/// режиме `block`: адрес строки `ZeroAIMemoryUrl=` блока — `server.public_url` или `Host` —
/// отличается от `--url`, а проект и хуки ищут токен по адресу блока; финальное ревью M7.1, I-3).
/// Другой токен под псевдонимом — как у адреса: замена только с `force`, иначе
/// [`HomePlan::Conflict`].
pub fn plan_home_aliases(
    home: &Path,
    url: &str,
    aliases: &[String],
    token: &str,
    force: bool,
    top: TopUrl,
) -> Result<HomePlan, String> {
    let (path, mut t, cfg) = home_doc(home)?;
    let key = normalize_url(url);
    let mut alias_keys: Vec<String> = aliases
        .iter()
        .map(|a| normalize_url(a))
        .filter(|a| !a.is_empty() && *a != key)
        .collect();
    alias_keys.sort_unstable();
    alias_keys.dedup();
    let top_token = nonempty(cfg.token.as_deref());
    // чей `token` верхнего уровня: `url` верхнего уровня; без него — ключ без адреса
    let owner = nonempty(cfg.url.as_deref()).map(|u| normalize_url(&u));
    let unbound_left = cfg.unbound_token().filter(|u| u != token);
    if top == TopUrl::Set
        && let Some(u) = &unbound_left
    {
        return Err(unbound_refusal(&path, u));
    }
    let old_of = |k: &str| {
        cfg.server_token(k)
            .map(|(_, v)| v)
            .or_else(|| top_token.clone().filter(|_| owner.as_deref() == Some(k)))
    };
    let olds: Vec<Option<String>> = std::iter::once(key.as_str())
        .chain(alias_keys.iter().map(String::as_str))
        .map(old_of)
        .collect();
    let migrate = top_token.is_some() && unbound_left.is_none();
    let write_url = top != TopUrl::Keep && owner.is_none() && unbound_left.is_none();
    if olds.iter().all(|o| o.as_deref() == Some(token)) && !migrate && !write_url {
        return Ok(HomePlan::Unchanged);
    }
    let replaced = olds
        .iter()
        .any(|o| o.as_deref().is_some_and(|o| o != token));
    if replaced && !force {
        return Ok(HomePlan::Conflict(path));
    }
    let mut servers = take_servers(&mut t);
    if migrate {
        t.remove("token");
        if let (Some(o), Some(lt)) = (&owner, &top_token)
            && cfg.server_token(o).is_none()
        {
            set_server_token(&mut servers, o, lt);
        }
    }
    set_server_token(&mut servers, &key, token);
    for a in &alias_keys {
        set_server_token(&mut servers, a, token);
    }
    t.insert("servers".into(), toml::Value::Table(servers));
    if write_url {
        t.insert("url".into(), toml::Value::String(key));
    }
    let text = toml::to_string(&t).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(HomePlan::Write {
        path,
        text,
        replaced,
        unbound_left: unbound_left.map(|u| zam_core::mask_token(&u)),
    })
}

/// Токен `token` адреса `key` (нормализованного) — в `[servers."<key>"]`; записи того же адреса
/// под другим написанием ключа сливаются в эту (их прочие поля сохраняются).
fn set_server_token(servers: &mut toml::Table, key: &str, token: &str) {
    let same: Vec<String> = servers
        .keys()
        .filter(|k| normalize_url(k) == key)
        .cloned()
        .collect();
    let mut entry = toml::Table::new();
    for k in same {
        if let Some(toml::Value::Table(e)) = servers.remove(&k) {
            for (f, v) in e {
                entry.insert(f, v);
            }
        }
    }
    entry.insert("token".into(), toml::Value::String(token.to_string()));
    servers.insert(key.to_string(), toml::Value::Table(entry));
}

/// Что `zam init --url-to home` делает с адресом в `~/.zeroaimemory/config.toml` (M6.6 §10.1).
#[derive(Debug, PartialEq, Eq)]
pub enum HomeUrlPlan {
    /// Там уже этот адрес.
    Unchanged,
    /// Записать `text` в `path`: адрес добавлен, остальное как было.
    Write { path: PathBuf, text: String },
    /// Там другой адрес `old`: он общий для всех проектов машины — не заменять.
    Conflict { path: PathBuf, old: String },
}

/// План записи адреса `url` в `~/.zeroaimemory/config.toml` (`--url-to home`, M6.6 §10.1;
/// M7 §4.4): токены других адресов и остальные ключи не трогаются; там уже другой адрес
/// (сравнение — после [`normalize_url`]) — [`HomeUrlPlan::Conflict`]. Ключ без адреса рядом с
/// `url` стал бы ключом этого адреса: равный `token` (записываемому или найденному для адреса)
/// переносится в `[servers."<адрес>"]`, другой (или у адреса уже свой ключ) — отказ, файл не
/// меняется. Битый файл — ошибка.
pub fn plan_home_url(home: &Path, url: &str, token: &str) -> Result<HomeUrlPlan, String> {
    let (path, mut t, cfg) = home_doc(home)?;
    let url = normalize_url(url);
    match nonempty(cfg.url.as_deref()).map(|u| normalize_url(&u)) {
        Some(old) if old == url => return Ok(HomeUrlPlan::Unchanged),
        Some(old) => return Ok(HomeUrlPlan::Conflict { path, old }),
        None => {}
    }
    if let Some(u) = cfg.unbound_token() {
        let own = cfg.server_token(&url).map(|(_, v)| v);
        if u != token || own.is_some_and(|o| o != u) {
            return Err(unbound_refusal(&path, &u));
        }
        t.remove("token");
        let mut servers = take_servers(&mut t);
        set_server_token(&mut servers, &url, &u);
        t.insert("servers".into(), toml::Value::Table(servers));
    }
    t.insert("url".into(), toml::Value::String(url));
    let text = toml::to_string(&t).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(HomeUrlPlan::Write { path, text })
}

/// Ближайший вверх от `start` файл решения: в каждом каталоге сначала
/// `.zeroaimemory/solution.toml` (§11.1), затем `solution.toml` (совместимость с M0).
pub fn find_solution(start: &Path) -> Option<PathBuf> {
    ancestors(start)
        .flat_map(|d| {
            [
                d.join(".zeroaimemory").join("solution.toml"),
                d.join("solution.toml"),
            ]
        })
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp/zam-cli")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn res(f: &Flags, e: &[(String, String)], cwd: &Path, home: &Path) -> Resolved {
        resolve(f, e, cwd, home).unwrap()
    }

    const TOKEN: &str = "zam_abcdefghijklmnopqrstuvwxyz0123456789ABCD";

    fn block(url: &str, token: &str) -> String {
        format!(
            "# проект\n<!-- zeroaimemory:begin v1 -->\nZeroAIMemoryUrl={url}\nZeroAIMemoryToken={token}\n<!-- zeroaimemory:end -->\n"
        )
    }

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn ceiling_stops_upward_search() {
        let root = if cfg!(windows) { "C:/w" } else { "/w" };
        let p = |s: &str| PathBuf::from(format!("{root}/{s}"));
        let c = vec![path_key(&p("repo/target/test-tmp"))];
        let start = p("repo/crates/x/../../target/test-tmp/t1/a/b");
        let got: Vec<Vec<String>> = ancestors_below(&start, &c).map(path_key).collect();
        assert_eq!(
            got.len(),
            3,
            "a/b, a, t1 — без самого потолка и выше: {got:?}"
        );
        assert_eq!(
            *got.last().unwrap(),
            path_key(&p("repo/target/test-tmp/t1"))
        );
        // вне потолка — поиск до корня, как раньше
        let out = p("repo/src/m");
        assert_eq!(ancestors_below(&out, &c).count(), out.ancestors().count());
        // сам потолок как стартовый каталог — без ограничения
        let at = p("repo/target/test-tmp");
        assert_eq!(ancestors_below(&at, &c).count(), at.ancestors().count());
        if cfg!(windows) {
            let upper = PathBuf::from("C:/W/REPO/TARGET/TEST-TMP/t2");
            assert_eq!(ancestors_below(&upper, &c).count(), 1);
        }
        assert_eq!(path_key(Path::new("a/../../b")), vec!["b".to_string()]);
    }

    #[test]
    fn flag_beats_env_beats_file_beats_home() {
        let root = tmp("order");
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".zeroaimemory")).unwrap();
        std::fs::write(
            home.join(".zeroaimemory/config.toml"),
            "url = \"http://home:1\"\ntoken = \"home-token\"\n",
        )
        .unwrap();
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("CLAUDE.md"), block("http://file:2", "file-token")).unwrap();
        let e = env(&[("ZAM_URL", "http://env:3"), ("ZAM_TOKEN", "env-token")]);
        let f = Flags {
            url: Some("http://flag:4".into()),
            token: Some("flag-token".into()),
        };

        let r = res(&f, &e, &proj, &home);
        assert_eq!(r.url.as_ref().unwrap().source, Source::Flag);
        assert_eq!(r.url.unwrap().value, "http://flag:4");
        assert_eq!(r.token.unwrap().source, Source::Flag);

        let r = res(&Flags::default(), &e, &proj, &home);
        assert_eq!(r.url.as_ref().unwrap().source, Source::Env("ZAM_URL"));
        assert_eq!(r.token.unwrap().source, Source::Env("ZAM_TOKEN"));

        let r = res(&Flags::default(), &[], &proj, &home);
        assert_eq!(
            r.url.as_ref().unwrap().source,
            Source::File(proj.join("CLAUDE.md"))
        );
        assert_eq!(r.url.unwrap().value, "http://file:2");
        assert_eq!(r.token.unwrap().value, "file-token");

        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let r = res(&Flags::default(), &[], &empty, &home);
        let u = r.url.unwrap();
        assert_eq!(
            u.source,
            Source::Home(home.join(".zeroaimemory/config.toml"))
        );
        assert_eq!(u.value, "http://home:1");
        assert_eq!(r.token.unwrap().value, "home-token");
    }

    #[test]
    fn finds_block_in_parent_dir_claude_md() {
        let root = tmp("parent");
        let cwd = root.join("a/b");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(root.join("CLAUDE.md"), block("http://srv:7700", TOKEN)).unwrap();
        let r = res(&Flags::default(), &[], &cwd, &root.join("nohome"));
        let u = r.url.unwrap();
        assert_eq!(u.source, Source::File(root.join("CLAUDE.md")));
        assert_eq!(u.value, "http://srv:7700");
        assert_eq!(r.token.unwrap().value, TOKEN);
    }

    #[test]
    fn url_and_token_resolved_independently() {
        let root = tmp("indep");
        std::fs::write(root.join("CLAUDE.md"), block("http://file:1", TOKEN)).unwrap();
        let e = env(&[("ZAM_URL", "http://env:2")]);
        let r = res(&Flags::default(), &e, &root, &root.join("nohome"));
        assert_eq!(r.url.as_ref().unwrap().source, Source::Env("ZAM_URL"));
        let t = r.token.unwrap();
        assert_eq!(t.source, Source::File(root.join("CLAUDE.md")));
        assert_eq!(t.value, TOKEN);
    }

    #[test]
    fn crlf_claude_md_parsed() {
        let root = tmp("crlf");
        let text = block("http://srv:7700", TOKEN).replace('\n', "\r\n");
        std::fs::write(root.join("CLAUDE.md"), text).unwrap();
        let r = res(&Flags::default(), &[], &root, &root.join("nohome"));
        assert_eq!(r.url.unwrap().value, "http://srv:7700");
        assert_eq!(r.token.unwrap().value, TOKEN);
    }

    #[test]
    fn block_values_beat_outside_values_and_spaces_are_allowed() {
        let root = tmp("blockprio");
        let text = format!(
            "ZeroAIMemoryUrl = http://outside:1\n<!-- zeroaimemory:begin v2 -->\nZeroAIMemoryUrl = `http://inside:2/`\n<!-- zeroaimemory:end -->\nZeroAIMemoryToken={TOKEN}\n"
        );
        std::fs::write(root.join("CLAUDE.md"), text).unwrap();
        let r = res(&Flags::default(), &[], &root, &root.join("nohome"));
        assert_eq!(r.url.unwrap().value, "http://inside:2");
        assert_eq!(r.token.unwrap().value, TOKEN);
    }

    #[test]
    fn nearest_dir_wins_and_file_order_in_dir() {
        let root = tmp("nearest");
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "ZeroAIMemoryUrl=http://far:1\n").unwrap();
        std::fs::write(sub.join("AGENTS.md"), "ZeroAIMemoryUrl=http://agents:2\n").unwrap();
        std::fs::write(
            sub.join("CLAUDE.local.md"),
            "ZeroAIMemoryUrl=http://local:3\n",
        )
        .unwrap();
        std::fs::write(sub.join("CLAUDE.md"), "ZeroAIMemoryUrl=http://claude:4\n").unwrap();
        // В каталоге: CLAUDE.local.md → CLAUDE.md → AGENTS.md.
        let r = res(&Flags::default(), &[], &sub, &root.join("nohome"));
        assert_eq!(r.url.unwrap().value, "http://local:3");
        std::fs::remove_file(sub.join("CLAUDE.local.md")).unwrap();
        let r = res(&Flags::default(), &[], &sub, &root.join("nohome"));
        assert_eq!(r.url.unwrap().value, "http://claude:4");
        std::fs::remove_file(sub.join("CLAUDE.md")).unwrap();
        let r = res(&Flags::default(), &[], &sub, &root.join("nohome"));
        assert_eq!(r.url.unwrap().value, "http://agents:2");
    }

    #[test]
    fn url_trailing_slash_trimmed_and_empty_env_ignored() {
        let root = tmp("norm");
        let e = env(&[("ZAM_URL", "http://env:2//"), ("ZAM_TOKEN", "")]);
        let r = res(&Flags::default(), &e, &root, &root.join("nohome"));
        assert_eq!(r.url.unwrap().value, "http://env:2");
        assert!(r.token.is_none());
    }

    #[test]
    fn finds_solution_toml_upwards() {
        let root = tmp("solution");
        let cwd = root.join("x/y");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(root.join("solution.toml"), "").unwrap();
        assert_eq!(find_solution(&cwd), Some(root.join("solution.toml")));
        // .zeroaimemory/solution.toml ближе — выигрывает
        let mid = root.join("x");
        std::fs::create_dir_all(mid.join(".zeroaimemory")).unwrap();
        let f = mid.join(".zeroaimemory").join("solution.toml");
        std::fs::write(&f, "").unwrap();
        assert_eq!(find_solution(&cwd), Some(f));
    }

    #[test]
    fn describe_sources() {
        assert_eq!(Source::Flag.describe(), "flag");
        assert_eq!(Source::Env("ZAM_URL").describe(), "env ZAM_URL");
        assert_eq!(
            Source::Home("h".into()).describe(),
            "~/.zeroaimemory/config.toml"
        );
        assert!(Source::File("p".into()).describe().starts_with("файл "));
    }

    #[test]
    fn broken_home_config_is_error_with_path() {
        let root = tmp("brokenhome");
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".zeroaimemory")).unwrap();
        let cfg = home.join(".zeroaimemory").join("config.toml");
        std::fs::write(&cfg, "url = [broken").unwrap();
        let e = resolve(&Flags::default(), &[], &root, &home).unwrap_err();
        assert!(e.contains(&cfg.display().to_string()), "{e}");
        // отсутствие файла — не ошибка
        assert!(resolve(&Flags::default(), &[], &root, &root.join("nohome")).is_ok());
    }

    #[test]
    fn oversized_files_skipped() {
        let root = tmp("big");
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let mut big = "x".repeat(MAX_FILE as usize + 1);
        big.push_str("\nZeroAIMemoryUrl=http://big:1\n");
        std::fs::write(sub.join("CLAUDE.md"), big).unwrap();
        std::fs::write(sub.join("AGENTS.md"), "ZeroAIMemoryUrl=http://next:2\n").unwrap();
        let r = res(&Flags::default(), &[], &sub, &root.join("nohome"));
        assert_eq!(r.url.unwrap().value, "http://next:2");
        // слишком большой config.toml тоже пропускается
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".zeroaimemory")).unwrap();
        std::fs::write(
            home.join(".zeroaimemory/config.toml"),
            format!("#{}\nurl = \"http://h:1\"", "x".repeat(MAX_FILE as usize)),
        )
        .unwrap();
        assert!(res(&Flags::default(), &[], &root, &home).url.is_none());
    }

    #[test]
    fn debug_masks_token() {
        let f = Found {
            value: TOKEN.into(),
            source: Source::Flag,
        };
        let r = Resolved {
            token: Some(f.clone()),
            ..Resolved::default()
        };
        assert!(!format!("{f:?}").contains(TOKEN));
        assert!(!format!("{r:?}").contains(TOKEN));
        assert!(format!("{f:?}").contains("zam_abcdefgh"));
    }

    /// M6.6 §10.1: `--url-to home` пишет адрес (токены других адресов и свои ключи — как были),
    /// тот же адрес — без записи, другой — отказ; `plan_home` без адреса (`--url-to local`) адрес
    /// домашнего конфига не трогает. M7 §4.4 (круг правок 1): ключ без адреса рядом с `url` —
    /// только свой токен (переносится в `[servers]`), иначе отказ без записи.
    #[test]
    fn home_url_plan_keeps_token_and_refuses_other_url() {
        let root = tmp("homeurl");
        let home = root.join("home");
        let cfg = home.join(".zeroaimemory").join("config.toml");
        let write = |s: &str| {
            std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
            std::fs::write(&cfg, s).unwrap();
        };
        let u = "http://192.0.2.5:7700";
        match plan_home_url(&home, &format!("{u}/"), "t").unwrap() {
            HomeUrlPlan::Write { path, text } => {
                assert_eq!(path, cfg);
                assert_eq!(text, format!("url = \"{u}\"\n"));
            }
            p => panic!("{p:?}"),
        }
        write(&format!("token = \"t\"\nurl = \"{u}/\"\n"));
        assert_eq!(
            plan_home_url(&home, u, "t").unwrap(),
            HomeUrlPlan::Unchanged
        );
        // адреса сравниваются после нормализации (M7 §4.4)
        write("url = \"HTTP://192.0.2.5:7700/\"\n");
        assert_eq!(
            plan_home_url(&home, u, "t").unwrap(),
            HomeUrlPlan::Unchanged
        );
        write("url = \"http://198.51.100.1:7700\"\n");
        assert_eq!(
            plan_home_url(&home, u, "t").unwrap(),
            HomeUrlPlan::Conflict {
                path: cfg.clone(),
                old: "http://198.51.100.1:7700".into()
            }
        );
        // ключ без адреса и свой токен — ключ этого адреса, свои ключи файла — как были
        let unbound = "token = \"t\"\n[hooks]\nprompt = false\n\n[servers.\"http://192.0.2.6:7700\"]\ntoken = \"t6\"\n";
        write(unbound);
        let HomeUrlPlan::Write { text, .. } = plan_home_url(&home, u, "t").unwrap() else {
            panic!("запись адреса");
        };
        let t: toml::Table = toml::from_str(&text).unwrap();
        assert!(t.get("token").is_none(), "{text}");
        assert_eq!(t["servers"][u]["token"].as_str(), Some("t"), "{text}");
        assert_eq!(
            t["servers"]["http://192.0.2.6:7700"]["token"].as_str(),
            Some("t6")
        );
        assert_eq!(t["url"].as_str(), Some(u));
        assert_eq!(t["hooks"]["prompt"].as_bool(), Some(false));
        // другой токен или у адреса уже свой ключ — отказ с маской
        let e = plan_home_url(&home, u, "t3").unwrap_err();
        assert!(
            e.contains("ключ без адреса") && e.contains("удалите строку token"),
            "{e}"
        );
        write(&format!("{unbound}\n[servers.\"{u}\"]\ntoken = \"t5\"\n"));
        assert!(plan_home_url(&home, u, "t").is_err());
        write("url = [");
        assert!(plan_home_url(&home, u, "t").is_err());
        // токен другого адреса: `--url-to local` адрес домашнего конфига не трогает, block другой
        // адрес не заменяет
        write("url = \"http://198.51.100.1:7700\"\n");
        for top in [TopUrl::Keep, TopUrl::IfAbsent] {
            let HomePlan::Write { text, replaced, .. } =
                plan_home(&home, u, "t2", true, top).unwrap()
            else {
                panic!("запись токена: {top:?}");
            };
            assert!(!replaced);
            let t: toml::Table = toml::from_str(&text).unwrap();
            assert_eq!(
                t["url"].as_str(),
                Some("http://198.51.100.1:7700"),
                "{text}"
            );
            assert_eq!(t["servers"][u]["token"].as_str(), Some("t2"), "{text}");
        }
    }

    /// Домашний конфиг для ключей по адресу (M7 §4.4).
    fn home_with(name: &str, text: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = tmp(name);
        let home = root.join("home");
        let cfg = home.join(".zeroaimemory").join("config.toml");
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, text).unwrap();
        (root, home, cfg)
    }

    fn at(url: &str) -> Flags {
        Flags {
            url: Some(url.into()),
            token: None,
        }
    }

    /// M7 §4.4: прежние `url`/`token` верхнего уровня — ключ этого `url`, `[servers."<адрес>"]` —
    /// ключ своего адреса (с «/» в конце и в другом регистре хоста — тот же); адрес без ключа —
    /// без токена, ключ другого адреса не берётся; флаг, `ZAM_TOKEN` и файл проекта сильнее.
    #[test]
    fn home_config_legacy_and_two_servers() {
        let (root, home, cfg) = home_with(
            "servers",
            "url = \"http://a:1\"\ntoken = \"t1\"\n\n[servers.\"http://b:2\"]\ntoken = \"t2\"\n",
        );
        // адрес проекта — из CLAUDE.md
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("CLAUDE.md"), "ZeroAIMemoryUrl=http://b:2/\n").unwrap();
        let r = res(&Flags::default(), &[], &proj, &home);
        assert_eq!(r.url.as_ref().unwrap().value, "http://b:2");
        let t = r.token.unwrap();
        assert_eq!(t.value, "t2");
        assert_eq!(
            t.source,
            Source::HomeServer(cfg.clone(), "http://b:2".into())
        );
        assert_eq!(
            t.source.describe(),
            "~/.zeroaimemory/config.toml [servers.\"http://b:2\"]"
        );
        let t = res(&at("http://A:1"), &[], &root, &home).token.unwrap();
        assert_eq!(t.value, "t1");
        assert_eq!(t.source, Source::Home(cfg.clone()));
        let r = res(&at("http://c:3"), &[], &root, &home);
        assert!(r.token.is_none(), "{r:?}");
        assert!(r.other_server_keys);
        // адрес — из домашнего конфига, ключ — его
        let r = res(&Flags::default(), &[], &root, &home);
        assert_eq!(r.url.unwrap().value, "http://a:1");
        assert_eq!(r.token.unwrap().value, "t1");
        assert!(!r.other_server_keys);
        // флаг, ZAM_TOKEN и ключ в файлах проекта — сильнее домашнего конфига
        let f = Flags {
            url: Some("http://b:2".into()),
            token: Some("flag".into()),
        };
        assert_eq!(res(&f, &[], &root, &home).token.unwrap().value, "flag");
        let e = env(&[("ZAM_TOKEN", "env")]);
        assert_eq!(
            res(&at("http://b:2"), &e, &root, &home)
                .token
                .unwrap()
                .value,
            "env"
        );
        std::fs::write(proj.join("CLAUDE.md"), block("http://b:2", "file")).unwrap();
        assert_eq!(
            res(&Flags::default(), &[], &proj, &home)
                .token
                .unwrap()
                .value,
            "file"
        );
    }

    /// M7 §4.4: прежний формат — ключ только своего `url`.
    #[test]
    fn home_token_not_sent_to_other_server() {
        let (root, home, cfg) = home_with("othersrv", "url = \"http://a:1\"\ntoken = \"t1\"\n");
        let r = res(&at("http://b:2"), &[], &root, &home);
        assert_eq!(r.url.as_ref().unwrap().value, "http://b:2");
        assert!(r.token.is_none(), "{r:?}");
        assert!(r.other_server_keys);
        assert!(r.unbound_token.is_none(), "{r:?}");
        // ключей нет вовсе — признака нет
        std::fs::write(&cfg, "[hooks]\nprompt = false\n").unwrap();
        let r = res(&at("http://a:1"), &[], &root, &home);
        assert!(r.token.is_none() && !r.other_server_keys, "{r:?}");
    }

    /// M7 §4.4 (решение координатора): ключ без адреса (`token` без `url`, так писал
    /// `--url-to local` 0.0.13-m6.6) уходит на любой адрес, пока записей `[servers]` с токеном нет
    /// (пустая таблица `[servers]` — не в счёт), как в 0.0.13-m6.6; с ними — не используется.
    /// Признак — [`Resolved::unbound_token`] с маской, без самого ключа.
    #[test]
    fn unbound_token_used_until_server_keys() {
        let t1 = ["zam_", &"1".repeat(40)].concat();
        let (root, home, cfg) = home_with("unbound", &format!("token = \"{t1}\"\n\n[servers]\n"));
        let mask = zam_core::mask_token(&t1);
        assert_ne!(mask, t1);
        for u in ["http://a:1", "http://b:2"] {
            let r = res(&at(u), &[], &root, &home);
            let t = r.token.as_ref().unwrap();
            assert_eq!(t.value, t1);
            assert_eq!(t.source, Source::Home(cfg.clone()));
            let unbound = UnboundToken {
                mask: mask.clone(),
                used: true,
            };
            assert_eq!(r.unbound_token, Some(unbound), "{u}");
            assert!(!r.other_server_keys);
        }
        std::fs::write(
            &cfg,
            format!("token = \"{t1}\"\n\n[servers.\"http://b:2\"]\ntoken = \"t2\"\n"),
        )
        .unwrap();
        let unused = Some(UnboundToken {
            mask: mask.clone(),
            used: false,
        });
        let r = res(&at("http://a:1"), &[], &root, &home);
        assert!(r.token.is_none(), "{r:?}");
        assert!(r.other_server_keys);
        assert_eq!(r.unbound_token, unused);
        assert!(!format!("{r:?}").contains(&t1));
        let r = res(&at("http://b:2"), &[], &root, &home);
        assert_eq!(r.token.unwrap().value, "t2");
        assert_eq!(r.unbound_token, unused);
    }

    #[test]
    fn normalize_url_lowercases_scheme_and_host_only() {
        assert_eq!(
            normalize_url(" HTTP://Srv.Lan:7700/ "),
            "http://srv.lan:7700"
        );
        assert_eq!(
            normalize_url("https://H:1/Zam/Api//"),
            "https://h:1/Zam/Api"
        );
        assert_eq!(normalize_url("http://User@HOST"), "http://User@host");
        assert_eq!(normalize_url("http://H?Q=A"), "http://h?Q=A");
        assert_eq!(normalize_url("Host:7700/"), "Host:7700");
    }

    /// M7 §4.4: `zam init --token` пишет `[servers."<адрес>"]`, другой адрес не трогает и не
    /// считает заменой; прежний формат переводится в новый без потери ключа (`url` верхнего
    /// уровня остаётся); тот же ключ — без записи; другой ключ того же адреса — только `force`.
    /// `url` верхнего уровня: local не трогает, block пишет, если его нет, home — свой.
    /// Ключ без адреса: тот же токен его привязывает, другой оставляет (маска — в плане), а у home
    /// — отказ.
    #[test]
    fn plan_home_keys_by_server() {
        let (a, b) = ("http://192.0.2.1:7700", "http://192.0.2.2:7700");
        let t1 = ["zam_", &"1".repeat(40)].concat();
        let t2 = ["zam_", &"2".repeat(40)].concat();
        let (_, home, cfg) = home_with("planservers", "");
        std::fs::remove_file(&cfg).unwrap();
        // запись плана: (замена, ключ без адреса остался)
        let write = |plan: HomePlan| -> (bool, Option<String>) {
            let HomePlan::Write {
                path,
                text,
                replaced,
                unbound_left,
            } = plan
            else {
                panic!("{plan:?}");
            };
            assert_eq!(path, cfg);
            std::fs::write(&cfg, text).unwrap();
            (replaced, unbound_left)
        };
        let table =
            || -> toml::Table { toml::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap() };
        let token_of =
            |t: &toml::Table, u: &str| t["servers"][u]["token"].as_str().map(String::from);
        let keep = TopUrl::Keep;
        assert_eq!(
            write(plan_home(&home, &format!("{a}/"), &t1, false, keep).unwrap()),
            (false, None)
        );
        assert_eq!(
            write(plan_home(&home, b, &t2, false, keep).unwrap()),
            (false, None)
        );
        let t = table();
        assert_eq!(token_of(&t, a), Some(t1.clone()), "{t:?}");
        assert_eq!(token_of(&t, b), Some(t2.clone()), "{t:?}");
        assert!(t.get("url").is_none() && t.get("token").is_none(), "{t:?}");
        // тот же ключ (адрес в другом регистре) — без записи; другой ключ — только force
        assert_eq!(
            plan_home(&home, "HTTP://192.0.2.2:7700", &t2, true, keep).unwrap(),
            HomePlan::Unchanged
        );
        assert_eq!(
            plan_home(&home, b, &t1, false, keep).unwrap(),
            HomePlan::Conflict(cfg.clone())
        );
        assert!(write(plan_home(&home, b, &t1, true, keep).unwrap()).0);
        assert_eq!(token_of(&table(), a), Some(t1.clone()));
        // прежний формат: ключ верхнего уровня уходит в [servers."<url>"], url и свои ключи
        // остаются; запись для другого адреса прежний ключ не теряет
        std::fs::write(
            &cfg,
            format!("url = \"{a}/\"\ntoken = \"{t1}\"\n\n[hooks]\nprompt = false\n"),
        )
        .unwrap();
        assert_eq!(
            write(plan_home(&home, b, &t2, false, TopUrl::IfAbsent).unwrap()),
            (false, None)
        );
        let t = table();
        assert_eq!(t["url"].as_str(), Some(format!("{a}/").as_str()));
        assert!(t.get("token").is_none(), "{t:?}");
        assert_eq!(token_of(&t, a), Some(t1.clone()), "{t:?}");
        assert_eq!(token_of(&t, b), Some(t2.clone()), "{t:?}");
        assert_eq!(t["hooks"]["prompt"].as_bool(), Some(false));
        // прежний формат и тот же ключ — перевод в новый формат
        std::fs::write(&cfg, format!("url = \"{a}\"\ntoken = \"{t1}\"\n")).unwrap();
        assert_eq!(
            write(plan_home(&home, a, &t1, false, keep).unwrap()),
            (false, None)
        );
        let t = table();
        assert!(t.get("token").is_none(), "{t:?}");
        assert_eq!(token_of(&t, a), Some(t1.clone()), "{t:?}");
        // ключ без адреса и другой токен: ключ остаётся как есть, план несёт его маску; block
        // `url` не пишет — рядом с ним ключ без адреса стал бы ключом этого адреса
        let unbound = format!("token = \"{t1}\"\n");
        for top in [keep, TopUrl::IfAbsent] {
            std::fs::write(&cfg, &unbound).unwrap();
            let left = Some(zam_core::mask_token(&t1));
            assert_eq!(
                write(plan_home(&home, b, &t2, false, top).unwrap()),
                (false, left)
            );
            let t = table();
            assert_eq!(t["token"].as_str(), Some(t1.as_str()), "{top:?}: {t:?}");
            assert!(t.get("url").is_none(), "{top:?}: {t:?}");
            assert_eq!(token_of(&t, b), Some(t2.clone()), "{top:?}: {t:?}");
            assert!(t["servers"].get(a).is_none(), "{top:?}: {t:?}");
        }
        // тот же токен — ключ этого адреса: уходит с верхнего уровня, block пишет и `url`
        std::fs::write(&cfg, &unbound).unwrap();
        assert_eq!(
            write(plan_home(&home, a, &t1, false, TopUrl::IfAbsent).unwrap()),
            (false, None)
        );
        let t = table();
        assert!(t.get("token").is_none(), "{t:?}");
        assert_eq!(t["url"].as_str(), Some(a), "{t:?}");
        assert_eq!(token_of(&t, a), Some(t1.clone()), "{t:?}");
        // `--url-to home` (круг правок 1): ключ без адреса и другой токен — отказ и с `force`
        // (ключ не становится ключом этого адреса и не удаляется); свой токен — ключ адреса
        std::fs::write(&cfg, &unbound).unwrap();
        for force in [false, true] {
            let e = plan_home(&home, a, &t2, force, TopUrl::Set).unwrap_err();
            assert!(
                e.contains("ключ без адреса") && e.contains(&zam_core::mask_token(&t1)),
                "{e}"
            );
            assert!(!e.contains(&t1), "{e}");
        }
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), unbound);
        assert_eq!(
            write(plan_home(&home, a, &t1, false, TopUrl::Set).unwrap()),
            (false, None)
        );
        let t = table();
        assert!(t.get("token").is_none(), "{t:?}");
        assert_eq!(t["url"].as_str(), Some(a), "{t:?}");
        assert_eq!(token_of(&t, a), Some(t1.clone()), "{t:?}");
        // block: `url` пишется, если его нет; другой адрес не заменяется; повтор — без записи
        std::fs::remove_file(&cfg).unwrap();
        write(plan_home(&home, a, &t1, false, TopUrl::IfAbsent).unwrap());
        write(plan_home(&home, b, &t2, false, TopUrl::IfAbsent).unwrap());
        let t = table();
        assert_eq!(t["url"].as_str(), Some(a), "{t:?}");
        assert_eq!(token_of(&t, b), Some(t2.clone()), "{t:?}");
        assert_eq!(
            plan_home(&home, b, &t2, false, TopUrl::IfAbsent).unwrap(),
            HomePlan::Unchanged
        );
        // home: адрес — вместе с ключом, повтор — без записи
        std::fs::remove_file(&cfg).unwrap();
        write(plan_home(&home, a, &t1, false, TopUrl::Set).unwrap());
        let t = table();
        assert_eq!(t["url"].as_str(), Some(a));
        assert_eq!(token_of(&t, a), Some(t1.clone()), "{t:?}");
        assert_eq!(
            plan_home(&home, a, &t1, false, TopUrl::Set).unwrap(),
            HomePlan::Unchanged
        );
        // `servers` не таблица — ошибка с путём
        std::fs::write(&cfg, "servers = 1\n").unwrap();
        assert!(plan_home(&home, a, &t1, true, keep).is_err());
    }

    /// Псевдонимы адреса (финальное ревью M7.1, I-3): тот же токен — и под адресом блока
    /// (нормализованным, без повтора `--url`); оба уже есть — без записи; другой токен под
    /// псевдонимом — замена только с `force`, иначе `Conflict`.
    #[test]
    fn plan_home_aliases_writes_block_address() {
        let (a, b) = ("http://192.0.2.1:7700", "http://192.0.2.9:7700");
        let t1 = ["zam_", &"3".repeat(40)].concat();
        let t2 = ["zam_", &"4".repeat(40)].concat();
        let (_, home, cfg) = home_with("planaliases", "");
        std::fs::remove_file(&cfg).unwrap();
        // адрес блока со `/` на конце и повтор `--url`
        let aliases = vec![format!("{b}/"), a.to_string()];
        let HomePlan::Write { text, replaced, .. } =
            plan_home_aliases(&home, a, &aliases, &t1, false, TopUrl::IfAbsent).unwrap()
        else {
            panic!("запись");
        };
        assert!(!replaced);
        std::fs::write(&cfg, &text).unwrap();
        let t: toml::Table = toml::from_str(&text).unwrap();
        let servers = t["servers"].as_table().unwrap();
        assert_eq!(servers.len(), 2, "{t:?}");
        for u in [a, b] {
            assert_eq!(
                servers[u]["token"].as_str(),
                Some(t1.as_str()),
                "{u}: {t:?}"
            );
        }
        assert_eq!(
            t["url"].as_str(),
            Some(a),
            "url верхнего уровня — адрес --url"
        );
        assert_eq!(
            plan_home_aliases(&home, a, &aliases, &t1, false, TopUrl::IfAbsent).unwrap(),
            HomePlan::Unchanged
        );
        // под адресом блока другой токен: без force — не трогать, с force — заменить
        std::fs::write(
            &cfg,
            format!(
                "url = \"{a}\"\n[servers.\"{a}\"]\ntoken = \"{t1}\"\n[servers.\"{b}\"]\ntoken = \"{t2}\"\n"
            ),
        )
        .unwrap();
        assert!(matches!(
            plan_home_aliases(&home, a, &aliases, &t1, false, TopUrl::IfAbsent).unwrap(),
            HomePlan::Conflict(_)
        ));
        let HomePlan::Write { text, replaced, .. } =
            plan_home_aliases(&home, a, &aliases, &t1, true, TopUrl::IfAbsent).unwrap()
        else {
            panic!("замена");
        };
        assert!(replaced);
        let t: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(
            t["servers"][b]["token"].as_str(),
            Some(t1.as_str()),
            "{t:?}"
        );
    }

    #[test]
    fn url_scheme_check() {
        assert!(check_url("http://h:1").is_ok());
        assert!(check_url("https://h").is_ok());
        for bad in ["host:7700", "ftp://h", "http://", "", "http:///x"] {
            let e = check_url(bad).unwrap_err();
            assert!(e.contains("http:// или https://"), "{e}");
        }
    }
}
