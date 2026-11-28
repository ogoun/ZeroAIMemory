//! `zam init` (§14.1; задача 11 M4): подключение проекта к памяти.
//!
//! 1. Читает или создаёт `.zeroaimemory/solution.toml` (`--id`, `--name`; по умолчанию — новый
//!    GUID v7 и имя каталога) и регистрирует решение (`POST /solutions`).
//! 2. Берёт блок инструкции и фрагменты у `GET /agent-setup` и вставляет блок между маркерами
//!    в CLAUDE.md (`--agent claude|both`) и/или AGENTS.md (`codex|both`); текст вне блока
//!    сохраняется байт в байт.
//! 3. После подтверждения (или `--yes`) сливает хуки в `.claude/settings.json` /
//!    `.codex/hooks.json` (наши заменяются, чужие остаются) и сервер в `.mcp.json`
//!    (`--no-hooks`, `--no-mcp` — пропуск). Состав событий задаёт сервер (с блока v3, M4.5 §11:
//!    `PostToolUse` успеха и `SubagentStart`); прежние обработчики `zam hook` заменяются.
//! 4. Пишет `.zeroaimemory/.gitignore` (`cache/`, `logs/`).
//!
//! Корень проекта — [`hook::project_root`]. Все файлы сначала разбираются и собираются в
//! памяти: битый JSON или блок без конца — ошибка (код 1) до записи чего-либо.
//!
//! **Токен** **[решение контроллера, задача 11 M4]**: в CLAUDE.md, AGENTS.md и `.mcp.json` не
//! пишется. Заданный флагом `--token` или найденный в прежнем блоке (v1 держал
//! `ZeroAIMemoryToken=`) того файла, где блок заменяется, токен сохраняется в
//! `~/.zeroaimemory/config.toml` (права 0600 на unix) ключом своего адреса
//! `[servers."<адрес>"]` (M7 §4.4) — там его находят `zam` и хуки ([`locate::resolve`]); токены
//! других адресов не трогаются. Другой токен этого адреса заменяет только `--token` (с
//! предупреждением), без него — предупреждение; после переноса из блока — совет сменить токен
//! (fix round 1, I-6).
//! MCP Claude Code (`.mcp.json`) — stdio-прокси `zam mcp` (M6 §7.1): токен, адрес и решение
//! — те же, что у `zam`; для Codex — подсказка про `~/.codex/config.toml`. Файлы проекта пишутся атомарно, BOM и CRLF
//! сохраняются. Повторный запуск идемпотентен.
//!
//! **Адрес сервера** (M6.6 §10.1, замечание обкатки 1) — [`UrlTo`]: `block` (по умолчанию) —
//! строкой `ZeroAIMemoryUrl=` в блоке; `home` и `local` — блок без этой строки (её вырезает
//! клиент, поэтому годится любой сервер), адрес — в `~/.zeroaimemory/config.toml` или в
//! `CLAUDE.local.md`, исключённом из git. Без `--url-to` режим берётся из прежнего блока. Код
//! адреса — в [`url`].
//!
//! **Подключение без следа в репозитории** (M7.6 §9.5):
//! - [`BlockTo`]: `agent` (по умолчанию) — блок в `CLAUDE.md`/`AGENTS.md`, как раньше; `local` —
//!   блок Claude Code в `CLAUDE.local.md` корня, `CLAUDE.md` не правится; файл вносится в
//!   `info/exclude` тем же путём, что у `--url-to local` (в индексе git — отказ, код 1). Адрес —
//!   строкой блока в том же файле; `--url-to home` его вырезает, прочие значения для этого файла
//!   ничего не меняют. Блок уже в `CLAUDE.md` — переносится (вырезается, остальной текст, BOM и
//!   CRLF сохраняются; «блок перенесён из CLAUDE.md — проверьте git diff»). `--agent codex|both`
//!   — код 1: Codex `CLAUDE.local.md` не читает. Без флага режим сохраняется: блок в
//!   `CLAUDE.local.md` и не в `CLAUDE.md` — `local`.
//! - `--exclude-local`: файлы подключения (созданные или изменённые этим запуском и уже
//!   существующие: `.mcp.json`, `.claude/settings.json`, `.zeroaimemory/` целиком,
//!   `CLAUDE.local.md`, `AGENTS.md`, хуки Codex), которые не отслеживаются git, — в
//!   `info/exclude` (вопрос или `--yes`); отслеживаемый и изменённый — предупреждение; с
//!   исключённым `.zeroaimemory/` — подсказка `zam init --id <guid>` для другого клона.
mod merge;
mod ojson;
mod url;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use url::LOCAL_MD;
pub use url::UrlTo;

