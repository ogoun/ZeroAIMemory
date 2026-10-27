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
//! `ZeroAIMemoryToken=`) того файла, где блок заменяется, токен сохраняется вместе с адресом в
//! `~/.zeroaimemory/config.toml` (права 0600 на unix) — там его находят `zam` и хуки
//! ([`locate::resolve`]). Другой токен там заменяет только `--token` (с предупреждением), без
//! него — предупреждение; после переноса из блока — совет сменить токен (fix round 1, I-6).
//! MCP по HTTP (`.mcp.json`, только Claude Code) берёт токен из `ZAM_TOKEN`; для Codex —
//! подсказка про `~/.codex/config.toml`. Файлы проекта пишутся атомарно, BOM и CRLF
//! сохраняются. Повторный запуск идемпотентен.
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

/// Слияние JSON-файла настроек: битый JSON — ошибка без записи. BOM и CRLF файла
/// сохраняются (fix round 1, I-1).
fn merge_json(
    root: &Path,
    path: PathBuf,
    f: impl FnOnce(&mut J) -> Result<(), String>,
) -> Result<Option<Planned>, CliError> {
    let old = read_opt(&path)?;
    let (bom, body) = match old.as_deref() {
        Some(t) => match t.strip_prefix(merge::BOM) {
            Some(rest) => (merge::BOM, rest),
            None => ("", t),
        },
        None => ("", ""),
    };
    let mut j = if body.trim().is_empty() {
        J::Obj(Vec::new())
    } else {
        J::parse(body).map_err(|e| {
            CliError::config(format!(
                "{}: некорректный JSON ({e}) — исправьте файл, zam init его не перезаписывает",
                rel(root, &path)
            ))
        })?
    };
    f(&mut j).map_err(|e| CliError::config(format!("{}: {e}", rel(root, &path))))?;
    let mut text = j.pretty();
    if body.contains("\r\n") {
        text = text.replace('\n', "\r\n");
    }
    let text = format!("{bom}{text}");
    Ok(Planned::new(path, old.as_deref(), text))
}

/// Метка временных файлов `zam init`: `.<имя>.zam-init.<pid>.tmp`.
const TMP_MARK: &str = ".zam-init.";

/// Куда на самом деле пишется файл проекта `path` (fix round 4, F1, F3): путь разрешается
/// через ближайшего существующего предка (`canonicalize` — сам файл, если он есть; иначе его
/// каталог) и сверяется с каноническим корнем проекта. Симлинк (файл или каталог по пути)
/// с целью вне корня — отказ: в чужом репозитории `AGENTS.md → ~/.bashrc` иначе дописал бы
/// блок в `~/.bashrc`; `--yes` это не подтверждает. Висячий симлинк (`canonicalize` не
/// срабатывает) — тоже отказ, а не замена ссылки обычным файлом. Ссылки внутри проекта
/// (`AGENTS.md → CLAUDE.md`) пишут свою цель.
fn resolve_in_root(root: &Path, path: &Path) -> Result<PathBuf, CliError> {
    let name = rel(root, path);
    let croot = std::fs::canonicalize(root)
        .map_err(|e| CliError::config(format!("корень проекта {}: {e}", root.display())))?;
    let mut base = path;
    let target = loop {
        if std::fs::symlink_metadata(base).is_ok() {
            let rest = path.strip_prefix(base).unwrap_or(Path::new(""));
            match std::fs::canonicalize(base) {
                Ok(c) if rest.as_os_str().is_empty() => break c,
                Ok(c) => break c.join(rest),
                Err(e) => {
                    let link = std::fs::read_link(base)
                        .map(|t| format!(" → {}", t.display()))
                        .unwrap_or_default();
                    return Err(CliError::config(format!(
                        "{name}: висячая символическая ссылка {}{link} ({e}) — zam init её не                          заменяет; исправьте или удалите ссылку",
                        rel(root, base)
                    )));
                }
            }
        }
        match base.parent() {
            Some(p) if !p.as_os_str().is_empty() => base = p,
            _ => return Ok(path.to_path_buf()),
        }
    };
    if !target.starts_with(&croot) {
        return Err(CliError::config(format!(
            "{name}: символическая ссылка ведёт за пределы проекта ({}) — zam init пишет только              внутри корня проекта; замените ссылку обычным файлом или удалите её",
            target.display()
        )));
    }
    Ok(target)
}

