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
//! - **Бюджеты** (§10.3): срок HTTP — абсолютный момент [`Env::http_deadline`]: старт процесса +
//!   бюджет − [`HTTP_RESERVE_MS`]. Он один на все запросы хука: срок каждого запроса — остаток
//!   до этого момента при отправке, последовательные запросы и параллельные потоки его делят;
//!   без остатка запрос не начинается (одна строка `… бюджет исчерпан до запроса к серверу`).
//! - **Переключатели** (решение контроллера R5, [`Settings`]): `[hooks]` в
//!   `~/.zeroaimemory/config.toml` и `<корень>/.zeroaimemory/config.toml` (проект сильнее
//!   домашнего), затем настройка сервера из ответа `/recall(session_start)` (кэш
//!   `cache/hooks.json`): тип выключен, если выключен в клиенте или на сервере. С M4.5 (§9.6)
//!   ключей девять ([`Flag`]): к типам команд добавлены `subagent_start`, `file_context`
//!   (знание о файле), `session_journal` (журнал сессии [`journal`]) и `recovery`; с M5.6 —
//!   десятый `code_map` (автосинхронизация карты кода, [`crate::cmd::code::auto`]); нет ключа
//!   — `true` (старый `hooks.json` и старый сервер новое не выключают).
//! - **`post-tool`** — два режима: ошибка (как M4: `/recall(post_tool)`, бюджет 250 мс) и
//!   успех (M4.5: журнал, знание о файле, очередь изменённых — без сети, 50 мс).
//! - **`subagent-start`** (M4.5 §9.4, событие `SubagentStart` Claude и Codex, 300 мс) —
//!   сессия субагента и пакет под задачу ([`subagent_start`]).
//! - **Контрольная группа A/B** (M6 §12, `hooks.holdout`): у сессии с маркером
//!   `cache/holdout-<сессия>` ([`cache::mark_holdout`]; ставят `session-start` и ответы сервера с
//!   `holdout: true`) вывод любого хука — пустой, включая локальный (знание о файле, триггеры,
//!   напоминание, петля, сводка журнала), и учёта локального вывода нет; журнал, очереди,
//!   привязка MCP и отправка учёта — как обычно. Субагенты Claude Code и Codex шлют события
//!   хуков с id родителя — маркер покрывает и их.
//! - **Учёт выданного** (M4.5 §9.5): непустой вывод без сети (`pre-tool`, успешный
//!   `post-tool`) — строка в очереди `cache/tokens-<сессия>.jsonl` (оценка токенов
//!   [`cache::estimate_tokens`]); сетевые хуки (`session-start`, `prompt`, `post-tool` ошибки,
//!   `checkpoint`, `subagent-start` — для сессии родителя) отправляют её параллельно со своим
//!   запросом ([`cache::flush_tokens`]). Вывод сетевых хуков сервер считает сам (`/recall`).
//! - **Снимок сессии** (M6 §8а, [`snapshot_queue`]): `checkpoint` (`Stop`, `PreCompact`,
//!   `PostCompact`, `SessionEnd`) и `session-start` (`compact`, догоняющий на `startup`) читают
//!   хвост журнала агента ([`snapshot`]) в отдельном потоке со сроком `hooks.snapshot_ms`, кладут
//!   снимок в очередь `cache/snapq-<сессия>.json` и отправляют `PUT /sessions/{id}/snapshot`.
//!   Ключи клиента — `hooks.session_snapshot`, `snapshot_tail_kb`, `snapshot_ms` ([`ClientKeys`]).
pub mod binding;
pub mod cache;
pub mod checkpoint;
pub mod input;
pub mod journal;
pub mod output;
pub mod post_tool;
pub mod pre_tool;
pub mod procs;
pub mod prompt;
pub mod session_start;
// снимок сессии §8а: адаптеры журналов и `take` (задача 17a)
pub mod snapshot;
// снимок в хуках: состояние `snapst-*`, очередь `snapq-*`, `PUT /sessions/{id}/snapshot` (17b)
pub mod snapshot_queue;
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
            Kind::Checkpoint => {
                matches!(event, "Stop" | "PreCompact" | "PostCompact" | "SessionEnd")
            }
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
    /// Карта кода (M5.6 §10): строка дайджеста, раздел `### Код` пакета под задачу и
    /// автосинхронизация из `session-start`/`checkpoint`.
    CodeMap,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_map: Option<bool>,
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
            Flag::CodeMap => self.code_map,
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
            code_map: self.code_map.or(base.code_map),
        }
    }
}

