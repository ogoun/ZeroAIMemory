//! Хуки агента `zam hook …` (§10.3; задача 10 M4) и локальный кэш триггеров.
//!
//! - **Вход** — JSON на stdin ([`input`], не больше 1 МБ); флаг `--agent claude|codex` задаёт
//!   формат вывода и предел текста ([`output`]).
//! - **Fail-open:** любая ошибка, паника, тайм-аут, выключенный сервер, пустой или битый
//!   stdin, чужое событие — пустой вывод и код 0. Ошибки — строкой в
//!   `.zeroaimemory/logs/hooks.log` ([`log`]) без текста промптов, команд и содержимого файлов.
//! - **Корень** — [`project_root`]: каталог с `.zeroaimemory/` вверх от `cwd` из stdin, иначе
//!   корень git, иначе `cwd`; там `.zeroaimemory/cache/` ([`cache`]) и `.zeroaimemory/logs/`.
//!   Решение — ближайший `solution.toml` вверх от `cwd`.
//! - **Бюджеты** (§10.3): тайм-аут HTTP — бюджет минус [`HTTP_RESERVE_MS`] от старта процесса.
//! - **Переключатели** (решение контроллера R5, [`Settings`]): `[hooks]` в
//!   `~/.zeroaimemory/config.toml` и `<корень>/.zeroaimemory/config.toml` (проект сильнее
//!   домашнего), затем настройка сервера из ответа `/recall(session_start)` (кэш
//!   `cache/hooks.json`): тип выключен, если выключен в клиенте или на сервере. С M4.5 (§9.6)
//!   ключей девять ([`Flag`]): к типам команд добавлены `subagent_start`, `file_context`
//!   (знание о файле), `session_journal` (журнал сессии [`journal`]) и `recovery`; нет ключа
//!   — `true` (старый `hooks.json` и старый сервер новое не выключают).
//! - **`post-tool`** — два режима: ошибка (как M4: `/recall(post_tool)`, бюджет 250 мс) и
//!   успех (M4.5: журнал, знание о файле, очередь изменённых — без сети, 50 мс).
//! - **`subagent-start`** (M4.5 §9.4, событие `SubagentStart` Claude и Codex, 300 мс) —
//!   сессия субагента и пакет под задачу ([`subagent_start`]).
pub mod cache;
pub mod checkpoint;
pub mod input;
pub mod journal;
pub mod output;
pub mod post_tool;
pub mod pre_tool;
pub mod prompt;
pub mod session_start;
pub mod subagent_start;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::client::{Client, Timeouts};
use crate::locate::{self, Flags};
use crate::target::{self, Solution};
use input::Input;
use output::Output;

/// Резерв бюджета хука на запуск процесса и вывод: тайм-аут HTTP — бюджет минус он (§10.3).
pub const HTTP_RESERVE_MS: u64 = 30;
/// Предел журнала хуков: больше — переименование в `hooks.log.1` (§10.3).
pub const LOG_MAX: u64 = 1024 * 1024;
/// Предел файла настроек `config.toml`, байт (как у поиска настроек подключения).
const CONFIG_MAX: u64 = 1024 * 1024;

/// Агент: формат вывода и предел текста.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    /// Имя агента для `POST /sessions`.
    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "claude-code",
            Agent::Codex => "codex",
        }
    }
}