use serde_json::Value;

use super::hook;
use super::{home_dir, missing_token, unbound_token_note, zam_env};
use crate::client::{CliError, Client, read_json};
use crate::locate::{self, Flags, Source};
use crate::target::{self, Solution};
use ojson::J;

/// Куда записать блок инструкции Claude Code (M7.6 §9.5 п. 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum BlockTo {
    /// В CLAUDE.md (AGENTS.md — у Codex), как раньше
    Agent,
    /// В CLAUDE.local.md (исключается из git); CLAUDE.md не меняется — только Claude Code
    Local,
}

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
    /// `None` — режим прежнего блока ([`url::kept_url_to`]).
    pub url_to: Option<UrlTo>,
    /// `None` — режим прежнего подключения ([`kept_block_local`]).
    pub block_to: Option<BlockTo>,
    /// Файлы подключения, не отслеживаемые git, — в `info/exclude` (M7.6 §9.5 п. 2).
    pub exclude_local: bool,
}

/// Без `--block-to`: блок в `CLAUDE.local.md` и не в `CLAUDE.md` — режим `local` (M7.6 §9.5 п. 1).
fn kept_block_local(root: &Path, claude_md: Option<&str>) -> Result<bool, CliError> {
    let has = |t: Option<&str>| t.is_some_and(|t| merge::find_block(t).ok().flatten().is_some());
    Ok(!has(claude_md) && has(read_opt(&root.join(LOCAL_MD))?.as_deref()))
}

/// Текст `CLAUDE.local.md` с блоком (`--block-to local`): строки адреса вне блока уходят — адрес
/// теперь строкой блока (или в домашнем конфиге у `--url-to home`); прочий текст, BOM и CRLF
/// сохраняются.
fn local_block_text(old: Option<&str>, block: &str) -> Result<String, String> {
    let old = old.map(merge::without_url_lines);
    merge::upsert_block(old.as_deref(), block)
}

/// Перенос блока из `CLAUDE.md` (`--block-to local`): план правки файла без блока; в файле
/// ничего не осталось (только BOM и пробелы) — файл удаляется (символическая ссылка — нет).
fn moved_block_plan(path: PathBuf, old: &str) -> Result<Option<Planned>, CliError> {
    let Some(text) =
        merge::remove_block(old).map_err(|e| CliError::config(format!("CLAUDE.md: {e}")))?
    else {
        return Ok(None);
    };
    let link = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());
    let blank = text
        .strip_prefix(merge::BOM)
        .unwrap_or(&text)
        .trim()
        .is_empty();
    Ok(Some(Planned {
        path,
        remove: blank && !link,
        text: if blank && !link { String::new() } else { text },
        existed: true,
    }))
}

