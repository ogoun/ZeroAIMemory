//! `zam init` (§14.1; задача 11 M4): подключение проекта к памяти.
//!
//! 1. Читает или создаёт `.zeroaimemory/solution.toml` (`--id`, `--name`; по умолчанию — новый
//!    GUID v7 и имя каталога) и регистрирует решение (`POST /solutions`).
//! 2. Берёт блок инструкции и фрагменты у `GET /agent-setup` и вставляет блок между маркерами
//!    в CLAUDE.md (`--agent claude|both`) и/или AGENTS.md (`codex|both`); текст вне блока
//!    сохраняется байт в байт.
//! 3. После подтверждения (или `--yes`) сливает хуки в `.claude/settings.json` /
//!    `.codex/hooks.json` (наши заменяются, чужие остаются) и сервер в `.mcp.json`
//!    (`--no-hooks`, `--no-mcp` — пропуск).
//! 4. Пишет `.zeroaimemory/.gitignore` (`cache/`, `logs/`).
//!
//! Корень проекта — [`hook::project_root`]. Все файлы сначала разбираются и собираются в
//! памяти: битый JSON или блок без конца — ошибка (код 1) до записи чего-либо.
//!
//! **Токен** **[решение контроллера, задача 11 M4]**: в CLAUDE.md, AGENTS.md и `.mcp.json` не
//! пишется. Заданный флагом `--token` или найденный в прежнем блоке (v1 держал
//! `ZeroAIMemoryToken=`) токен сохраняется в `~/.zeroaimemory/config.toml` (права 0600 на
//! unix) — там его находят `zam` и хуки ([`locate::resolve`]); MCP-клиент агента берёт его из
//! `ZAM_TOKEN` — `zam init` печатает, как её задать в Windows и Linux. Повторный запуск
//! идемпотентен.
mod merge;
mod ojson;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::hook;
use super::{home_dir, zam_env};
use crate::client::{CliError, Client, EXIT_TOKEN, read_json};
use crate::locate::{self, Flags, Source};
use crate::target::{self, Solution};
use ojson::J;

/// Для какого агента подключать.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum InitAgent {
    /// Claude Code: CLAUDE.md, `.claude/settings.json`
    Claude,
    /// Codex: AGENTS.md, `.codex/hooks.json`
    Codex,
    /// Оба
    Both,
}

pub struct Opts {
    pub name: Option<String>,
    pub id: Option<String>,
    pub url: Option<String>,
    pub token: Option<String>,
    pub agent: InitAgent,
    pub no_hooks: bool,
    pub no_mcp: bool,
    pub yes: bool,
    pub lang: String,
}

/// Файл, который надо записать.
struct Planned {
    path: PathBuf,
    text: String,
    /// Было ли содержимое до этого.
    existed: bool,
}

impl Planned {
    fn new(path: PathBuf, old: Option<&str>, text: String) -> Option<Planned> {
        (old != Some(text.as_str())).then(|| Planned {
            path,
            text,
            existed: old.is_some(),
        })
    }
}

/// Предел размера читаемых файлов проекта, байт.
const FILE_MAX: u64 = 4 * 1024 * 1024;