/// Команда хука.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Kind {
    SessionStart,
    Prompt,
    PreTool,
    PostTool,
    Checkpoint,
    SubagentStart,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::SessionStart => "session-start",
            Kind::Prompt => "prompt",
            Kind::PreTool => "pre-tool",
            Kind::PostTool => "post-tool",
            Kind::Checkpoint => "checkpoint",
            Kind::SubagentStart => "subagent-start",
        }
    }

    /// Бюджет команды (§10.3), мс; у `post-tool` — режима ошибки (успех — без сети, 50 мс
    /// на весь процесс, M4.5 §9.1).
    pub fn budget_ms(self) -> u64 {
        match self {
            Kind::SessionStart | Kind::SubagentStart => 300,
            Kind::Prompt | Kind::PostTool => 250,
            Kind::PreTool => 50,
            Kind::Checkpoint => 100,
        }
    }

    /// События stdin, на которые команда отвечает; остальные — тишина.
    fn accepts(self, event: &str) -> bool {
        match self {
            Kind::SessionStart => event == "SessionStart",
            Kind::Prompt => event == "UserPromptSubmit",
            Kind::PreTool => event == "PreToolUse",
            Kind::PostTool => matches!(event, "PostToolUseFailure" | "PostToolUse"),
            Kind::Checkpoint => matches!(event, "Stop" | "PreCompact"),
            Kind::SubagentStart => event == "SubagentStart",
        }
    }
}

impl Kind {
    /// Ключ настройки типа подсказок команды.
    pub fn flag(self) -> Flag {
        match self {
            Kind::SessionStart => Flag::SessionStart,
            Kind::Prompt => Flag::Prompt,
            Kind::PreTool => Flag::PreTool,
            Kind::PostTool => Flag::PostTool,
            Kind::Checkpoint => Flag::Checkpoint,
            Kind::SubagentStart => Flag::SubagentStart,
        }
    }
}

/// Ключ настройки подсказок `hooks.<ключ>` (R5; M4.5 §9.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    SessionStart,
    Prompt,
    PreTool,
    PostTool,
    Checkpoint,
    SubagentStart,
    /// Знание о файле при чтении и правке (§6.3).
    FileContext,
    /// Журнал событий сессии (§9.1) и подсказки по нему (§9.3).
    SessionJournal,
    /// Пакет восстановления после сжатия и `resume` (§9.2).
    Recovery,
}

/// Включённость типов подсказок (R5): именованные ключи, нет ключа — `None` (включён).
/// Неизвестные ключи (клиент старше сервера) пропускаются.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, serde::Serialize)]
pub struct HookFlags {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_start: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_tool: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_tool: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_start: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_context: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_journal: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<bool>,
}

impl HookFlags {
    pub fn get(&self, f: Flag) -> Option<bool> {
        match f {
            Flag::SessionStart => self.session_start,
            Flag::Prompt => self.prompt,
            Flag::PreTool => self.pre_tool,
            Flag::PostTool => self.post_tool,
            Flag::Checkpoint => self.checkpoint,
            Flag::SubagentStart => self.subagent_start,
            Flag::FileContext => self.file_context,
            Flag::SessionJournal => self.session_journal,
            Flag::Recovery => self.recovery,
        }
    }

    /// Слой поверх `base`: заданные здесь значения сильнее.
    fn over(self, base: HookFlags) -> HookFlags {
        HookFlags {
            session_start: self.session_start.or(base.session_start),
            prompt: self.prompt.or(base.prompt),
            pre_tool: self.pre_tool.or(base.pre_tool),
            post_tool: self.post_tool.or(base.post_tool),
            checkpoint: self.checkpoint.or(base.checkpoint),
            subagent_start: self.subagent_start.or(base.subagent_start),
            file_context: self.file_context.or(base.file_context),
            session_journal: self.session_journal.or(base.session_journal),
            recovery: self.recovery.or(base.recovery),
        }
    }
}

#[derive(Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    hooks: HookFlags,
}

/// Настройки типов подсказок: клиент (проект сильнее домашнего) и кэш сервера.
#[derive(Debug, Clone, Copy, Default)]
pub struct Settings {
    pub client: HookFlags,
    pub server: HookFlags,
}

impl Settings {
    /// Тип включён в клиенте (без учёта сервера).
    pub fn client_on(&self, k: Kind) -> bool {
        self.client.get(k.flag()).unwrap_or(true)
    }

    /// Тип включён: не выключен ни в клиенте, ни на сервере.
    pub fn on(&self, k: Kind) -> bool {
        self.flag_on(k.flag())
    }