/// Ключи `[hooks]` только клиента (M6 §12; сервер их не знает и не присылает): нет ключа —
/// `None` (умолчание).
#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
pub struct ClientKeys {
    /// Влияние диффа в напоминании `checkpoint` (`POST /code/impact`), по умолчанию `false`.
    #[serde(default)]
    pub checkpoint_impact: Option<bool>,
    /// Доля сессий контрольной группы агентного A/B, 0..=[`HOLDOUT_MAX`], по умолчанию 0.
    #[serde(default)]
    pub holdout: Option<f64>,
    /// Снимок сессии из журнала агента (M6 §8а), по умолчанию `true`; `false` — журнал агента
    /// не открывается вовсе.
    #[serde(default)]
    pub session_snapshot: Option<bool>,
    /// Хвост журнала агента для снимка, КиБ, [`SNAPSHOT_TAIL_KB`] (диапазон 64..=16384).
    #[serde(default)]
    pub snapshot_tail_kb: Option<u32>,
    /// Жёсткий срок снимка (весь ввод-вывод), мс, [`SNAPSHOT_MS`] (диапазон 5..=50).
    #[serde(default)]
    pub snapshot_ms: Option<u64>,
}

/// Предел `hooks.holdout`: контрольная группа — не больше половины сессий (M6 §12).
pub const HOLDOUT_MAX: f64 = 0.5;
/// Умолчание и диапазон `hooks.snapshot_tail_kb` (M6 §8а, §14).
pub const SNAPSHOT_TAIL_KB: u32 = 2048;
pub const SNAPSHOT_TAIL_KB_RANGE: std::ops::RangeInclusive<u32> = 64..=16384;
/// Умолчание и диапазон `hooks.snapshot_ms` (M6 §8а, §14).
pub const SNAPSHOT_MS: u64 = 25;
pub const SNAPSHOT_MS_RANGE: std::ops::RangeInclusive<u64> = 5..=50;

impl ClientKeys {
    fn over(self, base: ClientKeys) -> ClientKeys {
        ClientKeys {
            checkpoint_impact: self.checkpoint_impact.or(base.checkpoint_impact),
            holdout: self.holdout.or(base.holdout),
            session_snapshot: self.session_snapshot.or(base.session_snapshot),
            snapshot_tail_kb: self.snapshot_tail_kb.or(base.snapshot_tail_kb),
            snapshot_ms: self.snapshot_ms.or(base.snapshot_ms),
        }
    }
}

/// `[hooks]` файла настроек: включённость типов подсказок и ключи клиента.
#[derive(Deserialize, Default)]
struct HooksSection {
    #[serde(flatten)]
    flags: HookFlags,
    #[serde(flatten)]
    keys: ClientKeys,
}

#[derive(Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    hooks: HooksSection,
}

/// Настройки типов подсказок: клиент (проект сильнее домашнего) и кэш сервера; ключи клиента.
#[derive(Debug, Clone, Copy, Default)]
pub struct Settings {
    pub client: HookFlags,
    pub server: HookFlags,
    pub keys: ClientKeys,
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

    /// `hooks.checkpoint_impact` клиента (по умолчанию выключен).
    pub fn checkpoint_impact(&self) -> bool {
        self.keys.checkpoint_impact == Some(true)
    }

    /// `hooks.holdout` клиента в 0..=[`HOLDOUT_MAX`]; нет ключа или вне диапазона — 0 (вне
    /// диапазона пишет в журнал [`Env::settings`]).
    pub fn holdout(&self) -> f64 {
        self.keys
            .holdout
            .filter(|p| (0.0..=HOLDOUT_MAX).contains(p))
            .unwrap_or(0.0)
    }

