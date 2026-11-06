//! Набор ТС-2 «сессия» (спецификация M6 §3.1, §3.3, §3.5–§3.6; задача 4 плана):
//! `cargo xtask eval --sessions --split dev|holdout [--profile standard|heavy] [--ideas on|off]
//! [--base B4|B5.6|B5.7]`.
//!
//! **Набор** `eval/sessions.toml` ([`load_set`], [`validate_set`]): сессии двух профилей на корпусе
//! `eval/sessions-corpus` ([`super::sessions_corpus`]); у сессии — мир (ключи корпуса) и события.
//!
//! **Прогон** ([`main`]): на каждую сессию — свежий сервер, загрузка мира с учётом возможностей
//! сервера ([`Feats`]), затем [`play`] через [`Driver`]: хуки — процессом `zam hook <событие>`
//! (stdin как у Claude Code, текст — `additionalContext`), MCP — процессом `zam mcp` (stdio) с
//! `ZAM_SESSION_ID` (у клиента M6) или неявным путём (старый клиент, `--implicit-mcp`; каталог
//! процесса без файла привязки). Субагент ходит в тот же `zam mcp` — его вызовы учитываются в
//! сессии родителя (допускаемая неточность, `eval/README.md`).
//!
//! **Метрики** ([`summarize`], [`compare`]): токены по событиям (`R` к базе с парным бутстрепом по
//! сессиям), полнота обязательных строк дайджеста и восстановления, `hint_recall` общий (без
//! подсказок-идей) и подсказок-идей, совпадение подсказок-идей с базой (B5.7), доля повторной
//! выдачи, совпадение дайджеста при двух стартах, строки «идеи» и «снимок сессии». Чего нет у
//! старой базы (события, разделы дайджеста, виды обязательных строк, подсказки терминов), то
//! исключается из **обоих** плеч сравнения (§3.1, C2).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::caps::{self, ServerCaps};
use super::corpus::EVAL_SOLUTION;
use super::hints::{self, Hooks};
use super::metrics::{
    BOOTSTRAP_M6, Estimate, SEED, paired_diff_weighted, quantile, ratio_estimate_with,
};
use super::nuggets;
use super::run::{self, RunSpec, Session};
use super::sessions_corpus::{self, IdeaState, SessionsCorpus};
use super::split::{Part, split};
use super::tokens::{Counter, READ_BUDGET, read_elements, reread_items, reread_need};
use super::{Device, EvalArgs, Mode, Paths};
use crate::smoke::{admin_login, agent_with, api_with, scrub_zam_env};

/// Минимум сессий набора, стандартного и тяжёлого профилей (§3.1).
/// Ревью задачи 4 (I1): стандартных ≥ 16 — holdout профиля ≥ 6; тяжёлых ≥ 6 с разными мирами.
pub const MIN_SESSIONS: usize = 22;
pub const MIN_STANDARD: usize = 16;
pub const MIN_HEAVY: usize = 6;
/// Минимум разных миров тяжёлого профиля (число Rules, задач, ответов, идей).
pub const MIN_HEAVY_WORLDS: usize = 4;
/// Все записи файла корпуса в списке мира сессии.
pub const ALL: &str = "*";
/// Предел ответа `zam mcp` на вызов.
const MCP_TIMEOUT: Duration = Duration::from_secs(180);
/// Суффикс id второго старта (совпадение дайджеста, §3.6).
const SECOND_START: &str = "-b";

// ---------------------------------------------------------------------------------------------
// Набор

/// Профиль сессии (слой разбиения dev/holdout).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    Standard,
    Heavy,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Standard => "standard",
            Profile::Heavy => "heavy",
        }
    }
}

/// Нагет вопроса: дословный фрагмент источника (`rule:`/`insight:`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NuggetSpec {
    pub source: String,
    pub text: String,
}

/// Событие сессии (см. `eval/sessions.toml`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Prompt {
        text: String,
        #[serde(default)]
        expect: Vec<String>,
    },
    Question {
        id: String,
        query: String,
        expect: Vec<String>,
        #[serde(default)]
        nuggets: Vec<NuggetSpec>,
    },
    Repeat {
        of: String,
    },
    ToolError {
        command: String,
        error: String,
        #[serde(default)]
        expect: Vec<String>,
    },
    ReadFile {
        path: String,
        #[serde(default)]
        expect: Vec<String>,
    },
    Subagent {
        agent_type: String,
        task: String,
        #[serde(default)]
        queries: Vec<String>,
    },
    Compact,
    Checkpoint,
}

impl Event {
    pub fn kind(&self) -> &'static str {
        match self {
            Event::Prompt { .. } => "prompt",
            Event::Question { .. } => "question",
            Event::Repeat { .. } => "repeat",
            Event::ToolError { .. } => "tool_error",
            Event::ReadFile { .. } => "read_file",
            Event::Subagent { .. } => "subagent",
            Event::Compact => "compact",
            Event::Checkpoint => "checkpoint",
        }
    }

    fn expect(&self) -> &[String] {
        match self {
            Event::Prompt { expect, .. }
            | Event::ToolError { expect, .. }
            | Event::ReadFile { expect, .. } => expect,
            _ => &[],
        }
    }
}

/// Сессия набора.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSpec {
    pub id: String,
    pub profile: Profile,
    #[serde(default)]
    pub rules: Vec<String>,
    #[serde(default)]
    pub tasks: Vec<String>,
    #[serde(default)]
    pub questions: Vec<String>,
    #[serde(default)]
    pub access: Vec<String>,
    #[serde(default)]
    pub terms: Vec<String>,
    #[serde(default)]
    pub insights: Vec<String>,
    #[serde(default)]
    pub ideas: Vec<String>,
    #[serde(default)]
    pub code_map: bool,
    /// Идеи раздела `### Идеи` первого `session_start` (как отбирает M5.7, до 3).
    #[serde(default)]
    pub digest_ideas: Vec<String>,
    /// Фикстура журнала агента для снимка (§8а, задача 17d).
    #[serde(default)]
    pub transcript: Option<String>,
    #[serde(default, rename = "event")]
    pub events: Vec<Event>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetFile {
    #[serde(default)]
    session: Vec<SessionSpec>,
}

/// Читает набор сессий.
pub fn load_set(path: &Path) -> Result<Vec<SessionSpec>> {
    let text = fs::read_to_string(path).with_context(|| format!("чтение {}", path.display()))?;
    let f: SetFile = toml::from_str(&text).with_context(|| format!("разбор {}", path.display()))?;
    Ok(f.session)
}

/// Мир сессии — записи корпуса ТС-2 (списки `"*"` раскрыты).
#[derive(Debug, Clone, Default)]
pub struct World {
    pub rules: Vec<hints::Rule>,
    pub tasks: Vec<sessions_corpus::Task>,
    pub questions: Vec<sessions_corpus::Question>,
    pub access: Vec<sessions_corpus::Access>,
    pub terms: Vec<super::glossary::Term>,
    pub insights: Vec<super::corpus::Insight>,
    pub ideas: Vec<sessions_corpus::Idea>,
    pub code_map: bool,
}

fn pick<T: Clone>(
    what: &str,
    keys: &[String],
    all: &[T],
    key: impl Fn(&T) -> &str,
) -> Result<Vec<T>> {
    if keys.iter().any(|k| k == ALL) {
        if keys.len() != 1 {
            bail!("{what}: \"*\" — единственным элементом");
        }
        return Ok(all.to_vec());
    }
    keys.iter()
        .map(|k| {
            all.iter()
                .find(|x| key(x) == k)
                .cloned()
                .with_context(|| format!("{what} {k}: нет в корпусе ТС-2"))
        })
        .collect()
}

/// Мир сессии по корпусу.
pub fn world(s: &SessionSpec, c: &SessionsCorpus) -> Result<World> {
    Ok(World {
        rules: pick("rule", &s.rules, &c.rules, |r| &r.key)?,
        tasks: pick("task", &s.tasks, &c.tasks, |t| &t.key)?,
        questions: pick("question", &s.questions, &c.questions, |q| &q.key)?,
        access: pick("access", &s.access, &c.access, |a| &a.key)?,
        terms: pick("term", &s.terms, &c.terms, |t| &t.key)?,
        insights: pick("insight", &s.insights, &c.insights, |i| &i.key)?,
        ideas: pick("idea", &s.ideas, &c.ideas, |i| &i.key)?,
        code_map: s.code_map,
    })
}

impl World {
    /// Текст источника `rule:<key>` / `insight:<key>` (для нагетов).
    fn source_text(&self, r: &str) -> Option<String> {
        if let Some(k) = r.strip_prefix("rule:") {
            return self
                .rules
                .iter()
                .find(|x| x.key == k)
                .map(|x| x.text.clone());
        }
        if let Some(k) = r.strip_prefix("insight:") {
            return self
                .insights
                .iter()
                .find(|x| x.key == k)
                .map(|x| format!("{}\n\n{}", x.title, x.body));
        }
        None
    }

    /// Ссылка набора есть в мире (`rule:`, `insight:`, `term:`, `idea:`, `task:`).
    fn has(&self, r: &str) -> bool {
        let Some((kind, k)) = r.split_once(':') else {
            return false;
        };
        match kind {
            "rule" => self.rules.iter().any(|x| x.key == k),
            "insight" => self.insights.iter().any(|x| x.key == k),
            "term" => self.terms.iter().any(|x| x.key == k),
            "idea" => self.ideas.iter().any(|x| x.key == k),
            "task" => self.tasks.iter().any(|x| x.key == k),
            _ => false,
        }
    }

    fn active_tasks(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| matches!(t.status.as_str(), "in_progress" | "blocked"))
            .count()
    }

    fn answers(&self) -> usize {
        self.questions.iter().filter(|q| q.answer.is_some()).count()
    }
}

/// Итог проверки набора.
#[derive(Debug, Default, Serialize)]
pub struct SetSummary {
    pub sessions: usize,
    pub standard: usize,
    pub heavy: usize,
    pub events: BTreeMap<String, usize>,
    pub prompts_with_idea: usize,
    pub nuggets: usize,
    /// Разные миры тяжёлого профиля.
    pub heavy_worlds: usize,
}

/// Проверка набора по §3.1 (все нарушения — одной ошибкой).
pub fn validate_set(set: &[SessionSpec], c: &SessionsCorpus) -> Result<SetSummary> {
    let mut errs: Vec<String> = Vec::new();
    let mut ids = BTreeSet::new();
    let mut sum = SetSummary::default();
    let mut idea_prompt_by_profile: BTreeSet<Profile> = BTreeSet::new();
    let mut heavy_worlds: BTreeSet<(usize, usize, usize, usize)> = BTreeSet::new();
    for s in set {
        let e = |m: String| format!("{}: {m}", s.id);
        let prefix = match s.profile {
            Profile::Standard => "ss-",
            Profile::Heavy => "sh-",
        };
        if !s.id.starts_with(prefix) || !ids.insert(s.id.clone()) {
            errs.push(e(format!("id — уникальный, с префиксом {prefix}")));
        }
        let w = match world(s, c) {
            Ok(w) => w,
            Err(x) => {
                errs.push(e(format!("{x:#}")));
                continue;
            }
        };
        sum.sessions += 1;
        match s.profile {
            Profile::Standard => {
                sum.standard += 1;
                if !(2..=4).contains(&w.rules.len()) {
                    errs.push(e("стандартный профиль: 2–4 Rules".into()));
                }
                if w.tasks.len() > 2 {
                    errs.push(e("стандартный профиль: 0–2 задачи".into()));
                }
                if w.ideas.len() > 3 {
                    errs.push(e("стандартный профиль: 0–3 идеи".into()));
                }
            }
            Profile::Heavy => {
                sum.heavy += 1;
                heavy_worlds.insert((w.rules.len(), w.active_tasks(), w.answers(), w.ideas.len()));
                if w.active_tasks() > 5 || w.answers() > 5 || w.rules.len() > 20 {
                    errs.push(e(
                        "тяжёлый профиль: не больше 5 задач и 5 ответов (пределы разделов),                          20 Rules (RULES_MAX) — иначе строка обязана выпасть"
                            .into(),
                    ));
                }
                let human_open: Vec<_> = w
                    .ideas
                    .iter()
                    .filter(|i| {
                        i.origin == "human"
                            && matches!(i.state, IdeaState::New | IdeaState::Deferred)
                    })
                    .collect();
                let due = w
                    .ideas
                    .iter()
                    .any(|i| i.state == IdeaState::Deferred && i.horizon_days == Some(0));
                let released = w.ideas.iter().any(|i| i.state == IdeaState::Released);
                let checks = [
                    (w.rules.len() >= 10, "≥ 10 Rules"),
                    (w.active_tasks() >= 3, "≥ 3 задачи in_progress/blocked"),
                    (w.answers() >= 2, "≥ 2 ответа человека"),
                    (!w.access.is_empty(), "ожидающий запрос доступа"),
                    (!w.terms.is_empty(), "словарь"),
                    (w.code_map, "карта кода"),
                    (human_open.len() >= 4, "≥ 4 идеи человека new/deferred"),
                    (due, "идея с наступившим горизонтом"),
                    (released, "идея accepted со снятой задачей"),
                ];
                for (ok, what) in checks {
                    if !ok {
                        errs.push(e(format!("тяжёлый профиль: {what}")));
                    }
                }
            }
        }
        if s.digest_ideas.len() > 3 {
            errs.push(e("digest_ideas — до 3".into()));
        }
        for d in &s.digest_ideas {
            if !w.has(&format!("idea:{d}")) {
                errs.push(e(format!("digest_ideas {d}: идеи нет в мире сессии")));
            }
        }
        if let Some(t) = &s.transcript
            && t.trim().is_empty()
        {
            errs.push(e("transcript — пустая строка".into()));
        }
        let mut count: BTreeMap<&str, usize> = BTreeMap::new();
        let mut questions: BTreeSet<&str> = BTreeSet::new();
        for (i, ev) in s.events.iter().enumerate() {
            *count.entry(ev.kind()).or_default() += 1;
            *sum.events.entry(ev.kind().to_string()).or_default() += 1;
            for x in ev.expect() {
                if !w.has(x) {
                    errs.push(e(format!("событие {i}: ожидание {x} — нет в мире сессии")));
                }
            }
            match ev {
                Event::Prompt { text, expect } => {
                    if text.trim().is_empty() {
                        errs.push(e(format!("событие {i}: пустой prompt")));
                    }
                    if expect.iter().any(|x| x.starts_with("idea:")) {
                        sum.prompts_with_idea += 1;
                        idea_prompt_by_profile.insert(s.profile);
                    }
                }
                Event::Question {
                    id,
                    query,
                    expect,
                    nuggets,
                } => {
                    if !questions.insert(id) {
                        errs.push(e(format!("вопрос {id}: id повторяется")));
                    }
                    if query.trim().is_empty() || expect.is_empty() {
                        errs.push(e(format!("вопрос {id}: нужны query и expect")));
                    }
                    for x in expect {
                        if w.source_text(x).is_none() {
                            errs.push(e(format!("вопрос {id}: источник {x} — нет в мире")));
                        }
                        if !nuggets.iter().any(|n| &n.source == x) {
                            errs.push(e(format!("вопрос {id}: у источника {x} нет нагета")));
                        }
                    }
                    for n in nuggets {
                        sum.nuggets += 1;
                        let len = n.text.chars().count();
                        if !(20..=200).contains(&len) {
                            errs.push(e(format!("вопрос {id}: нагет — 20–200 символов")));
                        }
                        match w.source_text(&n.source) {
                            Some(t) => {
                                let src = nuggets::normalize_md(&t);
                                let nn = nuggets::normalize_md(&n.text);
                                if src.matches(&nn).count() != 1 {
                                    errs.push(e(format!(
                                        "вопрос {id}: нагет «{}» — не ровно один раз в {}",
                                        n.text, n.source
                                    )));
                                }
                            }
                            None => errs.push(e(format!(
                                "вопрос {id}: источник нагета {} — нет в мире",
                                n.source
                            ))),
                        }
                    }
                }
                Event::Repeat { of } => {
                    if !questions.contains(of.as_str()) {
                        errs.push(e(format!("repeat {of}: нет вопроса раньше")));
                    }
                }
                Event::Subagent { task, queries, .. }
                    if task.trim().is_empty() || queries.is_empty() =>
                {
                    errs.push(e("subagent: нужны task и queries".into()));
                }
                _ => {}
            }
        }
        let n = |k: &str| count.get(k).copied().unwrap_or(0);
        let ranges = [
            ("prompt", 3, 6),
            ("question", 1, 3),
            ("tool_error", 0, 2),
            ("read_file", 0, 2),
            ("subagent", 0, 1),
            ("repeat", 0, 1),
            ("compact", 0, 1),
            ("checkpoint", 1, 2),
        ];
        for (k, lo, hi) in ranges {
            if !(lo..=hi).contains(&n(k)) {
                errs.push(e(format!("событий {k}: {} (нужно {lo}–{hi})", n(k))));
            }
        }
        if !matches!(s.events.last(), Some(Event::Checkpoint)) {
            errs.push(e("последнее событие — checkpoint".into()));
        }
    }
    if sum.sessions < MIN_SESSIONS || sum.standard < MIN_STANDARD || sum.heavy < MIN_HEAVY {
        errs.push(format!(
            "сессий {} (стандартных {}, тяжёлых {}): нужно ≥ {MIN_SESSIONS} (≥ {MIN_STANDARD} и \
             ≥ {MIN_HEAVY})",
            sum.sessions, sum.standard, sum.heavy
        ));
    }
    if sum.heavy > 0 && heavy_worlds.len() < MIN_HEAVY_WORLDS.min(sum.heavy) {
        errs.push(format!(
            "миров тяжёлого профиля {} (число Rules, задач, ответов, идей): нужно ≥ {MIN_HEAVY_WORLDS}",
            heavy_worlds.len()
        ));
    }
    for p in [Profile::Standard, Profile::Heavy] {
        if !idea_prompt_by_profile.contains(&p) && !set.is_empty() {
            errs.push(format!(
                "профиль {}: нет prompt с подсказкой-идеей",
                p.as_str()
            ));
        }
    }
    sum.heavy_worlds = heavy_worlds.len();
    if !errs.is_empty() {
        bail!("набор сессий ТС-2:\n  {}", errs.join("\n  "));
    }
    Ok(sum)
}