    /// Ключ включён: не выключен ни в клиенте, ни на сервере (нет ключа — включён).
    pub fn flag_on(&self, f: Flag) -> bool {
        self.client.get(f).unwrap_or(true) && self.server.get(f).unwrap_or(true)
    }
}

/// `[hooks]` из `config.toml`; нет файла — пусто, битый — пусто и строка в журнал.
fn read_flags(path: &Path, env: &Env) -> HookFlags {
    let Ok(meta) = std::fs::metadata(path) else {
        return HookFlags::default();
    };
    if !meta.is_file() || meta.len() > CONFIG_MAX {
        return HookFlags::default();
    }
    let parsed = std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|t| {
            // BOM в начале файла (fix round 3)
            toml::from_str::<ConfigFile>(t.strip_prefix('\u{feff}').unwrap_or(&t))
                .map_err(|e| e.message().to_string())
        });
    match parsed {
        Ok(c) => c.hooks,
        Err(e) => {
            env.log(&format!("{} пропущен: {e}", path.display()));
            HookFlags::default()
        }
    }
}

/// Окружение запуска хука: команда, агент, корень, решение, начало отсчёта бюджета.
pub struct Env {
    pub kind: Kind,
    pub agent: Agent,
    pub started: Instant,
    pub cwd: PathBuf,
    pub root: PathBuf,
    pub solution: Option<Solution>,
    pub home: PathBuf,
}

impl Env {
    pub fn new(kind: Kind, agent: Agent, started: Instant, cwd: PathBuf, home: PathBuf) -> Env {
        let root = project_root(&cwd, &home);
        let sol_file = locate::find_solution(&cwd);
        let mut env = Env {
            kind,
            agent,
            started,
            cwd,
            root,
            solution: None,
            home,
        };
        if let Some(p) = sol_file {
            match target::load_solution(&p) {
                Ok(s) => env.solution = Some(s),
                Err(e) => env.log(&format!("solution.toml пропущен: {e}")),
            }
        }
        env
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.root.join(".zeroaimemory").join("cache")
    }

    pub fn log_path(&self) -> PathBuf {
        self.root
            .join(".zeroaimemory")
            .join("logs")
            .join("hooks.log")
    }

    /// Настройки типов подсказок (R5).
    pub fn settings(&self) -> Settings {
        let home = read_flags(&self.home.join(".zeroaimemory").join("config.toml"), self);
        let project = read_flags(&self.root.join(".zeroaimemory").join("config.toml"), self);
        Settings {
            client: project.over(home),
            server: cache::read_server_hooks(self).unwrap_or_default(),
        }
    }

    /// Остаток бюджета на HTTP: бюджет минус [`HTTP_RESERVE_MS`] от старта процесса.
    pub fn http_left(&self) -> Option<Duration> {
        let total = Duration::from_millis(self.kind.budget_ms().saturating_sub(HTTP_RESERVE_MS));
        total
            .checked_sub(self.started.elapsed())
            .filter(|d| !d.is_zero())
    }

    /// Клиент API с тайм-аутом по остатку бюджета; нет адреса, токена или времени — `None`
    /// (причина — в журнал).
    pub fn client(&self) -> Option<Client> {
        let r = match locate::resolve(&Flags::default(), &super::zam_env(), &self.cwd, &self.home) {
            Ok(r) => r,
            Err(e) => {
                self.log(&format!("настройки подключения: {e}"));
                return None;
            }
        };
        let (Some(url), Some(token)) = (r.url, r.token) else {
            self.log("не заданы адрес сервера или токен");
            return None;
        };
        if let Err(e) = locate::check_url(&url.value) {
            self.log(&e);
            return None;
        }
        let Some(left) = self.http_left() else {
            self.log("бюджет исчерпан до запроса к серверу");
            return None;
        };
        Some(Client::with_timeouts(
            &url.value,
            &token.value,
            self.solution.as_ref().map(|s| s.id.clone()),
            Timeouts::Deadline(left),
        ))
    }