    /// `hooks.session_snapshot` клиента (по умолчанию включён, M6 §8а).
    pub fn session_snapshot(&self) -> bool {
        self.keys.session_snapshot != Some(false)
    }

    /// `hooks.snapshot_tail_kb` в [`SNAPSHOT_TAIL_KB_RANGE`]; вне диапазона — умолчание (пишет
    /// в журнал [`Env::settings`]).
    pub fn snapshot_tail_kb(&self) -> u32 {
        self.keys
            .snapshot_tail_kb
            .filter(|v| SNAPSHOT_TAIL_KB_RANGE.contains(v))
            .unwrap_or(SNAPSHOT_TAIL_KB)
    }

    /// `hooks.snapshot_ms` в [`SNAPSHOT_MS_RANGE`], вне диапазона — умолчание.
    pub fn snapshot_ms(&self) -> u64 {
        self.keys
            .snapshot_ms
            .filter(|v| SNAPSHOT_MS_RANGE.contains(v))
            .unwrap_or(SNAPSHOT_MS)
    }

    /// Жёсткий срок снимка в хуке — [`Settings::snapshot_ms`]; отладочная сборка или фича
    /// `test-hooks` — `ZAM_TEST_SNAPSHOT_MS` (тесты содержимого под нагрузкой, как
    /// `ZAM_TEST_HOOK_BUDGET_MS`).
    pub fn snapshot_deadline(&self) -> Duration {
        #[cfg(any(feature = "test-hooks", debug_assertions))]
        if let Some(ms) = std::env::var("ZAM_TEST_SNAPSHOT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            return Duration::from_millis(ms);
        }
        Duration::from_millis(self.snapshot_ms())
    }
}

/// `[hooks]` из `config.toml`; нет файла — пусто, битый — пусто и строка в журнал.
fn read_flags(path: &Path, env: &Env) -> HooksSection {
    let Ok(meta) = std::fs::metadata(path) else {
        return HooksSection::default();
    };
    if !meta.is_file() || meta.len() > CONFIG_MAX {
        return HooksSection::default();
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
            HooksSection::default()
        }
    }
}

