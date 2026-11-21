//! `zam status`: сервер, токен, пользователь, решение (имя и GUID из solution.toml, имя реестра
//! при переименовании — M6.6 §9, замечание 23), роль в решении, число ожидающих запросов доступа
//! к решениям пользователя (M0, холодный старт ≤ 20 мс) и версия блока инструкции в
//! CLAUDE.md/AGENTS.md против версии сервера (`GET /agent-setup`, §15.5): устарел — одно
//! напоминание.
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{home_dir, missing_token, unbound_token_note, zam_env};
use crate::client::{EXIT_CONFIG, EXIT_TOKEN, EXIT_UNREACHABLE, Timeouts, agent};
use crate::locate::{self, Flags, Found};
use crate::target::Solution;

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

pub fn run(flags: Flags, as_json: bool) -> u8 {
    let cwd = std::env::current_dir().unwrap_or_default();
    let r = match locate::resolve(&flags, &zam_env(), &cwd, &home_dir()) {
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
    // ключ без адреса в домашнем конфиге — примечание с командой привязки (M7 §4.4)
    if let Some(u) = &r.unbound_token {
        let used = u.used.then_some(url.value.as_str());
        eprintln!("примечание: {}", unbound_token_note(&u.mask, used));
    }
    // в домашнем конфиге токены только других адресов — ошибка настройки до сети (M7 §4.4)
    if r.token.is_none() && r.other_server_keys {
        let e = missing_token(&url.value, true);
        eprintln!("{}", e.msg);
        return e.code;
    }
    let solution = locate::find_solution(&cwd);
    // файл читается сразу: имя и GUID печатаются и без ответа /spaces
    let sol = solution.as_deref().map(|p| {
        crate::target::load_solution(p).map_err(|e| {
            let lead = format!("{}: ", p.display());
            e.msg.strip_prefix(&lead).unwrap_or(&e.msg).to_string()
        })
    });
    let agent = agent(&url.value, Timeouts::Probe);

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
        sol,
        registry_name: None,
        role: None,
        pending: None,
        block: None,
    };

    // 2. токен: /spaces
    let Some(token) = r.token else {
        let e = missing_token(&rep.url.value, false);
        rep.print(as_json, Some(&e.msg));
        return e.code;
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
            rep.pending = v["access_requests_pending"].as_u64();
            if let Some(Ok(s)) = &rep.sol {
                rep.role = Some(solution_role(
                    &agent,
                    &rep.url.value,
                    &token.value,
                    &v,
                    &s.id,
                ));
                rep.registry_name = registry_name(&v, s);
            }
            rep.block = block_versions(&agent, &rep.url.value, &token.value, &cwd);
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

/// Роль пользователя в решении `guid`: из `/spaces`, иначе по ответу `/solutions/{guid}`.
fn solution_role(
    agent: &ureq::Agent,
    url: &str,
    token: &str,
    spaces: &Value,
    guid: &str,
) -> String {
    let found = spaces["spaces"]
        .as_array()
        .and_then(|l| l.iter().find(|s| s["agent_solution_id"] == guid));
    if let Some(s) = found {
        return match s["role"].as_str() {
            Some("member") => format!("участник ({})", str_of(&s["access"])),
            Some("owner") => "владелец".into(),
            other => other.unwrap_or("?").to_string(),
        };
    }
    match get(
        agent,
        &format!("{url}/api/v1/solutions/{guid}"),
        Some(token),
    ) {
        Ok((200, body)) => serde_json::from_str::<Value>(&body)
            .map(|v| str_of(&v["role"]))
            .unwrap_or_else(|_| "?".into()),
        Ok((403, body)) => {
            let state = serde_json::from_str::<Value>(&body)
                .map(|v| str_of(&v["state"]))
                .unwrap_or_else(|_| "?".into());
            format!("нет доступа (запрос: {state})")
        }
        Ok((404, _)) => "не зарегистрировано (zam write зарегистрирует)".into(),
        Ok((code, _)) => format!("неизвестна (HTTP {code})"),
        Err(e) => format!("неизвестна ({e})"),
    }
}

/// Имя решения в реестре сервера (`spaces[].name` с этим GUID), если оно отличается от имени в
/// `solution.toml` (переименовано `zam solution rename` или в WebUI).
fn registry_name(spaces: &Value, s: &Solution) -> Option<String> {
    spaces["spaces"]
        .as_array()?
        .iter()
        .find(|x| x["agent_solution_id"] == s.id.as_str())?["name"]
        .as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty() && *n != s.name)
        .map(String::from)
}

/// Версия блока ближайшего CLAUDE.md/AGENTS.md (как у хука `session-start`) и версия
/// сервера; сервер без `/agent-setup` (старый) — `None`.
fn block_versions(
    agent: &ureq::Agent,
    url: &str,
    token: &str,
    cwd: &Path,
) -> Option<(Option<u32>, u32)> {
    let (200, body) = get(agent, &format!("{url}/api/v1/agent-setup"), Some(token)).ok()? else {
        return None;
    };
    let server = serde_json::from_str::<Value>(&body).ok()?["version"].as_u64()? as u32;
    let root = super::hook::project_root(cwd, &home_dir());
    let local = super::hook::session_start::block_version(cwd, &root, super::hook::Agent::Claude);
    Some((local, server))
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
    /// Содержимое `solution.toml`; не читается — причина.
    sol: Option<Result<Solution, String>>,
    /// Имя решения в реестре сервера, если отличается от имени файла.
    registry_name: Option<String>,
    /// Роль в решении из `solution.toml`.
    role: Option<String>,
    /// Ожидающие запросы доступа к решениям пользователя.
    pending: Option<u64>,
    /// Версия блока инструкции: (в файле, на сервере).
    block: Option<(Option<u32>, u32)>,
}

impl Report {
    /// Печать результата; токен — только маска. `problem` идёт в stderr (и в поле `error` JSON).
    fn print(&self, as_json: bool, problem: Option<&str>) {
        let mask = self.token.as_ref().map(|t| zam_core::mask_token(&t.value));
        let sol = self.solution.as_deref().map(Path::display);
        let file = self.sol.as_ref().and_then(|r| r.as_ref().ok());
        let name = self
            .registry_name
            .as_deref()
            .or(file.map(|s| s.name.as_str()));
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
                "solution_id": file.map(|s| s.id.as_str()),
                "solution_name": name,
                "solution_file_name": file.map(|s| s.name.as_str()),
                "solution_error": self.sol.as_ref().and_then(|r| r.as_ref().err()),
                "solution_role": self.role,
                "access_requests_pending": self.pending,
                "agent_block": self.block.map(|(local, server)| json!({
                    "version": local,
                    "current": server,
                    "outdated": local.is_some_and(|l| l < server),
                })),
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
        match (sol, &self.sol) {
            (Some(d), Some(Ok(s))) => {
                let note = match &self.registry_name {
                    Some(_) => format!(" (в solution.toml — «{}»)", s.name),
                    None => String::new(),
                };
                println!(
                    "Решение:  «{}» ({}) — {d}{note}",
                    name.unwrap_or(&s.name),
                    s.id
                );
            }
            (Some(d), Some(Err(e))) => println!("Решение:  {d} — не читается: {e}"),
            _ => println!("Решение:  не найдено"),
        }
        if let Some(r) = &self.role {
            println!("Роль в решении: {r}");
        }
        if let Some(n) = self.pending.filter(|n| *n > 0) {
            println!("Ожидают подтверждения доступа: {n} (zam solution access requests)");
        }
        match self.block {
            Some((Some(l), s)) if l < s => println!(
                "Блок ZeroAIMemory устарел (v{l} → v{s}): обнови через `zam init` или WebUI"
            ),
            Some((Some(l), _)) => println!("Блок инструкции: v{l} (актуален)"),
            Some((None, _)) => println!("Блок инструкции: не найден (zam init)"),
            None => {}
        }
        if let Some(p) = problem {
            eprintln!("{p}");
        }
    }
}