/// Файлы подключения для `--exclude-local` (M7.6 §9.5 п. 2): записываемые этим запуском (кроме
/// удаляемых) и уже существующие файлы подключения выбранного агента; всё в `.zeroaimemory/` —
/// одним каталогом; `skip` — путь, который исключается отдельно (`CLAUDE.local.md` у
/// `--block-to local` и `--url-to local`).
fn local_items(
    root: &Path,
    planned: &[&Planned],
    o: &Opts,
    skip: Option<&str>,
) -> Vec<url::LocalItem> {
    let norm = |r: String| {
        if r.starts_with(".zeroaimemory/") {
            ".zeroaimemory/".to_string()
        } else {
            r
        }
    };
    let mut items: Vec<url::LocalItem> = Vec::new();
    let mut push = |rel: String, changed: bool| {
        if skip == Some(rel.as_str()) {
            return;
        }
        match items.iter_mut().find(|i| i.rel == rel) {
            Some(i) => i.changed |= changed,
            None => items.push(url::LocalItem { rel, changed }),
        }
    };
    for p in planned.iter().filter(|p| !p.remove) {
        push(norm(rel(root, &p.path)), true);
    }
    let claude = o.agent != InitAgent::Codex;
    let codex = o.agent != InitAgent::Claude;
    let known = [
        (".zeroaimemory", true),
        (".mcp.json", claude && !o.no_mcp),
        (".claude/settings.json", claude && !o.no_hooks),
        (LOCAL_MD, claude),
        ("AGENTS.md", codex),
        (".codex/hooks.json", codex && !o.no_hooks),
    ];
    for (f, want) in known {
        if want && std::fs::symlink_metadata(root.join(f)).is_ok() {
            let r = if f == ".zeroaimemory" {
                ".zeroaimemory/".to_string()
            } else {
                f.to_string()
            };
            push(r, false);
        }
    }
    items
}

/// Итог `--exclude-local`: внесённые файлы, предупреждения об отслеживаемых и не исключённых,
/// подсказка про GUID решения у исключённого `.zeroaimemory/`.
fn print_local_exclude(
    root: &Path,
    plan: &url::LocalExclude,
    ok: bool,
    failed: &[String],
    guid: &str,
) {
    let url::LocalExclude::Plan {
        path,
        add,
        done,
        tracked,
    } = plan
    else {
        match plan {
            url::LocalExclude::NotGit => println!("не git — исключать нечего"),
            _ => eprintln!(
                "предупреждение: git не ответил — проверьте, что файлы подключения исключены из git"
            ),
        }
        return;
    };
    let ex = rel(root, path);
    for t in tracked {
        eprintln!("предупреждение: {t} отслеживается git — правка попадёт в репозиторий");
    }
    let added: Vec<&str> = add
        .iter()
        .map(|(r, _)| r.as_str())
        .filter(|r| ok && !failed.iter().any(|f| f == r))
        .collect();
    if !added.is_empty() {
        println!("Внесены в {ex}: {}.", added.join(", "));
    }
    if !add.is_empty() && !ok {
        let all: Vec<&str> = add.iter().map(|(r, _)| r.as_str()).collect();
        eprintln!(
            "предупреждение: файлы подключения не внесены в {ex}: {} — повторите с --yes",
            all.join(", ")
        );
    }
    for f in failed {
        eprintln!(
            "предупреждение: строка {f} дописана в {ex}, но git его не исключает (правило «!» в \
             .gitignore?)"
        );
    }
    let dir = ".zeroaimemory/";
    if done.iter().any(|d| d == dir) || added.contains(&dir) {
        println!("GUID решения не в репозитории: в другом клоне — zam init --id {guid}");
    }
}

/// Файл, который надо записать (или удалить).
struct Planned {
    path: PathBuf,
    text: String,
    /// Было ли содержимое до этого.
    existed: bool,
    /// Удалить файл (`CLAUDE.local.md`, где кроме строки адреса ничего не было).
    remove: bool,
}

impl Planned {
    fn new(path: PathBuf, old: Option<&str>, text: String) -> Option<Planned> {
        (old != Some(text.as_str())).then(|| Planned {
            path,
            text,
            existed: old.is_some(),
            remove: false,
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
        && (n.chars().count() > zam_core::SOLUTION_NAME_MAX_CHARS
            || n.chars().any(char::is_control))
    {
        return Err(CliError::input(format!(
            "--name: до {} символов, без управляющих",
            zam_core::SOLUTION_NAME_MAX_CHARS
        )));
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
                        "{name}: висячая символическая ссылка {}{link} ({e}) — zam init её не \
                         заменяет; исправьте или удалите ссылку",
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
            "{name}: символическая ссылка ведёт за пределы проекта ({}) — zam init пишет только \
             внутри корня проекта; замените ссылку обычным файлом или удалите её",
            target.display()
        )));
    }
    Ok(target)
}

