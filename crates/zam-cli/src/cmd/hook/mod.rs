//! Хуки агента `zam hook …` (§10.3; задача 10 M4) и локальный кэш триггеров.
//!
//! - **Вход** — JSON на stdin ([`input`], не больше 1 МБ); флаг `--agent claude|codex` задаёт
//!   формат вывода и предел текста ([`output`]).
//! - **Fail-open:** любая ошибка, паника, тайм-аут, выключенный сервер, пустой или битый
//!   stdin, чужое событие — пустой вывод и код 0. Ошибки — строкой в
//!   `.zeroaimemory/logs/hooks.log` ([`log`]) без текста промптов, команд и содержимого файлов.
//! - **Корень** — каталог решения (`.zeroaimemory/solution.toml` вверх от `cwd` из stdin), без
//!   решения — сам `cwd`; там же `.zeroaimemory/cache/` ([`cache`]) и `.zeroaimemory/logs/`.
//! - **Бюджеты** (§10.3): тайм-аут HTTP — бюджет минус [`HTTP_RESERVE_MS`] от старта процесса.
//! - **Переключатели** (решение контроллера R5, [`Settings`]): `[hooks]` в
//!   `~/.zeroaimemory/config.toml` и `<корень>/.zeroaimemory/config.toml` (проект сильнее
//!   домашнего), затем настройка сервера из ответа `/recall(session_start)` (кэш
//!   `cache/hooks.json`): тип выключен, если выключен в клиенте или на сервере.
pub mod cache;
pub mod checkpoint;
pub mod input;
pub mod output;
pub mod post_tool;
pub mod pre_tool;
pub mod prompt;
pub mod session_start;

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
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::SessionStart => "session-start",
            Kind::Prompt => "prompt",
            Kind::PreTool => "pre-tool",
            Kind::PostTool => "post-tool",
            Kind::Checkpoint => "checkpoint",
        }
    }

    /// Бюджет команды (§10.3), мс.
    pub fn budget_ms(self) -> u64 {
        match self {
            Kind::SessionStart => 300,
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
        }
    }
}

/// Включённость типов подсказок (R5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, serde::Serialize)]
pub struct HookFlags {
    pub session_start: Option<bool>,
    pub prompt: Option<bool>,
    pub pre_tool: Option<bool>,
    pub post_tool: Option<bool>,
    pub checkpoint: Option<bool>,
}

impl HookFlags {
    pub fn get(&self, k: Kind) -> Option<bool> {
        match k {
            Kind::SessionStart => self.session_start,
            Kind::Prompt => self.prompt,
            Kind::PreTool => self.pre_tool,
            Kind::PostTool => self.post_tool,
            Kind::Checkpoint => self.checkpoint,
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
        self.client.get(k).unwrap_or(true)
    }

    /// Тип включён: не выключен ни в клиенте, ни на сервере.
    pub fn on(&self, k: Kind) -> bool {
        self.client_on(k) && self.server.get(k).unwrap_or(true)
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
        .and_then(|t| toml::from_str::<ConfigFile>(&t).map_err(|e| e.message().to_string()));
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
        let (root, sol_file) = project_root(&cwd);
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

/// Корень проекта: каталог решения (`<корень>/.zeroaimemory/solution.toml` или
/// `<корень>/solution.toml`) вверх от `cwd`, иначе сам `cwd`; второй элемент — файл решения.
pub fn project_root(cwd: &Path) -> (PathBuf, Option<PathBuf>) {
    match locate::find_solution(cwd) {
        Some(p) => {
            let dir = p.parent().unwrap_or(cwd);
            let root = if dir.file_name().is_some_and(|n| n == ".zeroaimemory") {
                dir.parent().unwrap_or(dir)
            } else {
                dir
            };
            (root.to_path_buf(), Some(p))
        }
        None => (cwd.to_path_buf(), None),
    }
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
    }
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
    fn events_per_command() {
        assert!(Kind::PostTool.accepts("PostToolUseFailure"));
        assert!(Kind::PostTool.accepts("PostToolUse"));
        assert!(!Kind::PreTool.accepts("PostToolUse"));
        assert!(Kind::Checkpoint.accepts("PreCompact"));
        assert!(!Kind::Checkpoint.accepts("SubagentStop"));
    }
}
