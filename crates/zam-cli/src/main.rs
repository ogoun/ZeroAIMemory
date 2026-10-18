//! `zam` — клиент ZeroAIMemory. В пути статуса нет tokio и tracing (холодный старт ≤ 20 мс).
mod locate;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use locate::{Flags, Found};

const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("ZAM_GIT_SHA"), ")");

/// Коды выхода.
const EXIT_CONFIG: u8 = 1;
const EXIT_UNREACHABLE: u8 = 2;
const EXIT_TOKEN: u8 = 3;

#[derive(Parser)]
#[command(name = "zam", version = VERSION, about = "Клиент ZeroAIMemory")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Проверить подключение: сервер, токен, пользователь, solution.toml
    Status {
        /// Адрес сервера (иначе ZAM_URL, CLAUDE.md/AGENTS.md, ~/.zeroaimemory/config.toml)
        #[arg(long)]
        url: Option<String>,
        /// Токен доступа (иначе ZAM_TOKEN и те же источники)
        #[arg(long)]
        token: Option<String>,
        /// Вывод в JSON
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Status { url, token, json } => ExitCode::from(status(Flags { url, token }, json)),
    }
}

fn home_dir() -> PathBuf {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/nonexistent-home"))
}

/// Хост из URL без схемы, userinfo и порта (IPv6 без скобок).
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
    if let Some(v6) = auth.strip_prefix('[') {
        return v6.split(']').next().unwrap_or("");
    }
    auth.split(':').next().unwrap_or("")
}

/// К loopback системный прокси не применяется; для остальных хостов работают
/// ALL_PROXY/HTTPS_PROXY/HTTP_PROXY/NO_PROXY (по умолчанию ureq).
fn bypass_proxy(url: &str) -> bool {
    matches!(host_of(url), "127.0.0.1" | "localhost" | "::1")
}

fn agent(url: &str) -> ureq::Agent {
    let mut b = ureq::Agent::config_builder()
        .timeout_resolve(Some(Duration::from_secs(2)))
        .timeout_global(Some(Duration::from_secs(10)))
        .timeout_connect(Some(Duration::from_secs(2)))
        .timeout_recv_response(Some(Duration::from_secs(5)))
        .timeout_recv_body(Some(Duration::from_secs(5)))
        .http_status_as_error(false);
    if bypass_proxy(url) {
        b = b.proxy(None);
    }
    ureq::Agent::new_with_config(b.build())
}

/// GET; Ok((код, тело)) или текст сетевой ошибки (токен в нём не встречается).
fn get(agent: &ureq::Agent, url: &str, token: Option<&str>) -> Result<(u16, String), String> {
    let mut req = agent.get(url);
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    let mut resp = req.call().map_err(|e| e.to_string())?;
    let code = resp.status().as_u16();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    Ok((code, body))
}