    /// Адрес сервера без сети (для сверки кэша): `None` — не задан.
    pub fn server_url(&self) -> Option<String> {
        locate::resolve(&Flags::default(), &super::zam_env(), &self.cwd, &self.home)
            .ok()
            .and_then(|r| r.url)
            .map(|u| u.value)
    }

    /// Строка в `hooks.log` (ротация по [`LOG_MAX`]); сбой журнала молча игнорируется.
    /// Текст промптов, команд и содержимого файлов сюда не передаётся.
    pub fn log(&self, msg: &str) {
        let path = self.log_path();
        let _ = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            if std::fs::metadata(&path).is_ok_and(|m| m.len() >= LOG_MAX) {
                let _ = std::fs::rename(&path, path.with_extension("log.1"));
            }
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            let line = format!(
                "{} {} {}\n",
                cache::now_rfc3339(),
                self.kind.as_str(),
                msg.replace(['\r', '\n'], " ")
            );
            f.write_all(line.as_bytes())
        })();
    }
}

/// Корень проекта (fix round 1, m-6; fix round 2): ближайший вверх от `cwd` каталог с
/// `.zeroaimemory/` (кроме домашнего — там `~/.zeroaimemory/config.toml` пользователя) или с
/// `solution.toml` (как [`locate::find_solution`], решение в подкаталоге монорепо), иначе корень
/// git (каталог с `.git` — каталогом или файлом у worktree), иначе сам `cwd`.
pub fn project_root(cwd: &Path, home: &Path) -> PathBuf {
    let marked = cwd.ancestors().find(|d| {
        (*d != home && d.join(".zeroaimemory").is_dir()) || d.join("solution.toml").is_file()
    });
    let git = || cwd.ancestors().find(|d| d.join(".git").exists());
    marked.or_else(git).unwrap_or(cwd).to_path_buf()
}

/// Запуск хука: всегда код 0 (fail-open); паника перехватывается и пишется в журнал.
pub fn run(kind: Kind, agent: Agent, started: Instant) -> u8 {
    // сообщение паники не должно уходить агенту в stderr
    std::panic::set_hook(Box::new(|_| {}));
    let raw = input::read_stdin();
    let res = std::panic::catch_unwind(|| handle(kind, agent, started, &raw));
    let out = match res {
        Ok(o) => o,
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            let cwd = std::env::current_dir().unwrap_or_default();
            Env::new(kind, agent, started, cwd, super::home_dir()).log(&format!(
                "паника: {}",
                msg.chars().take(200).collect::<String>()
            ));
            Output::Empty
        }
    };
    let text = out.render(agent);
    if !text.is_empty() {
        let mut so = std::io::stdout().lock();
        let _ = so.write_all(text.as_bytes()).and_then(|_| so.flush());
    }
    0
}

fn handle(kind: Kind, agent: Agent, started: Instant, raw: &input::Raw) -> Output {
    let Some(inp) = Input::parse(raw) else {
        return Output::Empty;
    };
    if !kind.accepts(&inp.event) {
        return Output::Empty;
    }
    let cwd = inp
        .cwd
        .clone()
        .filter(|p| p.is_dir())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let env = Env::new(kind, agent, started, cwd, super::home_dir());
    let settings = env.settings();
    match kind {
        Kind::SessionStart => session_start::run(&env, &inp, &settings),
        Kind::Prompt => prompt::run(&env, &inp, &settings),
        Kind::PreTool => pre_tool::run(&env, &inp, &settings),
        Kind::PostTool => post_tool::run(&env, &inp, &settings),
        Kind::Checkpoint => checkpoint::run(&env, &inp, &settings),
        Kind::SubagentStart => subagent_start::run(&env, &inp, &settings),
    }
}

