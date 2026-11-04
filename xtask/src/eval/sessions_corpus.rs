//! Корпус ТС-2 `eval/sessions-corpus/` (спецификация M6 §3.1–§3.3, задача 1c): сущности сессий
//! — заметки Rules, задачи, вопросы с ответами человека, запросы доступа, словарь, insight с
//! триггерами — и 50 идей для эталона подсказок-идей M5.7 (задача 1a2).
//!
//! Корпус ТС-1 (`eval/corpus`) его не включает: у баз B4 и B5.6 нет вида `idea`, и общий корпус
//! дал бы плечам ТС-1 разные данные (рецензия плана, C1). Его грузят только ТС-2 (`eval
//! --sessions`, задача 4) и наборы идей (`--idea-hints`, задача 1a2); здесь — разбор и проверка
//! разметки (`cargo xtask eval --check`).
//!
//! Файлы (все необязательны по отдельности, проверка полного корпуса — [`validate`]):
//! - `rules.toml` — `[[rule]] key, path, text` (как в `eval/hints.toml`);
//! - `journal.toml` — `[[task]]`, `[[question]]`, `[[access]]`;
//! - `glossary.toml` — `[[term]]` (как в `eval/glossary.toml`);
//! - `insights.toml` — `[[insight]]` (как в `eval/insights.toml`);
//! - `ideas/ideas.toml` — `[[idea]]`, ровно [`HINT_IDEAS`].

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::corpus::{Insight, Lang};
use super::glossary::Term;
use super::hints::Rule;

/// Идей в фикстуре эталона подсказок-идей (задача 1a2, задача 16: «50 идей»).
pub const HINT_IDEAS: usize = 50;
/// Минимум заметок Rules (тяжёлый профиль ТС-2, §3.1).
pub const MIN_RULES: usize = 10;
/// Минимум задач `in_progress`/`blocked` (тяжёлый профиль).
pub const MIN_ACTIVE_TASKS: usize = 3;
/// Минимум отвеченных человеком вопросов (тяжёлый профиль).
pub const MIN_ANSWERS: usize = 2;
/// Минимум идей человека `new`/`deferred` (тяжёлый профиль).
pub const MIN_HUMAN_OPEN_IDEAS: usize = 4;
/// Минимум идей с общим глаголом в заголовке («добавить», «fix»…) — материал стоп-основ идей
/// (задача 16).
pub const MIN_STOP_VERB_IDEAS: usize = 10;
/// Минимум идей со словом, которое начинается со стоп-основы, но длиннее её больше чем на 3
/// символа («changelog», «модульный», «makefile»): такие слова служебными не считаются.
pub const MIN_FALSE_FRIEND_IDEAS: usize = 5;

/// Стоп-основы идей из спецификации M6 (задача 16, `IDEA_STOP_STEMS`) — справочно для проверки
/// разнообразия фикстуры; рабочий список — в сервере (задача 16).
pub const IDEA_STOP_STEMS_REF: [&str; 17] = [
    "сдел",
    "добав",
    "исправ",
    "обнов",
    "поправ",
    "почин",
    "модул",
    "тест",
    "функци",
    "файл",
    "make",
    "add",
    "fix",
    "updat",
    "chang",
    "modul",
    "test",
];

/// Задача журнала (`kind: task`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub key: String,
    pub lang: Lang,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// `open | in_progress | blocked | done | dropped`.
    pub status: String,
    #[serde(default)]
    pub next_step: Option<String>,
    /// Блокер (`data.blocked_by`); обязателен у `blocked`.
    #[serde(default)]
    pub blocked_by: Option<String>,
}

/// Вариант ответа вопроса.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QOption {
    pub key: String,
    pub text: String,
}

/// Ответ человека (`POST /entries/{id}/answer`, `by: human`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    #[serde(default)]
    pub option: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

/// Вопрос к человеку (`kind: question`); с `answer` — отвечен.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub key: String,
    pub lang: Lang,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub options: Vec<QOption>,
    #[serde(default)]
    pub recommendation: Option<String>,
    #[serde(default)]
    pub answer: Option<Answer>,
}