fn status(flags: Flags, as_json: bool) -> u8 {
    let cwd = std::env::current_dir().unwrap_or_default();
    let env: Vec<(String, String)> = ["ZAM_URL", "ZAM_TOKEN"]
        .iter()
        .filter_map(|k| Some((k.to_string(), std::env::var(k).ok()?)))
        .collect();
    let r = match locate::resolve(&flags, &env, &cwd, &home_dir()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return EXIT_CONFIG;
        }
    };
    let Some(url) = r.url else {
        eprintln!(
            "не задан адрес сервера: флаг --url, ZAM_URL, блок ZeroAIMemory в CLAUDE.md/AGENTS.md или ~/.zeroaimemory/config.toml"
        );
        return EXIT_CONFIG;
    };
    if let Err(e) = locate::check_url(&url.value) {
        eprintln!("{e}");
        return EXIT_CONFIG;
    }
    let solution = locate::find_solution(&cwd);
    let agent = agent(&url.value);

    // 1. сервер: /health без аутентификации
    let health = match get(&agent, &format!("{}/api/v1/health", url.value), None) {
        Ok((200, body)) => serde_json::from_str::<Value>(&body).ok(),
        Ok((code, _)) => {
            eprintln!("сервер {} ответил HTTP {code} на /health", url.value);
            return EXIT_UNREACHABLE;
        }
        Err(e) => {
            eprintln!("сервер {} недоступен: {e}", url.value);
            return EXIT_UNREACHABLE;
        }
    };
    let field = |k: &str| {
        health
            .as_ref()
            .and_then(|h| h.get(k))
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string()
    };
    let mut rep = Report {
        url,
        version: field("version"),
        git: field("git"),
        token: r.token.clone(),
        user: None,
        access: None,
        solution,
    };

    // 2. токен: /spaces
    let Some(token) = r.token else {
        rep.print(as_json, Some("токен не задан"));
        return EXIT_TOKEN;
    };
    match get(
        &agent,
        &format!("{}/api/v1/spaces", rep.url.value),
        Some(&token.value),
    ) {
        Ok((200, body)) => {
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            rep.user = Some((str_of(&v["user"]["name"]), str_of(&v["user"]["id"])));
            rep.access = Some(str_of(&v["access"]));
            rep.print(as_json, None);
            0
        }
        Ok((401 | 403, _)) => {
            rep.print(as_json, Some("токен отклонён сервером"));
            EXIT_TOKEN
        }
        Ok((code, _)) => {
            eprintln!("сервер ответил HTTP {code} на /spaces");
            EXIT_UNREACHABLE
        }
        Err(e) => {
            eprintln!("сервер {} недоступен: {e}", rep.url.value);
            EXIT_UNREACHABLE
        }
    }
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or("?").to_string()
}

struct Report {
    url: Found,
    version: String,
    git: String,
    token: Option<Found>,
    user: Option<(String, String)>,
    access: Option<String>,
    solution: Option<PathBuf>,
}

impl Report {
    /// Печать результата; токен — только маска. `problem` идёт в stderr (и в поле `error` JSON).
    fn print(&self, as_json: bool, problem: Option<&str>) {
        let mask = self.token.as_ref().map(|t| zam_core::mask_token(&t.value));
        let sol = self.solution.as_deref().map(Path::display);
        if as_json {
            let v = json!({
                "url": self.url.value,
                "url_source": self.url.source.describe(),
                "server": { "version": self.version, "git": self.git },
                "token": mask,
                "token_source": self.token.as_ref().map(|t| t.source.describe()),
                "access": self.access,
                "user": self.user.as_ref().map(|(n, i)| json!({ "name": n, "id": i })),
                "solution": sol.map(|d| d.to_string()),
                "error": problem,
            });
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            return;
        }
        println!(
            "Сервер:   {}  (версия {}, git {})",
            self.url.value, self.version, self.git
        );
        println!("Источник: {}", self.url.source.describe());
        match (&self.token, &mask) {
            (Some(t), Some(m)) => {
                let access = self
                    .access
                    .as_deref()
                    .map_or(String::new(), |a| format!("  (доступ {a})"));
                println!("Токен:    {m}{access}");
                println!("Источник токена: {}", t.source.describe());
            }
            _ => println!("Токен:    не задан"),
        }
        match &self.user {
            Some((n, i)) => println!("Пользователь: {n} ({i})"),
            None => println!("Пользователь: неизвестен"),
        }
        match sol {
            Some(d) => println!("Решение:  {d}"),
            None => println!("Решение:  не найдено"),
        }
        if let Some(p) = problem {
            eprintln!("{p}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts_bypass_proxy() {
        for u in [
            "http://127.0.0.1:7700",
            "http://localhost",
            "https://localhost:1/x",
            "http://[::1]:7700",
            "http://user:pw@127.0.0.1/",
        ] {
            assert!(bypass_proxy(u), "{u}");
        }
        for u in [
            "http://192.168.1.5:7700",
            "http://zam.lan",
            "http://localhost.evil.com",
            "http://127.0.0.10",
        ] {
            assert!(!bypass_proxy(u), "{u}");
        }
    }
}
