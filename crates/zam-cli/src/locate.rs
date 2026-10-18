//! Поиск настроек подключения (§11.4): флаги → env → CLAUDE.md/AGENTS.md вверх от cwd → config.toml в home.
//! URL и токен разрешаются независимо.
use std::path::{Path, PathBuf};

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
}

impl Source {
    /// Текст для вывода пользователю.
    pub fn describe(&self) -> String {
        match self {
            Source::Flag => "flag".into(),
            Source::Env(n) => format!("env {n}"),
            Source::File(p) => format!("файл {}", p.display()),
            Source::Home(_) => "~/.zeroaimemory/config.toml".into(),
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
        let l = l.trim_start_matches(['-', '*', ' ']).trim_matches('`');
        let Some((k, v)) = l.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches(['`', '"', '\'']).trim();
        if v.is_empty() {
            continue;
        }
        let slot = if in_block { &mut inside } else { &mut outside };
        match k.trim() {
            "ZeroAIMemoryUrl" => slot.0 = Some(v.to_string()),
            "ZeroAIMemoryToken" => slot.1 = Some(v.to_string()),
            _ => {}
        }
    }
    (inside.0.or(outside.0), inside.1.or(outside.1))
}

#[derive(serde::Deserialize, Default)]
struct HomeConfig {
    url: Option<String>,
    token: Option<String>,
}

fn nonempty(v: Option<&str>) -> Option<String> {
    v.map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

fn norm_url(u: String) -> String {
    u.trim_end_matches('/').to_string()
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
    for dir in cwd.ancestors() {
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
    // 4. ~/.zeroaimemory/config.toml
    if r.url.is_none() || r.token.is_none() {
        let path = home.join(".zeroaimemory").join("config.toml");
        let cfg: HomeConfig = match read_limited(&path) {
            Read::Skip => HomeConfig::default(),
            Read::Failed(e) => {
                return Err(format!("не удалось прочитать {}: {e}", path.display()));
            }
            Read::Text(t) => toml::from_str(&t)
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
            && let Some(value) = nonempty(cfg.token.as_deref())
        {
            r.token = Some(Found {
                value,
                source: Source::Home(path),
            });
        }
    }
    if let Some(u) = r.url.as_mut() {
        u.value = norm_url(std::mem::take(&mut u.value));
    }
    Ok(r)
}

/// Ближайший вверх от `start` файл `solution.toml`.
pub fn find_solution(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|d| d.join("solution.toml"))
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
            url: None,
            token: Some(f.clone()),
        };
        assert!(!format!("{f:?}").contains(TOKEN));
        assert!(!format!("{r:?}").contains(TOKEN));
        assert!(format!("{f:?}").contains("zam_abcdefgh"));
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