/// Переменная, по которой видно, что `zam` запустил Codex (не настройка пользователя вроде
/// `CODEX_HOME` или `CODEX_API_KEY`): переменные Codex из [`zam_core::AGENT_ENV_VARS`] — один
/// список с тестами и smoke (финальное ревью M4, M-7). ⚠️ документацией Codex не подтверждены,
/// взяты из его исходников (песочница команд) — проверить на машине владельца.
fn codex_runtime_var(name: &str) -> bool {
    name.starts_with("CODEX_") && zam_core::AGENT_ENV_VARS.contains(&name)
}

/// Команду запустил агент (fix round 1, I-1): `CLAUDECODE=1` (Claude Code ставит её в
/// подпроцессах инструментов Bash/PowerShell и хуков — документировано), переменные Codex
/// ([`codex_runtime_var`]) или явный `ZAM_AGENT=1`.
pub fn launched_by_agent(get: &impl Fn(&str) -> Option<String>, names: &[String]) -> bool {
    let one = |k: &str| get(k).is_some_and(|v| v.trim() == "1");
    one("CLAUDECODE") || one("ZAM_AGENT") || names.iter().any(|n| codex_runtime_var(n))
}

/// Id сессии из переменной: непустой, до 128 печатных символов.
fn session_var(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s.chars().count() <= 128 && !s.chars().any(char::is_control))
}

/// Сессия для `X-Zam-Session` команд `zam` (см. `cmd::connect`): `ZAM_SESSION` (`-` — без
/// сессии), иначе при запуске агентом — `current` (свежий `session.json` сессии хуков, ≤ 12 ч)
/// и запасной `CLAUDE_CODE_SESSION_ID` (⚠️ переменная из журнала изменений Claude Code 2.1.132,
/// в справочнике не описана; fix round 2); человек — без сессии.
pub fn agent_session_from(
    get: impl Fn(&str) -> Option<String>,
    names: &[String],
    current: impl FnOnce() -> Option<String>,
) -> Option<String> {
    if let Some(v) = get("ZAM_SESSION") {
        if v.trim() == "-" {
            return None;
        }
        if let Some(s) = session_var(Some(v)) {
            return Some(s);
        }
    }
    if !launched_by_agent(&get, names) {
        return None;
    }
    current().or_else(|| session_var(get("CLAUDE_CODE_SESSION_ID")))
}