/// Переход с `--url-to local` (`--url-to home|block`): строки адреса уходят из `CLAUDE.local.md`
/// (`path`, текст `old`). `Ok((план, удалён))`: свой текст остаётся, файл только со строкой
/// адреса удаляется; символическая ссылка не удаляется — в её цель внутри проекта пишется текст
/// без строк. `Err(причина)` — строку оставить с предупреждением, не роняя `zam init`: в блоке
/// нет строки адреса (`lost` — адрес потерялся бы) или ссылка ведёт за пределы проекта (ревью
/// задачи 10, круг 2, N1, N2). Висячая ссылка сюда не доходит: `read_opt` читает её как
/// отсутствующий файл, строки адреса в нём нет.
fn drop_local_url(
    root: &Path,
    path: PathBuf,
    old: &str,
    lost: bool,
) -> Result<(Planned, bool), String> {
    if lost {
        return Err(
            "в блоке нет строки ZeroAIMemoryUrl= (свой шаблон администратора) — без неё zam и \
             хуки потеряли бы адрес сервера"
                .into(),
        );
    }
    let link = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());
    if link {
        resolve_in_root(root, &path).map_err(|e| {
            format!(
                "{} — уберите строку сами: она важнее домашнего конфига и блока",
                e.msg
            )
        })?;
    }
    let (text, remove) = match merge::drop_url_lines(old) {
        Some(text) => (text, false),
        None if link => (merge::without_url_lines(old), false),
        None => (String::new(), true),
    };
    Ok((
        Planned {
            path,
            text,
            existed: true,
            remove,
        },
        remove,
    ))
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
    let elsewhere = r.other_server_keys;
    let token = r
        .token
        .ok_or_else(|| missing_token(&url.value, elsewhere))?;
    let root = hook::project_root(&cwd, &home);
    if root == home {
        return Err(CliError::config(
            "zam init запускается в каталоге проекта, а не в домашнем",
        ));
    }
    if o.block_to == Some(BlockTo::Local) && o.agent != InitAgent::Claude {
        return Err(CliError::config(
            "Codex не читает CLAUDE.local.md: блок для Codex — только в AGENTS.md",
        ));
    }
    let (sol, sol_path, sol_new) = solution(&root, &o)?;
    let client = Client::new(&url.value, &token.value, Some(sol.id.clone()));

    // режим блока (M7.6 §9.5 п. 1): флаг или прежнее подключение (блок в CLAUDE.local.md и не в
    // CLAUDE.md); у Codex блок — только в AGENTS.md
    let claude_md = root.join("CLAUDE.md");
    let claude_old = if o.agent != InitAgent::Codex {
        read_opt(&claude_md)?
    } else {
        None
    };
    let block_local = o.agent != InitAgent::Codex
        && match o.block_to {
            Some(b) => b == BlockTo::Local,
            None => kept_block_local(&root, claude_old.as_deref())?,
        };
    let claude_file = if block_local { LOCAL_MD } else { "CLAUDE.md" };

    // 1. фрагменты сервера (только чтение)
    let agents: Vec<(&str, &str)> = match o.agent {
        InitAgent::Claude => vec![("claude", claude_file)],
        InitAgent::Codex => vec![("codex", "AGENTS.md")],
        InitAgent::Both => vec![("claude", claude_file), ("codex", "AGENTS.md")],
    };
    let mut setups = Vec::new();
    for (agent, file) in agents {
        setups.push((agent, file, agent_setup(&client, agent, &sol, &o)?));
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
    // временные файлы прерванных запусков (N-4); каталог-симлинк не обходится — он может
    // вести за пределы проекта (F1)
    remove_stale_tmp(&root);
    for d in [
        root.join(".zeroaimemory"),
        root.join(".claude"),
        root.join(".codex"),
    ] {
        if std::fs::symlink_metadata(&d).is_ok_and(|m| !m.file_type().is_symlink()) {
            remove_stale_tmp(&d);
        }
    }
    // режим адреса (M6.6 §10.1): флаг или прежний блок; адреса — клиента и подставленный
    // сервером в блок (`url` ответа /agent-setup, `server.public_url`)
    let olds = setups
        .iter()
        .map(|(_, file, _)| read_opt(&root.join(file)))
        .collect::<Result<Vec<_>, _>>()?;
    let urls = url::urls_of(&url.value, setups.iter().map(|(_, _, v)| v));
    // режим — по первому блоку: сначала файлы, которые коммитятся (AGENTS.md), затем переносимый
    // из CLAUDE.md блок, последним — блок в CLAUDE.local.md (строка адреса в нём есть всегда,
    // кроме --url-to home: по нему одному AGENTS.md получил бы адрес)
    let mut mode_olds: Vec<Option<String>> = setups
        .iter()
        .zip(&olds)
        .filter(|((_, file, _), _)| *file != LOCAL_MD)
        .map(|(_, old)| old.clone())
        .collect();
    if block_local {
        mode_olds.push(claude_old.clone());
        mode_olds.extend(
            setups
                .iter()
                .zip(&olds)
                .filter(|((_, file, _), _)| *file == LOCAL_MD)
                .map(|(_, old)| old.clone()),
        );
    }
    let url_to = match o.url_to {
        Some(m) => m,
        None => url::kept_url_to(&mode_olds, &urls, &root)?,
    };
    let mut ur = url::UrlReport::new(url_to, o.agent != InitAgent::Claude);
    ur.block_local = block_local;
    // токен прежнего блока (v1) того файла, где блок заменяется (I-6); у `both` — из любого
    // из двух, разные — предупреждение, берётся из первого (N-6)
    let mut migrated: Option<(String, &str)> = None;
    let mut differs: Option<&str> = None;
    // `block`: у всех итоговых блоков есть строка адреса (её находит `zam`)
    let mut block_has_url = true;
    for ((_, file, v), old) in setups.iter().zip(&olds) {
        let block = v["block"]
            .as_str()
            .ok_or_else(|| CliError::config("в ответе /agent-setup нет block"))?;
        // блок в CLAUDE.local.md (`--block-to local`): адрес — строкой блока в том же
        // исключённом файле, кроме `--url-to home`
        let local_file = *file == LOCAL_MD;
        let block = match url_to {
            UrlTo::Block | UrlTo::Local if local_file => block.to_string(),
            UrlTo::Block => {
                block_has_url &= !merge::cut_url_lines(block).1.is_empty();
                block.to_string()
            }
            UrlTo::Home if local_file => url::block_without_url(file, block, &urls)?,
            UrlTo::Home | UrlTo::Local => {
                // своя строка адреса вне блока — текст пользователя: не вырезается, а
                // предупреждение (файл коммитится)
                if old.as_deref().and_then(locate::outside_url).is_some() {
                    ur.outside.push(file.to_string());
                }
                url::block_without_url(file, block, &urls)?
            }
        };
        if let Some(t) = old.as_deref().and_then(locate::block_token) {
            match &migrated {
                None => migrated = Some((t, *file)),
                Some((first, _)) if *first != t => differs = Some(*file),
                Some(_) => {}
            }
        }
        let text = if local_file {
            local_block_text(old.as_deref(), &block)
        } else {
            merge::upsert_block(old.as_deref(), &block)
        }
        .map_err(|e| CliError::config(format!("{file}: {e}")))?;
        plan.extend(Planned::new(root.join(file), old.as_deref(), text));
    }
    // перенос блока из CLAUDE.md (M7.6 §9.5 п. 1): токен прежнего блока v1 — как у заменяемого
    if block_local && let Some(old) = claude_old.as_deref() {
        if let Some(t) = locate::block_token(old) {
            match &migrated {
                None => migrated = Some((t, "CLAUDE.md")),
                Some((first, _)) if *first != t => differs = Some("CLAUDE.md"),
                Some(_) => {}
            }
        }
        if let Some(p) = moved_block_plan(claude_md.clone(), old)? {
            ur.moved = true;
            plan.push(p);
        }
    }
    // адрес вне блока: home — домашний конфиг (другой адрес там — отказ), local —
    // CLAUDE.local.md и его исключение из git; block — проверка, что файл не уйдёт в git
    let local_path = root.join(LOCAL_MD);
    // токен — в домашний конфиг ключом своего адреса `[servers."<адрес>"]` (не в файлы проекта,
    // M7 §4.4): `--token` заменяет другой токен этого адреса с предупреждением, перенесённый из
    // блока v1 — нет; токены других адресов не трогаются
    let forced = token.source == Source::Flag;
    let candidate = if forced {
        Some(token.value.clone())
    } else {
        migrated.as_ref().map(|(t, _)| t.clone())
    };
    // токен адреса после запуска: записываемый или найденный
    let addr_token = candidate.as_deref().unwrap_or(&token.value);
    let mut home_url = locate::HomeUrlPlan::Unchanged;
    match url_to {
        UrlTo::Block => {
            // CLAUDE.local.md с блоком исключается ниже — его не проверять
            ur.tracked = setups
                .iter()
                .filter(|(_, file, _)| {
                    *file != LOCAL_MD && url::git_ignored(&root, file) == Some(false)
                })
                .map(|(_, file, _)| file.to_string())
                .collect();
        }
        // ключ без адреса рядом с `url` стал бы ключом этого адреса: сверка с токеном, который
        // пишется или найден для адреса
        UrlTo::Home => home_url = url::plan_home(&home, &url.value, addr_token)?,
        // блок в CLAUDE.local.md уже несёт строку адреса
        UrlTo::Local if block_local => {}
        UrlTo::Local => {
            let old = read_opt(&local_path)?;
            let text = merge::set_url_line(old.as_deref(), &url.value);
            plan.extend(Planned::new(local_path.clone(), old.as_deref(), text));
            ur.exclude = url::plan_exclude(&root, false)?;
        }
    }
    // блок в CLAUDE.local.md — исключение тем же путём, что у `--url-to local` (M7.6 §9.5 п. 1)
    if block_local {
        ur.exclude = url::plan_exclude(&root, true)?;
    }
    // переход с local (`--url-to home|block`): строки адреса уходят из CLAUDE.local.md — она
    // важнее домашнего конфига и блока, а повтор без флага выбрал бы по ней local; у блока в
    // CLAUDE.local.md строки адреса вне блока уже убраны
    if !block_local
        && o.url_to.is_some_and(|m| m != UrlTo::Local)
        && let Some(old) = read_opt(&local_path)?
        && !merge::url_values(&old).is_empty()
    {
        let lost = url_to == UrlTo::Block && !block_has_url;
        match drop_local_url(&root, local_path, &old, lost) {
            Ok((p, removed)) => {
                plan.push(p);
                ur.local_dropped = Some(removed);
            }
            Err(why) => ur.local_kept = Some(why),
        }
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
    let codex = ur.codex;
    if !o.no_mcp && claude {
        let frag = J::from(&setups[0].2["mcp_json"]);
        agent_plan.extend(merge_json(&root, root.join(".mcp.json"), |j| {
            merge::merge_mcp(j, &frag)
        })?);
    }
    // файлы подключения в info/exclude (M7.6 §9.5 п. 2): только чтение git, до любой записи
    let local_ex = o.exclude_local.then(|| {
        let skip = (block_local || url_to == UrlTo::Local).then_some(LOCAL_MD);
        let planned: Vec<&Planned> = plan.iter().chain(agent_plan.iter()).collect();
        url::plan_local_exclude(&root, &local_items(&root, &planned, &o, skip))
    });
    // `url` верхнего уровня (адрес для каталогов без своего): block — если его нет, home — этот,
    // local — не трогается
    let top = match url_to {
        UrlTo::Block => locate::TopUrl::IfAbsent,
        UrlTo::Home => locate::TopUrl::Set,
        UrlTo::Local => locate::TopUrl::Keep,
    };
    // block: проект и хуки берут адрес из строки `ZeroAIMemoryUrl=` блока (`server.public_url` или
    // `Host` запроса) и ищут токен по нему — он пишется и под адресами блока, отличными от
    // `--url`, где токена ещё нет (финальное ревью M7.1, I-3); другой токен под адресом блока не
    // заменяется и с `--token` — адрес назвал сервер (N-1), только предупреждение
    let aliases: Vec<String> = if url_to == UrlTo::Block {
        let key = locate::normalize_url(&url.value);
        let mut a: Vec<String> = urls
            .iter()
            .map(|u| locate::normalize_url(u))
            .filter(|u| *u != key)
            .collect();
        a.sort_unstable();
        a.dedup();
        a
    } else {
        Vec::new()
    };
    let alias_state = if aliases.is_empty() {
        locate::AliasTokens::default()
    } else {
        locate::alias_tokens(&home, &url.value, &aliases, addr_token).map_err(CliError::config)?
    };
    let home_plan = match &candidate {
        Some(t) => locate::plan_home_aliases(&home, &url.value, &aliases, t, forced, top)
            .map_err(CliError::config)?,
        // токен адреса из домашнего конфига: дописать его и под адресом блока (без `url`
        // верхнего уровня — его этот запуск не задавал); токен окружения и файла проекта в
        // домашний конфиг не переносится
        None if !aliases.is_empty()
            && matches!(token.source, Source::Home(_) | Source::HomeServer(..)) =>
        {
            locate::plan_home_aliases(
                &home,
                &url.value,
                &aliases,
                &token.value,
                false,
                locate::TopUrl::Keep,
            )
            .map_err(CliError::config)?
        }
        None => locate::HomePlan::Unchanged,
    };
    // ключ без адреса, который после запуска остаётся и не используется, — предупреждение и без
    // записи токена (M7 §4.4): у записи — из плана, иначе — из файла
    let unbound_left = match &home_plan {
        locate::HomePlan::Write { unbound_left, .. } => unbound_left.clone(),
        _ => locate::unused_unbound_token(&home, addr_token).map_err(CliError::config)?,
    };
    // одна запись домашнего конфига: план токена (у `--url-to home` — с адресом) или только адрес
    let home_write = match (&home_plan, &home_url) {
        (locate::HomePlan::Write { path, text, .. }, _)
        | (_, locate::HomeUrlPlan::Write { path, text }) => Some((path, text)),
        _ => None,
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
    // исключение CLAUDE.local.md из git — тоже с подтверждением (или `--yes`); уже дописанная
    // строка, которую перекрывает правило `!` (`Ineffective`), — без вопроса и записи, итог
    // предупредит; файл в индексе git сюда не доходит — `plan_exclude` отказывает раньше
    ur.exclude_ok = match &ur.exclude {
        url::Exclude::Add { path, .. } => {
            o.yes
                || ask(&format!(
                    "Внести {LOCAL_MD} в {} ({} не попадёт в git)?",
                    rel(&root, path),
                    if block_local {
                        "блок с адресом сервера"
                    } else {
                        "адрес сервера"
                    }
                ))
        }
        _ => false,
    };
    let local_ok = match &local_ex {
        Some(url::LocalExclude::Plan { path, add, .. }) if !add.is_empty() => {
            let list: Vec<&str> = add.iter().map(|(r, _)| r.as_str()).collect();
            o.yes
                || ask(&format!(
                    "Внести файлы подключения в {}: {}?",
                    rel(&root, path),
                    list.join(", ")
                ))
        }
        _ => false,
    };

    // запись: исключение — раньше CLAUDE.local.md с адресом (обрыв между ними не оставит его
    // вне исключения), затем повторная проверка git; файлы подключения — следом, текст
    // info/exclude перечитывается
    ur.excluded = url::apply_exclude(&root, &ur.exclude, ur.exclude_ok)?;
    let not_excluded = match (&local_ex, local_ok) {
        (Some(plan), true) => url::apply_local_exclude(&root, plan)?,
        _ => Vec::new(),
    };
    let mut report = Vec::new();
    let skipped = !agent_ok;
    let all = plan
        .into_iter()
        .chain(agent_plan.into_iter().filter(|_| agent_ok));
    for p in all {
        let target = &targets[&p.path];
        let verb = if p.remove {
            // удаляется сам путь, а не разрешённая цель (у ссылки удаление не планируется)
            std::fs::remove_file(&p.path)
                .map_err(|e| CliError::config(format!("удаление {}: {e}", p.path.display())))?;
            "удалён  "
        } else {
            write_project_file(target, p.text.as_bytes())?;
            if p.existed {
                "обновлён"
            } else {
                "создан  "
            }
        };
        report.push(format!("{verb} {}", rel(&root, &p.path)));
    }
    if let Some((path, text)) = home_write {
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
    let key = locate::normalize_url(&url.value);
    // записанные ключи: адрес и адреса блока, где токена не было
    let keys = std::iter::once(&key)
        .chain(&alias_state.missing)
        .map(|k| format!("[servers.\"{k}\"]"))
        .collect::<Vec<_>>()
        .join(" и ");
    match &home_plan {
        locate::HomePlan::Write { replaced: true, .. } => eprintln!(
            "предупреждение: в ~/.zeroaimemory/config.toml для {key} был другой токен — заменён на \
             заданный --token"
        ),
        locate::HomePlan::Write { .. } => {
            println!("Токен сохранён в ~/.zeroaimemory/config.toml {keys} (его берут zam и хуки).")
        }
        locate::HomePlan::Conflict(_) => eprintln!(
            "предупреждение: в ~/.zeroaimemory/config.toml для {key} другой токен — не заменён \
             (заменить: zam init --token <токен>)"
        ),
        locate::HomePlan::Unchanged => {}
    }
    // адрес блока с другим токеном не заменён и с --token (N-1)
    for a in &alias_state.other {
        eprintln!(
            "предупреждение: под адресом {a} уже другой токен — не заменён; проект с блоком этого \
             адреса пойдёт с прежним токеном"
        );
    }
    if let Some(mask) = &unbound_left {
        eprintln!("предупреждение: {}", unbound_token_note(mask, None));
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
    ur.print(&root);
    if let Some(plan) = &local_ex {
        print_local_exclude(&root, plan, local_ok, &not_excluded, &sol.id);
    }
    if !o.no_mcp && claude && agent_ok {
        // M6 §7.1: stdio `zam mcp` — адрес, токен, решение и машина те же, что у zam и хуков
        println!(
            "MCP (.mcp.json) — stdio-прокси zam mcp: адрес, токен и решение — как у zam и хуков; \
             сессию агента он берёт из привязки хука session-start. Перезапустите агента."
        );
    }
    // хуки и `zam mcp` агент запускает по имени `zam` (абсолютный путь в файлы проекта не
    // пишется: они бывают общими для машин) — без него в PATH ни то, ни другое не заработает
    let uses_zam = agent_ok && ((!o.no_mcp && claude) || !o.no_hooks);
    if uses_zam && !in_path(std::env::var_os("PATH").as_deref()) {
        eprintln!(
            "предупреждение: zam не найден в PATH — агент не запустит хуки и zam mcp \
             (.mcp.json). Добавьте каталог zam в PATH и перезапустите агента."
        );
    }
    if !o.no_mcp && codex {
        println!(
            "MCP для Codex: [mcp_servers.zeroaimemory] (command = \"zam\", args = [\"mcp\"]) в \
             ~/.codex/config.toml — пример в \
             deploy/README-install.md («Подключение агента»)."
        );
    }
    Ok(())
}

/// Строка TOML в кавычках.
fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Есть ли исполняемый `zam` в каталогах `path` (значение `PATH`); на Windows — `zam.exe`.
fn in_path(path: Option<&std::ffi::OsStr>) -> bool {
    let name = if cfg!(windows) { "zam.exe" } else { "zam" };
    path.is_some_and(|p| std::env::split_paths(p).any(|d| d.join(name).is_file()))
}

#[cfg(test)]
mod tests {
    use super::in_path;

    #[test]
    fn zam_in_path_lookup() {
        let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp/zam-cli-init-path");
        let _ = std::fs::remove_dir_all(&d);
        let (with, without) = (d.join("with"), d.join("without"));
        std::fs::create_dir_all(&with).unwrap();
        std::fs::create_dir_all(&without).unwrap();
        let name = if cfg!(windows) { "zam.exe" } else { "zam" };
        std::fs::write(with.join(name), b"").unwrap();
        let joined = |dirs: &[&std::path::Path]| std::env::join_paths(dirs).unwrap();
        assert!(in_path(Some(&joined(&[&without, &with]))));
        assert!(!in_path(Some(&joined(&[&without]))));
        assert!(!in_path(None));
    }
}