// ---------------------------------------------------------------------------------------------
// Возможности сервера

/// Что умеет плечо (сервер и клиент `zam` его версии) и что включено замером.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feats {
    /// Задачи, вопросы и ответы человека, разделы дайджеста M4.5 (M4.5).
    pub journal: bool,
    /// Словарь: термины, подсказки терминов в `prompt`, строка словаря дайджеста (M4.5).
    pub glossary: bool,
    /// `subagent-start` и пакет `brief` (M4.5).
    pub subagent: bool,
    /// Пакет восстановления после compact (M4.5).
    pub recovery: bool,
    /// Знание о файле при чтении — успешный `post-tool` Read (M4.5).
    pub file_context: bool,
    /// Карта кода и её строка в дайджесте (M5.6).
    pub code_map: bool,
    /// Вид `idea` у сервера (M5.7).
    pub ideas: bool,
    /// Идеи загружаются и `ideas.proactive` включён (`--ideas on` и сервер с идеями).
    pub ideas_on: bool,
    /// Снимок сессии (M6 §8а).
    pub snapshot: bool,
    /// `zam mcp` шлёт `X-Zam-Session` из `ZAM_SESSION_ID` (клиент M6, без `--implicit-mcp`).
    pub mcp_explicit: bool,
}

/// Возможности плеча по `/health`.
pub fn feats(c: &ServerCaps, ideas_on: bool, implicit_mcp: bool) -> Feats {
    let at = |v: (u32, u32, u32)| c.m6 || (c.version.major, c.version.minor, c.version.patch) >= v;
    Feats {
        journal: at((0, 0, 6)),
        glossary: at((0, 0, 6)),
        subagent: at((0, 0, 6)),
        recovery: at((0, 0, 6)),
        file_context: at((0, 0, 6)),
        code_map: at((0, 0, 9)),
        ideas: c.ideas,
        ideas_on: c.ideas && ideas_on,
        snapshot: c.snapshot,
        mcp_explicit: c.m6 && !implicit_mcp,
    }
}

/// Метки шагов (событие учёта токенов).
pub mod label {
    pub const SESSION_START: &str = "session_start";
    pub const PROMPT: &str = "prompt";
    pub const QUESTION: &str = "question";
    pub const REPEAT: &str = "repeat";
    pub const PRE_TOOL: &str = "pre_tool";
    pub const POST_TOOL: &str = "post_tool";
    pub const FILE_CONTEXT: &str = "file_context";
    pub const SUBAGENT_START: &str = "subagent_start";
    pub const SUBAGENT_MCP: &str = "subagent_mcp";
    pub const PRE_COMPACT: &str = "pre_compact";
    pub const RECOVERY: &str = "recovery";
    pub const CHECKPOINT: &str = "checkpoint";
}

/// Классы строк выдачи, которые сравнение может вычесть ([`classify`]).
pub mod class {
    /// Раздел `### Идеи` и строки подсказок-идей (M5.7).
    pub const IDEAS: &str = "ideas";
    /// Подсказки терминов `prompt` (M4.5).
    pub const TERMS: &str = "terms";
    /// Разделы дайджеста M4.5–M5.6: задачи, ответы человека, открытые вопросы, окружение,
    /// строки словаря, проверенных команд и карты кода.
    pub const JOURNAL: &str = "journal";
    /// Раздел снимка сессии «Последнее известное состояние» (M6 §8а).
    pub const SNAPSHOT: &str = "snapshot";
}

/// Виды обязательных строк.
pub mod req {
    pub const RULE: &str = "rule";
    pub const TASK: &str = "task";
    pub const ANSWER: &str = "answer";
    pub const ACCESS: &str = "access";
    pub const IDEA: &str = "idea";
}

impl Feats {
    /// Метки шагов, которых у плеча нет (события не воспроизводятся).
    pub fn excluded_labels(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.subagent {
            out.extend([label::SUBAGENT_START, label::SUBAGENT_MCP]);
        }
        if !self.recovery {
            out.extend([label::PRE_COMPACT, label::RECOVERY]);
        }
        if !self.file_context {
            out.push(label::FILE_CONTEXT);
        }
        out.into_iter().map(str::to_string).collect()
    }

    /// Классы строк, которых у плеча нет (в сравнении вычитаются из обоих плеч). Идеи сюда не
    /// входят: §3.3 — их токены ложатся на плечо с идеями (сравнение консервативно).
    pub fn excluded_classes(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.glossary {
            out.push(class::TERMS);
        }
        if !self.journal {
            out.push(class::JOURNAL);
        }
        out.into_iter().map(str::to_string).collect()
    }

    /// Виды обязательных строк, которых плечо не умеет.
    pub fn excluded_required(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.journal {
            out.extend([req::TASK, req::ANSWER]);
        }
        if !self.ideas_on {
            out.push(req::IDEA);
        }
        out.into_iter().map(str::to_string).collect()
    }

    /// Виды ожидаемых подсказок, которых плечо не умеет (`term:` без словаря, `idea:` без идей).
    pub fn excluded_expect(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.glossary {
            out.push("term");
        }
        if !self.ideas_on {
            out.push("idea");
        }
        out.into_iter().map(str::to_string).collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Разбор выдачи

/// Ссылки `<буква>:<id>` (id — 26 символов) в тексте по порядку, без повторов; `c_<id>`
/// выжимок не входят.
pub fn raw_refs(text: &str) -> Vec<(char, String)> {
    let b = text.as_bytes();
    let mut out: Vec<(char, String)> = Vec::new();
    let mut i = 0;
    while i + 28 <= b.len() {
        let prev_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if prev_ok
            && b[i].is_ascii_lowercase()
            && b[i + 1] == b':'
            && b[i + 2..i + 28].iter().all(u8::is_ascii_alphanumeric)
            && b.get(i + 28).is_none_or(|c| !c.is_ascii_alphanumeric())
        {
            let r = (b[i] as char, text[i + 2..i + 28].to_string());
            if !out.contains(&r) {
                out.push(r);
            }
            i += 28;
        } else {
            i += 1;
        }
    }
    out
}

/// id записи или узла → ссылка набора (`rule:`, `insight:`, `task:`, `question:`, `term:`,
/// `idea:`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RefMap(pub BTreeMap<String, String>);

impl RefMap {
    /// Ссылки набора в тексте по порядку (неизвестные id пропускаются).
    pub fn keys(&self, text: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (_, id) in raw_refs(text) {
            if let Some(k) = self.0.get(&id)
                && !out.contains(k)
            {
                out.push(k.clone());
            }
        }
        out
    }

    /// `(ссылка набора, сырая ссылка)` по порядку текста.
    fn pairs(&self, text: &str) -> Vec<(String, String)> {
        raw_refs(text)
            .into_iter()
            .filter_map(|(k, id)| {
                self.0
                    .get(&id)
                    .map(|key| (key.clone(), format!("{k}:{id}")))
            })
            .collect()
    }
}

/// Заглушка id для [`fixed_ids`]: ULID, у которого `n:<id>` стоит 19 токенов bge-m3 — как
/// средний id набора (повторное ревью задачи 4: 1514 разных id пилота на гибриде, среднее
/// 19,17, разброс 14–24). Цифры `0…0` стоили бы 6 токенов и занизили бы счёт ~18 %.
pub const ID_STUB: &str = "01M6NYKVK8E7DVZVME2RR071DW";

/// Правило решающего счёта токенов (`SessionsInfo.count_rule`): id ссылок — заглушкой
/// средней стоимости [`ID_STUB`] (v1 — заглушка из нулей, до повторного ревью задачи 4).
pub const COUNT_RULE: &str = "fixed_ids v2 (ID_STUB)";

/// Текст для счёта токенов: `<буква>:<id>` и `c_<id>` (id — 26 символов) — постоянной
/// заглушкой [`ID_STUB`] средней стоимости: число токенов не зависит от случайных id загрузки.
pub fn fixed_ids(text: &str) -> String {
    const STUB: &str = ID_STUB;
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut i = 0;
    while i + 28 <= b.len() {
        let prev_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        let head =
            (b[i].is_ascii_lowercase() && b[i + 1] == b':') || (b[i] == b'c' && b[i + 1] == b'_');
        if prev_ok
            && head
            && b[i + 2..i + 28].iter().all(u8::is_ascii_alphanumeric)
            && b.get(i + 28).is_none_or(|c| !c.is_ascii_alphanumeric())
        {
            out.push_str(&text[last..i + 2]);
            out.push_str(STUB);
            i += 28;
            last = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&text[last..]);
    out
}

/// Строка помечена «уже выдавалось»: `(seen)`, `(выдавалось)` карточки или пометка заголовка
/// `/read` (M6 §7.3, `· уже выдавалось в этой сессии`).
fn marked_seen(line: &str) -> bool {
    line.contains("(seen)")
        || line.contains("(выдавалось)")
        || line.contains("(выдавалось, в выжимке)")
        || line.contains("· уже выдавалось в этой сессии")
}

/// Заголовки разделов дайджеста класса [`class::JOURNAL`].
const JOURNAL_SECTIONS: [&str; 4] = [
    "### Задачи",
    "### Ответы человека",
    "### Открытые вопросы",
    "### Окружение",
];
/// Строки дайджеста класса [`class::JOURNAL`] вне разделов (начало строки).
const JOURNAL_LINES: [&str; 3] = [
    "Словарь:",
    "Проверенные команды:",
    "ZeroAIMemory: карта кода",
];
const IDEAS_SECTION: &str = "### Идеи";
const SNAPSHOT_SECTION: &str = "### Последнее известное состояние";

/// Токены классов строк текста ([`class`]): строка — в класс своего раздела `### …` или по
/// своему началу; токены — счёт строк класса, соединённых переводом строки.
pub fn classify(text: &str, count: &dyn Fn(&str) -> usize) -> BTreeMap<String, usize> {
    let mut lines: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut section: Option<&str> = None;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("### ") || t.starts_with("## ") {
            section = None;
            if JOURNAL_SECTIONS.iter().any(|s| t.starts_with(s)) {
                section = Some(class::JOURNAL);
            } else if t.starts_with(IDEAS_SECTION) {
                section = Some(class::IDEAS);
            } else if t.starts_with(SNAPSHOT_SECTION) {
                section = Some(class::SNAPSHOT);
            }
        }
        let cls = if let Some(s) = section {
            Some(s)
        } else if t.contains("идея «") || t.contains("idea «") {
            Some(class::IDEAS)
        } else if t.contains("термин «") {
            Some(class::TERMS)
        } else if JOURNAL_LINES.iter().any(|p| t.starts_with(p)) {
            Some(class::JOURNAL)
        } else {
            None
        };
        if let Some(c) = cls {
            lines.entry(c).or_default().push(line);
        }
    }
    lines
        .into_iter()
        .map(|(c, ls)| (c.to_string(), count(&ls.join("\n"))))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Обязательные строки

/// Признак обязательной строки: ссылка набора (`rule:`, `task:`, `question:`, `idea:`) — строка
/// найдена, если в тексте есть ссылка на её id (`n:`/`k:`/`q:`/`w:`), — или текст.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Needle {
    Ref(String),
    Text(String),
}

/// Обязательная строка дайджеста или восстановления: вид, ключ, признак.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Required {
    pub kind: &'static str,
    pub key: String,
    pub needle: Needle,
}

/// Строка дайджеста об ожидающих запросах доступа (одинакова с M4): логина в ней нет, ссылки —
/// тоже, поэтому признак — текст.
const ACCESS_LINE: &str = "запросы доступа к вашим решениям ждут ответа";

/// Обязательные строки мира (§3.1): все Rules, задачи `in_progress`/`blocked`, ответы человека,
/// запросы доступа, идеи раздела `### Идеи` (`digest_ideas`). `ideas` — включать идеи.
pub fn required_lines(s: &SessionSpec, w: &World, ideas: bool) -> Vec<Required> {
    let r = |kind: &'static str, key: &str, set_ref: String| Required {
        kind,
        key: key.to_string(),
        needle: Needle::Ref(set_ref),
    };
    let mut out = Vec::new();
    for x in &w.rules {
        out.push(r(req::RULE, &x.key, format!("rule:{}", x.key)));
    }
    for t in &w.tasks {
        if matches!(t.status.as_str(), "in_progress" | "blocked") {
            out.push(r(req::TASK, &t.key, format!("task:{}", t.key)));
        }
    }
    for q in &w.questions {
        if q.answer.is_some() {
            out.push(r(req::ANSWER, &q.key, format!("question:{}", q.key)));
        }
    }
    for a in &w.access {
        out.push(Required {
            kind: req::ACCESS,
            key: a.key.clone(),
            needle: Needle::Text(ACCESS_LINE.to_string()),
        });
    }
    if ideas {
        for k in &s.digest_ideas {
            if w.ideas.iter().any(|i| &i.key == k) {
                out.push(r(req::IDEA, k, format!("idea:{k}")));
            }
        }
    }
    out
}

/// Строка выдана: ссылка на id признака (через `refs`) или текст признака целиком (после
/// [`nuggets::normalize_md`]).
fn line_found(text: &str, needle: &Needle, refs: &RefMap) -> bool {
    match needle {
        Needle::Ref(k) => refs.keys(text).iter().any(|x| x == k),
        Needle::Text(t) => nuggets::found(&nuggets::normalize_md(text), t),
    }
}

// ---------------------------------------------------------------------------------------------
// Воспроизведение

/// Ответ хука: текст для агента и новые строки `hooks.log`.
#[derive(Debug, Clone, Default)]
pub struct HookOut {
    pub text: String,
    pub log: Vec<String>,
    /// Непустой вывод, который не разобрался как JSON хука.
    pub bad_output: bool,
}

/// Исполнитель событий: хуки и MCP одной сессии (настоящий — процессы `zam`, в тестах — мок).
pub trait Driver {
    /// `zam hook <cmd>` с JSON события (без `cwd` — его ставит исполнитель).
    fn hook(&mut self, cmd: &str, input: Value) -> Result<HookOut>;
    /// `tools/call` через `zam mcp`: текст результата.
    fn mcp(&mut self, tool: &str, args: Value) -> Result<String>;
    /// Абсолютный путь файла проекта (как у Claude Code).
    fn abs(&self, rel: &str) -> String;
}