/// Окружение запуска хука: команда, агент, корень, решение, начало отсчёта бюджета.
#[derive(Clone)]
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
        let keys = project.keys.over(home.keys);
        if let Some(p) = keys.holdout
            && !(0.0..=HOLDOUT_MAX).contains(&p)
        {
            self.log(&format!(
                "hooks.holdout = {p} вне 0..={HOLDOUT_MAX}: контрольной группы нет"
            ));
        }
        if let Some(v) = keys.snapshot_tail_kb
            && !SNAPSHOT_TAIL_KB_RANGE.contains(&v)
        {
            self.log(&format!(
                "hooks.snapshot_tail_kb = {v} вне 64..=16384: {SNAPSHOT_TAIL_KB}"
            ));
        }
        if let Some(v) = keys.snapshot_ms
            && !SNAPSHOT_MS_RANGE.contains(&v)
        {
            self.log(&format!(
                "hooks.snapshot_ms = {v} вне 5..=50: {SNAPSHOT_MS}"
            ));
        }
        Settings {
            client: project.flags.over(home.flags),
            server: cache::read_server_hooks(self).unwrap_or_default(),
            keys,
        }
    }

    /// Бюджет хука, мс (§10.3); отладочная сборка или фича `test-hooks` —
    /// `ZAM_TEST_HOOK_BUDGET_MS`.
    pub fn budget_ms(&self) -> u64 {
        #[allow(unused_mut)]
        let mut budget = self.kind.budget_ms();
        // щедрый бюджет для тестов содержимого (отладочная сборка под нагрузкой; повторное
        // ревью 8a M6, п. 3) — отладочная сборка или фича `test-hooks`
        #[cfg(any(feature = "test-hooks", debug_assertions))]
        if let Some(ms) = std::env::var("ZAM_TEST_HOOK_BUDGET_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            budget = ms;
        }
        budget
    }

    /// Остаток всего бюджета хука от старта процесса (без резерва HTTP).
    pub fn budget_left(&self) -> Duration {
        Duration::from_millis(self.budget_ms()).saturating_sub(self.started.elapsed())
    }

    /// Абсолютный срок HTTP хука: старт процесса + бюджет − [`HTTP_RESERVE_MS`]. К этому моменту
    /// кончаются все запросы хука, когда бы они ни начались.
    pub fn http_deadline(&self) -> Instant {
        self.started + Duration::from_millis(self.budget_ms().saturating_sub(HTTP_RESERVE_MS))
    }

    /// Остаток бюджета на HTTP — до [`Env::http_deadline`]; вышел — `None`.
    pub fn http_left(&self) -> Option<Duration> {
        self.http_deadline()
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
    }

    /// Клиент API со сроком [`Env::http_deadline`] на все его запросы (клиент можно
    /// переиспользовать и делить между потоками: второго срока не будет); нет адреса, токена или
    /// времени — `None` (причина — в журнал).
    pub fn client(&self) -> Option<Client> {
        self.client_within(Duration::MAX)
    }

    /// [`Env::client`] со своим сроком внутри бюджета хука: все запросы клиента кончаются не
    /// позже чем через `cap` от его создания и не позже [`Env::http_deadline`] (`POST
    /// /code/impact` у `checkpoint`, метка контрольной группы, `PUT` снимка).
    pub fn client_within(&self, cap: Duration) -> Option<Client> {
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
        if self.http_left().is_none() {
            // сколько прошло от старта процесса — для разбора промаха (журнал задачи 5 M6)
            self.log(&format!(
                "{} (прошло {} мс)",
                zam_core::HOOK_BUDGET_SPENT,
                self.started.elapsed().as_millis()
            ));
            return None;
        }
        let hook = self.http_deadline();
        let at = Instant::now()
            .checked_add(cap)
            .map_or(hook, |own| own.min(hook));
        Some(Client::with_timeouts(
            &url.value,
            &token.value,
            self.solution.as_ref().map(|s| s.id.clone()),
            Timeouts::Deadline(at),
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
        log_line(&self.log_path(), self.kind.as_str(), msg);
    }
}

/// Строка `<время> <источник> <текст>` в журнал хуков `path` (ротация по [`LOG_MAX`]); сбой
/// журнала молча игнорируется. Пишут хуки ([`Env::log`]) и фоновая `zam code sync`.
pub fn log_line(path: &Path, source: &str, msg: &str) {
    let _ = (|| -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if std::fs::metadata(path).is_ok_and(|m| m.len() >= LOG_MAX) {
            let _ = std::fs::rename(path, path.with_extension("log.1"));
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let line = format!(
            "{} {source} {}\n",
            cache::now_rfc3339(),
            msg.replace(['\r', '\n'], " ")
        );
        f.write_all(line.as_bytes())
    })();
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
    test_timing(kind, started);
    0
}

/// Тестовый канал времени (только отладочная сборка и фича `test-hooks`, как
/// `ZAM_TEST_HOOK_BUDGET_MS`): `ZAM_TEST_HOOK_TIMING` — файл, в который хук после вывода дописывает
/// строку `<команда> <микросекунды от старта процесса>`. Тесты времени сверяют с бюджетом хука
/// это время — внутри процесса, от того же `started`, что и бюджет, — а не время запуска и выхода
/// отладочного exe.
fn test_timing(kind: Kind, started: Instant) {
    #[cfg(any(feature = "test-hooks", debug_assertions))]
    if let Some(path) = std::env::var_os("ZAM_TEST_HOOK_TIMING") {
        let line = format!("{} {}\n", kind.as_str(), started.elapsed().as_micros());
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
    }
    #[cfg(not(any(feature = "test-hooks", debug_assertions)))]
    let _ = (kind, started);
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
    let out = match kind {
        Kind::SessionStart => session_start::run(&env, &inp, &settings),
        Kind::Prompt => prompt::run(&env, &inp, &settings),
        Kind::PreTool => pre_tool::run(&env, &inp, &settings),
        Kind::PostTool => post_tool::run(&env, &inp, &settings),
        Kind::Checkpoint => checkpoint::run(&env, &inp, &settings),
        Kind::SubagentStart => subagent_start::run(&env, &inp, &settings),
    };
    // контрольная группа A/B (M6 §12): сессия с маркером — тишина у всех хуков (служебная
    // работа выше уже сделана)
    if inp
        .session_id
        .as_deref()
        .is_some_and(|id| cache::is_holdout(&env, id))
    {
        return Output::Empty;
    }
    out
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
pub(crate) fn session_var(v: Option<String>) -> Option<String> {
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
            keys: ClientKeys::default(),
        };
        assert!(s.on(Kind::Prompt));
        assert!(!s.on(Kind::PreTool));
        assert!(!s.on(Kind::Checkpoint));
        assert!(s.client_on(Kind::Checkpoint));
        assert!(s.on(Kind::SessionStart));
    }

    /// `[hooks]` с ключами клиента (M6 §12): типы подсказок и ключи — из одной таблицы, проект
    /// сильнее домашнего; `holdout` вне 0..=0,5 — 0.
    #[test]
    fn client_keys_parsed_and_layered() {
        let parse = |t: &str| toml::from_str::<ConfigFile>(t).unwrap().hooks;
        let home = parse("[hooks]\nprompt = false\ncheckpoint_impact = true\nholdout = 0.2\n");
        let project = parse("[hooks]\nholdout = 0.5\n");
        let s = Settings {
            client: project.flags.over(home.flags),
            server: HookFlags::default(),
            keys: project.keys.over(home.keys),
        };
        assert!(!s.on(Kind::Prompt));
        assert!(s.checkpoint_impact());
        assert_eq!(s.holdout(), 0.5);
        let bad = Settings {
            keys: parse("[hooks]\nholdout = 0.9\n").keys,
            ..Settings::default()
        };
        assert_eq!(bad.holdout(), 0.0);
        assert!(!Settings::default().checkpoint_impact());
        assert_eq!(Settings::default().holdout(), 0.0);
        // неизвестный ключ (конфигурация новее клиента) — пропускается
        assert_eq!(parse("[hooks]\nfuture = 1\n").keys, ClientKeys::default());
        // снимок сессии (M6 §8а): умолчания, слой проекта, вне диапазона — умолчание
        let d = Settings::default();
        assert!(d.session_snapshot());
        assert_eq!((d.snapshot_tail_kb(), d.snapshot_ms()), (2048, 25));
        let home = parse("[hooks]\nsession_snapshot = false\nsnapshot_tail_kb = 64\n");
        let project = parse("[hooks]\nsnapshot_ms = 50\n");
        let s = Settings {
            keys: project.keys.over(home.keys),
            ..Settings::default()
        };
        assert!(!s.session_snapshot());
        assert_eq!((s.snapshot_tail_kb(), s.snapshot_ms()), (64, 50));
        let bad = Settings {
            keys: parse("[hooks]\nsnapshot_tail_kb = 1\nsnapshot_ms = 51\n").keys,
            ..Settings::default()
        };
        assert_eq!((bad.snapshot_tail_kb(), bad.snapshot_ms()), (2048, 25));
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
        // снимок сессии (M6 §8а): новые события `checkpoint`, без вывода
        assert!(Kind::Checkpoint.accepts("PostCompact"));
        assert!(Kind::Checkpoint.accepts("SessionEnd"));
        assert_eq!(Kind::Checkpoint.budget_ms(), 100);
        assert!(!Kind::Checkpoint.accepts("SubagentStop"));
        assert!(Kind::SubagentStart.accepts("SubagentStart"));
        assert!(!Kind::SubagentStart.accepts("SubagentStop"));
        assert_eq!(Kind::SubagentStart.budget_ms(), 300);
    }
}