/// Процентное кодирование сегмента пути URL (id сессии в `/sessions/{id}`).
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Первые `max` байт строки по границе символа.
pub fn head_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_layering_project_over_home() {
        let home = HookFlags {
            prompt: Some(false),
            pre_tool: Some(false),
            ..HookFlags::default()
        };
        let project = HookFlags {
            prompt: Some(true),
            ..HookFlags::default()
        };
        let s = Settings {
            client: project.over(home),
            server: HookFlags {
                checkpoint: Some(false),
                ..HookFlags::default()
            },
        };
        assert!(s.on(Kind::Prompt));
        assert!(!s.on(Kind::PreTool));
        assert!(!s.on(Kind::Checkpoint));
        assert!(s.client_on(Kind::Checkpoint));
        assert!(s.on(Kind::SessionStart));
    }

    #[test]
    fn segment_encoding_and_head() {
        assert_eq!(encode_segment("a-b_c.1"), "a-b_c.1");
        assert_eq!(encode_segment("a/b c"), "a%2Fb%20c");
        assert_eq!(head_bytes("абв", 3), "а");
        assert_eq!(head_bytes("abc", 10), "abc");
    }

    #[test]
    fn project_root_prefers_zeroaimemory_then_git() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp/zam-cli-root");
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let repo = home.join("repo");
        let sub = repo.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(home.join(".zeroaimemory")).unwrap();
        // домашний .zeroaimemory — не корень, ближе — git (файл .git у worktree); случай «нет
        // ничего — cwd» здесь не проверить: каталог тестов лежит внутри git самого проекта
        std::fs::write(repo.join(".git"), "gitdir: x").unwrap();
        assert_eq!(project_root(&sub, &home), repo);
        std::fs::create_dir_all(repo.join("a/.zeroaimemory")).unwrap();
        assert_eq!(project_root(&sub, &home), repo.join("a"));
        // решение M0 (`solution.toml` без `.zeroaimemory/`) в подкаталоге монорепо
        std::fs::write(sub.join("solution.toml"), "").unwrap();
        let deep = sub.join("c");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(project_root(&deep, &home), sub);
    }

    #[test]
    fn agent_session_rules() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        let names =
            |pairs: &[(&str, &str)]| pairs.iter().map(|(k, _)| k.to_string()).collect::<Vec<_>>();
        let cur = || Some("hook-s".to_string());
        // человек: без сессии, даже если файл есть
        let p: &[(&str, &str)] = &[("CODEX_HOME", "/x")];
        assert_eq!(
            agent_session_from(env(&[("CODEX_HOME", "/x")]), &names(p), cur),
            None
        );
        // Claude Code
        let p: &[(&str, &str)] = &[("CLAUDECODE", "1")];
        assert_eq!(
            agent_session_from(env(&[("CLAUDECODE", "1")]), &names(p), cur).as_deref(),
            Some("hook-s")
        );
        let p: &[(&str, &str)] = &[("CLAUDECODE", "1"), ("CLAUDE_CODE_SESSION_ID", "cc")];
        // session.json сильнее CLAUDE_CODE_SESSION_ID, без файла — запасной вариант
        assert_eq!(
            agent_session_from(
                env(&[("CLAUDECODE", "1"), ("CLAUDE_CODE_SESSION_ID", "cc")]),
                &names(p),
                cur
            )
            .as_deref(),
            Some("hook-s")
        );
        assert_eq!(
            agent_session_from(
                env(&[("CLAUDECODE", "1"), ("CLAUDE_CODE_SESSION_ID", "cc")]),
                &names(p),
                || None
            )
            .as_deref(),
            Some("cc")
        );
        // Codex и ZAM_AGENT
        let p: &[(&str, &str)] = &[("CODEX_SANDBOX", "seatbelt")];
        assert_eq!(
            agent_session_from(env(&[("CODEX_SANDBOX", "seatbelt")]), &names(p), cur).as_deref(),
            Some("hook-s")
        );
        let p: &[(&str, &str)] = &[("ZAM_AGENT", "1")];
        assert_eq!(
            agent_session_from(env(&[("ZAM_AGENT", "1")]), &names(p), cur).as_deref(),
            Some("hook-s")
        );
        // явный ZAM_SESSION сильнее; «-» — без сессии
        let p: &[(&str, &str)] = &[("ZAM_SESSION", "mine")];
        assert_eq!(
            agent_session_from(env(&[("ZAM_SESSION", "mine")]), &names(p), cur).as_deref(),
            Some("mine")
        );
        let p: &[(&str, &str)] = &[("CLAUDECODE", "1"), ("ZAM_SESSION", "-")];
        assert_eq!(
            agent_session_from(
                env(&[("CLAUDECODE", "1"), ("ZAM_SESSION", "-")]),
                &names(p),
                cur
            ),
            None
        );
    }

    #[test]
    fn events_per_command() {
        assert!(Kind::PostTool.accepts("PostToolUseFailure"));
        assert!(Kind::PostTool.accepts("PostToolUse"));
        assert!(!Kind::PreTool.accepts("PostToolUse"));
        assert!(Kind::Checkpoint.accepts("PreCompact"));
        assert!(!Kind::Checkpoint.accepts("SubagentStop"));
        assert!(Kind::SubagentStart.accepts("SubagentStart"));
        assert!(!Kind::SubagentStart.accepts("SubagentStop"));
        assert_eq!(Kind::SubagentStart.budget_ms(), 300);
    }
}