/// Шаг сессии — один ответ агенту.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StepRun {
    pub label: String,
    /// Номер события набора (`None` — старт сессии).
    pub event: Option<usize>,
    pub tool: String,
    /// Токены текста, в котором id ссылок заменены постоянной заглушкой ([`fixed_ids`]):
    /// случайные id каждой загрузки не дают шума (ревью задачи 4); решающая величина.
    pub tokens: usize,
    /// Токены текста как есть (справочно).
    #[serde(default)]
    pub tokens_raw: usize,
    /// Вывод хука не разобрался как JSON Claude Code (текст агенту неизвестен).
    #[serde(default)]
    pub bad_output: bool,
    /// Токены классов строк ([`classify`], на тексте [`fixed_ids`]).
    pub classes: BTreeMap<String, usize>,
    /// Ссылки набора в строках без пометки «уже выдавалось».
    pub refs: Vec<String>,
    /// Ссылки набора в строках с пометкой.
    pub marked: Vec<String>,
    pub text: String,
}

/// Проверка ожидаемых подсказок события.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HintCheck {
    pub event: usize,
    pub kind: String,
    pub expect: Vec<String>,
    pub shown: Vec<String>,
    /// Подсказки-идеи по порядку (`idea:<key>`).
    pub idea_hints: Vec<String>,
    /// Ожидаемые, выданные раньше в сессии (подсказка — раз за сессию: правило из дайджеста,
    /// идея из `### Идеи`, повтор insight).
    #[serde(default)]
    pub earlier: Vec<String>,
    /// Источник раньше выданной ожидаемой (по порядку `expect`; пусто — раньше не было):
    /// `digest` (`session_start`/`recovery`), `mcp`, `hooks` (прочие хуки).
    #[serde(default)]
    pub credit: Vec<String>,
}

/// Источник зачёта «было раньше» по метке шага.
fn source_of(l: &str) -> &'static str {
    match l {
        label::SESSION_START | label::RECOVERY => "digest",
        label::QUESTION | label::REPEAT | label::SUBAGENT_MCP => "mcp",
        _ => "hooks",
    }
}

/// Проверка обязательной строки в выдаче шага `step` (`session_start` или `recovery`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequiredCheck {
    pub step: String,
    pub kind: String,
    pub key: String,
    pub found: bool,
}

/// Вопрос по протоколу ТС-1.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuestionRun {
    pub event: usize,
    pub id: String,
    pub repeat: bool,
    pub selected: Vec<String>,
    pub reread: Vec<String>,
    pub nuggets_total: usize,
    pub hits: Vec<bool>,
    pub note: String,
}

/// Итог сессии.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionRun {
    pub id: String,
    pub profile: String,
    pub split: String,
    pub steps: Vec<StepRun>,
    pub hints: Vec<HintCheck>,
    pub required: Vec<RequiredCheck>,
    pub questions: Vec<QuestionRun>,
    /// Дайджест второго старта (другая сессия, до первого) побайтово равен первому.
    pub digest_match: Option<bool>,
    /// События, которые плечо не воспроизводит (метки шагов).
    pub skipped: Vec<String>,
    pub notes: Vec<String>,
    /// Номер повтора сессии (`--repeat K`: свежий сервер на каждый повтор).
    #[serde(default)]
    pub rep: usize,
}

/// Счётчик токенов.
pub type Count<'a> = &'a dyn Fn(&str) -> usize;

struct Play<'a> {
    d: &'a mut dyn Driver,
    refs: &'a RefMap,
    count: Count<'a>,
    run: SessionRun,
}

impl Play<'_> {
    fn step(&mut self, label: &str, event: Option<usize>, tool: &str, text: String) -> usize {
        let mut refs = Vec::new();
        let mut marked = Vec::new();
        for line in text.lines() {
            let keys = self.refs.keys(line);
            let into = if marked_seen(line) {
                &mut marked
            } else {
                &mut refs
            };
            for k in keys {
                if !into.contains(&k) {
                    into.push(k);
                }
            }
        }
        let fixed = fixed_ids(&text);
        let n = |t: &str| if t.is_empty() { 0 } else { (self.count)(t) };
        self.run.steps.push(StepRun {
            label: label.to_string(),
            event,
            tool: tool.to_string(),
            tokens: n(&fixed),
            tokens_raw: n(&text),
            bad_output: false,
            classes: classify(&fixed, self.count),
            refs,
            marked,
            text,
        });
        self.run.steps.len() - 1
    }

    fn hook(
        &mut self,
        label: &str,
        event: Option<usize>,
        cmd: &str,
        input: Value,
    ) -> Result<usize> {
        let out = self.d.hook(cmd, input)?;
        for l in out.log {
            self.run.notes.push(format!("hooks.log ({cmd}): {l}"));
        }
        let i = self.step(label, event, cmd, out.text);
        if out.bad_output {
            self.run.steps[i].bad_output = true;
            self.run
                .notes
                .push(format!("{cmd} ({label}): вывод хука — не JSON Claude Code"));
        }
        Ok(i)
    }

    fn mcp(&mut self, label: &str, event: usize, tool: &str, args: Value) -> Result<usize> {
        let text = self.d.mcp(tool, args)?;
        Ok(self.step(label, Some(event), tool, text))
    }

    fn text(&self, i: usize) -> &str {
        &self.run.steps[i].text
    }

    fn required(&mut self, step: usize, req: &[Required], kinds_skip: &[&str]) {
        let label = self.run.steps[step].label.clone();
        for r in req.iter().filter(|r| !kinds_skip.contains(&r.kind)) {
            let found = line_found(&self.run.steps[step].text, &r.needle, self.refs);
            self.run.required.push(RequiredCheck {
                step: label.clone(),
                kind: r.kind.to_string(),
                key: r.key.clone(),
                found,
            });
        }
    }

    fn hints(&mut self, event: usize, kind: &str, expect: &[String], steps: &[usize]) {
        let mut shown: Vec<String> = Vec::new();
        let mut ideas: Vec<String> = Vec::new();
        let first = steps.iter().copied().min().unwrap_or(self.run.steps.len());
        // после compact контекст агента потерян: «было раньше» — только с последнего пакета
        // восстановления (он сам — дайджест)
        let from = self.run.steps[..first]
            .iter()
            .rposition(|s| s.label == label::RECOVERY)
            .unwrap_or(0);
        let mut earlier: Vec<String> = Vec::new();
        let mut credit: Vec<String> = Vec::new();
        for e in expect {
            let src = self.run.steps[from..first]
                .iter()
                .find(|s| s.refs.contains(e) || s.marked.contains(e))
                .map(|s| source_of(&s.label));
            match src {
                Some(src) => {
                    earlier.push(e.clone());
                    credit.push(src.to_string());
                }
                None => credit.push(String::new()),
            }
        }
        for &s in steps {
            for k in self.refs.keys(&self.run.steps[s].text) {
                if k.starts_with("idea:") && !ideas.contains(&k) {
                    ideas.push(k.clone());
                }
                if !shown.contains(&k) {
                    shown.push(k);
                }
            }
        }
        self.run.hints.push(HintCheck {
            event,
            kind: kind.to_string(),
            expect: expect.to_vec(),
            shown,
            idea_hints: ideas,
            earlier,
            credit,
        });
    }

    /// Вопрос по протоколу ТС-1 (§3.1, §3.4): поиск brief → оракул → чтение 4000 → дочитывание →
    /// выжимка в кэш (у повтора — без выжимки).
    #[allow(clippy::too_many_arguments)]
    fn question(
        &mut self,
        event: usize,
        label: &str,
        id: &str,
        query: &str,
        expect: &[String],
        nuggets_spec: &[NuggetSpec],
        put: bool,
    ) -> Result<()> {
        let scope = json!({ "solution": EVAL_SOLUTION });
        let s = self.mcp(
            label,
            event,
            "memory_search",
            json!({ "query": query, "scope": scope, "mode": "brief" }),
        )?;
        let mut selected: Vec<(String, String)> = Vec::new();
        for (k, raw) in self.refs.pairs(self.text(s)) {
            if expect.contains(&k) && !selected.iter().any(|(_, r)| *r == raw) {
                selected.push((k, raw));
            }
        }
        let mut q = QuestionRun {
            event,
            id: id.to_string(),
            repeat: !put,
            selected: selected.iter().map(|(k, _)| k.clone()).collect(),
            nuggets_total: nuggets_spec.len(),
            ..QuestionRun::default()
        };
        let mut delivered = self.text(s).to_string();
        if selected.is_empty() {
            q.note = "ни одного ожидаемого источника в карточках".into();
        } else {
            let items: Vec<Value> = selected.iter().map(|(_, r)| json!({ "id": r })).collect();
            let r = self.mcp(
                label,
                event,
                "memory_read",
                json!({ "items": items, "budget_tokens": READ_BUDGET }),
            )?;
            let read = self.text(r).to_string();
            delivered.push('\n');
            delivered.push_str(&read);
            let raws: Vec<String> = selected.iter().map(|(_, raw)| raw.clone()).collect();
            let elements = read_elements(&read, &raws);
            for (k, raw) in &selected {
                let norm = nuggets::normalize_md(&delivered);
                let missing = nuggets_spec
                    .iter()
                    .filter(|n| &n.source == k)
                    .any(|n| !nuggets::found(&norm, &n.text));
                if !missing {
                    continue;
                }
                let Some(e) = elements.iter().find(|e| &e.raw == raw) else {
                    continue;
                };
                if let Some(need) = reread_need(&e.text) {
                    let again = self.mcp(
                        label,
                        event,
                        "memory_read",
                        json!({ "items": reread_items(raw, &need), "budget_tokens": READ_BUDGET }),
                    )?;
                    delivered.push('\n');
                    delivered.push_str(self.text(again));
                    q.reread.push(k.clone());
                }
            }
            if put {
                let summary = super::tokens::summary_of(query, &read);
                let sources: Vec<Value> =
                    selected.iter().map(|(_, r)| json!({ "id": r })).collect();
                self.mcp(
                    label,
                    event,
                    "memory_cache_put",
                    json!({ "query": query, "summary": summary, "sources": sources }),
                )?;
            }
        }
        let norm = nuggets::normalize_md(&delivered);
        q.hits = nuggets_spec
            .iter()
            .map(|n| nuggets::found(&norm, &n.text))
            .collect();
        self.run.questions.push(q);
        Ok(())
    }
}

/// JSON события хука как от Claude Code.
fn hook_input(event: &str, session: &str, extra: Value) -> Value {
    let mut v = json!({ "hook_event_name": event, "session_id": session });
    if let (Value::Object(o), Value::Object(e)) = (&mut v, extra) {
        o.extend(e);
    }
    v
}

/// Воспроизводит сессию `spec` (мир `w`, ссылки `refs`, возможности плеча `f`) через `d`.
/// `sid` — id сессии агента.
pub fn play(
    d: &mut dyn Driver,
    spec: &SessionSpec,
    w: &World,
    f: &Feats,
    refs: &RefMap,
    count: Count<'_>,
    sid: &str,
) -> Result<SessionRun> {
    let mut p = Play {
        d,
        refs,
        count,
        run: SessionRun {
            id: spec.id.clone(),
            profile: spec.profile.as_str().to_string(),
            ..SessionRun::default()
        },
    };
    let req = required_lines(spec, w, f.ideas_on);
    let skip_kinds: Vec<String> = f.excluded_required();
    let skip: Vec<&str> = skip_kinds.iter().map(String::as_str).collect();
    // второй старт — другой сессией и до первого: у неявного пути MCP последней остаётся сессия
    // агента, а первый дайджест сессии — с разделом `### Идеи`
    let other = format!("{sid}{SECOND_START}");
    let second = p.d.hook(
        "session-start",
        hook_input("SessionStart", &other, json!({ "source": "startup" })),
    )?;
    let first = p.hook(
        label::SESSION_START,
        None,
        "session-start",
        hook_input("SessionStart", sid, json!({ "source": "startup" })),
    )?;
    p.run.digest_match = Some(second.text == p.text(first));
    p.required(first, &req, &skip);
    let mut asked: BTreeMap<String, (String, Vec<String>, Vec<NuggetSpec>)> = BTreeMap::new();
    let mut skipped: BTreeSet<String> = BTreeSet::new();
    for (i, ev) in spec.events.iter().enumerate() {
        match ev {
            Event::Prompt { text, expect } => {
                let s = p.hook(
                    label::PROMPT,
                    Some(i),
                    "prompt",
                    hook_input("UserPromptSubmit", sid, json!({ "prompt": text })),
                )?;
                p.hints(i, ev.kind(), expect, &[s]);
            }
            Event::Question {
                id,
                query,
                expect,
                nuggets: ns,
            } => {
                p.question(i, label::QUESTION, id, query, expect, ns, true)?;
                asked.insert(id.clone(), (query.clone(), expect.clone(), ns.clone()));
            }
            Event::Repeat { of } => {
                let (query, expect, ns) = asked
                    .get(of)
                    .cloned()
                    .with_context(|| format!("repeat {of}: вопроса не было"))?;
                p.question(i, label::REPEAT, of, &query, &expect, &ns, false)?;
            }
            Event::ToolError {
                command,
                error,
                expect,
            } => {
                let ti = json!({ "command": command });
                let a = p.hook(
                    label::PRE_TOOL,
                    Some(i),
                    "pre-tool",
                    hook_input(
                        "PreToolUse",
                        sid,
                        json!({ "tool_name": "Bash", "tool_input": ti }),
                    ),
                )?;
                let b = p.hook(
                    label::POST_TOOL,
                    Some(i),
                    "post-tool",
                    hook_input(
                        "PostToolUseFailure",
                        sid,
                        json!({ "tool_name": "Bash", "tool_input": ti, "error": error }),
                    ),
                )?;
                p.hints(i, ev.kind(), expect, &[a, b]);
            }
            Event::ReadFile { path, expect } => {
                let ti = json!({ "file_path": p.d.abs(path) });
                let a = p.hook(
                    label::PRE_TOOL,
                    Some(i),
                    "pre-tool",
                    hook_input(
                        "PreToolUse",
                        sid,
                        json!({ "tool_name": "Read", "tool_input": ti }),
                    ),
                )?;
                let mut steps = vec![a];
                if f.file_context {
                    let b = p.hook(
                        label::FILE_CONTEXT,
                        Some(i),
                        "post-tool",
                        hook_input(
                            "PostToolUse",
                            sid,
                            json!({ "tool_name": "Read", "tool_input": ti,
                                    "tool_response": { "type": "text" } }),
                        ),
                    )?;
                    steps.push(b);
                } else {
                    skipped.insert(label::FILE_CONTEXT.into());
                }
                p.hints(i, ev.kind(), expect, &steps);
            }
            Event::Subagent {
                agent_type,
                task,
                queries,
            } => {
                if !f.subagent {
                    skipped.insert(label::SUBAGENT_START.into());
                    skipped.insert(label::SUBAGENT_MCP.into());
                    continue;
                }
                p.hook(
                    label::SUBAGENT_START,
                    Some(i),
                    "pre-tool",
                    hook_input(
                        "PreToolUse",
                        sid,
                        json!({ "tool_name": "Agent",
                                "tool_input": { "subagent_type": agent_type, "prompt": task } }),
                    ),
                )?;
                p.hook(
                    label::SUBAGENT_START,
                    Some(i),
                    "subagent-start",
                    hook_input(
                        "SubagentStart",
                        sid,
                        json!({ "agent_id": format!("{sid}-agent{i}"), "agent_type": agent_type }),
                    ),
                )?;
                for q in queries {
                    let scope = json!({ "solution": EVAL_SOLUTION });
                    p.mcp(
                        label::SUBAGENT_MCP,
                        i,
                        "memory_search",
                        json!({ "query": q, "scope": scope, "mode": "brief" }),
                    )?;
                }
            }
            Event::Compact => {
                if !f.recovery {
                    skipped.insert(label::PRE_COMPACT.into());
                    skipped.insert(label::RECOVERY.into());
                    continue;
                }
                p.hook(
                    label::PRE_COMPACT,
                    Some(i),
                    "checkpoint",
                    hook_input("PreCompact", sid, json!({ "trigger": "auto" })),
                )?;
                let r = p.hook(
                    label::RECOVERY,
                    Some(i),
                    "session-start",
                    hook_input("SessionStart", sid, json!({ "source": "compact" })),
                )?;
                let mut skip_r = skip.clone();
                skip_r.push(req::IDEA);
                p.required(r, &req, &skip_r);
            }
            Event::Checkpoint => {
                p.hook(
                    label::CHECKPOINT,
                    Some(i),
                    "checkpoint",
                    hook_input("Stop", sid, json!({ "stop_hook_active": false })),
                )?;
            }
        }
    }
    p.run.skipped = skipped.into_iter().collect();
    Ok(p.run)
}

// ---------------------------------------------------------------------------------------------
// Сводка

/// Доля `num / den` (0 при `den = 0`).
fn share(num: usize, den: usize) -> Option<f64> {
    (den > 0).then(|| num as f64 / den as f64)
}