fn read_opt(path: &Path) -> Result<Option<String>, CliError> {
    match std::fs::metadata(path) {
        Ok(m) if !m.is_file() => Err(CliError::config(format!("{}: не файл", path.display()))),
        Ok(m) if m.len() > FILE_MAX => Err(CliError::config(format!(
            "{}: файл больше {FILE_MAX} байт",
            path.display()
        ))),
        Ok(_) => std::fs::read_to_string(path)
            .map(Some)
            .map_err(|e| CliError::config(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(CliError::config(format!("{}: {e}", path.display()))),
    }
}

/// Путь относительно корня для вывода.
fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .display()
        .to_string()
        .replace('\\', "/")
}

/// Решение: из `solution.toml` корня или новое.
fn solution(root: &Path, o: &Opts) -> Result<(Solution, PathBuf, bool), CliError> {
    let id =
        o.id.as_deref()
            .map(|g| {
                let g = g.trim();
                if target::is_guid(g) {
                    Ok(g.to_ascii_lowercase())
                } else {
                    Err(CliError::input("--id: GUID решения (UUID с дефисами)"))
                }
            })
            .transpose()?;
    let name = o
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(String::from);
    if let Some(n) = &name
        && (n.chars().count() > 128 || n.chars().any(char::is_control))
    {
        return Err(CliError::input("--name: до 128 символов, без управляющих"));
    }
    let existing = [
        root.join(".zeroaimemory").join("solution.toml"),
        root.join("solution.toml"),
    ]
    .into_iter()
    .find(|p| p.is_file());
    if let Some(path) = existing {
        let s = target::load_solution(&path)?;
        if let Some(g) = &id
            && *g != s.id
        {
            return Err(CliError::config(format!(
                "{}: решение {} уже задано, а --id {g} — другое (удалите файл, чтобы сменить решение)",
                path.display(),
                s.id
            )));
        }
        if let Some(n) = &name
            && *n != s.name
        {
            eprintln!(
                "имя решения берётся из {} («{}»); переименование — zam solution rename",
                path.display(),
                s.name
            );
        }
        return Ok((s, path, false));
    }
    let dir_name = root
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or("Solution")
        .to_string();
    let s = Solution {
        id: id.unwrap_or_else(|| zam_core::SolutionGuid::new_v7().to_string()),
        name: name.unwrap_or(dir_name),
    };
    Ok((s, root.join(".zeroaimemory").join("solution.toml"), true))
}

/// `GET /agent-setup` для агента сервера (`claude` | `codex`).
fn agent_setup(c: &Client, agent: &str, s: &Solution, o: &Opts) -> Result<Value, CliError> {
    let flag = |b: bool| if b { "true" } else { "false" };
    let q = [
        ("agent", agent),
        ("solution", s.id.as_str()),
        ("name", s.name.as_str()),
        ("mcp", flag(!o.no_mcp)),
        ("hooks", flag(!o.no_hooks)),
        ("lang", o.lang.as_str()),
    ];
    read_json(
        c.get("/agent-setup", &q)
            .map_err(|e| e.context("GET /agent-setup"))?,
    )
}

/// Слияние JSON-файла настроек: битый JSON — ошибка без записи.
fn merge_json(
    root: &Path,
    path: PathBuf,
    f: impl FnOnce(&mut J) -> Result<(), String>,
) -> Result<Option<Planned>, CliError> {
    let old = read_opt(&path)?;
    let mut j = match old.as_deref().filter(|t| !t.trim().is_empty()) {
        Some(t) => J::parse(t).map_err(|e| {
            CliError::config(format!(
                "{}: некорректный JSON ({e}) — исправьте файл, zam init его не перезаписывает",
                rel(root, &path)
            ))
        })?,
        None => J::Obj(Vec::new()),
    };
    f(&mut j).map_err(|e| CliError::config(format!("{}: {e}", rel(root, &path))))?;
    let mut text = j.pretty();
    if old.as_deref().is_some_and(|t| t.contains("\r\n")) {
        text = text.replace('\n', "\r\n");
    }
    Ok(Planned::new(path, old.as_deref(), text))
}

/// Токен для `~/.zeroaimemory/config.toml`: заданный флагом или найденный в CLAUDE.md /
/// AGENTS.md (прежний блок v1 держал его в файле; новый блок его не содержит).
fn token_to_save(src: &Source) -> bool {
    matches!(src, Source::Flag | Source::File(_))
}

fn ask(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(
        line.trim().to_lowercase().as_str(),
        "y" | "yes" | "д" | "да"
    )
}

pub fn run(o: Opts) -> Result<(), CliError> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let home = home_dir();
    let flags = Flags {
        url: o.url.clone(),
        token: o.token.clone(),
    };
    let r = locate::resolve(&flags, &zam_env(), &cwd, &home).map_err(CliError::config)?;
    let url = r.url.ok_or_else(|| {
        CliError::config(
            "не задан адрес сервера: zam init --url http://<сервер>:7700 --token <токен>",
        )
    })?;
    locate::check_url(&url.value).map_err(CliError::config)?;
    let token = r.token.ok_or_else(|| {
        CliError::new(
            EXIT_TOKEN,
            "токен не задан: zam init --url … --token <токен> (или ZAM_TOKEN)",
        )
    })?;
    let root = hook::project_root(&cwd, &home);
    if root == home {
        return Err(CliError::config(
            "zam init запускается в каталоге проекта, а не в домашнем",
        ));
    }
    let (sol, sol_path, sol_new) = solution(&root, &o)?;
    let client = Client::new(&url.value, &token.value, Some(sol.id.clone()));

    // 1–2. фрагменты сервера и регистрация решения
    let agents: &[(&str, &str)] = match o.agent {
        InitAgent::Claude => &[("claude", "CLAUDE.md")],
        InitAgent::Codex => &[("codex", "AGENTS.md")],
        InitAgent::Both => &[("claude", "CLAUDE.md"), ("codex", "AGENTS.md")],
    };
    let mut setups = Vec::new();
    for (agent, file) in agents {
        setups.push((*agent, *file, agent_setup(&client, agent, &sol, &o)?));
    }
    let reg = client
        .post_json(
            "/solutions",
            &serde_json::json!({ "agent_solution_id": sol.id, "name": sol.name }),
        )
        .map_err(|e| e.context("регистрация решения"))?;
    let pending = reg.status().as_u16() == 202;

    // план файлов: всё разбирается до первой записи
    let mut plan: Vec<Planned> = Vec::new();
    if sol_new {
        plan.extend(Planned::new(
            sol_path.clone(),
            None,
            format!(
                "agent_solution_id = \"{}\"\nname = {}\n",
                sol.id,
                toml_str(&sol.name)
            ),
        ));
    }
    let gi = root.join(".zeroaimemory").join(".gitignore");
    let old = read_opt(&gi)?;
    plan.extend(Planned::new(
        gi,
        old.as_deref(),
        merge::gitignore(old.as_deref()),
    ));
    for (_, file, v) in &setups {
        let block = v["block"]
            .as_str()
            .ok_or_else(|| CliError::config("в ответе /agent-setup нет block"))?;
        let path = root.join(file);
        let old = read_opt(&path)?;
        let text = merge::upsert_block(old.as_deref(), block)
            .map_err(|e| CliError::config(format!("{file}: {e}")))?;
        plan.extend(Planned::new(path, old.as_deref(), text));
    }
    let mut agent_plan: Vec<Planned> = Vec::new();
    if !o.no_hooks {
        for (agent, _, v) in &setups {
            let (key, path) = match *agent {
                "claude" => (
                    "claude_settings",
                    root.join(".claude").join("settings.json"),
                ),
                _ => ("codex_hooks", root.join(".codex").join("hooks.json")),
            };
            let frag = J::from(&v[key]);
            if frag == J::Null {
                return Err(CliError::config(format!("в ответе /agent-setup нет {key}")));
            }
            agent_plan.extend(merge_json(&root, path, |j| merge::merge_hooks(j, &frag))?);
        }
    }
    if !o.no_mcp {
        let frag = J::from(&setups[0].2["mcp_json"]);
        agent_plan.extend(merge_json(&root, root.join(".mcp.json"), |j| {
            merge::merge_mcp(j, &frag)
        })?);
    }
    // токен — в домашний конфиг (не в файлы проекта)
    let home_cfg = if token_to_save(&token.source) {
        locate::plan_home_token(&home, &token.value).map_err(CliError::config)?
    } else {
        None
    };

    // 3. подтверждение для файлов агента
    let agent_ok = agent_plan.is_empty() || o.yes || {
        println!("Будут изменены файлы агента (чужие хуки и серверы сохраняются):");
        for p in &agent_plan {
            println!("  {}", rel(&root, &p.path));
        }
        ask("Записать?")
    };

    // запись
    let mut report = Vec::new();
    let skipped = !agent_ok;
    let all = plan
        .into_iter()
        .chain(agent_plan.into_iter().filter(|_| agent_ok));
    for p in all {
        if let Some(dir) = p.path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| CliError::config(format!("{}: {e}", dir.display())))?;
        }
        std::fs::write(&p.path, p.text.as_bytes())
            .map_err(|e| CliError::config(format!("{}: {e}", p.path.display())))?;
        report.push(format!(
            "{} {}",
            if p.existed {
                "обновлён"
            } else {
                "создан  "
            },
            rel(&root, &p.path)
        ));
    }
    if let Some((path, text)) = &home_cfg {
        hook::cache::write_atomic(path, text.as_bytes())
            .map_err(|e| CliError::config(format!("{}: {e}", path.display())))?;
    }

    // итог
    println!("Решение: {} ({})", sol.name, sol.id);
    if pending {
        println!(
            "Решение принадлежит другому пользователю: запрос доступа ждёт подтверждения владельца."
        );
    }
    if report.is_empty() {
        println!("Файлы проекта актуальны.");
    }
    for l in &report {
        println!("  {l}");
    }
    if skipped {
        println!("Хуки и .mcp.json не записаны: повторите с --yes.");
    }
    if home_cfg.is_some() {
        println!("Токен сохранён в ~/.zeroaimemory/config.toml (его берут zam и хуки).");
    }
    if !o.no_mcp && agent_ok {
        let set = std::env::var("ZAM_TOKEN").is_ok_and(|v| !v.trim().is_empty());
        if set {
            println!("MCP берёт токен из ZAM_TOKEN — переменная задана.");
        } else {
            println!(
                "MCP берёт токен из переменной ZAM_TOKEN (в .mcp.json его нет). Задайте её и перезапустите агента:\n  \
                 Windows (PowerShell): [Environment]::SetEnvironmentVariable('ZAM_TOKEN', '<токен>', 'User')\n  \
                 Linux:                echo 'export ZAM_TOKEN=<токен>' >> ~/.profile"
            );
        }
    }
    Ok(())
}

/// Строка TOML в кавычках.
fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}