/// Ожидающий запрос доступа к решению от другого пользователя (ТЗ §11.6).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Access {
    pub key: String,
    /// Логин запрашивающего пользователя (создаётся загрузчиком).
    pub user: String,
}

/// Состояние идеи после загрузки (M5.7): статус и состояние её задачи.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdeaState {
    New,
    /// `deferred` с горизонтом `horizon_days`.
    Deferred,
    Proposed,
    Declined,
    /// `accepted`, задача жива (`open`).
    Accepted,
    /// `accepted`, задача снята (`dropped`) — кандидат второго прохода подсказок.
    Released,
    /// `done` (задача выполнена).
    Done,
}

/// Идея (`kind: idea`, M5.7).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Idea {
    pub key: String,
    pub lang: Lang,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// `low | normal | high` (нет — `normal`).
    #[serde(default)]
    pub priority: Option<String>,
    /// `human | agent`.
    pub origin: String,
    pub state: IdeaState,
    /// Только у `deferred`: горизонт — дата прогона UTC + дни (0 — наступил).
    #[serde(default)]
    pub horizon_days: Option<u32>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RulesFile {
    #[serde(default)]
    rule: Vec<Rule>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct JournalFile {
    #[serde(default)]
    task: Vec<Task>,
    #[serde(default)]
    question: Vec<Question>,
    #[serde(default)]
    access: Vec<Access>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct GlossaryFile {
    #[serde(default)]
    term: Vec<Term>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct InsightFile {
    #[serde(default)]
    insight: Vec<Insight>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct IdeasFile {
    #[serde(default)]
    idea: Vec<Idea>,
}

/// Корпус ТС-2.
#[derive(Debug, Clone, Default)]
pub struct SessionsCorpus {
    pub rules: Vec<Rule>,
    pub tasks: Vec<Task>,
    pub questions: Vec<Question>,
    pub access: Vec<Access>,
    pub terms: Vec<Term>,
    pub insights: Vec<Insight>,
    pub ideas: Vec<Idea>,
}

/// Итог проверки корпуса ТС-2.
#[derive(Debug, Default)]
pub struct SessionsSummary {
    pub rules: usize,
    pub tasks: usize,
    pub active_tasks: usize,
    pub questions: usize,
    pub answered: usize,
    pub access: usize,
    pub terms: usize,
    pub insights: usize,
    pub ideas: usize,
    /// Идеи по состоянию (`new`, `deferred`, …).
    pub ideas_by_state: Vec<(String, usize)>,
    pub stop_verb_ideas: usize,
    pub false_friend_ideas: usize,
}

fn read<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    if !path.is_file() {
        return Ok(T::default());
    }
    let text = fs::read_to_string(path).with_context(|| format!("чтение {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("разбор {}", path.display()))
}

/// Читает корпус ТС-2 из каталога `dir`.
pub fn load(dir: &Path) -> Result<SessionsCorpus> {
    if !dir.is_dir() {
        bail!("нет каталога корпуса ТС-2 {}", dir.display());
    }
    let rules: RulesFile = read(&dir.join("rules.toml"))?;
    let journal: JournalFile = read(&dir.join("journal.toml"))?;
    let glossary: GlossaryFile = read(&dir.join("glossary.toml"))?;
    let insights: InsightFile = read(&dir.join("insights.toml"))?;
    let ideas: IdeasFile = read(&dir.join("ideas").join("ideas.toml"))?;
    Ok(SessionsCorpus {
        rules: rules.rule,
        tasks: journal.task,
        questions: journal.question,
        access: journal.access,
        terms: glossary.term,
        insights: insights.insight,
        ideas: ideas.idea,
    })
}

/// Слово для сравнения основ, как у идей сервера: нижний регистр, `ё` → `е`.
fn norm(w: &str) -> String {
    w.to_lowercase().replace('ё', "е")
}

/// Слово начинается со стоп-основы идей ([`IDEA_STOP_STEMS_REF`]).
fn starts_with_stop_stem(w: &str) -> bool {
    let w = norm(w);
    IDEA_STOP_STEMS_REF.iter().any(|s| w.starts_with(s))
}

/// Служебное слово по правилу спецификации (задача 16): начинается со стоп-основы и длиннее её
/// не больше чем на 3 символа.
fn is_stop_word_ref(w: &str) -> bool {
    let w = norm(w);
    let n = w.chars().count();
    IDEA_STOP_STEMS_REF
        .iter()
        .any(|s| w.starts_with(s) && n <= s.chars().count() + 3)
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
}

fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn text_ok(s: &str) -> bool {
    !s.trim().is_empty() && !s.contains('\r')
}

/// Проверка разметки и состава корпуса ТС-2 (§3.1: тяжёлый профиль; задача 1a2: 50 идей).
/// Все нарушения собираются в одну ошибку.
pub fn validate(c: &SessionsCorpus) -> Result<SessionsSummary> {
    let mut errs: Vec<String> = Vec::new();
    let mut keys: BTreeSet<String> = BTreeSet::new();
    let mut key = |what: &str, k: &str, errs: &mut Vec<String>| {
        if !valid_key(k) || !keys.insert(format!("{what}:{k}")) {
            errs.push(format!("{what} {k}: ключ — [a-z0-9-], уникальный"));
        }
    };
    for r in &c.rules {
        key("rule", &r.key, &mut errs);
        let path_ok = !r.path.is_empty()
            && r.path.split('/').all(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            });
        if !path_ok {
            errs.push(format!("rule {}: путь — сегменты [a-z0-9-]", r.key));
        }
        if !text_ok(&r.text) || !r.text.trim_start().starts_with("# ") {
            errs.push(format!("rule {}: текст с заголовком `# `, без CRLF", r.key));
        }
    }
    for t in &c.tasks {
        key("task", &t.key, &mut errs);
        if !text_ok(&t.title) {
            errs.push(format!("task {}: пустой заголовок", t.key));
        }
        if !["open", "in_progress", "blocked", "done", "dropped"].contains(&t.status.as_str()) {
            errs.push(format!(
                "task {}: статус «{}» — open | in_progress | blocked | done | dropped",
                t.key, t.status
            ));
        }
        if t.status == "blocked" && t.blocked_by.as_deref().is_none_or(|b| b.trim().is_empty()) {
            errs.push(format!("task {}: у blocked нужен blocked_by", t.key));
        }
        if t.body.contains('\r') || t.next_step.as_deref().is_some_and(|n| !text_ok(n)) {
            errs.push(format!("task {}: next_step непустой, без CRLF", t.key));
        }
    }
    for q in &c.questions {
        key("question", &q.key, &mut errs);
        if !text_ok(&q.title) || q.body.contains('\r') {
            errs.push(format!("question {}: пустой заголовок", q.key));
        }
        if q.options
            .iter()
            .any(|o| !valid_key(&o.key) || !text_ok(&o.text))
        {
            errs.push(format!(
                "question {}: вариант — ключ [a-z0-9-] и текст",
                q.key
            ));
        }
        if q.recommendation
            .as_ref()
            .is_some_and(|r| !q.options.iter().any(|o| &o.key == r))
        {
            errs.push(format!(
                "question {}: recommendation — ключ варианта",
                q.key
            ));
        }
        if let Some(a) = &q.answer {
            let opt_ok = a
                .option
                .as_ref()
                .is_none_or(|o| q.options.iter().any(|x| &x.key == o));
            let text = a.text.as_deref().is_some_and(text_ok);
            if !opt_ok || (a.option.is_none() && !text) {
                errs.push(format!(
                    "question {}: ответ — вариант из options и/или текст",
                    q.key
                ));
            }
        }
    }
    for a in &c.access {
        key("access", &a.key, &mut errs);
        if !valid_key(&a.user) {
            errs.push(format!("access {}: логин — [a-z0-9-]", a.key));
        }
    }
    for t in &c.terms {
        key("term", &t.key, &mut errs);
        if !text_ok(&t.title) || !text_ok(&t.definition) || t.aliases.is_empty() {
            errs.push(format!(
                "term {}: имя, определение и псевдонимы обязательны",
                t.key
            ));
        }
    }
    for i in &c.insights {
        key("insight", &i.key, &mut errs);
        if !text_ok(&i.title) || !text_ok(&i.body) {
            errs.push(format!("insight {}: заголовок и тело без CRLF", i.key));
        }
    }
    let mut stop_verb = 0;
    let mut false_friend = 0;
    for i in &c.ideas {
        key("idea", &i.key, &mut errs);
        if !text_ok(&i.title) || i.title.chars().count() > 500 || i.body.contains('\r') {
            errs.push(format!(
                "idea {}: заголовок 1–500 символов, без CRLF",
                i.key
            ));
        }
        if !matches!(i.origin.as_str(), "human" | "agent") {
            errs.push(format!(
                "idea {}: origin «{}» — human | agent",
                i.key, i.origin
            ));
        }
        if i.priority
            .as_deref()
            .is_some_and(|p| !matches!(p, "low" | "normal" | "high"))
        {
            errs.push(format!("idea {}: priority — low | normal | high", i.key));
        }
        if (i.state == IdeaState::Deferred) != i.horizon_days.is_some() {
            errs.push(format!(
                "idea {}: horizon_days — только и обязательно у deferred",
                i.key
            ));
        }
        if i.origin == "agent" && i.state != IdeaState::New {
            errs.push(format!(
                "idea {}: идея агента — только new (решения принимает человек)",
                i.key
            ));
        }
        if words(&i.title).any(is_stop_word_ref) {
            stop_verb += 1;
        }
        if words(&i.title).any(|w| starts_with_stop_stem(w) && !is_stop_word_ref(w)) {
            false_friend += 1;
        }
    }
    let count = |st: IdeaState| c.ideas.iter().filter(|i| i.state == st).count();
    let active_tasks = c
        .tasks
        .iter()
        .filter(|t| matches!(t.status.as_str(), "in_progress" | "blocked"))
        .count();
    let answered = c.questions.iter().filter(|q| q.answer.is_some()).count();
    let mut need = |ok: bool, m: String| {
        if !ok {
            errs.push(m);
        }
    };
    need(
        c.rules.len() >= MIN_RULES,
        format!("заметок Rules {} < {MIN_RULES}", c.rules.len()),
    );
    need(
        active_tasks >= MIN_ACTIVE_TASKS,
        format!("задач in_progress/blocked {active_tasks} < {MIN_ACTIVE_TASKS}"),
    );
    need(
        c.tasks.iter().any(|t| t.status == "blocked"),
        "нет задачи blocked".into(),
    );
    need(
        answered >= MIN_ANSWERS,
        format!("ответов человека {answered} < {MIN_ANSWERS}"),
    );
    need(
        c.questions.iter().any(|q| q.answer.is_none()),
        "нет открытого вопроса".into(),
    );
    need(!c.access.is_empty(), "нет запроса доступа".into());
    need(!c.terms.is_empty(), "нет терминов словаря".into());
    need(
        c.ideas.len() == HINT_IDEAS,
        format!("идей {} ≠ {HINT_IDEAS}", c.ideas.len()),
    );
    let human_open = c
        .ideas
        .iter()
        .filter(|i| i.origin == "human" && matches!(i.state, IdeaState::New | IdeaState::Deferred))
        .count();
    need(
        human_open >= MIN_HUMAN_OPEN_IDEAS,
        format!("идей человека new/deferred {human_open} < {MIN_HUMAN_OPEN_IDEAS}"),
    );
    need(
        c.ideas
            .iter()
            .any(|i| i.state == IdeaState::Deferred && i.horizon_days == Some(0)),
        "нет идеи deferred с наступившим горизонтом (horizon_days = 0)".into(),
    );
    need(
        c.ideas
            .iter()
            .any(|i| i.state == IdeaState::Deferred && i.horizon_days.is_some_and(|d| d > 0)),
        "нет идеи deferred с горизонтом в будущем".into(),
    );
    for st in [
        IdeaState::Proposed,
        IdeaState::Declined,
        IdeaState::Accepted,
        IdeaState::Released,
        IdeaState::Done,
    ] {
        need(count(st) > 0, format!("нет идеи в состоянии {st:?}"));
    }
    need(
        c.ideas.iter().any(|i| i.origin == "agent"),
        "нет идеи агента".into(),
    );
    for lang in [Lang::Ru, Lang::En] {
        need(
            c.ideas.iter().any(|i| i.lang == lang),
            format!("нет идей на {lang:?}"),
        );
        need(
            c.tasks.iter().any(|t| t.lang == lang) && c.questions.iter().any(|q| q.lang == lang),
            format!("нет задач или вопросов на {lang:?}"),
        );
    }
    need(
        stop_verb >= MIN_STOP_VERB_IDEAS,
        format!("идей с общим глаголом в заголовке {stop_verb} < {MIN_STOP_VERB_IDEAS}"),
    );
    need(
        false_friend >= MIN_FALSE_FRIEND_IDEAS,
        format!(
            "идей со словом на стоп-основу длиннее её на 4+ символа {false_friend} < \
             {MIN_FALSE_FRIEND_IDEAS}"
        ),
    );
    if !errs.is_empty() {
        bail!(
            "корпус ТС-2 не прошёл проверку ({} нарушений):\n  {}",
            errs.len(),
            errs.join("\n  ")
        );
    }
    let states = [
        IdeaState::New,
        IdeaState::Deferred,
        IdeaState::Proposed,
        IdeaState::Declined,
        IdeaState::Accepted,
        IdeaState::Released,
        IdeaState::Done,
    ];
    Ok(SessionsSummary {
        rules: c.rules.len(),
        tasks: c.tasks.len(),
        active_tasks,
        questions: c.questions.len(),
        answered,
        access: c.access.len(),
        terms: c.terms.len(),
        insights: c.insights.len(),
        ideas: c.ideas.len(),
        ideas_by_state: states
            .iter()
            .map(|s| (format!("{s:?}").to_lowercase(), count(*s)))
            .collect(),
        stop_verb_ideas: stop_verb,
        false_friend_ideas: false_friend,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    /// Корпус ТС-2 репозитория проходит проверку полного состава.
    #[test]
    fn sessions_corpus_is_valid() {
        let c = load(&repo().join("eval").join("sessions-corpus")).unwrap();
        let s = validate(&c).unwrap();
        assert!(s.rules >= MIN_RULES && s.ideas == HINT_IDEAS, "{s:?}");
    }

    fn idea(key: &str, title: &str, state: IdeaState, origin: &str) -> Idea {
        Idea {
            key: key.into(),
            lang: Lang::Ru,
            title: title.into(),
            body: String::new(),
            priority: None,
            origin: origin.into(),
            state,
            horizon_days: None,
        }
    }

    /// Нарушения разметки собираются в одну ошибку.
    #[test]
    fn sessions_corpus_checks_catch_bad_markup() {
        let mut c = load(&repo().join("eval").join("sessions-corpus")).unwrap();
        c.tasks[0].status = "paused".into();
        c.ideas
            .push(idea("x-1", "Добавить экспорт", IdeaState::New, "robot"));
        let mut d = idea("x-2", "Отложенная", IdeaState::New, "human");
        d.horizon_days = Some(3);
        c.ideas.push(d);
        c.rules.truncate(3);
        let e = validate(&c).unwrap_err().to_string();
        for m in ["paused", "robot", "horizon_days", "Rules", "идей 52"] {
            assert!(e.contains(m), "{m}: {e}");
        }
    }

    /// Общий глагол — слово, начинающееся со стоп-основы и длиннее её не больше чем на 3
    /// символа; «changelog», «модульный» — не глаголы.
    #[test]
    fn stop_verb_rule_matches_spec_examples() {
        assert!(is_stop_word_ref("добавьте"));
        assert!(is_stop_word_ref("fix"));
        assert!(!is_stop_word_ref("changelog"));
        assert!(!is_stop_word_ref("модульный"));
        assert!(!is_stop_word_ref("address"));
        assert!(!is_stop_word_ref("makefile"));
        for w in ["changelog", "makefile", "address", "модульный"] {
            assert!(starts_with_stop_stem(w), "{w}");
        }
        assert!(!starts_with_stop_stem("экспорт"));
    }
}