/// Счёт «найдено из ожидаемого».
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Tally {
    pub found: usize,
    pub total: usize,
}

impl Tally {
    pub fn share(&self) -> Option<f64> {
        share(self.found, self.total)
    }
    fn add(&mut self, hit: bool) {
        self.total += 1;
        if hit {
            self.found += 1;
        }
    }
}

/// Сводка плеча (или сравнения после исключений).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionsSummary {
    pub sessions: usize,
    /// Σ токенов по меткам шагов.
    pub tokens_by_label: BTreeMap<String, usize>,
    pub tokens_total: usize,
    /// Обязательные строки дайджеста и восстановления.
    pub required: Tally,
    pub required_missing: Vec<String>,
    /// `hint_recall` без подсказок-идей: ожидаемая выдана в событии или раньше в сессии.
    pub hint_recall: Tally,
    /// То же, только в ответе самого события (справочно).
    #[serde(default)]
    pub hint_recall_strict: Tally,
    /// `hint_recall` подсказок-идей.
    pub idea_hint_recall: Tally,
    /// То же, только в ответе события.
    #[serde(default)]
    pub idea_hint_recall_strict: Tally,
    /// `hint_recall` по видам ожиданий (`rule`, `insight`, `term`, `idea`): зачёт и строго.
    #[serde(default)]
    pub hint_by_kind: BTreeMap<String, Tally>,
    #[serde(default)]
    pub hint_by_kind_strict: BTreeMap<String, Tally>,
    /// Чем засчитаны выданные ожидаемые: `event` (ответ события), `digest`, `mcp`, `hooks`.
    #[serde(default)]
    pub hint_credit: BTreeMap<String, usize>,
    /// Нагеты вопросов (микро-полнота).
    pub nuggets: Tally,
    /// Совпадение дайджеста при двух стартах.
    pub digest_match: Tally,
    /// Повторная выдача: ссылки строк без пометки, уже выданные раньше в сессии тем же каналом
    /// (хуки, MCP агента, MCP субагента).
    pub repeated: Tally,
    /// То же между каналами (справочно).
    #[serde(default)]
    pub repeated_cross: Tally,
    /// Шаги с неразобранным выводом хука.
    #[serde(default)]
    pub bad_outputs: usize,
    /// Хуки, не уложившиеся в бюджет (`hooks.log`: «бюджет исчерпан», «тайм-аут»): ответ агенту
    /// пуст или неполон — так ведёт себя продукт, шаг считается как есть.
    #[serde(default)]
    pub hook_budget_misses: usize,
    /// Повторы (`<сессия> r<повтор>`) с промахом бюджета хука или неразобранным выводом: они
    /// действительны (поведение продукта), но любой такой повтор в плече блокирует приёмку 13b.
    #[serde(default)]
    pub reps_with_misses: Vec<String>,
    /// Обязательные строки по повторам (все сессии повтора): критерий «= 1,0» — по худшему.
    #[serde(default)]
    pub required_by_rep: BTreeMap<usize, Tally>,
    /// Повторов на сессию (`--repeat`) и 95-й перцентиль |токены повтора − среднее| / среднее
    /// сессии — шум A/A внутри плеча.
    #[serde(default)]
    pub reps: usize,
    #[serde(default)]
    pub rep_noise_p95: Option<f64>,
    /// Токены идей по меткам (`session_start`, `prompt`).
    pub idea_tokens: BTreeMap<String, usize>,
    /// Токены снимка сессии по меткам.
    pub snapshot_tokens: BTreeMap<String, usize>,
}

/// Токены шага после вычета классов `ex`.
pub fn step_tokens(s: &StepRun, ex: &BTreeSet<String>) -> usize {
    let minus: usize = s
        .classes
        .iter()
        .filter(|(c, _)| ex.contains(*c))
        .map(|(_, n)| *n)
        .sum();
    s.tokens.saturating_sub(minus)
}

/// Повторная выдача сессии: `(повторов, всего)` ссылок строк без пометки «уже выдавалось»,
/// выданных тем же каналом (хуки или MCP) в **прошлых** событиях сессии (карточка и чтение того
/// же источника в одном вопросе — не повтор); пакет восстановления (контекст потерян) не
/// считается.
fn repeats(r: &SessionRun, labels_ex: &BTreeSet<String>, cross: bool) -> Tally {
    // (канал, ссылка) прошлых событий и текущего; `cross` — один общий канал
    let mut seen: BTreeSet<(u8, &str)> = BTreeSet::new();
    let mut current: BTreeSet<(u8, &str)> = BTreeSet::new();
    let mut event: Option<Option<usize>> = None;
    let mut t = Tally::default();
    for s in &r.steps {
        if labels_ex.contains(&s.label) || s.label == label::RECOVERY {
            continue;
        }
        if event != Some(s.event) {
            seen.append(&mut current);
            event = Some(s.event);
        }
        let ch = match s.label.as_str() {
            _ if cross => 0,
            label::QUESTION | label::REPEAT => 1,
            label::SUBAGENT_MCP => 2,
            _ => 0,
        };
        for k in &s.refs {
            if current.insert((ch, k.as_str())) {
                t.add(seen.contains(&(ch, k.as_str())));
            }
        }
    }
    t
}

/// Строка `hooks.log` (в примечаниях сессии) — хук не уложился в бюджет: остаток бюджета
/// кончился до запроса ([`zam_core::HOOK_BUDGET_SPENT`] — та же константа у клиента) или запрос
/// оборвался по сроку (ошибка сети `ureq` — «timeout» / «timed out»). Строки фоновой
/// синхронизации карты кода (`code-sync`) — не промах хука.
pub fn budget_miss(note: &str) -> bool {
    // строки фоновой `zam code sync` (источник `code-sync`): её сроки — не бюджет хука
    let code_sync = note
        .split_whitespace()
        .nth(3)
        .is_some_and(|src| src == "code-sync");
    note.starts_with("hooks.log")
        && !code_sync
        && (note.contains(zam_core::HOOK_BUDGET_SPENT)
            || (note.contains("недоступен")
                && (note.contains("timeout") || note.contains("timed out"))))
}

/// Исключения сравнения: метки шагов, классы строк, виды обязательных строк, виды ожиданий.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Exclusions {
    pub labels: BTreeSet<String>,
    pub classes: BTreeSet<String>,
    pub required: BTreeSet<String>,
    pub expect: BTreeSet<String>,
}

impl Exclusions {
    pub fn of(f: &Feats) -> Exclusions {
        Exclusions {
            labels: f.excluded_labels().into_iter().collect(),
            classes: f.excluded_classes().into_iter().collect(),
            required: f.excluded_required().into_iter().collect(),
            expect: f.excluded_expect().into_iter().collect(),
        }
    }

    /// Объединение исключений двух плеч.
    pub fn union(&self, o: &Exclusions) -> Exclusions {
        let u = |a: &BTreeSet<String>, b: &BTreeSet<String>| a.union(b).cloned().collect();
        Exclusions {
            labels: u(&self.labels, &o.labels),
            classes: u(&self.classes, &o.classes),
            required: u(&self.required, &o.required),
            expect: u(&self.expect, &o.expect),
        }
    }
}

fn expect_kind(e: &str) -> &str {
    e.split_once(':').map_or(e, |(k, _)| k)
}

/// Токены сессии по меткам после исключений.
pub fn session_tokens(r: &SessionRun, ex: &Exclusions) -> BTreeMap<String, usize> {
    let mut m: BTreeMap<String, usize> = BTreeMap::new();
    for s in &r.steps {
        if ex.labels.contains(&s.label) {
            continue;
        }
        *m.entry(s.label.clone()).or_default() += step_tokens(s, &ex.classes);
    }
    m
}

/// Сводка сессий с исключениями `ex`.
pub fn summarize(runs: &[SessionRun], ex: &Exclusions) -> SessionsSummary {
    let mut sum = SessionsSummary {
        sessions: runs.len(),
        ..SessionsSummary::default()
    };
    for r in runs {
        for (l, n) in session_tokens(r, ex) {
            *sum.tokens_by_label.entry(l).or_default() += n;
            sum.tokens_total += n;
        }
        for s in r.steps.iter().filter(|s| !ex.labels.contains(&s.label)) {
            if let Some(n) = s.classes.get(class::IDEAS) {
                *sum.idea_tokens.entry(s.label.clone()).or_default() += n;
            }
            if let Some(n) = s.classes.get(class::SNAPSHOT) {
                *sum.snapshot_tokens.entry(s.label.clone()).or_default() += n;
            }
        }
        for c in &r.required {
            if ex.required.contains(&c.kind) || ex.labels.contains(&c.step) {
                continue;
            }
            sum.required.add(c.found);
            sum.required_by_rep.entry(r.rep).or_default().add(c.found);
            if !c.found {
                sum.required_missing
                    .push(format!("{} {} {}:{}", r.id, c.step, c.kind, c.key));
            }
        }
        for h in &r.hints {
            for e in &h.expect {
                let k = expect_kind(e);
                if ex.expect.contains(k) {
                    continue;
                }
                let now = h.shown.contains(e);
                let hit = now || h.earlier.contains(e);
                if k == "idea" {
                    sum.idea_hint_recall.add(hit);
                    sum.idea_hint_recall_strict.add(now);
                } else {
                    sum.hint_recall.add(hit);
                    sum.hint_recall_strict.add(now);
                }
                sum.hint_by_kind.entry(k.to_string()).or_default().add(hit);
                sum.hint_by_kind_strict
                    .entry(k.to_string())
                    .or_default()
                    .add(now);
                let src = if now {
                    "event".to_string()
                } else {
                    let i = h.expect.iter().position(|x| x == e).unwrap_or(0);
                    h.credit.get(i).cloned().unwrap_or_default()
                };
                if !src.is_empty() {
                    *sum.hint_credit.entry(src).or_default() += 1;
                }
            }
        }
        for q in &r.questions {
            for h in &q.hits {
                sum.nuggets.add(*h);
            }
        }
        if let Some(m) = r.digest_match {
            sum.digest_match.add(m);
        }
        let t = repeats(r, &ex.labels, false);
        sum.repeated.found += t.found;
        sum.repeated.total += t.total;
        let t = repeats(r, &ex.labels, true);
        sum.repeated_cross.found += t.found;
        sum.repeated_cross.total += t.total;
        sum.bad_outputs += r.steps.iter().filter(|s| s.bad_output).count();
        let misses = r.notes.iter().filter(|n| budget_miss(n)).count();
        sum.hook_budget_misses += misses;
        if misses > 0 || r.steps.iter().any(|s| s.bad_output) {
            sum.reps_with_misses.push(format!("{} r{}", r.id, r.rep));
        }
    }
    let mut by_id: BTreeMap<&str, Vec<&SessionRun>> = BTreeMap::new();
    for r in runs {
        by_id.entry(r.id.as_str()).or_default().push(r);
    }
    sum.sessions = by_id.len();
    sum.reps = by_id.values().map(Vec::len).max().unwrap_or(0);
    // разброс повторов относительно среднего сессии (повторное ревью задачи 4)
    let mut d = Vec::new();
    for rs in by_id.values().filter(|rs| rs.len() > 1) {
        let tot: Vec<f64> = rs
            .iter()
            .map(|r| session_tokens(r, ex).values().sum::<usize>() as f64)
            .collect();
        let mean = tot.iter().sum::<f64>() / tot.len() as f64;
        if mean > 0.0 {
            d.extend(tot.iter().map(|t| (t - mean).abs() / mean));
        }
    }
    sum.rep_noise_p95 = (!d.is_empty()).then(|| quantile(&d, 0.95));
    sum
}

// ---------------------------------------------------------------------------------------------
// Результат и сравнение

/// Сведения прогона.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionsInfo {
    pub date: String,
    pub git: String,
    pub label: Option<String>,
    pub split: String,
    pub profile: Option<String>,
    pub base: Option<String>,
    pub mode: String,
    pub model: Option<String>,
    pub server_version: String,
    pub server_git: Option<String>,
    pub server_m6: bool,
    pub ideas_flag: bool,
    pub feats: Feats,
    pub exclusions: Exclusions,
    pub mcp: String,
    pub counter: String,
    pub sessions_blake3: String,
    /// Отпечаток корпуса ТС-2 (`eval/sessions-corpus` целиком, с `code/`; blake3, `\r\n` → `\n`).
    #[serde(default)]
    pub corpus_blake3: String,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub repeat: usize,
    /// Правило решающего счёта ([`COUNT_RULE`]; пусто — файл до правила v2). Сохранённые
    /// плечи при `--arm`/`--compare` пересчитываются текущим правилом ([`recount`]).
    #[serde(default)]
    pub count_rule: String,
    pub notes: Vec<String>,
}

/// Результат плеча.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionsResult {
    pub info: SessionsInfo,
    pub summary: SessionsSummary,
    pub sessions: Vec<SessionRun>,
}

/// `R` по метке: Σ средних по повторам токенов сессий.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelRatio {
    pub label: String,
    pub base: f64,
    pub arm: f64,
    pub r: Option<Estimate>,
}

/// Сравнение на части сессий (весь набор или профиль).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartComparison {
    /// `всего`, `standard`, `heavy`.
    pub part: String,
    pub sessions: Vec<String>,
    pub by_label: Vec<LabelRatio>,
    pub total: LabelRatio,
    pub base_summary: SessionsSummary,
    pub arm_summary: SessionsSummary,
    /// Δ = плечо − база на общих элементах (односторонние 95 % границы, бутстреп по сессиям):
    /// `hint_recall` общий (зачёт и строго) и полнота обязательных строк.
    pub d_hint_recall: Option<Estimate>,
    pub d_hint_recall_strict: Option<Estimate>,
    pub d_required: Option<Estimate>,
}

/// Сравнение плеча с базой на пересечении событий.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsComparison {
    pub base_name: String,
    pub arm_name: String,
    pub base_server: String,
    pub arm_server: String,
    pub only_base: Vec<String>,
    pub only_arm: Vec<String>,
    pub exclusions: Exclusions,
    /// Весь набор, затем профили.
    pub parts: Vec<PartComparison>,
    /// Подсказки-идеи `prompt`: доля событий с тем же списком идей, что у базы (обе с идеями).
    pub idea_hints_match: Option<Tally>,
    /// 95-й перцентиль |Δ| / база токенов сессии (порог шума A/A, если плечи одной версии).
    pub abs_rel_diff_p95: Option<f64>,
    pub same_server: bool,
    pub bootstrap_samples: usize,
    pub bootstrap_seed: u64,
}

/// Подсказки-идеи `prompt` плеча и базы (повтор 0): доля общих событий с тем же списком.
pub fn idea_hints_match(base: &[SessionRun], arm: &[SessionRun]) -> Tally {
    let mut t = Tally::default();
    for a in arm.iter().filter(|r| r.rep == 0) {
        let Some(b) = base.iter().find(|b| b.id == a.id && b.rep == 0) else {
            continue;
        };
        for h in a.hints.iter().filter(|h| h.kind == "prompt") {
            if let Some(hb) = b.hints.iter().find(|x| x.event == h.event) {
                t.add(hb.idea_hints == h.idea_hints);
            }
        }
    }
    t
}

/// Средние по повторам токены сессии `id` по меткам.
fn mean_tokens(rs: &[SessionRun], id: &str, ex: &Exclusions) -> BTreeMap<String, f64> {
    let reps: Vec<&SessionRun> = rs.iter().filter(|r| r.id == id).collect();
    let mut m: BTreeMap<String, f64> = BTreeMap::new();
    for r in &reps {
        for (l, n) in session_tokens(r, ex) {
            *m.entry(l).or_default() += n as f64 / reps.len() as f64;
        }
    }
    m
}

/// Элементы зачёта повтора сессии: (событие, ожидание) → (засчитан, строго).
type HintElems = BTreeMap<(usize, String), (bool, bool)>;
/// Обязательные строки повтора: (шаг, вид, ключ) → найдена.
type ReqElems = BTreeMap<(String, String, String), bool>;

/// Элементы зачёта повтора сессии.
fn hint_elements(r: &SessionRun, ex: &Exclusions) -> HintElems {
    let mut m = BTreeMap::new();
    for h in &r.hints {
        for e in &h.expect {
            let k = expect_kind(e);
            if ex.expect.contains(k) || k == "idea" {
                continue;
            }
            let now = h.shown.contains(e);
            m.insert((h.event, e.clone()), (now || h.earlier.contains(e), now));
        }
    }
    m
}

