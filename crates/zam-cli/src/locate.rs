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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub value: String,
    pub source: Source,
}

#[derive(Debug, Default, Clone)]
pub struct Resolved {
    pub url: Option<Found>,
    pub token: Option<Found>,
}

const FILES: [&str; 3] = ["CLAUDE.md", "CLAUDE.local.md", "AGENTS.md"];
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

pub fn resolve(flags: &Flags, env: &[(String, String)], cwd: &Path, home: &Path) -> Resolved {
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
            let Ok(text) = std::fs::read_to_string(&path) else {
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
        let cfg: HomeConfig = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
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
    r
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

        let r = resolve(&f, &e, &proj, &home);
        assert_eq!(r.url.as_ref().unwrap().source, Source::Flag);
        assert_eq!(r.url.unwrap().value, "http://flag:4");
        assert_eq!(r.token.unwrap().source, Source::Flag);

        let r = resolve(&Flags::default(), &e, &proj, &home);
        assert_eq!(r.url.as_ref().unwrap().source, Source::Env("ZAM_URL"));
        assert_eq!(r.token.unwrap().source, Source::Env("ZAM_TOKEN"));

        let r = resolve(&Flags::default(), &[], &proj, &home);
        assert_eq!(
            r.url.as_ref().unwrap().source,
            Source::File(proj.join("CLAUDE.md"))
        );
        assert_eq!(r.url.unwrap().value, "http://file:2");
        assert_eq!(r.token.unwrap().value, "file-token");

        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let r = resolve(&Flags::default(), &[], &empty, &home);
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
        let r = resolve(&Flags::default(), &[], &cwd, &root.join("nohome"));
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
        let r = resolve(&Flags::default(), &e, &root, &root.join("nohome"));
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
        let r = resolve(&Flags::default(), &[], &root, &root.join("nohome"));
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
        let r = resolve(&Flags::default(), &[], &root, &root.join("nohome"));
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
        let r = resolve(&Flags::default(), &[], &sub, &root.join("nohome"));
        assert_eq!(r.url.unwrap().value, "http://local:3");
    }

    #[test]
    fn url_trailing_slash_trimmed_and_empty_env_ignored() {
        let root = tmp("norm");
        let e = env(&[("ZAM_URL", "http://env:2//"), ("ZAM_TOKEN", "")]);
        let r = resolve(&Flags::default(), &e, &root, &root.join("nohome"));
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
}