/// Запись файла проекта (fix round 1, M-5; fix round 3, N-3): `target` — уже разрешённый
/// [`resolve_in_root`] путь, так что ссылка остаётся ссылкой; временный файл в
/// каталоге цели с правами прежнего и rename (3 попытки через 50 мс: на Windows файл бывает
/// занят антивирусом или редактором); не удалось — запись поверх файла.
fn write_project_file(target: &Path, bytes: &[u8]) -> Result<(), CliError> {
    let fail = |e: std::io::Error| CliError::config(format!("запись {}: {e}", target.display()));
    let dir = target.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(fail)?;
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    let tmp = dir.join(format!(".{name}{TMP_MARK}{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        fail(e)
    })?;
    if let Ok(m) = std::fs::metadata(target) {
        let _ = std::fs::set_permissions(&tmp, m.permissions());
    }
    for attempt in 0..3 {
        if std::fs::rename(&tmp, target).is_ok() {
            return Ok(());
        }
        if attempt < 2 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(target, bytes).map_err(fail)
}

/// Убирает временные файлы прерванных запусков `zam init` (`.*.zam-init.*.tmp`) в каталоге
/// (fix round 3, N-4).
fn remove_stale_tmp(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let n = e.file_name();
        let Some(n) = n.to_str() else { continue };
        if n.starts_with('.') && n.contains(TMP_MARK) && n.ends_with(".tmp") {
            let _ = std::fs::remove_file(e.path());
        }
    }
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

    // 1. фрагменты сервера (только чтение)
    let agents: &[(&str, &str)] = match o.agent {
        InitAgent::Claude => &[("claude", "CLAUDE.md")],
        InitAgent::Codex => &[("codex", "AGENTS.md")],
        InitAgent::Both => &[("claude", "CLAUDE.md"), ("codex", "AGENTS.md")],
    };
    let mut setups = Vec::new();
    for (agent, file) in agents {
        setups.push((*agent, *file, agent_setup(&client, agent, &sol, &o)?));
    }

    // 2. план файлов: всё разбирается и проверяется до регистрации и первой записи (M-5)
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
    // временные файлы прерванных запусков (N-4)
    for d in [
        root.clone(),
        root.join(".zeroaimemory"),
        root.join(".claude"),
        root.join(".codex"),
    ] {
        remove_stale_tmp(&d);
    }
    // токен прежнего блока (v1) того файла, где блок заменяется (I-6); у `both` — из любого
    // из двух, разные — предупреждение, берётся из первого (N-6)
    let mut migrated: Option<(String, &str)> = None;
    let mut differs: Option<&str> = None;
    for (_, file, v) in &setups {
        let block = v["block"]
            .as_str()
            .ok_or_else(|| CliError::config("в ответе /agent-setup нет block"))?;
        let path = root.join(file);
        let old = read_opt(&path)?;
        if let Some(t) = old.as_deref().and_then(locate::block_token) {
            match &migrated {
                None => migrated = Some((t, *file)),
                Some((first, _)) if *first != t => differs = Some(*file),
                Some(_) => {}
            }
        }
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
    // `.mcp.json` читает Claude Code; Codex берёт MCP из своего config.toml (M-6)
    let claude = o.agent != InitAgent::Codex;
    let codex = o.agent != InitAgent::Claude;
    if !o.no_mcp && claude {
        let frag = J::from(&setups[0].2["mcp_json"]);
        agent_plan.extend(merge_json(&root, root.join(".mcp.json"), |j| {
            merge::merge_mcp(j, &frag)
        })?);
    }
    // токен — в домашний конфиг вместе с адресом (не в файлы проекта): `--token` заменяет
    // другой токен с предупреждением, перенесённый из блока v1 — нет
    let forced = token.source == Source::Flag;
    let candidate = if forced {
        Some(token.value.clone())
    } else {
        migrated.as_ref().map(|(t, _)| t.clone())
    };
    let home_plan = match &candidate {
        Some(t) => locate::plan_home(&home, &url.value, t, forced).map_err(CliError::config)?,
        None => locate::HomePlan::Unchanged,
    };

    // куда пишется каждый файл: симлинк вне корня или висячий — отказ до любой записи и
    // регистрации, `--yes` его не снимает (fix round 4, F1, F3)
    let mut targets = std::collections::HashMap::new();
    for p in plan.iter().chain(agent_plan.iter()) {
        targets.insert(p.path.clone(), resolve_in_root(&root, &p.path)?);
    }

    // 3. регистрация решения — после проверки всех файлов
    let reg = client
        .post_json(
            "/solutions",
            &serde_json::json!({ "agent_solution_id": sol.id, "name": sol.name }),
        )
        .map_err(|e| e.context("регистрация решения"))?;
    let pending = reg.status().as_u16() == 202;

    // 4. подтверждение для файлов агента
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
        write_project_file(&targets[&p.path], p.text.as_bytes())?;
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
    if let locate::HomePlan::Write { path, text, .. } = &home_plan {
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
    match &home_plan {
        locate::HomePlan::Write { replaced: true, .. } => eprintln!(
            "предупреждение: в ~/.zeroaimemory/config.toml был другой токен — заменён на заданный --token"
        ),
        locate::HomePlan::Write { .. } => {
            println!("Токен и адрес сохранены в ~/.zeroaimemory/config.toml (их берут zam и хуки).")
        }
        locate::HomePlan::Conflict(_) => eprintln!(
            "предупреждение: в ~/.zeroaimemory/config.toml другой токен — не заменён (заменить: zam init --token <токен>)"
        ),
        locate::HomePlan::Unchanged => {}
    }
    if let (Some((_, first)), Some(other)) = (&migrated, differs) {
        eprintln!(
            "предупреждение: в блоках {first} и {other} разные токены — перенесён токен из {first}"
        );
    }
    if let Some((_, file)) = &migrated {
        println!(
            "Токен из прежнего блока {file} убран из файла. Он лежал в файле проекта (возможно, в \
             репозитории): выпустите новый токен в WebUI и отзовите старый."
        );
    }
    if !o.no_mcp && claude && agent_ok {
        let set = std::env::var("ZAM_TOKEN").is_ok_and(|v| !v.trim().is_empty());
        if set {
            println!("MCP (.mcp.json) берёт токен из ZAM_TOKEN — переменная задана.");
        } else {
            println!(
                "MCP по HTTP (.mcp.json) берёт токен из переменной ZAM_TOKEN; zam и хуки — из \
                 ~/.zeroaimemory/config.toml (права 0600, его пишет zam init --token).\n\
                 Задайте ZAM_TOKEN в окружении агента и перезапустите его:\n  \
                 Windows (PowerShell): [Environment]::SetEnvironmentVariable('ZAM_TOKEN', '<токен>', 'User')\n  \
                 Linux: export ZAM_TOKEN=<токен> — в файле, который читает только ваш пользователь \
                 (chmod 600): переменную видят все процессы агента"
            );
        }
    }
    if !o.no_mcp && codex {
        println!(
            "MCP для Codex: [mcp_servers.zeroaimemory] в ~/.codex/config.toml — пример в \
             deploy/README-install.md («Подключение агента»)."
        );
    }
    Ok(())
}

/// Строка TOML в кавычках.
fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}