fn required_elements(r: &SessionRun, ex: &Exclusions) -> ReqElems {
    r.required
        .iter()
        .filter(|c| !ex.required.contains(&c.kind) && !ex.labels.contains(&c.step))
        .map(|c| ((c.step.clone(), c.kind.clone(), c.key.clone()), c.found))
        .collect()
}

/// Δ доли на общих элементах сессий: `get(сессия) → (база, плечо)` долей и число элементов.
fn delta(per: &[(f64, f64, f64)]) -> Option<Estimate> {
    let per: Vec<&(f64, f64, f64)> = per.iter().filter(|p| p.2 > 0.0).collect();
    if per.is_empty() {
        return None;
    }
    let a: Vec<f64> = per.iter().map(|p| p.0).collect();
    let b: Vec<f64> = per.iter().map(|p| p.1).collect();
    let w: Vec<f64> = per.iter().map(|p| p.2).collect();
    Some(paired_diff_weighted(&a, &b, &w, BOOTSTRAP_M6, SEED))
}

fn part(
    name: &str,
    ids: &[String],
    base: &[SessionRun],
    arm: &[SessionRun],
    ex: &Exclusions,
) -> PartComparison {
    let keep = |rs: &[SessionRun]| -> Vec<SessionRun> {
        rs.iter().filter(|r| ids.contains(&r.id)).cloned().collect()
    };
    let (bs, as_) = (keep(base), keep(arm));
    let tb: Vec<BTreeMap<String, f64>> = ids.iter().map(|i| mean_tokens(&bs, i, ex)).collect();
    let ta: Vec<BTreeMap<String, f64>> = ids.iter().map(|i| mean_tokens(&as_, i, ex)).collect();
    let labels: BTreeSet<String> = tb
        .iter()
        .chain(&ta)
        .flat_map(|m| m.keys().cloned())
        .collect();
    let ratio = |l: Option<&str>| -> LabelRatio {
        let get = |m: &BTreeMap<String, f64>| -> f64 {
            match l {
                Some(l) => m.get(l).copied().unwrap_or(0.0),
                None => m.values().sum::<f64>(),
            }
        };
        let pairs: Vec<(f64, f64)> = ta.iter().zip(&tb).map(|(a, b)| (get(a), get(b))).collect();
        LabelRatio {
            label: l.unwrap_or("всего").to_string(),
            base: pairs.iter().map(|p| p.1).sum(),
            arm: pairs.iter().map(|p| p.0).sum(),
            r: ratio_estimate_with(&pairs, BOOTSTRAP_M6, SEED),
        }
    };
    // доли на сессию — среднее по всем повторам плеча (повторное ревью задачи 4, I2); элементы —
    // общие для всех повторов обоих плеч
    let mut dh = Vec::new();
    let mut dhs = Vec::new();
    let mut dr = Vec::new();
    for id in ids {
        let rb: Vec<&SessionRun> = bs.iter().filter(|r| &r.id == id).collect();
        let ra: Vec<&SessionRun> = as_.iter().filter(|r| &r.id == id).collect();
        if rb.is_empty() || ra.is_empty() {
            continue;
        }
        let hints: Vec<_> = rb.iter().chain(&ra).map(|r| hint_elements(r, ex)).collect();
        let common: Vec<(usize, String)> = hints[0]
            .keys()
            .filter(|k| hints.iter().all(|m| m.contains_key(*k)))
            .cloned()
            .collect();
        let n = common.len() as f64;
        let mean_share = |ms: &[HintElems], strict: bool| {
            ms.iter()
                .map(|m| {
                    common
                        .iter()
                        .filter(|k| {
                            let v = m[*k];
                            if strict { v.1 } else { v.0 }
                        })
                        .count() as f64
                        / n.max(1.0)
                })
                .sum::<f64>()
                / ms.len() as f64
        };
        let (hb, ha) = hints.split_at(rb.len());
        dh.push((mean_share(hb, false), mean_share(ha, false), n));
        dhs.push((mean_share(hb, true), mean_share(ha, true), n));
        let reqs: Vec<_> = rb
            .iter()
            .chain(&ra)
            .map(|r| required_elements(r, ex))
            .collect();
        let common: Vec<(String, String, String)> = reqs[0]
            .keys()
            .filter(|k| reqs.iter().all(|m| m.contains_key(*k)))
            .cloned()
            .collect();
        let n = common.len() as f64;
        let mean_req = |ms: &[ReqElems]| {
            ms.iter()
                .map(|m| common.iter().filter(|k| m[*k]).count() as f64 / n.max(1.0))
                .sum::<f64>()
                / ms.len() as f64
        };
        let (qb, qa) = reqs.split_at(rb.len());
        dr.push((mean_req(qb), mean_req(qa), n));
    }
    PartComparison {
        part: name.to_string(),
        sessions: ids.to_vec(),
        by_label: labels.iter().map(|l| ratio(Some(l))).collect(),
        total: ratio(None),
        base_summary: summarize(&bs, ex),
        arm_summary: summarize(&as_, ex),
        d_hint_recall: delta(&dh),
        d_hint_recall_strict: delta(&dhs),
        d_required: delta(&dr),
    }
}

/// Сравнение плеча `arm` с базой `base` (§3.3, §3.5): общие сессии, исключения обоих плеч;
/// весь набор и профили отдельно.
pub fn compare(
    base: &SessionsResult,
    arm: &SessionsResult,
    names: (&str, &str),
) -> Result<SessionsComparison> {
    let (bi, ai) = (&base.info, &arm.info);
    let mut errs = Vec::new();
    let mut same = |what: &str, x: &dyn std::fmt::Debug, y: &dyn std::fmt::Debug, eq: bool| {
        if !eq {
            errs.push(format!("{what}: база {x:?}, плечо {y:?}"));
        }
    };
    same("часть набора", &bi.split, &ai.split, bi.split == ai.split);
    same(
        "набор (blake3 eval/sessions.toml)",
        &bi.sessions_blake3,
        &ai.sessions_blake3,
        bi.sessions_blake3 == ai.sessions_blake3,
    );
    same(
        "корпус ТС-2 (blake3 eval/sessions-corpus)",
        &bi.corpus_blake3,
        &ai.corpus_blake3,
        bi.corpus_blake3 == ai.corpus_blake3,
    );
    same(
        "счётчик",
        &bi.counter,
        &ai.counter,
        bi.counter == ai.counter,
    );
    same("режим", &bi.mode, &ai.mode, bi.mode == ai.mode);
    same("модель", &bi.model, &ai.model, bi.model == ai.model);
    same("устройство", &bi.device, &ai.device, bi.device == ai.device);
    // окна дайджеста (ответы, решения, горизонты идей) считаются от суток UTC прогона
    same("дата UTC", &bi.date, &ai.date, bi.date == ai.date);
    if !errs.is_empty() {
        bail!("плечи не сравниваются:\n  {}", errs.join("\n  "));
    }
    let ex = bi.exclusions.union(&ai.exclusions);
    let ids_b: BTreeSet<&str> = base.sessions.iter().map(|s| s.id.as_str()).collect();
    let ids_a: BTreeSet<&str> = arm.sessions.iter().map(|s| s.id.as_str()).collect();
    let common: Vec<String> = ids_b.intersection(&ids_a).map(|s| s.to_string()).collect();
    let profile_of = |id: &str| {
        base.sessions
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.profile.clone())
            .unwrap_or_default()
    };
    let mut parts = vec![part("всего", &common, &base.sessions, &arm.sessions, &ex)];
    for p in [Profile::Standard, Profile::Heavy] {
        let ids: Vec<String> = common
            .iter()
            .filter(|i| profile_of(i) == p.as_str())
            .cloned()
            .collect();
        if !ids.is_empty() {
            parts.push(part(p.as_str(), &ids, &base.sessions, &arm.sessions, &ex));
        }
    }
    let diffs: Vec<f64> = common
        .iter()
        .filter_map(|id| {
            let a: f64 = mean_tokens(&arm.sessions, id, &ex).values().sum();
            let b: f64 = mean_tokens(&base.sessions, id, &ex).values().sum();
            (b > 0.0).then(|| (a - b).abs() / b)
        })
        .collect();
    let both_ideas = bi.feats.ideas_on && ai.feats.ideas_on;
    Ok(SessionsComparison {
        base_name: names.0.to_string(),
        arm_name: names.1.to_string(),
        base_server: bi.server_version.clone(),
        arm_server: ai.server_version.clone(),
        only_base: ids_b.difference(&ids_a).map(|s| s.to_string()).collect(),
        only_arm: ids_a.difference(&ids_b).map(|s| s.to_string()).collect(),
        exclusions: ex,
        parts,
        idea_hints_match: both_ideas.then(|| idea_hints_match(&base.sessions, &arm.sessions)),
        abs_rel_diff_p95: (!diffs.is_empty()).then(|| quantile(&diffs, 0.95)),
        same_server: bi.server_version == ai.server_version
            && bi.server_git == ai.server_git
            && bi.ideas_flag == ai.ideas_flag,
        bootstrap_samples: BOOTSTRAP_M6,
        bootstrap_seed: SEED,
    })
}

// ---------------------------------------------------------------------------------------------
// Настоящий исполнитель: процессы `zam hook` и `zam mcp`

/// `zam mcp` (stdio): построчный JSON-RPC.
struct StdioMcp {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    next: u64,
}

impl StdioMcp {
    fn spawn(h: &Hooks, cwd: &Path, session: Option<&str>) -> Result<StdioMcp> {
        let mut c = Command::new(&h.zam);
        scrub_zam_env(&mut c, &[]);
        for k in zam_core::AGENT_ENV_VARS {
            c.env_remove(k);
        }
        c.arg("mcp")
            .current_dir(cwd)
            .env("ZAM_URL", &h.base)
            .env("ZAM_TOKEN", &h.token)
            .env("USERPROFILE", &h.home)
            .env("HOME", &h.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(s) = session {
            c.env(zam_core::MCP_SESSION_ENV, s);
        }
        let mut child = c
            .spawn()
            .with_context(|| format!("запуск {} mcp", h.zam.display()))?;
        let stdin = child.stdin.take().context("stdin zam mcp")?;
        let out = child.stdout.take().context("stdout zam mcp")?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines() {
                let Ok(l) = line else { break };
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        let mut m = StdioMcp {
            child,
            stdin,
            rx,
            next: 1,
        };
        m.rpc(
            "initialize",
            json!({ "protocolVersion": super::mcp_client::PROTOCOL, "capabilities": {},
                    "clientInfo": { "name": "zam-xtask-sessions", "version": "0" } }),
        )?;
        m.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
        Ok(m)
    }

    fn send(&mut self, v: &Value) -> Result<()> {
        writeln!(self.stdin, "{v}")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next;
        self.next += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        loop {
            let line = self
                .rx
                .recv_timeout(MCP_TIMEOUT)
                .with_context(|| format!("zam mcp {method}: нет ответа"))?;
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v["id"].as_u64() != Some(id) {
                continue;
            }
            if let Some(e) = v.get("error") {
                bail!("zam mcp {method}: ошибка JSON-RPC: {e}");
            }
            return Ok(v["result"].clone());
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Result<String> {
        let r = self.rpc("tools/call", json!({ "name": tool, "arguments": args }))?;
        let text = super::mcp_client::content_text(&r);
        if r["isError"].as_bool() == Some(true) {
            bail!("{tool}: ошибка инструмента: {text}");
        }
        Ok(text)
    }
}

impl Drop for StdioMcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Текст агенту из вывода хука: `hookSpecificOutput.additionalContext` или `reason` блокировки
/// (`decision: block`, `checkpoint`); пустой вывод — тишина; непустой не-JSON или JSON без
/// текста — `(_, true)`: такой шаг помечается, а не считается молча нулём.
pub fn hook_text(out: &str) -> (String, bool) {
    let t = out.trim();
    if t.is_empty() {
        return (String::new(), false);
    }
    let Ok(v) = serde_json::from_str::<Value>(t) else {
        return (String::new(), true);
    };
    if let Some(c) = v["hookSpecificOutput"]["additionalContext"].as_str() {
        return (c.to_string(), false);
    }
    if v["decision"] == "block"
        && let Some(r) = v["reason"].as_str()
    {
        return (r.to_string(), false);
    }
    (String::new(), true)
}

/// Процессы `zam` одной сессии.
struct RealDriver {
    hooks: Hooks,
    mcp: StdioMcp,
}

impl Driver for RealDriver {
    fn hook(&mut self, cmd: &str, mut input: Value) -> Result<HookOut> {
        input["cwd"] = json!(self.hooks.project.to_string_lossy());
        let before = self.hooks.log_lines();
        let (out, _) = self.hooks.run(cmd, &input)?;
        let (text, bad_output) = hook_text(&out);
        Ok(HookOut {
            text,
            log: self.hooks.new_log(before),
            bad_output,
        })
    }

    fn mcp(&mut self, tool: &str, args: Value) -> Result<String> {
        self.mcp.call(tool, args)
    }

    fn abs(&self, rel: &str) -> String {
        self.hooks.project.join(rel).to_string_lossy().into_owned()
    }
}

// ---------------------------------------------------------------------------------------------
// Загрузка мира

/// База сравнения по тегу выпуска.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize)]
pub enum Base {
    #[value(name = "B4")]
    B4,
    #[value(name = "B5.6")]
    B56,
    #[value(name = "B5.7")]
    B57,
}

impl Base {
    pub fn tag(self) -> &'static str {
        match self {
            Base::B4 => "v0.0.5-m4",
            Base::B56 => "v0.0.9-m5.6",
            Base::B57 => "v0.0.10-m5.7",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Base::B4 => "B4",
            Base::B56 => "B5.6",
            Base::B57 => "B5.7",
        }
    }

    /// `target/tags/<тег>/release/{zam-server,zam}`.
    fn bins(self, repo: &Path) -> (PathBuf, PathBuf) {
        let d = repo
            .join("target")
            .join("tags")
            .join(self.tag())
            .join("release");
        let exe = |n: &str| d.join(format!("{n}{}", std::env::consts::EXE_SUFFIX));
        (exe("zam-server"), exe("zam"))
    }
}

/// `POST` от имени администратора WebUI (cookie и CSRF).
fn admin_post(base: &str, path: &str, body: &Value) -> Result<(u16, Value)> {
    let admin = admin_login(base)?;
    let mut r = agent_with(Duration::from_secs(30))
        .post(&format!("{base}{path}"))
        .header("Cookie", &admin.cookie)
        .header("X-CSRF-Token", &admin.csrf)
        .content_type("application/json")
        .send(body.to_string())?;
    let st = r.status().as_u16();
    let text = r.body_mut().read_to_string()?;
    Ok((
        st,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    ))
}

fn post(s: &Session, path: &str, body: &Value) -> Result<(u16, Value)> {
    api_with(
        &s.base,
        s.token(),
        path,
        Some(body),
        &[],
        Duration::from_secs(30),
    )
}

fn id_of(v: &Value, what: &str) -> Result<String> {
    v["id"]
        .as_str()
        .map(str::to_string)
        .with_context(|| format!("{what}: нет id в {v}"))
}

/// Загружает мир сессии (кроме запросов доступа, [`load_access`]) с учётом возможностей;
/// возвращает карту ссылок, id идей с горизонтом «сегодня» (их правит SQL при остановленном
/// сервере) и примечания.
fn load_world(s: &mut Session, w: &World, f: &Feats) -> Result<(RefMap, Vec<String>, Vec<String>)> {
    let mut refs = RefMap::default();
    let mut notes = Vec::new();
    let reg = json!({ "agent_solution_id": EVAL_SOLUTION, "name": "eval" });
    let (st, v) = post(s, "/api/v1/solutions", &reg)?;
    if st != 201 && st != 200 {
        bail!("POST /solutions: HTTP {st}: {v}");
    }
    hints::load_rules(s, &w.rules)?;
    for (id, path) in s.node_paths() {
        if path.starts_with(hints::RULE_REF) {
            refs.0.insert(id.clone(), path.clone());
        }
    }
    s.load_insights(&w.insights)?;
    for (id, key) in s.entry_keys() {
        refs.0.insert(id.clone(), format!("insight:{key}"));
    }
    let feed = format!("/api/v1/spaces/{EVAL_SOLUTION}/entries");
    if f.journal {
        for t in &w.tasks {
            let mut data = json!({});
            if let Some(n) = &t.next_step {
                data["next_step"] = json!(n);
            }
            if let Some(b) = &t.blocked_by {
                data["blocked_by"] = json!(b);
            }
            let body = json!({ "kind": "task", "title": t.title, "body": t.body, "data": data });
            let (st, v) = post(s, &feed, &body)?;
            if st != 201 {
                bail!("task {}: HTTP {st}: {v}", t.key);
            }
            let id = id_of(&v, &t.key)?;
            if t.status != "open" {
                let (st, v) = hints::ideas::patch_entry(s, &id, &json!({ "status": t.status }))?;
                if st != 200 {
                    bail!("task {} → {}: HTTP {st}: {v}", t.key, t.status);
                }
            }
            refs.0.insert(id, format!("task:{}", t.key));
        }
        for q in &w.questions {
            let mut data = json!({});
            if !q.options.is_empty() {
                data["options"] = json!(
                    q.options
                        .iter()
                        .map(|o| json!({ "key": o.key, "text": o.text }))
                        .collect::<Vec<_>>()
                );
            }
            if let Some(r) = &q.recommendation {
                data["recommendation"] = json!(r);
            }
            let body =
                json!({ "kind": "question", "title": q.title, "body": q.body, "data": data });
            let (st, v) = post(s, &feed, &body)?;
            if st != 201 {
                bail!("question {}: HTTP {st}: {v}", q.key);
            }
            let id = id_of(&v, &q.key)?;
            if let Some(a) = &q.answer {
                let mut body = json!({ "by": "human" });
                if let Some(o) = &a.option {
                    body["option"] = json!(o);
                }
                if let Some(t) = &a.text {
                    body["text"] = json!(t);
                }
                let (st, v) = post(s, &format!("/api/v1/entries/{id}/answer"), &body)?;
                if st != 200 {
                    bail!("question {}: ответ: HTTP {st}: {v}", q.key);
                }
            }
            refs.0.insert(id, format!("question:{}", q.key));
        }
    } else if !w.tasks.is_empty() || !w.questions.is_empty() {
        notes.push("задачи и вопросы не загружены: у сервера нет журнала (M4.5)".into());
    }
    if f.glossary {
        for t in &w.terms {
            let body = json!({ "kind": "term", "title": t.title, "body": t.definition,
                               "data": { "definition": t.definition, "aliases": t.aliases } });
            let (st, v) = post(s, &feed, &body)?;
            if st != 201 {
                bail!("term {}: HTTP {st}: {v}", t.key);
            }
            refs.0.insert(id_of(&v, &t.key)?, format!("term:{}", t.key));
        }
    } else if !w.terms.is_empty() {
        notes.push("словарь не загружен: у сервера его нет (M4.5)".into());
    }
    let mut due = Vec::new();
    if f.ideas_on {
        let (keys, d) = hints::ideas::load_ideas(s, &w.ideas)?;
        for (w_id, key) in keys {
            refs.0
                .insert(w_id.trim_start_matches("w:").to_string(), key);
        }
        due = d;
    } else if !w.ideas.is_empty() {
        notes.push(if f.ideas {
            "идеи не загружены: --ideas off".into()
        } else {
            "идеи не загружены: у сервера нет вида idea (M5.7)".into()
        });
    }
    Ok((refs, due, notes))
}

/// Ожидающие запросы доступа (ТЗ §11.6): второй пользователь и его `POST /solutions` → 202.
/// После правки горизонтов SQL: та правит единственную базу пользователя.
fn load_access(s: &Session, w: &World) -> Result<()> {
    let reg = json!({ "agent_solution_id": EVAL_SOLUTION, "name": "eval" });
    for a in &w.access {
        let (st, v) = admin_post(&s.base, "/api/v1/admin/users", &json!({ "name": a.user }))?;
        if st != 201 {
            bail!("access {}: пользователь {}: HTTP {st}: {v}", a.key, a.user);
        }
        let tok = v["token"]["plaintext"]
            .as_str()
            .with_context(|| format!("access {}: нет токена в {v}", a.key))?
            .to_string();
        let (st, v) = api_with(
            &s.base,
            &tok,
            "/api/v1/solutions",
            Some(&reg),
            &[],
            Duration::from_secs(30),
        )?;
        if st != 202 {
            bail!(
                "access {}: запрос доступа: HTTP {st} (ожидался 202): {v}",
                a.key
            );
        }
    }
    Ok(())
}

/// Копия фикстуры карты кода в каталог проекта и `zam code sync`.
fn code_sync(h: &Hooks, fixture: &Path) -> Result<String> {
    run::copy_dir(fixture, &h.project)?;
    let mut c = Command::new(&h.zam);
    scrub_zam_env(&mut c, &[]);
    for k in zam_core::AGENT_ENV_VARS {
        c.env_remove(k);
    }
    let out = c
        .args(["code", "sync", "--full"])
        .current_dir(&h.project)
        .env("ZAM_URL", &h.base)
        .env("ZAM_TOKEN", &h.token)
        .env("USERPROFILE", &h.home)
        .env("HOME", &h.home)
        .output()
        .context("zam code sync")?;
    if !out.status.success() {
        bail!(
            "zam code sync: код {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Каталог процесса `zam mcp` неявного пути: `solution.toml` без файлов привязки хуков.
fn implicit_mcp_dir(dir: &Path) -> Result<PathBuf> {
    let d = dir.join("project-mcp");
    fs::create_dir_all(d.join(".zeroaimemory"))?;
    fs::write(
        d.join(".zeroaimemory").join("solution.toml"),
        format!("agent_solution_id = \"{EVAL_SOLUTION}\"\nname = \"eval\"\n"),
    )?;
    Ok(d)
}

// ---------------------------------------------------------------------------------------------
// Точка входа

fn read_result(path: &Path) -> Result<SessionsResult> {
    let t = fs::read_to_string(path).with_context(|| path.display().to_string())?;
    serde_json::from_str(&t).with_context(|| format!("разбор {}", path.display()))
}

fn file_name(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn write_pair(prefix: &Path, json_v: &impl Serialize, md: &str) -> Result<(PathBuf, PathBuf)> {
    if let Some(d) = prefix.parent() {
        fs::create_dir_all(d)?;
    }
    // не with_extension: в имени есть версия сервера с точками
    let j = PathBuf::from(format!("{}.json", prefix.display()));
    let m = PathBuf::from(format!("{}.md", prefix.display()));
    fs::write(&j, serde_json::to_string_pretty(json_v)? + "\n")?;
    fs::write(&m, md)?;
    Ok((j, m))
}

/// Пересчёт токенов шагов по сохранённым текстам текущим правилом счёта ([`fixed_ids`],
/// [`classify`]) и сводки — сравнения сохранённых плеч не зависят от версии раннера, снявшего их.
pub fn recount(r: &mut SessionsResult, count: Count<'_>) {
    for x in &mut r.sessions {
        for st in &mut x.steps {
            let fixed = fixed_ids(&st.text);
            let n = |t: &str| if t.is_empty() { 0 } else { count(t) };
            st.tokens = n(&fixed);
            st.tokens_raw = n(&st.text);
            st.classes = classify(&fixed, count);
        }
    }
    r.summary = summarize(&r.sessions, &own_exclusions(&r.info.feats));
    r.info.count_rule = COUNT_RULE.into();
}

fn compare_files(repo: &Path, arm_path: &Path, bases: &[PathBuf]) -> Result<()> {
    if bases.is_empty() {
        bail!("--arm без --compare: сравнивать не с чем");
    }
    let counter = Counter::load(repo)?;
    let count = |t: &str| counter.count(t);
    let mut arm = read_result(arm_path)?;
    recount(&mut arm, &count);
    for b in bases {
        let mut base = read_result(b)?;
        recount(&mut base, &count);
        let c = compare(&base, &arm, (&file_name(b), &file_name(arm_path)))?;
        let prefix =
            arm_path.with_file_name(format!("{}-vs-{}", file_name(arm_path), file_name(b)));
        let md = super::report::sessions_comparison_md(&c);
        let (j, m) = write_pair(&prefix, &c, &md)?;
        print!("{md}");
        println!("  {}\n  {}", j.display(), m.display());
    }
    Ok(())
}

/// Одна сессия на свежем сервере: загрузка мира, перезапуск (во всех плечах одинаково: после
/// SQL-правки горизонтов идей или без неё), карта кода, прогрев, воспроизведение, ожидание
/// фоновой синхронизации карты кода хуков, остановка.
#[allow(clippy::too_many_arguments)]
fn run_session(
    p: &Paths,
    spec: &dyn Fn(String, Vec<(String, String)>) -> RunSpec,
    sets: &[(String, String)],
    server: &Path,
    zam: &Path,
    corpus: &SessionsCorpus,
    f: &Feats,
    sp: &SessionSpec,
    rep: usize,
    label: &str,
    count: Count<'_>,
) -> Result<SessionRun> {
    let w = world(sp, corpus)?;
    let name = if rep == 0 {
        format!("sessions-{label}-{}", sp.id)
    } else {
        format!("sessions-{label}-{}-r{rep}", sp.id)
    };
    let mut s = Session::start(p, &spec(name.clone(), sets.to_vec()), server, true)?;
    s.check_model()?;
    let (refs, due, wnotes) = load_world(&mut s, &w, f)?;
    let data = s.data.clone();
    s.stop()?;
    if !due.is_empty() {
        hints::ideas::set_horizon_today(&data, &due)?;
    }
    s = Session::start(p, &spec(name.clone(), sets.to_vec()), server, false)?;
    load_access(&s, &w)?;
    if s.spec.model.is_some() {
        s.wait_indexed()?;
    }
    let dir = p.work.join(&name);
    let hooks = Hooks::prepare(&dir, zam.to_path_buf(), &s.base, s.token())?;
    // свой репозиторий git: иначе карта кода видит ветку и коммит рабочей копии разработчика
    let gi = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&hooks.project)
        .status()
        .context("git init каталога проекта сессии")?;
    if !gi.success() {
        bail!("git init {}: {gi}", hooks.project.display());
    }
    let mut snotes = wnotes;
    if w.code_map {
        if f.code_map {
            let out = code_sync(&hooks, &p.sessions_corpus.join("code"))?;
            snotes.push(format!("zam code sync: {out}"));
        } else {
            snotes.push("карта кода не загружена: у сервера её нет (M5.6)".into());
        }
    }
    // прогрев внутреннего поиска: первый /recall(prompt) холодного сервера не уложился бы в
    // бюджет хука
    let warm = json!({ "event": "prompt", "solution": EVAL_SOLUTION, "prompt": "warm up",
                       "record": false });
    let _ = post(&s, "/api/v1/recall", &warm);
    let sid = format!("ts2-{}-{}-r{rep}", run::today(), sp.id);
    let mcp_dir = if f.mcp_explicit {
        hooks.project.clone()
    } else {
        implicit_mcp_dir(&dir)?
    };
    let mcp = StdioMcp::spawn(&hooks, &mcp_dir, f.mcp_explicit.then_some(sid.as_str()))?;
    let mut d = RealDriver { hooks, mcp };
    let mut r =
        play(&mut d, sp, &w, f, &refs, count, &sid).with_context(|| format!("сессия {}", sp.id))?;
    if let Some(n) = wait_code_sync(&d.hooks) {
        snotes.push(n);
    }
    drop(d);
    r.rep = rep;
    r.notes.splice(0..0, snotes);
    s.stop()?;
    Ok(r)
}

/// Фоновая синхронизация карты кода, которую запускают хуки M5.6+, должна закончиться до
/// остановки сервера: число строк «… запущена» и итогов `code-sync` в `hooks.log` сравнивается,
/// не дольше 20 с. Не дождались — примечание.
fn wait_code_sync(h: &Hooks) -> Option<String> {
    let log = h
        .project
        .join(".zeroaimemory")
        .join("logs")
        .join("hooks.log");
    let t0 = std::time::Instant::now();
    loop {
        let text = fs::read_to_string(&log).unwrap_or_default();
        let started = text
            .lines()
            .filter(|l| l.contains("синхронизация карты запущена"))
            .count();
        let done = text.lines().filter(|l| l.contains(" code-sync ")).count();
        if done >= started {
            return None;
        }
        if t0.elapsed() > Duration::from_secs(20) {
            return Some(format!(
                "фоновая синхронизация карты кода не закончилась за 20 с ({done} из {started})"
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// blake3 каталога: относительные пути (`/`) и содержимое файлов по порядку путей, у текста
/// `\r\n` → `\n` (как у отпечатков ТС-1).
pub fn dir_blake3(dir: &Path) -> Result<String> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for e in fs::read_dir(d).with_context(|| d.display().to_string())? {
            let e = e?;
            if e.file_type()?.is_dir() {
                walk(&e.path(), out)?;
            } else {
                out.push(e.path());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(dir, &mut files)?;
    let rel = |f: &PathBuf| {
        f.strip_prefix(dir)
            .unwrap_or(f)
            .to_string_lossy()
            .replace('\\', "/")
    };
    files.sort_by_key(rel);
    let mut h = blake3::Hasher::new();
    for f in &files {
        h.update(rel(f).as_bytes());
        h.update(&[0]);
        let b = fs::read(f)?;
        let mut prev_cr = false;
        let mut norm = Vec::with_capacity(b.len());
        for &c in &b {
            if prev_cr && c != b'\n' {
                norm.push(b'\r');
            }
            prev_cr = c == b'\r';
            if !prev_cr {
                norm.push(c);
            }
        }
        if prev_cr {
            norm.push(b'\r');
        }
        h.update(&norm);
        h.update(&[0]);
    }
    Ok(h.finalize().to_hex()[..32].to_string())
}

/// Исключения своей сводки плеча: только виды ожиданий и обязательных строк, которых плечо не
/// умеет (события оно и так не воспроизводит, а классы строк вычитает только сравнение).
pub fn own_exclusions(f: &Feats) -> Exclusions {
    let ex = Exclusions::of(f);
    Exclusions {
        required: ex.required,
        expect: ex.expect,
        ..Exclusions::default()
    }
}

/// `cargo xtask eval --sessions`.
pub fn main(a: &EvalArgs, p: &Paths) -> Result<()> {
    if a.grid.is_some() {
        bail!("--sessions с --grid — сетки ТС-2 у задачи 8c");
    }
    if let Some(arm) = &a.arm {
        return compare_files(&p.repo, arm, &a.compare);
    }
    let Some(part) = a.split else {
        bail!("--sessions: укажите --split dev|holdout");
    };
    let set_path = p.repo.join("eval").join("sessions.toml");
    let set = load_set(&set_path)?;
    let corpus = sessions_corpus::load(&p.sessions_corpus)?;
    let sum = validate_set(&set, &corpus)?;
    println!(
        "набор ТС-2: сессий {} (стандартных {}, тяжёлых {}), событий {:?}, нагетов {}",
        sum.sessions, sum.standard, sum.heavy, sum.events, sum.nuggets
    );
    let items: Vec<(String, String)> = set
        .iter()
        .map(|s| (s.id.clone(), s.profile.as_str().to_string()))
        .collect();
    let holdout = split(&items);
    let chosen: Vec<&SessionSpec> = set
        .iter()
        .filter(|s| holdout.contains(&s.id) == (part == Part::Holdout))
        .filter(|s| a.profile.is_none_or(|pr| pr == s.profile))
        .collect();
    println!(
        "часть {}: сессий {} ({})",
        part.as_str(),
        chosen.len(),
        chosen
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let (server, zam) = match a.base {
        Some(b) => {
            let (s, z) = b.bins(&p.repo);
            for x in [&s, &z] {
                if !x.is_file() {
                    bail!(
                        "нет {} — бинарники баз собирает задача 1a2 (Global Constraints плана)",
                        x.display()
                    );
                }
            }
            (
                run::build_server(&p.repo, Some(&s))?,
                run::build_cli(&p.repo, Some(&z))?,
            )
        }
        None => (
            run::build_server(&p.repo, a.server_bin.as_deref())?,
            run::build_cli(&p.repo, a.cli_bin.as_deref())?,
        ),
    };
    let counter = Counter::load(&p.repo)?;
    let count = |t: &str| counter.count(t);
    let only_fts = a.modes.iter().all(|m| *m == Mode::Fts);
    let model = (!only_fts).then(|| {
        a.model
            .clone()
            .unwrap_or_else(|| run::default_model(a.device).to_string())
    });
    let ideas_flag = a.ideas.is_none_or(|i| i == Toggle::On);
    let label = a.label.as_deref().unwrap_or("ts2");
    let spec = |name: String, sets: Vec<(String, String)>| RunSpec {
        name,
        model: model.clone(),
        device: if only_fts { Device::Cpu } else { a.device },
        reranker: None,
        sets,
        keep_cuda_path: a.keep_cuda_path,
        ctrl_break: false,
    };
    let probe = Session::start(
        p,
        &spec(format!("sessions-{label}-probe"), Vec::new()),
        &server,
        true,
    )?;
    let caps = caps::probe(&probe.base);
    probe.stop()?;
    let caps = caps?;
    let f = feats(&caps, ideas_flag, a.implicit_mcp);
    println!(
        "сервер {} (git {}): {f:?}",
        caps.tag(),
        caps.git.as_deref().unwrap_or("—")
    );
    let mut sets: Vec<(String, String)> = Vec::new();
    if f.ideas {
        sets.push(("ideas.proactive".into(), f.ideas_on.to_string()));
    }
    for (k, v) in run::parse_sets(&a.sets)? {
        sets.push((k, v));
    }
    let env: Vec<(String, String)> = sets
        .iter()
        .map(|(k, v)| (run::env_key(k), v.clone()))
        .collect();
    caps::check_known(&env)?;
    let (kept, dropped) = caps::split_overrides(&caps, &env)?;
    if !dropped.is_empty() {
        bail!(
            "ключей нет у сервера {}: {}",
            caps.tag(),
            dropped.join(", ")
        );
    }
    let sets: Vec<(String, String)> = kept
        .iter()
        .filter_map(|(k, v)| caps::env_to_key(k).map(|key| (key, v.clone())))
        .collect();
    let mut runs = Vec::new();
    let mut notes = Vec::new();
    let reps = a.repeat.max(1);
    for rep in 0..reps {
        for sp in &chosen {
            let r = run_session(
                p, &spec, &sets, &server, &zam, &corpus, &f, sp, rep, label, &count,
            )?;
            let mut r = r;
            r.split = part.as_str().to_string();
            let ss = summarize(std::slice::from_ref(&r), &own_exclusions(&f));
            println!(
                "  {}{} [{}]: токенов {}, обязательных строк {}/{}, hint {}/{} (строго {}), идеи \
                 {}/{}, дайджест {}",
                r.id,
                if reps > 1 {
                    format!(" r{rep}")
                } else {
                    String::new()
                },
                r.profile,
                ss.tokens_total,
                ss.required.found,
                ss.required.total,
                ss.hint_recall.found,
                ss.hint_recall.total,
                ss.hint_recall_strict.found,
                ss.idea_hint_recall.found,
                ss.idea_hint_recall.total,
                match r.digest_match {
                    Some(true) => "=",
                    Some(false) => "≠",
                    None => "—",
                }
            );
            for m in &ss.required_missing {
                println!("    нет строки: {m}");
            }
            runs.push(r);
        }
    }
    if f.mcp_explicit {
        notes.push("MCP — zam mcp с ZAM_SESSION_ID (явная сессия)".to_string());
    } else {
        notes.push(
            "MCP — zam mcp без ZAM_SESSION_ID и без файла привязки (неявный путь R13)".to_string(),
        );
    }
    let ex = Exclusions::of(&f);
    let r = SessionsResult {
        info: SessionsInfo {
            date: run::today(),
            git: run::git_rev(&p.repo),
            label: a.label.clone(),
            split: part.as_str().to_string(),
            profile: a.profile.map(|x| x.as_str().to_string()),
            base: a.base.map(|b| b.name().to_string()),
            mode: if only_fts {
                "fts".into()
            } else {
                "hybrid".into()
            },
            model: model.clone(),
            server_version: caps.tag(),
            server_git: caps.git.clone(),
            server_m6: caps.m6,
            ideas_flag,
            feats: f,
            exclusions: ex.clone(),
            mcp: if f.mcp_explicit {
                "explicit".into()
            } else {
                "implicit".into()
            },
            counter: "bge-m3 tokenizer.json".into(),
            sessions_blake3: hints::ideas::file_blake3(&set_path)?,
            corpus_blake3: dir_blake3(&p.sessions_corpus)?,
            device: model.as_ref().map(|_| a.device.as_str().to_string()),
            repeat: reps,
            count_rule: COUNT_RULE.into(),
            notes,
        },
        summary: summarize(&runs, &own_exclusions(&f)),
        sessions: runs,
    };
    let stem = format!(
        "{}-m6-{}-sessions-{}-srv{}{}-{}",
        r.info.date,
        model.as_deref().unwrap_or("fts"),
        label,
        caps.tag(),
        if ideas_flag { "" } else { "-ideas-off" },
        part.as_str()
    );
    let prefix = match &a.out {
        Some(o) if o.is_dir() || o.to_string_lossy().ends_with(['/', '\\']) => o.join(&stem),
        Some(o) => o.clone(),
        None => p.results.join(&stem),
    };
    let md = super::report::sessions_md(&r);
    let (j, m) = write_pair(&prefix, &r, &md)?;
    print!("{md}");
    println!("  {}\n  {}", j.display(), m.display());
    for b in &a.compare {
        let mut base = read_result(b)?;
        recount(&mut base, &count);
        let c = compare(&base, &r, (&file_name(b), &stem))?;
        let cp = PathBuf::from(format!("{}-vs-{}", prefix.display(), file_name(b)));
        let cmd = super::report::sessions_comparison_md(&c);
        let (j, m) = write_pair(&cp, &c, &cmd)?;
        print!("{cmd}");
        println!("  {}\n  {}", j.display(), m.display());
    }
    Ok(())
}

/// Переключатель `on|off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Toggle {
    On,
    Off,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::caps::caps_from;

    /// Заготовка ответа мока по команде (инструменту) и входу.
    type Reply = Box<dyn FnMut(&str, &Value) -> String>;

    /// Мок сервера: хуки и MCP отвечают заготовками по команде, вызовы записываются.
    struct Mock {
        calls: Vec<String>,
        hook: Reply,
        mcp: Reply,
    }

    impl Driver for Mock {
        fn hook(&mut self, cmd: &str, input: Value) -> Result<HookOut> {
            self.calls
                .push(format!("hook {cmd} {}", input["hook_event_name"]));
            Ok(HookOut {
                text: (self.hook)(cmd, &input),
                ..HookOut::default()
            })
        }
        fn mcp(&mut self, tool: &str, args: Value) -> Result<String> {
            self.calls.push(format!("mcp {tool}"));
            Ok((self.mcp)(tool, &args))
        }
        fn abs(&self, rel: &str) -> String {
            format!("/p/{rel}")
        }
    }

    fn words(t: &str) -> usize {
        t.split_whitespace().count()
    }

    fn corpus() -> SessionsCorpus {
        sessions_corpus::load(
            &crate::eval::tests_repo()
                .join("eval")
                .join("sessions-corpus"),
        )
        .unwrap()
    }

    const ID_RULE: &str = "01RULE000000000000000000AA";
    const ID_TASK: &str = "01TASK000000000000000000AA";
    const ID_IDEA: &str = "01IDEA000000000000000000AA";
    const ID_TERM: &str = "01TERM000000000000000000AA";
    const ID_INS: &str = "01INSI000000000000000000AA";

    fn refs() -> RefMap {
        RefMap(
            [
                (ID_RULE, "rule:migrations"),
                (ID_TASK, "task:t-export-stream"),
                (ID_IDEA, "idea:i01"),
                (ID_TERM, "term:storefront"),
                (ID_INS, "insight:s-migration-lock"),
            ]
            .into_iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
        )
    }

    fn spec() -> SessionSpec {
        toml::from_str::<SetFile>(
            r#"
[[session]]
id = "ss-01"
profile = "standard"
rules = ["migrations", "commits"]
tasks = ["t-export-stream"]
ideas = ["i01"]
digest_ideas = ["i01"]
terms = ["storefront"]
insights = ["s-migration-lock"]

[[session.event]]
type = "prompt"
text = "миграция индекса"
expect = ["rule:migrations", "insight:s-migration-lock", "idea:i01", "term:storefront"]

[[session.event]]
type = "subagent"
agent_type = "Explore"
task = "найти"
queries = ["q"]

[[session.event]]
type = "compact"

[[session.event]]
type = "read_file"
path = "a.rs"

[[session.event]]
type = "checkpoint"
"#,
        )
        .unwrap()
        .session
        .remove(0)
    }

    fn digest() -> String {
        format!(
            "## ZeroAIMemory\n### Задачи\n[in_progress] Потоковая выгрузка заказов в CSV — k:{ID_TASK}\n\
             ### Идеи\nZeroAIMemory: идея «Экспорт отчётов в PDF» [high] — w:{ID_IDEA}\n\
             ### Правила\n# Миграции базы\nМиграции Postgres только добавляют — n:{ID_RULE}\n"
        )
    }

    fn prompt_text() -> String {
        format!(
            "ZeroAIMemory: термин «Витрина» — веб — t:{ID_TERM}\n\
             ZeroAIMemory: правило миграций — n:{ID_RULE}\n\
             ZeroAIMemory: идея «Экспорт отчётов в PDF» — w:{ID_IDEA}\n"
        )
    }

    fn mock() -> Mock {
        Mock {
            calls: Vec::new(),
            hook: Box::new(|cmd, inp| match (cmd, inp["source"].as_str()) {
                ("session-start", Some("startup")) => digest(),
                ("session-start", Some("compact")) => {
                    format!(
                        "### Задачи\n[in_progress] Потоковая выгрузка заказов в CSV — k:{ID_TASK}\n"
                    )
                }
                ("prompt", _) => prompt_text(),
                ("subagent-start", _) => "### Правила\nМиграции".into(),
                _ => String::new(),
            }),
            mcp: Box::new(|_, _| "карточка".into()),
        }
    }

    fn m6_caps() -> ServerCaps {
        caps_from(&json!({ "version": "0.0.10-m5.7", "git": "abc", "features": ["m6"] })).unwrap()
    }

    fn b4_caps() -> ServerCaps {
        caps_from(&json!({ "version": "0.0.5-m4", "git": "56f1873c18b9" })).unwrap()
    }

    fn result(runs: Vec<SessionRun>, f: Feats, server: &str) -> SessionsResult {
        SessionsResult {
            info: SessionsInfo {
                split: "dev".into(),
                server_version: server.into(),
                mode: "fts".into(),
                counter: "words".into(),
                feats: f,
                exclusions: Exclusions::of(&f),
                ideas_flag: true,
                ..SessionsInfo::default()
            },
            summary: summarize(&runs, &Exclusions::default()),
            sessions: runs,
        }
    }

    #[test]
    fn required_lines_detected() {
        let c = corpus();
        let sp = spec();
        let w = world(&sp, &c).unwrap();
        let f = feats(&m6_caps(), true, false);
        let mut m = mock();
        let r = play(&mut m, &sp, &w, &f, &refs(), &words, "sid").unwrap();
        let start: Vec<(&str, &str, bool)> = r
            .required
            .iter()
            .filter(|c| c.step == label::SESSION_START)
            .map(|c| (c.kind.as_str(), c.key.as_str(), c.found))
            .collect();
        // правило «Коммиты» в дайджесте мока нет — строка не найдена
        assert_eq!(
            start,
            vec![
                ("rule", "migrations", true),
                ("rule", "commits", false),
                ("task", "t-export-stream", true),
                ("idea", "i01", true),
            ]
        );
        // в восстановлении идей нет и быть не должно
        assert!(
            r.required
                .iter()
                .filter(|c| c.step == label::RECOVERY)
                .all(|c| c.kind != "idea")
        );
        let s = summarize(&[r], &Exclusions::default());
        assert_eq!(s.required.total, 4 + 3);
        assert_eq!(s.required.found, 3 + 1);
        assert!(
            s.required_missing
                .iter()
                .any(|m| m.contains("rule:commits"))
        );
        assert_eq!(s.digest_match, Tally { found: 1, total: 1 });
    }

    #[test]
    fn idea_hints_excluded_from_general_hint_recall() {
        let c = corpus();
        let sp = spec();
        let w = world(&sp, &c).unwrap();
        let f = feats(&m6_caps(), true, false);
        let r = play(&mut mock(), &sp, &w, &f, &refs(), &words, "sid").unwrap();
        let s = summarize(std::slice::from_ref(&r), &Exclusions::default());
        // общий: правило, insight, термин — показаны правило и термин
        assert_eq!(s.hint_recall, Tally { found: 2, total: 3 });
        assert_eq!(s.idea_hint_recall, Tally { found: 1, total: 1 });
        let h = &r.hints[0];
        assert_eq!(h.idea_hints, vec!["idea:i01".to_string()]);
        // токены идей — отдельной строкой: дайджест и prompt
        assert!(s.idea_tokens[label::SESSION_START] > 0);
        assert!(s.idea_tokens[label::PROMPT] > 0);
        // без идей у плеча ожидание-идея не считается вовсе, общий hint_recall тот же
        let off = feats(&m6_caps(), false, false);
        let s2 = summarize(&[r], &Exclusions::of(&off));
        assert_eq!(s2.hint_recall, Tally { found: 2, total: 3 });
        assert_eq!(s2.idea_hint_recall.total, 0);
    }

    #[test]
    fn idea_hints_compared_to_b57() {
        let c = corpus();
        let sp = spec();
        let w = world(&sp, &c).unwrap();
        let b57 = caps_from(&json!({ "version": "0.0.10-m5.7", "git": "91363056d3ba" })).unwrap();
        let fb = feats(&b57, true, false);
        let fa = feats(&m6_caps(), true, false);
        let base = play(&mut mock(), &sp, &w, &fb, &refs(), &words, "sid").unwrap();
        let same = play(&mut mock(), &sp, &w, &fa, &refs(), &words, "sid").unwrap();
        // плечо без подсказки-идеи в prompt
        let mut other = mock();
        other.hook = Box::new(|cmd, inp| match (cmd, inp["source"].as_str()) {
            ("session-start", Some("startup")) => digest(),
            ("prompt", _) => format!("ZeroAIMemory: правило — n:{ID_RULE}\n"),
            _ => String::new(),
        });
        let diff = play(&mut other, &sp, &w, &fa, &refs(), &words, "sid").unwrap();
        let rb = result(vec![base], fb, "0.0.10-m5.7");
        let c1 = compare(
            &rb,
            &result(vec![same], fa, "0.0.10-m5.7+m6"),
            ("B5.7", "M6"),
        )
        .unwrap();
        assert_eq!(c1.idea_hints_match, Some(Tally { found: 1, total: 1 }));
        assert_eq!(
            c1.parts[0].arm_summary.idea_hint_recall,
            Tally { found: 1, total: 1 }
        );
        let c2 = compare(
            &rb,
            &result(vec![diff], fa, "0.0.10-m5.7+m6"),
            ("B5.7", "M6"),
        )
        .unwrap();
        assert_eq!(c2.idea_hints_match, Some(Tally { found: 0, total: 1 }));
        // идея уже была в `### Идеи` дайджеста — ожидание выполнено, но подсказка prompt с B5.7
        // не совпала: это и ловит idea_hints_match
        assert_eq!(
            c2.parts[0].arm_summary.idea_hint_recall,
            Tally { found: 1, total: 1 }
        );
        assert!(
            !c2.parts[0]
                .arm_summary
                .idea_tokens
                .contains_key(label::PROMPT)
        );
        // база без идей (B5.6) — совпадения нет: сравнивать не с чем
        let b56 = caps_from(&json!({ "version": "0.0.9-m5.6", "git": "97825a10bc19" })).unwrap();
        let f56 = feats(&b56, true, false);
        let r56 = play(&mut mock(), &sp, &w, &f56, &refs(), &words, "sid").unwrap();
        let c3 = compare(
            &result(vec![r56], f56, "0.0.9-m5.6"),
            &result(
                vec![play(&mut mock(), &sp, &w, &fa, &refs(), &words, "sid").unwrap()],
                fa,
                "m6",
            ),
            ("B5.6", "M6"),
        )
        .unwrap();
        assert_eq!(c3.idea_hints_match, None);
        // идеи у B5.6 не обязательны: вид idea исключён из обязательных строк обоих плеч
        assert!(c3.exclusions.required.contains(req::IDEA));
    }

    #[test]
    fn events_missing_in_b4_excluded_both_arms() {
        let c = corpus();
        let sp = spec();
        let w = world(&sp, &c).unwrap();
        let f4 = feats(&b4_caps(), true, false);
        assert!(!f4.subagent && !f4.recovery && !f4.file_context && !f4.journal && !f4.ideas);
        let mut m4 = mock();
        let r4 = play(&mut m4, &sp, &w, &f4, &refs(), &words, "sid").unwrap();
        // у B4 события M4.5 не воспроизводятся вовсе
        assert!(!m4.calls.iter().any(|c| c.contains("subagent-start")));
        assert!(!m4.calls.iter().any(|c| c.contains("PreCompact")));
        assert!(!m4.calls.iter().any(|c| c.contains("PostToolUse")));
        assert_eq!(
            r4.skipped,
            vec![
                "file_context",
                "pre_compact",
                "recovery",
                "subagent_mcp",
                "subagent_start"
            ]
        );
        let f6 = feats(&m6_caps(), true, false);
        let r6 = play(&mut mock(), &sp, &w, &f6, &refs(), &words, "sid").unwrap();
        assert!(r6.steps.iter().any(|s| s.label == label::SUBAGENT_START));
        let base = result(vec![r4], f4, "0.0.5-m4");
        let arm = result(vec![r6.clone()], f6, "0.0.10-m5.7+m6");
        let cmp = compare(&base, &arm, ("B4", "M6")).unwrap();
        for l in [
            label::SUBAGENT_START,
            label::SUBAGENT_MCP,
            label::RECOVERY,
            label::FILE_CONTEXT,
        ] {
            assert!(cmp.exclusions.labels.contains(l), "{l}");
            assert!(
                !cmp.parts[0].arm_summary.tokens_by_label.contains_key(l),
                "{l}"
            );
            assert!(
                !cmp.parts[0].base_summary.tokens_by_label.contains_key(l),
                "{l}"
            );
        }
        // разделы дайджеста M4.5 и подсказки терминов вычитаются из плеча M6
        let start = r6
            .steps
            .iter()
            .find(|s| s.label == label::SESSION_START)
            .unwrap();
        assert!(start.classes[class::JOURNAL] > 0);
        let arm_start = cmp.parts[0].arm_summary.tokens_by_label[label::SESSION_START];
        assert_eq!(arm_start, start.tokens - start.classes[class::JOURNAL]);
        let pr = r6.steps.iter().find(|s| s.label == label::PROMPT).unwrap();
        assert_eq!(
            cmp.parts[0].arm_summary.tokens_by_label[label::PROMPT],
            pr.tokens - pr.classes[class::TERMS]
        );
        // задачи — не обязательные строки ни в одном плече, термины — не ожидания
        assert!(cmp.exclusions.required.contains(req::TASK));
        assert_eq!(cmp.parts[0].arm_summary.hint_recall.total, 2);
        assert_eq!(cmp.parts[0].base_summary.hint_recall.total, 2);
        assert!(cmp.parts[0].total.r.is_some());
        // профиль сессии мока — standard: часть «всего» и часть профиля
        let names: Vec<&str> = cmp.parts.iter().map(|p| p.part.as_str()).collect();
        assert_eq!(names, vec!["всего", "standard"]);
    }

    #[test]
    fn sessions_set_is_valid() {
        let repo = crate::eval::tests_repo();
        let set = load_set(&repo.join("eval").join("sessions.toml")).unwrap();
        let s = validate_set(&set, &corpus()).unwrap();
        assert!(s.sessions >= MIN_SESSIONS && s.standard >= MIN_STANDARD && s.heavy >= MIN_HEAVY);
    }

    #[test]
    fn set_checks_catch_bad_markup() {
        let mut sp = spec();
        sp.events.push(Event::Question {
            id: "q1".into(),
            query: "что".into(),
            expect: vec!["rule:migrations".into()],
            nuggets: vec![NuggetSpec {
                source: "rule:migrations".into(),
                text: "этого фрагмента в заметке нет вовсе".into(),
            }],
        });
        let err = format!("{:#}", validate_set(&[sp], &corpus()).unwrap_err());
        assert!(err.contains("не ровно один раз"), "{err}");
        assert!(err.contains("событий prompt"), "{err}");
        assert!(err.contains("последнее событие"), "{err}");
    }

    #[test]
    fn classify_sections_and_lines() {
        let t = format!(
            "{}\nСловарь: 6 терминов — memory_search kinds=[term]\n",
            digest()
        );
        let c = classify(&t, &words);
        assert!(c[class::JOURNAL] > 0 && c[class::IDEAS] > 0);
        assert!(!c.contains_key(class::TERMS));
        let p = classify(&prompt_text(), &words);
        assert!(p[class::TERMS] > 0 && p[class::IDEAS] > 0);
    }

    /// M6 §7.3: заголовок `/read` с пометкой «уже выдавалось в этой сессии» — помеченная строка
    /// (повтор с пометкой не считается неотмеченным).
    #[test]
    fn reread_header_is_marked() {
        assert!(marked_seen(&format!(
            "### i:{ID_INS} insight/gotcha «x»  active · rev1 · ~5 tok · уже выдавалось в этой сессии"
        )));
        assert!(!marked_seen(&format!(
            "### i:{ID_INS} insight/gotcha «x»  active · rev1 · ~5 tok"
        )));
        // карточка источника выжимки того же ответа (M6 §7.1, решение контролёра 2026-11-06)
        assert!(marked_seen(&format!(
            "1. i:{ID_INS} insight/gotcha «x»  [1.0] hyp · 64t   (выдавалось, в выжимке)"
        )));
    }

    #[test]
    fn repeated_output_counts_unmarked_refs_only() {
        let mut m = mock();
        m.mcp = Box::new(|tool, _| match tool {
            "memory_search" => format!("1. миграции — n:{ID_RULE}\n2. (выдавалось) — i:{ID_INS}\n"),
            _ => String::new(),
        });
        let sp: SessionSpec = toml::from_str::<SetFile>(
            r#"
[[session]]
id = "ss-02"
profile = "standard"
[[session.event]]
type = "subagent"
agent_type = "Explore"
task = "t"
queries = ["a", "b"]

[[session.event]]
type = "subagent"
agent_type = "Explore"
task = "t2"
queries = ["c"]
"#,
        )
        .unwrap()
        .session
        .remove(0);
        let w = World::default();
        let f = feats(&m6_caps(), true, false);
        let r = play(&mut m, &sp, &w, &f, &refs(), &words, "sid").unwrap();
        let s = summarize(&[r], &Exclusions::default());
        // событие 1: правило в двух поисках — не повтор (одно событие); insight помечен и не
        // считается; событие 2: правило снова — повтор
        assert!(s.tokens_by_label[label::SUBAGENT_MCP] > 0);
        // хуки: три ссылки дайджеста — впервые; MCP: правило дважды (повтор во 2-м событии)
        assert_eq!(s.repeated, Tally { found: 1, total: 5 });
    }

    #[test]
    fn required_lines_by_refs_not_text() {
        let c = corpus();
        let sp = spec();
        let w = world(&sp, &c).unwrap();
        let f = feats(&m6_caps(), true, false);
        let mut m = mock();
        // заголовок правила есть текстом, ссылки нет — строка не найдена; ссылка без текста —
        // найдена
        m.hook = Box::new(|cmd, inp| match (cmd, inp["source"].as_str()) {
            ("session-start", Some("startup")) => format!(
                "# Миграции базы\nПотоковая выгрузка заказов в CSV — k:{ID_TASK}\nw:{ID_IDEA}\n"
            ),
            _ => String::new(),
        });
        let r = play(&mut m, &sp, &w, &f, &refs(), &words, "sid").unwrap();
        let got: Vec<(&str, bool)> = r
            .required
            .iter()
            .filter(|c| c.step == label::SESSION_START)
            .map(|c| (c.key.as_str(), c.found))
            .collect();
        assert_eq!(
            got,
            vec![
                ("migrations", false),
                ("commits", false),
                ("t-export-stream", true),
                ("i01", true)
            ]
        );
    }

    #[test]
    fn hint_credit_resets_after_compact() {
        let c = corpus();
        let mut sp = spec();
        // после compact — тот же prompt: правило было в дайджесте до compact, в восстановлении
        // его нет — не засчитывается
        sp.events.insert(
            3,
            Event::Prompt {
                text: "ещё раз про миграции".into(),
                expect: vec!["rule:migrations".into()],
            },
        );
        let w = world(&sp, &c).unwrap();
        let f = feats(&m6_caps(), true, false);
        let mut m = mock();
        m.hook = Box::new(|cmd, inp| match (cmd, inp["source"].as_str()) {
            ("session-start", Some("startup")) => digest(),
            ("session-start", Some("compact")) => "### Задачи\n".into(),
            _ => String::new(),
        });
        let r = play(&mut m, &sp, &w, &f, &refs(), &words, "sid").unwrap();
        let first = &r.hints[0];
        assert_eq!(first.credit[0], "digest");
        let after = r.hints.iter().find(|h| h.event == 3).unwrap();
        assert!(after.earlier.is_empty(), "{after:?}");
        let s = summarize(&[r], &Exclusions::default());
        assert_eq!(s.hint_by_kind["rule"], Tally { found: 1, total: 2 });
        assert_eq!(s.hint_by_kind_strict["rule"], Tally { found: 0, total: 2 });
        assert_eq!(s.hint_credit["digest"], 2);
    }

    #[test]
    fn fixed_ids_and_hook_text() {
        let t = format!("правило — n:{ID_RULE}; выжимка c_{ID_IDEA}; abn:{ID_RULE}");
        let f = fixed_ids(&t);
        assert!(f.contains(&format!("n:{ID_STUB};")));
        assert!(f.contains(&format!("c_{ID_STUB};")));
        // не ссылка: буква-префикс внутри слова
        assert!(f.ends_with(&format!("abn:{ID_RULE}")));
        assert_eq!(hook_text(""), (String::new(), false));
        assert_eq!(hook_text("oops"), (String::new(), true));
        assert_eq!(
            hook_text(r#"{"decision":"block","reason":"запиши"}"#),
            ("запиши".to_string(), false)
        );
        assert_eq!(
            hook_text(r#"{"hookSpecificOutput":{"additionalContext":"x"}}"#),
            ("x".to_string(), false)
        );
    }

    #[test]
    fn compare_rejects_other_corpus_date_or_model() {
        let c = corpus();
        let sp = spec();
        let w = world(&sp, &c).unwrap();
        let f = feats(&m6_caps(), true, false);
        let r = play(&mut mock(), &sp, &w, &f, &refs(), &words, "sid").unwrap();
        let base = result(vec![r.clone()], f, "m6");
        for (what, change) in [
            (
                "корпус",
                Box::new(|i: &mut SessionsInfo| i.corpus_blake3 = "x".into())
                    as Box<dyn Fn(&mut SessionsInfo)>,
            ),
            (
                "дата",
                Box::new(|i: &mut SessionsInfo| i.date = "2000-01-01".into()),
            ),
            (
                "модель",
                Box::new(|i: &mut SessionsInfo| i.model = Some("m".into())),
            ),
            (
                "устройство",
                Box::new(|i: &mut SessionsInfo| i.device = Some("cuda".into())),
            ),
        ] {
            let mut arm = result(vec![r.clone()], f, "m6");
            change(&mut arm.info);
            let e = format!("{:#}", compare(&base, &arm, ("a", "b")).unwrap_err());
            assert!(e.contains(what), "{what}: {e}");
        }
    }

    #[test]
    fn compare_deltas_on_common_elements_and_repeats() {
        let c = corpus();
        let sp = spec();
        let w = world(&sp, &c).unwrap();
        let f = feats(&m6_caps(), true, false);
        let good = play(&mut mock(), &sp, &w, &f, &refs(), &words, "sid").unwrap();
        let mut m = mock();
        m.hook = Box::new(|cmd, inp| match (cmd, inp["source"].as_str()) {
            ("session-start", Some("startup")) => "### Правила\n".into(),
            _ => String::new(),
        });
        let bad = play(&mut m, &sp, &w, &f, &refs(), &words, "sid").unwrap();
        let mut bad1 = bad.clone();
        bad1.rep = 1;
        let base = result(vec![good], f, "m6");
        let arm = result(vec![bad, bad1], f, "m6");
        assert_eq!(arm.summary.reps, 2);
        assert_eq!(arm.summary.rep_noise_p95, Some(0.0));
        let cmp = compare(&base, &arm, ("a", "b")).unwrap();
        let all = &cmp.parts[0];
        assert!(all.d_required.as_ref().unwrap().mean < -0.5);
        assert!(all.d_hint_recall.as_ref().unwrap().mean < 0.0);
        assert!(all.total.arm < all.total.base);
    }

    /// Средняя стоимость `<буква>:<id>` в токенах bge-m3 (пилот на гибриде, задача 4).
    const ID_MEAN_TOKENS: f64 = 19.17;

    #[test]
    fn id_stub_costs_mean_id() {
        let repo = crate::eval::tests_repo();
        let c = match Counter::load(&repo) {
            Ok(c) => c,
            // в CI с моделями (`ZAM_REQUIRE_ML=1`) отсутствие токенизатора — ошибка, а не пропуск
            Err(e) if std::env::var_os("ZAM_REQUIRE_ML").is_some() => {
                panic!("ZAM_REQUIRE_ML=1, а токенизатора bge-m3 нет: {e}")
            }
            Err(_) => {
                eprintln!(
                    "нет tokenizer.json bge-m3 в target/models — проверка стоимости пропущена"
                );
                return;
            }
        };
        let n = c.count(&format!("n:{ID_STUB}")) as f64;
        assert!(
            (n - ID_MEAN_TOKENS).abs() <= 1.0,
            "n:<заглушка> = {n} токенов"
        );
        // в тексте: настоящий id пилота pilot2 средней стоимости (n:<id> = 19 токенов) после
        // замены заглушкой стоит столько же
        let real = "правило — n:01M6P1EQJRE1DV7NHBDRNPYS5C x";
        assert_eq!(c.count("n:01M6P1EQJRE1DV7NHBDRNPYS5C"), 19);
        assert_eq!(c.count(&fixed_ids(real)), c.count(real));
    }

    #[test]
    fn budget_miss_matches_client_texts() {
        let t0 = "hooks.log (session-start): 2026-11-05T14:39:13Z session-start";
        assert!(budget_miss(&format!(
            "{t0} {}",
            zam_core::HOOK_BUDGET_SPENT
        )));
        // ошибка сети клиента по сроку: «сервер … недоступен: <ureq>»
        let e = ureq::Error::Timeout(ureq::Timeout::Global);
        assert!(budget_miss(&format!(
            "{t0} recall(session_start): сервер http://127.0.0.1:1 недоступен: {e}"
        )));
        assert!(!budget_miss(&format!(
            "{t0} code-sync карта кода: файлов 0"
        )));
        // фоновая синхронизация карты кода с тайм-аутом сети — не промах бюджета хука
        assert!(!budget_miss(&format!(
            "hooks.log (session-start): 2026-11-05T14:39:13Z code-sync карта кода: сервер http://127.0.0.1:1 недоступен: {e}"
        )));
        assert!(
            !budget_miss(zam_core::HOOK_BUDGET_SPENT),
            "только строки hooks.log"
        );
    }

    #[test]
    fn miss_only_in_rep1_counts_in_mean_and_worst() {
        let c = corpus();
        let sp = spec();
        let w = world(&sp, &c).unwrap();
        let f = feats(&m6_caps(), true, false);
        let good = play(&mut mock(), &sp, &w, &f, &refs(), &words, "sid").unwrap();
        let mut m = mock();
        m.hook = Box::new(|cmd, inp| match (cmd, inp["source"].as_str()) {
            ("session-start", Some("startup")) => "### Правила\n".into(),
            _ => String::new(),
        });
        let mut bad = play(&mut m, &sp, &w, &f, &refs(), &words, "sid").unwrap();
        bad.rep = 1;
        bad.notes.push(format!(
            "hooks.log (session-start): x {}",
            zam_core::HOOK_BUDGET_SPENT
        ));
        let base = result(
            vec![good.clone(), {
                let mut g = good.clone();
                g.rep = 1;
                g
            }],
            f,
            "m6",
        );
        let arm = result(vec![good.clone(), bad], f, "m6");
        // худший повтор плеча — 1, промах в нём виден в сводке
        assert_eq!(arm.summary.required_by_rep[&0].share(), Some(4.0 / 7.0));
        assert!(arm.summary.required_by_rep[&1].share().unwrap() < 4.0 / 7.0);
        assert_eq!(arm.summary.reps_with_misses, vec!["ss-01 r1".to_string()]);
        assert_eq!(arm.summary.hook_budget_misses, 1);
        let cmp = compare(&base, &arm, ("a", "b")).unwrap();
        let d = cmp.parts[0].d_required.as_ref().unwrap().mean;
        // повтор 0 одинаков, повтор 1 хуже — Δ среднего по повторам отрицательна, а не 0
        assert!(d < 0.0, "{d}");
    }
}
