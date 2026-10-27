//! Точность подсказок хуков (`cargo xtask eval --hints`, ТЗ §10.3, §18.2; задача 15 M4).
//!
//! Набор `eval/hints.toml`: `[[insight]]` с триггерами (формат `eval/insights.toml`),
//! `[[rule]]` — заметки Rules и `[[case]]` — случаи `{event, input, tool?, expected, note}`:
//! половина «релевантно» (`expected` — подсказки, которые должны быть показаны), половина
//! «нерелевантно» (`expected = []`, ожидается тишина).
//!
//! Прогон поднимает сервер на корпусе `eval/corpus`, insight `eval/insights.toml` и записях
//! набора (Rules — `POST /nodes`), затем:
//! - `prompt` — `POST /recall {event: prompt}` без сессии (без учёта повторов);
//! - `post_tool` — `POST /recall {event: post_tool, tool: "Bash"}`;
//! - `pre_tool` — процесс `zam hook pre-tool` (только локально) в каталоге проекта с
//!   `.zeroaimemory/solution.toml` решения набора; кэш триггеров — от `zam hook session-start`;
//!   у каждого случая своя сессия (показ — раз за сессию).
//!
//! **Метрики** ([`hint_metrics`]): `hint_precision` — доля показанных подсказок, которые
//! ожидались (подсказка на нерелевантном случае — ложная); `hint_recall` — доля ожидаемых,
//! которые показаны; `silence_rate` — доля нерелевантных случаев без подсказок; справочно —
//! доля релевантных случаев хотя бы с одной ожидаемой подсказкой. Интервалы — бутстреп по
//! случаям (2000 выборок, фиксированное зерно).
//!
//! **Калибровка** (решение контроллера T15-b): порог `recall.prompt_min_score` — по выдаче
//! того же внутреннего поиска, что делает `/recall(prompt)` (`POST /search` `mode=brief`,
//! insight и Rules, баллы), перебором порогов ([`prompt_pick`]); порог покрытия FTS у
//! `post_tool` — моделью отбора сервера ([`post_tool_pick`]: `error_regex`, строгий запрос,
//! ослабленный с порогом покрытия) на тех же функциях `zam_search::recall`. Модель сверяется с
//! фактическими ответами `/recall` при текущих значениях (доля совпавших наборов подсказок).
//!
//! **Задержки** (T15-b): `/recall` для `prompt` (тексты `eval/queries.toml`, каждый один раз —
//! эмбеддинг запроса не из кэша), `post_tool`, `session_start` и процессы `zam hook prompt |
//! post-tool | session-start | pre-tool` — время на клиенте (с запуском процесса у хуков) и
//! `timings.total_ms` сервера, число ответов с `recall timeout`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zam_search::recall::{error_regex_matches, error_terms, normalize_error, term_coverage};

use super::corpus::{Corpus, EVAL_SOLUTION, INSIGHT_REF, Query, fnv1a, valid_key};
use super::insights::Insight;
use super::metrics::{
    Estimate, estimate, paired_diff, paired_ratio_diff, quantile, ratio_estimate,
};
use super::run::{self, RunSpec, Session};
use super::{Device, EvalArgs, Mode, Paths};
use crate::smoke::{admin_login, agent_with, api_timeout, scrub_zam_env};

/// Ссылка на заметку Rules набора: `rule:<key>`.
pub const RULE_REF: &str = "rule:";
/// Не меньше стольких случаев в наборе.
pub const MIN_CASES: usize = 60;
/// Доля релевантных (и нерелевантных) случаев — «половина»: от 45 до 55 %.
const HALF: std::ops::RangeInclusive<f64> = 0.45..=0.55;
/// Подсказок `prompt` и `post_tool` в ответе (`PROMPT_MAX`, `POST_TOOL_MAX` сервера).
const HINTS_MAX: usize = 3;
/// Тело внутреннего поиска `/recall(prompt)` (`PROMPT_SEARCH_TOP_K`, `PROMPT_SEARCH_BUDGET`).
const PROMPT_SEARCH_TOP_K: usize = 20;
const PROMPT_SEARCH_BUDGET: usize = 4000;
/// Пороги `recall.prompt_min_score` для перебора.
const PROMPT_THRESHOLDS: [f64; 13] = [
    0.30, 0.35, 0.40, 0.45, 0.50, 0.55, 0.60, 0.65, 0.70, 0.75, 0.80, 0.85, 0.90,
];
/// Текущее умолчание `recall.prompt_min_score`.
pub const PROMPT_MIN_SCORE: f64 = 0.6;
/// Текстов `prompt` из `eval/queries.toml` для задержек: через API и через хук.
const LATENCY_PROMPTS_API: usize = 150;
const LATENCY_PROMPTS_HOOK: usize = 100;
/// Повторов на случай `post_tool` (API / хук), `pre_tool` (хук) и вызовов `session_start`.
const LATENCY_POST_API_REPS: usize = 5;
const LATENCY_POST_HOOK_REPS: usize = 3;
const LATENCY_PRE_HOOK_REPS: usize = 3;
const LATENCY_SESSION_START: usize = 40;
/// Предел одного вызова `/recall` и процесса хука (хук сам укладывается в бюджет §10.3).
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Попыток `zam hook session-start` для кэша триггеров перед случаями `pre_tool`.
const START_ATTEMPTS: usize = 5;
/// Предупреждение сервера о тайм-ауте сбора (`WARN_RECALL_TIMEOUT`).
const WARN_RECALL_TIMEOUT: &str = "recall timeout";

/// Событие случая.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    Prompt,
    PostTool,
    PreTool,
}

impl Event {
    pub const ALL: [Event; 3] = [Event::Prompt, Event::PostTool, Event::PreTool];

    pub fn as_str(self) -> &'static str {
        match self {
            Event::Prompt => "prompt",
            Event::PostTool => "post_tool",
            Event::PreTool => "pre_tool",
        }
    }
}

/// Заметка Rules набора.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub key: String,
    /// Путь в пространстве Rules.
    pub path: String,
    pub text: String,
}

/// Случай набора.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub event: Event,
    /// Текст запроса (`prompt`), ошибки (`post_tool`), путь файла или команда (`pre_tool`).
    pub input: String,
    /// `pre_tool`: `Write | Edit | Bash`.
    #[serde(default)]
    pub tool: Option<String>,
    /// Ожидаемые подсказки (`insight:<key>`, `rule:<key>`); пусто — тишина.
    pub expected: Vec<String>,
    #[serde(default)]
    pub note: String,
}

/// Набор `eval/hints.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HintSet {
    #[serde(default)]
    pub insight: Vec<Insight>,
    #[serde(default)]
    pub rule: Vec<Rule>,
    #[serde(default, rename = "case")]
    pub cases: Vec<Case>,
}

/// Читает набор подсказок.
pub fn load_hints(path: &Path) -> Result<(HintSet, String)> {
    let text = fs::read_to_string(path).with_context(|| format!("чтение {}", path.display()))?;
    let set: HintSet =
        toml::from_str(&text).with_context(|| format!("разбор {}", path.display()))?;
    Ok((set, fnv1a(text.into_bytes())))
}

/// Сводка проверки набора.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HintSummary {
    pub cases: usize,
    pub relevant: usize,
    pub irrelevant: usize,
    /// Событие → (релевантных, нерелевантных).
    pub by_event: BTreeMap<String, (usize, usize)>,
    pub insights: usize,
    pub rules: usize,
}

const TRIGGER_TYPES: [&str; 4] = ["file_glob", "command_regex", "error_regex", "keyword"];

/// Корпус с insight набора подсказок (для проверки ссылок и загрузки).
pub fn with_hint_insights(corpus: &Corpus, set: &HintSet) -> Corpus {
    let mut c = corpus.clone();
    c.insights.extend(set.insight.iter().cloned());
    c
}

/// Проверка набора: объём, половина релевантных, ссылки на действующие записи, инструменты,
/// триггеры у записей, ожидаемых от `pre_tool`.
pub fn validate_hints(set: &HintSet, corpus: &Corpus) -> Result<HintSummary> {
    let mut errs: Vec<String> = Vec::new();
    let full = with_hint_insights(corpus, set);
    let corpus_keys: BTreeSet<&str> = corpus.insights.iter().map(|i| i.key.as_str()).collect();
    let mut keys = BTreeSet::new();
    for i in &set.insight {
        if !valid_key(&i.key) || !keys.insert(i.key.as_str()) || corpus_keys.contains(&*i.key) {
            errs.push(format!(
                "insight {}: ключ — [a-z0-9-], без повторов (и в insights.toml)",
                i.key
            ));
        }
        for t in &i.triggers {
            let ty = t["type"].as_str().unwrap_or_default();
            if !TRIGGER_TYPES.contains(&ty) || t["pattern"].as_str().is_none_or(str::is_empty) {
                errs.push(format!("insight {}: триггер {t}", i.key));
            }
        }
    }
    let mut rule_keys = BTreeSet::new();
    let mut rule_paths = BTreeSet::new();
    for r in &set.rule {
        if !valid_key(&r.key) || !rule_keys.insert(r.key.as_str()) {
            errs.push(format!("rule {}: ключ — [a-z0-9-], без повторов", r.key));
        }
        if r.path.trim().is_empty()
            || !rule_paths.insert(r.path.as_str())
            || r.text.trim().is_empty()
        {
            errs.push(format!(
                "rule {}: путь без повторов и текст обязательны",
                r.key
            ));
        }
    }
    let mut ids = BTreeSet::new();
    let mut s = HintSummary {
        cases: set.cases.len(),
        insights: set.insight.len(),
        rules: set.rule.len(),
        ..HintSummary::default()
    };
    for c in &set.cases {
        let mut bad = |m: String| errs.push(format!("case {}: {m}", c.id));
        if c.id.trim().is_empty() || !ids.insert(c.id.as_str()) {
            bad("id пуст или повторяется".into());
        }
        if c.input.trim().is_empty() {
            bad("пустой input".into());
        }
        match (c.event, c.tool.as_deref()) {
            (Event::PreTool, Some("Write" | "Edit" | "Bash")) => {}
            (Event::PreTool, _) => bad("pre_tool: tool — Write, Edit или Bash".into()),
            (Event::PostTool, None | Some("Bash")) | (Event::Prompt, None) => {}
            _ => bad("tool — только у pre_tool (и Bash у post_tool)".into()),
        }
        let mut seen = BTreeSet::new();
        for e in &c.expected {
            if !seen.insert(e.as_str()) {
                bad(format!("{e} повторяется"));
            }
            if let Some(k) = e.strip_prefix(INSIGHT_REF) {
                if !full.insight_active(k) {
                    bad(format!("{e}: нет такого действующего insight"));
                }
                if c.event == Event::PreTool {
                    let local = full.insight(k).is_some_and(|i| {
                        i.triggers.iter().any(|t| {
                            matches!(t["type"].as_str(), Some("file_glob" | "command_regex"))
                        })
                    });
                    if !local {
                        bad(format!(
                            "{e}: pre_tool ждёт запись без file_glob/command_regex"
                        ));
                    }
                }
            } else if let Some(k) = e.strip_prefix(RULE_REF) {
                if !rule_keys.contains(k) {
                    bad(format!("{e}: нет такой заметки Rules"));
                }
                if c.event != Event::Prompt {
                    bad(format!("{e}: правила подсказывает только prompt"));
                }
            } else {
                bad(format!("{e}: ожидается insight:<key> или rule:<key>"));
            }
        }
        let slot = s.by_event.entry(c.event.as_str().to_string()).or_default();
        if c.expected.is_empty() {
            s.irrelevant += 1;
            slot.1 += 1;
        } else {
            s.relevant += 1;
            slot.0 += 1;
        }
    }
    if s.cases < MIN_CASES {
        errs.push(format!("случаев {} < {MIN_CASES}", s.cases));
    }
    let share = s.relevant as f64 / s.cases.max(1) as f64;
    if !HALF.contains(&share) {
        errs.push(format!(
            "релевантных {:.0} % — ожидается половина (45–55 %)",
            share * 100.0
        ));
    }
    for e in Event::ALL {
        let (r, i) = s.by_event.get(e.as_str()).copied().unwrap_or_default();
        if r == 0 || i == 0 {
            errs.push(format!(
                "{}: нужны и релевантные, и нерелевантные случаи",
                e.as_str()
            ));
        }
    }
    if !errs.is_empty() {
        bail!("набор подсказок:\n  {}", errs.join("\n  "));
    }
    Ok(s)
}

// ---------------------------------------------------------------- метрики

/// Итог случая для метрик: показанные и ожидаемые подсказки.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Outcome {
    pub shown: Vec<String>,
    pub expected: Vec<String>,
}

/// Метрики подсказок по набору случаев (см. модуль).
#[derive(Debug, Clone, Default, Serialize)]
pub struct HintMetrics {
    pub cases: usize,
    pub relevant: usize,
    pub irrelevant: usize,
    /// Показано подсказок всего и из них ожидаемых; ожидалось всего.
    pub shown: usize,
    pub shown_expected: usize,
    pub expected: usize,
    /// Показанные, которые ожидались; `None` — не показано ни одной.
    pub hint_precision: Option<Estimate>,
    /// Ожидаемые, которые показаны, — относительно `min(|expected|, 3)` (сервер показывает не
    /// больше трёх подсказок, [`recall_cap`]); `None` — релевантных случаев нет.
    pub hint_recall: Option<Estimate>,
    /// Нерелевантные случаи без подсказок; `None` — нерелевантных нет.
    pub silence_rate: Option<Estimate>,
    /// Релевантные случаи хотя бы с одной ожидаемой подсказкой (справочно).
    pub case_hit_rate: Option<Estimate>,
}

/// Знаменатель полноты случая: ожидаемых, но не больше лимита подсказок ответа
/// (`HINTS_MAX` = 3): случай с пятью релевантными записями при трёх показанных и верных —
/// полный, полнота мерит отбор, а не лимит сервера **[решение исполнителя, fix round 1]**.
pub fn recall_cap(o: &Outcome) -> f64 {
    o.expected.len().min(HINTS_MAX) as f64
}

/// Попаданий случая для полноты (не больше [`recall_cap`]).
fn capped_hit(o: &Outcome) -> f64 {
    (o.shown.iter().filter(|s| o.expected.contains(s)).count() as f64).min(recall_cap(o))
}

/// Метрики набора: точность считает и подсказки на нерелевантных случаях (они ложные), тишина
/// — доля нерелевантных случаев без подсказок, полнота — по ожидаемым (с лимитом подсказок).
pub fn hint_metrics(outcomes: &[Outcome]) -> HintMetrics {
    let hit = |o: &Outcome| o.shown.iter().filter(|s| o.expected.contains(s)).count() as f64;
    let shown: Vec<f64> = outcomes.iter().map(|o| o.shown.len() as f64).collect();
    let good: Vec<f64> = outcomes.iter().map(hit).collect();
    let rel: Vec<&Outcome> = outcomes.iter().filter(|o| !o.expected.is_empty()).collect();
    let irr: Vec<&Outcome> = outcomes.iter().filter(|o| o.expected.is_empty()).collect();
    let flag = |b: bool| if b { 1.0 } else { 0.0 };
    let opt = |v: Vec<f64>| (!v.is_empty()).then(|| estimate(&v));
    HintMetrics {
        cases: outcomes.len(),
        relevant: rel.len(),
        irrelevant: irr.len(),
        shown: shown.iter().sum::<f64>() as usize,
        shown_expected: good.iter().sum::<f64>() as usize,
        expected: rel.iter().map(|o| o.expected.len()).sum(),
        hint_precision: ratio_estimate(&good, &shown),
        hint_recall: ratio_estimate(
            &rel.iter().map(|o| capped_hit(o)).collect::<Vec<_>>(),
            &rel.iter().map(|o| recall_cap(o)).collect::<Vec<_>>(),
        ),
        silence_rate: opt(irr.iter().map(|o| flag(o.shown.is_empty())).collect()),
        case_hit_rate: opt(rel.iter().map(|o| flag(hit(o) > 0.0)).collect()),
    }
}

// ---------------------------------------------------------------- модели отбора (калибровка)

/// Кандидат подсказки `prompt`: ссылка набора и балл поиска (в порядке выдачи).
#[derive(Debug, Clone, Serialize)]
pub struct PromptCand {
    pub r#ref: String,
    pub score: f64,
}

/// Отбор `/recall(prompt)`: кандидаты с баллом ≥ `min` по порядку выдачи, не больше трёх.
pub fn prompt_pick(cands: &[PromptCand], min: f64) -> Vec<String> {
    cands
        .iter()
        .filter(|c| c.score >= min)
        .take(HINTS_MAX)
        .map(|c| c.r#ref.clone())
        .collect()
}

/// Правило порога покрытия слов ошибки у ослабленного FTS `post_tool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Coverage {
    /// `clamp(⌈n/k⌉, lo, hi)`, не больше `n` (сервер: k = 3, lo = 2, hi = 4).
    Ceil { k: usize, lo: usize, hi: usize },
    /// Ослабленный запрос не даёт подсказок — только строгий и `error_regex`.
    StrictOnly,
}

impl Coverage {
    /// Текущее правило сервера (`zam_search::recall::coverage_enough`).
    pub const CURRENT: Coverage = Coverage::Ceil { k: 3, lo: 2, hi: 4 };

    pub fn need(self, n: usize) -> Option<usize> {
        match self {
            Coverage::Ceil { k, lo, hi } => Some(n.div_ceil(k).clamp(lo, hi).min(n)),
            Coverage::StrictOnly => None,
        }
    }

    pub fn label(self) -> String {
        match self {
            Coverage::Ceil { lo, hi, .. } if lo == hi => format!("ровно {lo}"),
            Coverage::Ceil { k, lo, hi } => format!("clamp(⌈n/{k}⌉, {lo}, {hi})"),
            Coverage::StrictOnly => "только строгий запрос".into(),
        }
    }
}

/// Правила порога покрытия для перебора.
pub const COVERAGE_RULES: [Coverage; 8] = [
    Coverage::CURRENT,
    Coverage::Ceil { k: 1, lo: 1, hi: 1 },
    Coverage::Ceil { k: 1, lo: 2, hi: 2 },
    Coverage::Ceil { k: 1, lo: 3, hi: 3 },
    Coverage::Ceil { k: 2, lo: 2, hi: 4 },
    Coverage::Ceil { k: 3, lo: 2, hi: 6 },
    Coverage::Ceil { k: 4, lo: 2, hi: 4 },
    Coverage::StrictOnly,
];

/// Действующий insight как кандидат `post_tool`.
#[derive(Debug, Clone)]
pub struct ErrCand {
    pub r#ref: String,
    /// Заголовок и тело (текст записи в FTS).
    pub text: String,
    /// `error_regex` триггеров: шаблон и инструменты.
    pub regex: Vec<(String, Option<Vec<String>>)>,
}

/// Слова запроса как у FTS сервера (`zam_store::search::terms`): буквы и цифры, нижний регистр.
fn words(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in s
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let w = w.to_lowercase();
        if !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

/// Строгий запрос FTS: каждое слово ошибки есть в тексте (от 3 символов — префиксом слова).
fn strict_match(err_words: &[String], text: &str) -> bool {
    let tw = words(text);
    !err_words.is_empty()
        && err_words.iter().all(|e| {
            if e.chars().count() >= 3 {
                tw.iter().any(|w| w.starts_with(e.as_str()))
            } else {
                tw.iter().any(|w| w == e)
            }
        })
}

/// Модель отбора `/recall(post_tool)` (см. модуль): `error_regex` по сырой ошибке, затем FTS по
/// нормализованной — строгий запрос (все слова), если он ничего не нашёл — ослабленный с порогом
/// покрытия `rule`; порядок FTS — по покрытию (замена bm25), не больше трёх подсказок.
pub fn post_tool_pick(
    error: &str,
    tool: Option<&str>,
    cands: &[ErrCand],
    rule: Coverage,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in cands {
        let hit = c.regex.iter().any(|(p, tools)| {
            let tool_ok = match (tools, tool) {
                (Some(ts), Some(t)) => ts.iter().any(|x| x == t),
                _ => true,
            };
            tool_ok && error_regex_matches(p, error)
        });
        if hit && out.len() < HINTS_MAX {
            out.push(c.r#ref.clone());
        }
    }
    let normalized = normalize_error(error);
    let terms = error_terms(&normalized);
    if terms.is_empty() || out.len() >= HINTS_MAX {
        return out;
    }
    let all = words(&normalized);
    let mut scored: Vec<(usize, &ErrCand)> = cands
        .iter()
        .filter(|c| !out.contains(&c.r#ref))
        .map(|c| (term_coverage(&terms, &c.text), c))
        .collect();
    let strict: Vec<&ErrCand> = scored
        .iter()
        .filter(|(_, c)| strict_match(&all, &c.text))
        .map(|(_, c)| *c)
        .collect();
    let picked: Vec<&ErrCand> = if !strict.is_empty() {
        strict
    } else if all.len() > 1 {
        let Some(need) = rule.need(terms.len()) else {
            return out;
        };
        scored.retain(|(cov, _)| *cov >= need.max(1));
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.r#ref.cmp(&b.1.r#ref)));
        scored.into_iter().map(|(_, c)| c).collect()
    } else {
        Vec::new()
    };
    for c in picked {
        if out.len() >= HINTS_MAX {
            break;
        }
        out.push(c.r#ref.clone());
    }
    out
}

/// Кандидаты `post_tool`: действующие insight корпуса и набора.
fn err_cands(full: &Corpus) -> Vec<ErrCand> {
    full.insights
        .iter()
        .filter(|i| full.insight_active(&i.key))
        .map(|i| ErrCand {
            r#ref: format!("{INSIGHT_REF}{}", i.key),
            text: format!("{}\n{}", i.title, i.body),
            regex: i
                .triggers
                .iter()
                .filter(|t| t["type"] == "error_regex")
                .filter_map(|t| {
                    let p = t["pattern"].as_str()?.to_string();
                    let tools = t["tools"].as_array().map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    });
                    Some((p, tools))
                })
                .collect(),
        })
        .collect()
}

// ---------------------------------------------------------------- прогон

/// Итог случая.
#[derive(Debug, Clone, Serialize)]
pub struct CaseRun {
    pub id: String,
    pub event: Event,
    pub input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub expected: Vec<String>,
    pub shown: Vec<String>,
    /// `reason` подсказок `/recall` (у `pre_tool` — нет).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    /// `prompt`: кандидаты внутреннего поиска с баллами (калибровка порога).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<PromptCand>,
    pub ms: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub note: String,
}

/// Строка перебора порога.
#[derive(Debug, Clone, Serialize)]
pub struct SweepRow {
    pub setting: String,
    pub current: bool,
    pub metrics: HintMetrics,
    /// Парная разность с текущим значением (этот − текущий) по тем же случаям.
    pub delta: Deltas,
}

/// Парные разности метрик двух конфигураций на одних случаях.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Deltas {
    pub hint_precision: Option<Estimate>,
    pub hint_recall: Option<Estimate>,
    pub silence_rate: Option<Estimate>,
}

/// Разности `b − a` метрик по одним случаям (`a`, `b` — итоги в одном порядке).
pub fn deltas(a: &[Outcome], b: &[Outcome]) -> Deltas {
    assert_eq!(a.len(), b.len(), "парное сравнение на одних случаях");
    let hit = |o: &Outcome| o.shown.iter().filter(|s| o.expected.contains(s)).count() as f64;
    let shown = |o: &Outcome| o.shown.len() as f64;
    let expected = |o: &Outcome| recall_cap(o);
    let rhit = |o: &Outcome| capped_hit(o);
    let col = |v: &[Outcome], f: &dyn Fn(&Outcome) -> f64| v.iter().map(f).collect::<Vec<_>>();
    let rel = |v: &[Outcome]| -> Vec<Outcome> {
        v.iter()
            .filter(|o| !o.expected.is_empty())
            .cloned()
            .collect()
    };
    let silent = |v: &[Outcome]| -> Vec<f64> {
        v.iter()
            .filter(|o| o.expected.is_empty())
            .map(|o| if o.shown.is_empty() { 1.0 } else { 0.0 })
            .collect()
    };
    let (ra, rb) = (rel(a), rel(b));
    let (sa, sb) = (silent(a), silent(b));
    Deltas {
        hint_precision: paired_ratio_diff(
            &col(a, &hit),
            &col(a, &shown),
            &col(b, &hit),
            &col(b, &shown),
        ),
        hint_recall: paired_ratio_diff(
            &col(&ra, &rhit),
            &col(&ra, &expected),
            &col(&rb, &rhit),
            &col(&rb, &expected),
        ),
        silence_rate: (!sa.is_empty()).then(|| paired_diff(&sa, &sb)),
    }
}

/// Калибровка порогов.
#[derive(Debug, Clone, Serialize)]
pub struct Calibration {
    pub prompt: Vec<SweepRow>,
    /// Доля случаев `prompt`, где модель отбора при 0,6 совпала с ответом `/recall`.
    pub prompt_agreement: f64,
    pub post_tool: Vec<SweepRow>,
    /// То же для `post_tool` при текущем правиле.
    pub post_tool_agreement: f64,
    /// Случаи `post_tool`, где модель и сервер разошлись.
    pub post_tool_mismatches: Vec<String>,
}

/// Сводка задержек, мс.
#[derive(Debug, Clone, Serialize)]
pub struct Latency {
    pub n: usize,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
    pub mean: f64,
    /// `timings.total_ms` сервера (у `/recall`), p50 / p95.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_p50: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_p95: Option<f64>,
    /// Ответов с `recall timeout` (у хуков — пустой вывод, если подсказки ожидались).
    pub timeouts: usize,
    /// Непустых ответов (подсказка показана).
    pub non_empty: usize,
    /// Бюджет §10.3 / §17, мс.
    pub budget_ms: f64,
    /// Доля замеров дольше бюджета.
    pub over_budget: f64,
}

fn latency(ms: &[f64], server: &[f64], timeouts: usize, non_empty: usize, budget: f64) -> Latency {
    let mean = if ms.is_empty() {
        0.0
    } else {
        ms.iter().sum::<f64>() / ms.len() as f64
    };
    Latency {
        n: ms.len(),
        p50: quantile(ms, 0.5),
        p95: quantile(ms, 0.95),
        max: ms.iter().copied().fold(0.0, f64::max),
        mean,
        server_p50: (!server.is_empty()).then(|| quantile(server, 0.5)),
        server_p95: (!server.is_empty()).then(|| quantile(server, 0.95)),
        timeouts,
        non_empty,
        budget_ms: budget,
        over_budget: ms.iter().filter(|m| **m > budget).count() as f64 / ms.len().max(1) as f64,
    }
}

/// Сведения о прогоне.
#[derive(Debug, Clone, Serialize)]
pub struct HintsInfo {
    pub date: String,
    pub git: String,
    pub label: Option<String>,
    pub model: Option<String>,
    pub model_id: Option<String>,
    pub device: Option<String>,
    pub sets: BTreeMap<String, String>,
    pub server_config: Value,
    pub corpus_fingerprint: String,
    pub hints_fingerprint: String,
    pub index_secs: f64,
}

/// Итог `eval --hints`.
#[derive(Debug, Clone, Serialize)]
pub struct HintsResult {
    pub info: HintsInfo,
    pub overall: HintMetrics,
    pub by_event: BTreeMap<String, HintMetrics>,
    pub calibration: Calibration,
    /// `/recall` по событиям и процессы хуков по командам.
    pub recall_latency: BTreeMap<String, Latency>,
    pub hook_latency: BTreeMap<String, Latency>,
    /// Строки `hooks.log` проекта (ошибки хуков) после прогона.
    pub hook_log_lines: usize,
    pub cases: Vec<CaseRun>,
}

/// Бюджеты §10.3 (хуки, с процессом) и §17 (`recall` с сетью LAN), мс.
fn hook_budget(cmd: &str) -> f64 {
    match cmd {
        "session-start" => 300.0,
        "pre-tool" => 50.0,
        _ => 250.0,
    }
}

fn recall_budget(device: Option<&str>) -> f64 {
    if device == Some("cuda") { 150.0 } else { 250.0 }
}

/// Каталог проекта для хуков: `.zeroaimemory/solution.toml` решения набора; свой «дом» без
/// `~/.zeroaimemory/config.toml` (адрес и токен — `ZAM_URL`, `ZAM_TOKEN`).
struct Hooks {
    zam: PathBuf,
    project: PathBuf,
    home: PathBuf,
    base: String,
    token: String,
}

impl Hooks {
    fn prepare(dir: &Path, zam: PathBuf, base: &str, token: &str) -> Result<Hooks> {
        let project = dir.join("project");
        let home = dir.join("home");
        if project.exists() {
            fs::remove_dir_all(&project)?;
        }
        fs::create_dir_all(project.join(".zeroaimemory"))?;
        fs::create_dir_all(&home)?;
        fs::write(
            project.join(".zeroaimemory").join("solution.toml"),
            format!("agent_solution_id = \"{EVAL_SOLUTION}\"\nname = \"eval\"\n"),
        )?;
        Ok(Hooks {
            zam,
            project,
            home,
            base: base.to_string(),
            token: token.to_string(),
        })
    }

    /// `zam hook <cmd>` со входом `stdin`: вывод и время процесса, мс.
    fn run(&self, cmd: &str, stdin: &Value) -> Result<(String, f64)> {
        let mut c = Command::new(&self.zam);
        scrub_zam_env(&mut c, &[]);
        for k in ["CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "CODEX_THREAD_ID"] {
            c.env_remove(k);
        }
        c.args(["hook", cmd])
            .current_dir(&self.project)
            .env("ZAM_URL", &self.base)
            .env("ZAM_TOKEN", &self.token)
            .env("USERPROFILE", &self.home)
            .env("HOME", &self.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let t0 = Instant::now();
        let mut child = c
            .spawn()
            .with_context(|| format!("запуск {}", self.zam.display()))?;
        child
            .stdin
            .take()
            .context("stdin хука")?
            .write_all(stdin.to_string().as_bytes())?;
        // ожидание — в своём потоке: время выхода точное, а зависший процесс убивается по
        // CALL_TIMEOUT (хук обязан укладываться в свой бюджет и никогда не висеть)
        let pid = child.id();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let out = child.wait_with_output();
            let _ = tx.send((out, Instant::now()));
        });
        let (out, done) = match rx.recv_timeout(CALL_TIMEOUT) {
            Ok(r) => r,
            Err(_) => {
                kill_pid(pid);
                bail!(
                    "zam hook {cmd}: не завершился за {} с — процесс {pid} убит",
                    CALL_TIMEOUT.as_secs()
                );
            }
        };
        let out = out?;
        let ms = done.duration_since(t0).as_secs_f64() * 1000.0;
        if !out.status.success() {
            bail!(
                "zam hook {cmd}: код {:?} (хук обязан выходить с 0)",
                out.status.code()
            );
        }
        Ok((String::from_utf8_lossy(&out.stdout).into_owned(), ms))
    }

    fn input(&self, event: &str, session: &str, extra: Value) -> Value {
        let mut v = json!({
            "hook_event_name": event,
            "session_id": session,
            "cwd": self.project.to_string_lossy(),
        });
        if let (Value::Object(o), Value::Object(e)) = (&mut v, extra) {
            o.extend(e);
        }
        v
    }

    fn log_text(&self) -> String {
        fs::read_to_string(
            self.project
                .join(".zeroaimemory")
                .join("logs")
                .join("hooks.log"),
        )
        .unwrap_or_default()
    }

    /// Строки журнала хуков проекта.
    fn log_lines(&self) -> usize {
        self.log_text().lines().count()
    }

    /// Строки журнала после первых `before`.
    fn new_log(&self, before: usize) -> Vec<String> {
        self.log_text()
            .lines()
            .skip(before)
            .map(String::from)
            .collect()
    }
}

/// Принудительное завершение процесса по pid (зависший хук).
fn kill_pid(pid: u32) {
    let st = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else {
        Command::new("kill")
            .args(["-9", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
    };
    if !st.is_ok_and(|s| s.success()) {
        eprintln!("не удалось завершить процесс хука {pid}");
    }
}

/// Текст подсказки хука из вывода (`hookSpecificOutput.additionalContext`).
fn hook_context(out: &str) -> String {
    serde_json::from_str::<Value>(out.trim())
        .ok()
        .and_then(|v| {
            v["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .map(String::from)
        })
        .unwrap_or_default()
}

/// Ссылки `i:<id>` / `n:<id>` в тексте по порядку (id — 26 символов Crockford).
pub fn refs_in_text(text: &str) -> Vec<(char, String)> {
    let b = text.as_bytes();
    let mut out: Vec<(char, String)> = Vec::new();
    let mut i = 0;
    while i + 28 <= b.len() {
        let prev_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if prev_ok
            && matches!(b[i], b'i' | b'n' | b'c' | b'r' | b'e')
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

/// Ссылки набора: insight — `insight:<key>`, узел Rules — `rule:<key>`, узел корпуса — путь.
struct RefMap {
    entries: BTreeMap<String, String>,
    nodes: BTreeMap<String, String>,
}

impl RefMap {
    fn of(s: &Session) -> RefMap {
        RefMap {
            entries: s.entry_keys().clone(),
            nodes: s.node_paths().clone(),
        }
    }

    /// `i:<id>` → `insight:<key>`; `n:<id>` → `rule:<key>` или путь; иначе как есть.
    fn map(&self, r: &str) -> String {
        let (kind, id) = match r.split_once(':') {
            Some((k, id)) => (k, id),
            None => ("", r),
        };
        match kind {
            "i" | "e" => match self.entries.get(id) {
                Some(k) => format!("{INSIGHT_REF}{k}"),
                None => format!("entry:{id}"),
            },
            "n" => self
                .nodes
                .get(id)
                .cloned()
                .unwrap_or_else(|| format!("node:{id}")),
            _ => r.to_string(),
        }
    }
}

fn warnings_of(v: &Value) -> Vec<String> {
    v["warnings"]
        .as_array()
        .map(|w| {
            w.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// `POST /recall`: ответ и время на клиенте, мс.
fn recall(s: &Session, body: &Value) -> Result<(Value, f64)> {
    let t0 = Instant::now();
    let (st, v) = api_timeout(
        &s.base,
        s.token(),
        "/api/v1/recall",
        Some(body),
        CALL_TIMEOUT,
    )?;
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    if st != 200 {
        bail!("POST /recall: HTTP {st}: {v}");
    }
    Ok((v, ms))
}

/// Кандидаты `prompt` того же внутреннего поиска, что у `/recall(prompt)`.
fn prompt_candidates(s: &Session, refs: &RefMap, prompt: &str) -> Result<Vec<PromptCand>> {
    let query: String = prompt.chars().take(512).collect();
    let body = json!({
        "query": query,
        "scope": {"solution": EVAL_SOLUTION, "include_knowledge": true, "include_rules": true},
        "filters": {"kinds": ["insight", "note"]},
        "mode": "brief",
        "top_k": PROMPT_SEARCH_TOP_K,
        "budget_tokens": PROMPT_SEARCH_BUDGET,
        "rerank": "off",
        "skip_seen": false,
        "no_cache": true,
        "format": "json",
    });
    let (st, v) = api_timeout(
        &s.base,
        s.token(),
        "/api/v1/search",
        Some(&body),
        CALL_TIMEOUT,
    )?;
    if st != 200 {
        bail!("POST /search: HTTP {st}: {v}");
    }
    let mut out = Vec::new();
    for it in v["items"].as_array().into_iter().flatten() {
        let space = it["space"]["kind"].as_str().unwrap_or_default();
        let r = match (
            it["kind"].as_str(),
            it["entry_id"].as_str(),
            it["node_id"].as_str(),
        ) {
            (Some("insight"), Some(id), _) => refs.map(&format!("i:{id}")),
            (Some("note"), _, Some(id)) if space == "rules" => refs.map(&format!("n:{id}")),
            _ => continue,
        };
        out.push(PromptCand {
            r#ref: r,
            score: it["score"].as_f64().unwrap_or(0.0),
        });
    }
    Ok(out)
}

/// Загрузка заметок Rules набора (`POST /nodes`, пространство `rules`): id → `rule:<key>`.
/// Пишет администратор через сессию WebUI: агенту запись в Rules по умолчанию запрещена
/// (`rules.agent_write = false`, §11), человек пишет в свои Rules всегда (решение T12-a).
fn load_rules(s: &mut Session, rules: &[Rule]) -> Result<()> {
    let admin = admin_login(&s.base)?;
    let agent = agent_with(CALL_TIMEOUT);
    for r in rules {
        let body = json!({ "space": "rules", "path": r.path, "kind": "note", "content": r.text });
        let mut resp = agent
            .post(&format!("{}/api/v1/nodes", s.base))
            .header("Cookie", &admin.cookie)
            .header("X-CSRF-Token", &admin.csrf)
            .content_type("application/json")
            .send(body.to_string())?;
        let st = resp.status().as_u16();
        let text = resp.body_mut().read_to_string()?;
        if st != 201 {
            bail!("POST /nodes rules/{}: HTTP {st}: {text}", r.path);
        }
        let v: Value = serde_json::from_str(&text)?;
        let id = v["id"].as_str().context("POST /nodes: нет id")?.to_string();
        s.note_node(&id, &format!("{RULE_REF}{}", r.key))?;
    }
    Ok(())
}

/// Путь файла случая `pre_tool` в каталоге проекта (абсолютный, как у Claude Code).
fn tool_input(h: &Hooks, c: &Case) -> Value {
    match c.tool.as_deref() {
        Some("Bash") => json!({ "command": c.input }),
        _ => json!({ "file_path": h.project.join(&c.input).to_string_lossy() }),
    }
}

/// Случаи набора.
fn run_cases(
    s: &Session,
    h: &Hooks,
    refs: &RefMap,
    set: &HintSet,
    tag: &str,
) -> Result<Vec<CaseRun>> {
    let mut out = Vec::with_capacity(set.cases.len());
    for c in &set.cases {
        let mut run = CaseRun {
            id: c.id.clone(),
            event: c.event,
            input: c.input.clone(),
            tool: c.tool.clone(),
            expected: c.expected.clone(),
            shown: Vec::new(),
            reasons: Vec::new(),
            candidates: Vec::new(),
            ms: 0.0,
            warnings: Vec::new(),
            note: c.note.clone(),
        };
        match c.event {
            Event::Prompt | Event::PostTool => {
                let body = match c.event {
                    Event::Prompt => {
                        json!({"event": "prompt", "prompt": c.input, "solution": EVAL_SOLUTION})
                    }
                    _ => json!({"event": "post_tool", "error": c.input, "tool": "Bash",
                                "solution": EVAL_SOLUTION}),
                };
                let (v, ms) = recall(s, &body)?;
                run.ms = ms;
                run.warnings = warnings_of(&v);
                for it in v["items"].as_array().into_iter().flatten() {
                    run.shown
                        .push(refs.map(it["id"].as_str().unwrap_or_default()));
                    run.reasons
                        .push(it["reason"].as_str().unwrap_or_default().to_string());
                }
                if c.event == Event::Prompt {
                    run.candidates = prompt_candidates(s, refs, &c.input)?;
                }
            }
            Event::PreTool => {
                let session = format!("hints-{tag}-{}", c.id);
                let inp = h.input(
                    "PreToolUse",
                    &session,
                    json!({"tool_name": c.tool, "tool_input": tool_input(h, c)}),
                );
                let (o, ms) = h.run("pre-tool", &inp)?;
                run.ms = ms;
                run.shown = refs_in_text(&hook_context(&o))
                    .into_iter()
                    .map(|(k, id)| refs.map(&format!("{k}:{id}")))
                    .collect();
            }
        }
        out.push(run);
    }
    Ok(out)
}

fn outcomes<'a>(runs: impl Iterator<Item = &'a CaseRun>) -> Vec<Outcome> {
    runs.map(|r| Outcome {
        shown: r.shown.clone(),
        expected: r.expected.clone(),
    })
    .collect()
}

/// Строки перебора: итоги каждой настройки и разности с текущей по тем же случаям.
fn sweep(rows: Vec<(String, bool, Vec<Outcome>)>) -> Vec<SweepRow> {
    let current = rows
        .iter()
        .find(|r| r.1)
        .map(|r| r.2.clone())
        .unwrap_or_default();
    rows.into_iter()
        .map(|(setting, is_current, out)| SweepRow {
            delta: deltas(&current, &out),
            metrics: hint_metrics(&out),
            setting,
            current: is_current,
        })
        .collect()
}

/// Перебор порогов по данным прогона (см. модуль).
fn calibrate(runs: &[CaseRun], full: &Corpus) -> Calibration {
    let prompts: Vec<&CaseRun> = runs.iter().filter(|r| r.event == Event::Prompt).collect();
    let prompt = sweep(
        PROMPT_THRESHOLDS
            .iter()
            .map(|&t| {
                (
                    format!("{t:.2}"),
                    (t - PROMPT_MIN_SCORE).abs() < 1e-9,
                    prompts
                        .iter()
                        .map(|r| Outcome {
                            shown: prompt_pick(&r.candidates, t),
                            expected: r.expected.clone(),
                        })
                        .collect(),
                )
            })
            .collect(),
    );
    let agree = |a: &[String], b: &[String]| {
        a.iter().collect::<BTreeSet<_>>() == b.iter().collect::<BTreeSet<_>>()
    };
    let prompt_agreement = prompts
        .iter()
        .filter(|r| agree(&prompt_pick(&r.candidates, PROMPT_MIN_SCORE), &r.shown))
        .count() as f64
        / prompts.len().max(1) as f64;
    let cands = err_cands(full);
    let posts: Vec<&CaseRun> = runs.iter().filter(|r| r.event == Event::PostTool).collect();
    let post_tool = sweep(
        COVERAGE_RULES
            .iter()
            .map(|&rule| {
                (
                    rule.label(),
                    rule == Coverage::CURRENT,
                    posts
                        .iter()
                        .map(|r| Outcome {
                            shown: post_tool_pick(&r.input, Some("Bash"), &cands, rule),
                            expected: r.expected.clone(),
                        })
                        .collect(),
                )
            })
            .collect(),
    );
    let mut mismatches = Vec::new();
    for r in &posts {
        let sim = post_tool_pick(&r.input, Some("Bash"), &cands, Coverage::CURRENT);
        if !agree(&sim, &r.shown) {
            mismatches.push(format!("{}: модель {:?}, сервер {:?}", r.id, sim, r.shown));
        }
    }
    let post_tool_agreement = 1.0 - mismatches.len() as f64 / posts.len().max(1) as f64;
    Calibration {
        prompt,
        prompt_agreement,
        post_tool,
        post_tool_agreement,
        post_tool_mismatches: mismatches,
    }
}

/// Задержки `/recall` и хуков (см. модуль).
fn measure_latency(
    s: &Session,
    h: &Hooks,
    set: &HintSet,
    queries: &[Query],
    tag: &str,
) -> Result<(BTreeMap<String, Latency>, BTreeMap<String, Latency>)> {
    let device = s.status["device"]["kind"].as_str().map(String::from);
    let rb = recall_budget(device.as_deref());
    let mut api = BTreeMap::new();
    let mut hooks = BTreeMap::new();
    let has_timeout = |v: &Value| warnings_of(v).iter().any(|w| w == WARN_RECALL_TIMEOUT);
    let server_ms = |v: &Value| v["timings"]["total_ms"].as_f64();

    // prompt через API: тексты запросов набора, каждый один раз
    let texts: Vec<&str> = queries.iter().map(|q| q.text.as_str()).collect();
    let (api_texts, hook_texts) = texts.split_at(LATENCY_PROMPTS_API.min(texts.len()));
    let mut ms = Vec::new();
    let mut srv = Vec::new();
    let (mut to, mut ne) = (0, 0);
    for t in api_texts {
        let (v, m) = recall(
            s,
            &json!({"event": "prompt", "prompt": t, "solution": EVAL_SOLUTION}),
        )?;
        ms.push(m);
        srv.extend(server_ms(&v));
        to += usize::from(has_timeout(&v));
        ne += usize::from(!v["text"].as_str().unwrap_or_default().is_empty());
    }
    api.insert("prompt".to_string(), latency(&ms, &srv, to, ne, rb));

    // post_tool через API
    let errors: Vec<&str> = set
        .cases
        .iter()
        .filter(|c| c.event == Event::PostTool)
        .map(|c| c.input.as_str())
        .collect();
    let (mut ms, mut srv, mut to, mut ne) = (Vec::new(), Vec::new(), 0, 0);
    for _ in 0..LATENCY_POST_API_REPS {
        for e in &errors {
            let (v, m) = recall(
                s,
                &json!({"event": "post_tool", "error": e, "tool": "Bash", "solution": EVAL_SOLUTION}),
            )?;
            ms.push(m);
            srv.extend(server_ms(&v));
            to += usize::from(has_timeout(&v));
            ne += usize::from(!v["text"].as_str().unwrap_or_default().is_empty());
        }
    }
    api.insert("post_tool".to_string(), latency(&ms, &srv, to, ne, rb));

    // session_start через API: новая сессия на каждый вызов
    let (mut ms, mut srv, mut to, mut ne) = (Vec::new(), Vec::new(), 0, 0);
    for i in 0..LATENCY_SESSION_START {
        let (v, m) = recall(
            s,
            &json!({"event": "session_start", "session_id": format!("lat-{tag}-api-{i}"),
                    "solution": EVAL_SOLUTION}),
        )?;
        ms.push(m);
        srv.extend(server_ms(&v));
        to += usize::from(has_timeout(&v));
        ne += usize::from(!v["text"].as_str().unwrap_or_default().is_empty());
    }
    api.insert("session_start".to_string(), latency(&ms, &srv, to, ne, rb));

    // хуки: session-start (новая сессия), prompt (остальные тексты), post-tool, pre-tool
    let mut hook = |cmd: &str, inputs: Vec<Value>| -> Result<()> {
        let mut ms = Vec::new();
        let mut ne = 0;
        let mut timeouts = 0;
        for inp in &inputs {
            let before = h.log_lines();
            let (o, m) = h.run(cmd, inp)?;
            ms.push(m);
            ne += usize::from(!hook_context(&o).is_empty() || o.contains("\"decision\""));
            // хук молчит по тайм-ауту (fail-open) и пишет причину в hooks.log
            timeouts += usize::from(
                h.new_log(before)
                    .iter()
                    .any(|l| l.contains("timeout") || l.contains("бюджет исчерпан")),
            );
        }
        hooks.insert(
            cmd.to_string(),
            latency(&ms, &[], timeouts, ne, hook_budget(cmd)),
        );
        Ok(())
    };
    hook(
        "session-start",
        (0..LATENCY_SESSION_START)
            .map(|i| {
                h.input(
                    "SessionStart",
                    &format!("lat-{tag}-hook-{i}"),
                    json!({"source": "startup"}),
                )
            })
            .collect(),
    )?;
    hook(
        "prompt",
        hook_texts
            .iter()
            .take(LATENCY_PROMPTS_HOOK)
            .enumerate()
            .map(|(i, t)| {
                h.input(
                    "UserPromptSubmit",
                    &format!("lat-{tag}-prompt-{i}"),
                    json!({"prompt": t}),
                )
            })
            .collect(),
    )?;
    hook(
        "post-tool",
        (0..LATENCY_POST_HOOK_REPS)
            .flat_map(|rep| errors.iter().enumerate().map(move |(i, e)| (rep, i, *e)))
            .map(|(rep, i, e)| {
                h.input(
                    "PostToolUseFailure",
                    &format!("lat-{tag}-post-{rep}-{i}"),
                    json!({"tool_name": "Bash", "tool_input": {"command": "x"}, "error": e}),
                )
            })
            .collect(),
    )?;
    let pre: Vec<&Case> = set
        .cases
        .iter()
        .filter(|c| c.event == Event::PreTool)
        .collect();
    hook(
        "pre-tool",
        (0..LATENCY_PRE_HOOK_REPS)
            .flat_map(|rep| pre.iter().map(move |c| (rep, *c)))
            .map(|(rep, c)| {
                h.input(
                    "PreToolUse",
                    &format!("lat-{tag}-pre-{rep}-{}", c.id),
                    json!({"tool_name": c.tool, "tool_input": tool_input(h, c)}),
                )
            })
            .collect(),
    )?;
    Ok((api, hooks))
}

/// `cargo xtask eval --hints`.
pub fn run(a: &EvalArgs, p: &Paths, corpus: &Corpus, queries: &[Query]) -> Result<()> {
    let (set, fingerprint) = load_hints(&p.hints)?;
    let summary = validate_hints(&set, corpus)?;
    println!(
        "набор подсказок: {} случаев (релевантных {}, нерелевантных {}), insight {}, правил {}",
        summary.cases, summary.relevant, summary.irrelevant, summary.insights, summary.rules
    );
    let full = with_hint_insights(corpus, &set);
    let server = run::server_dir(a, p)?;
    let zam = run::build_cli(&p.repo, a.cli_bin.as_deref())?;
    let only_fts = a.modes.iter().all(|m| *m == Mode::Fts);
    let model = (!only_fts).then(|| {
        a.model
            .clone()
            .unwrap_or_else(|| run::default_model(a.device).to_string())
    });
    let label = match &a.label {
        Some(l) => format!("hints-{l}"),
        None => "hints".to_string(),
    };
    let spec = RunSpec {
        name: run::run_name(model.as_deref(), Some(&label)),
        model: model.clone(),
        device: if only_fts { Device::Cpu } else { a.device },
        reranker: None,
        sets: run::parse_sets(&a.sets)?,
        keep_cuda_path: a.keep_cuda_path,
        ctrl_break: false,
    };
    let mut s = Session::start(p, &spec, &server, !a.reuse)?;
    s.check_model()?;
    println!(
        "сервер {}: модель {}, устройство {}",
        s.base, s.status["model"]["id"], s.status["device"]
    );
    if !a.reuse {
        s.load_corpus(&full)?;
        load_rules(&mut s, &set.rule)?;
    }
    if model.is_some() {
        s.wait_indexed()?;
        println!("индексация: {:.1} с", s.index_secs);
    }
    let dir = s.data.parent().context("каталог прогона")?.to_path_buf();
    let hooks = Hooks::prepare(&dir, zam, &s.base, s.token())?;
    let refs = RefMap::of(&s);
    let tag = run::today();
    // кэш триггеров pre-tool: session-start пишет .zeroaimemory/cache/triggers.json; хук
    // укладывается в бюджет 300 мс или молчит (fail-open), поэтому — несколько попыток
    let log = hooks
        .project
        .join(".zeroaimemory")
        .join("logs")
        .join("hooks.log");
    let triggers = hooks
        .project
        .join(".zeroaimemory")
        .join("cache")
        .join("triggers.json");
    for attempt in 1..=START_ATTEMPTS {
        hooks.run(
            "session-start",
            &hooks.input(
                "SessionStart",
                &format!("hints-{tag}-start-{attempt}"),
                json!({"source": "startup"}),
            ),
        )?;
        if triggers.is_file() {
            break;
        }
        println!("  session-start: кэш триггеров не записан (попытка {attempt}), повтор");
    }
    if !triggers.is_file() {
        bail!(
            "zam hook session-start не записал кэш триггеров за {START_ATTEMPTS} попыток (см. {})",
            log.display()
        );
    }
    let runs = run_cases(&s, &hooks, &refs, &set, &tag)?;
    let calibration = calibrate(&runs, &full);
    println!("задержки /recall и хуков…");
    let (recall_latency, hook_latency) = measure_latency(&s, &hooks, &set, queries, &tag)?;
    let by_event = Event::ALL
        .iter()
        .map(|e| {
            (
                e.as_str().to_string(),
                hint_metrics(&outcomes(runs.iter().filter(|r| r.event == *e))),
            )
        })
        .collect();
    let st = &s.status;
    let result = HintsResult {
        info: HintsInfo {
            date: run::today(),
            git: run::git_rev(&p.repo),
            label: a.label.clone(),
            model: model.clone(),
            model_id: model
                .as_ref()
                .map(|_| st["model"]["id"].as_str().unwrap_or_default().to_string()),
            device: model.as_ref().map(|_| {
                st["device"]["kind"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            }),
            sets: spec.sets.iter().cloned().collect(),
            server_config: s.server_config.clone(),
            corpus_fingerprint: corpus.fingerprint(),
            hints_fingerprint: fingerprint,
            index_secs: s.index_secs,
        },
        overall: hint_metrics(&outcomes(runs.iter())),
        by_event,
        calibration,
        recall_latency,
        hook_latency,
        hook_log_lines: hooks.log_lines(),
        cases: runs,
    };
    s.stop()?;
    let (json_path, md_path) = write(&p.results, &result)?;
    print!("{}", render_md(&result));
    println!("  {}\n  {}", json_path.display(), md_path.display());
    Ok(())
}

// ---------------------------------------------------------------- отчёт

fn est(e: &Option<Estimate>) -> String {
    match e {
        Some(e) => format!("{:.3} [{:.3}…{:.3}]", e.mean, e.lo, e.hi),
        None => "—".into(),
    }
}

fn delta(e: &Option<Estimate>) -> String {
    match e {
        Some(e) if e.mean == 0.0 && e.lo == 0.0 && e.hi == 0.0 => "0".into(),
        Some(e) => format!("{:+.3} [{:+.3}…{:+.3}]", e.mean, e.lo, e.hi),
        None => "—".into(),
    }
}

fn sweep_rows(s: &mut String, what: &str, rows: &[SweepRow]) {
    s.push_str(&format!(
        "| {what} | hint_precision | hint_recall | silence_rate | ожидаемых/показано | \
         Δ precision | Δ recall | Δ silence |\n|---|---|---|---|---|---|---|---|\n"
    ));
    for row in rows {
        let name = if row.current {
            format!("**{}** (сейчас)", row.setting)
        } else {
            row.setting.clone()
        };
        let m = &row.metrics;
        s.push_str(&format!(
            "| {name} | {} | {} | {} | {}/{} | {} | {} | {} |\n",
            est(&m.hint_precision),
            est(&m.hint_recall),
            est(&m.silence_rate),
            m.shown_expected,
            m.shown,
            delta(&row.delta.hint_precision),
            delta(&row.delta.hint_recall),
            delta(&row.delta.silence_rate)
        ));
    }
}

fn metrics_row(name: &str, m: &HintMetrics) -> String {
    format!(
        "| {name} | {} ({}/{}) | {} | {} | {} | {} | {}/{} |\n",
        m.cases,
        m.relevant,
        m.irrelevant,
        est(&m.hint_precision),
        est(&m.hint_recall),
        est(&m.silence_rate),
        est(&m.case_hit_rate),
        m.shown_expected,
        m.shown
    )
}

const METRICS_HEAD: &str = "| | случаев (рел./нерел.) | hint_precision | hint_recall | silence_rate | случаев с попаданием | ожидаемых/показано |\n|---|---|---|---|---|---|---|\n";

fn latency_row(name: &str, l: &Latency) -> String {
    let srv = match (l.server_p50, l.server_p95) {
        (Some(a), Some(b)) => format!("{a:.1} / {b:.1}"),
        _ => "—".into(),
    };
    format!(
        "| {name} | {} | {:.1} | {:.1} | {:.1} | {srv} | {:.0} | {:.1} % | {} | {} |\n",
        l.n,
        l.p50,
        l.p95,
        l.max,
        l.budget_ms,
        l.over_budget * 100.0,
        l.timeouts,
        l.non_empty
    )
}

/// Markdown-отчёт прогона.
pub fn render_md(r: &HintsResult) -> String {
    let i = &r.info;
    let mut s = format!(
        "# Подсказки хуков: {} {} ({})\n\n",
        i.model.as_deref().unwrap_or("fts"),
        i.device.as_deref().unwrap_or("—"),
        i.date
    );
    s.push_str(&format!(
        "Коммит `{}`, модель `{}`, отпечаток корпуса `{}`, набора подсказок `{}`, индексация {:.0} с; \
         переопределения: {:?}.\n\n",
        i.git,
        i.model_id.as_deref().unwrap_or("—"),
        i.corpus_fingerprint,
        i.hints_fingerprint,
        i.index_secs,
        i.sets
    ));
    s.push_str("## Точность\n\n");
    s.push_str(METRICS_HEAD);
    s.push_str(&metrics_row("все", &r.overall));
    for (e, m) in &r.by_event {
        s.push_str(&metrics_row(e, m));
    }
    s.push_str("\n## Калибровка\n\n");
    s.push_str(&format!(
        "`recall.prompt_min_score` (модель отбора совпала с `/recall` при 0,60 в {:.0} % случаев):\n\n",
        r.calibration.prompt_agreement * 100.0
    ));
    sweep_rows(&mut s, "порог", &r.calibration.prompt);
    s.push_str(&format!(
        "\nПорог покрытия ослабленного FTS `post_tool` (модель совпала с `/recall` при текущем правиле в {:.0} % случаев):\n\n",
        r.calibration.post_tool_agreement * 100.0
    ));
    sweep_rows(&mut s, "правило", &r.calibration.post_tool);
    if !r.calibration.post_tool_mismatches.is_empty() {
        s.push_str("\nРасхождения модели и сервера:\n\n");
        for m in &r.calibration.post_tool_mismatches {
            s.push_str(&format!("- {m}\n"));
        }
    }
    s.push_str("\n## Задержки, мс\n\n");
    s.push_str("| | n | p50 | p95 | max | сервер p50 / p95 | бюджет | дольше бюджета | `recall timeout` | непустых |\n|---|---|---|---|---|---|---|---|---|---|\n");
    for (e, l) in &r.recall_latency {
        s.push_str(&latency_row(&format!("`/recall` {e}"), l));
    }
    for (c, l) in &r.hook_latency {
        s.push_str(&latency_row(&format!("`zam hook {c}`"), l));
    }
    s.push_str(&format!(
        "\nСтрок в `hooks.log` проекта: {}.\n\n## Случаи\n\n| id | событие | ожидалось | показано | мс |\n|---|---|---|---|---|\n",
        r.hook_log_lines
    ));
    for c in &r.cases {
        let mark = |v: &[String]| {
            if v.is_empty() {
                "—".to_string()
            } else {
                v.join(", ")
            }
        };
        s.push_str(&format!(
            "| {} | {} | {} | {} | {:.0} |\n",
            c.id,
            c.event.as_str(),
            mark(&c.expected),
            mark(&c.shown),
            c.ms
        ));
    }
    s
}

/// `eval/results/<дата>-<модель|fts>-hints[-метка].{json,md}`.
fn write(dir: &Path, r: &HintsResult) -> Result<(PathBuf, PathBuf)> {
    let mut stem = format!(
        "{}-{}-hints",
        r.info.date,
        r.info.model.as_deref().unwrap_or("fts")
    );
    if let Some(l) = &r.info.label {
        stem.push('-');
        stem.push_str(l);
    }
    fs::create_dir_all(dir)?;
    let json_path = dir.join(format!("{stem}.json"));
    let md_path = dir.join(format!("{stem}.md"));
    fs::write(&json_path, serde_json::to_string(r)? + "\n")?;
    fs::write(&md_path, render_md(r))?;
    Ok((json_path, md_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(shown: &[&str], expected: &[&str]) -> Outcome {
        Outcome {
            shown: shown.iter().map(|s| s.to_string()).collect(),
            expected: expected.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Точность учитывает подсказки на нерелевантных случаях (ложные), тишина — доля
    /// нерелевантных без подсказок; молчание на релевантном снижает полноту, не точность.
    #[test]
    fn hint_precision_counts_silence() {
        let m = hint_metrics(&[
            o(&["a"], &["a", "b"]), // релевантный: 1 из 2 ожидаемых
            o(&[], &["c"]),         // релевантный, тишина: полнота страдает
            o(&["x"], &[]),         // нерелевантный с подсказкой: ложная
            o(&[], &[]),            // нерелевантный, тишина
            o(&[], &[]),            // нерелевантный, тишина
        ]);
        assert_eq!((m.cases, m.relevant, m.irrelevant), (5, 2, 3));
        assert_eq!((m.shown, m.shown_expected, m.expected), (2, 1, 3));
        assert!((m.hint_precision.unwrap().mean - 0.5).abs() < 1e-12);
        assert!((m.hint_recall.unwrap().mean - 1.0 / 3.0).abs() < 1e-12);
        assert!((m.silence_rate.unwrap().mean - 2.0 / 3.0).abs() < 1e-12);
        assert!((m.case_hit_rate.unwrap().mean - 0.5).abs() < 1e-12);
        // полнота — относительно min(|expected|, 3): три верных из пяти ожидаемых — полный случай
        let capped = hint_metrics(&[o(&["a", "b", "c"], &["a", "b", "c", "d", "e"])]);
        assert_eq!(capped.hint_recall.unwrap().mean, 1.0);
        assert_eq!(capped.expected, 5);
        // полная тишина: точность не определена, тишина — 1
        let quiet = hint_metrics(&[o(&[], &["a"]), o(&[], &[])]);
        assert!(quiet.hint_precision.is_none());
        assert_eq!(quiet.silence_rate.unwrap().mean, 1.0);
        assert_eq!(quiet.hint_recall.unwrap().mean, 0.0);
        // интервал содержит среднее
        let p = m.hint_precision.unwrap();
        assert!(p.lo <= p.mean && p.mean <= p.hi);
    }

    #[test]
    fn prompt_pick_threshold_and_cap() {
        let c = |r: &str, s: f64| PromptCand {
            r#ref: r.into(),
            score: s,
        };
        let cands = [
            c("a", 0.9),
            c("b", 0.55),
            c("c", 0.7),
            c("d", 0.65),
            c("e", 0.61),
        ];
        assert_eq!(prompt_pick(&cands, 0.6), ["a", "c", "d"]);
        assert_eq!(prompt_pick(&cands, 0.8), ["a"]);
        assert!(prompt_pick(&cands, 0.95).is_empty());
    }

    #[test]
    fn coverage_rule_matches_server() {
        for n in 0..40 {
            for m in 0..=n {
                let ours = Coverage::CURRENT.need(n).is_some_and(|k| n > 0 && m >= k);
                assert_eq!(
                    ours,
                    zam_search::recall::coverage_enough(m, n),
                    "n={n} m={m}"
                );
            }
        }
        assert_eq!(Coverage::StrictOnly.need(5), None);
    }

    #[test]
    fn post_tool_pick_regex_strict_relaxed() {
        let cand = |r: &str, text: &str, re: Option<&str>| ErrCand {
            r#ref: r.into(),
            text: text.into(),
            regex: re.map(|p| (p.to_string(), None)).into_iter().collect(),
        };
        let cands = [
            cand(
                "busy",
                "database is locked: SQLITE_BUSY",
                Some("database is locked"),
            ),
            cand(
                "lock",
                "Blocking waiting for file lock on build directory",
                None,
            ),
            cand(
                "npm",
                "npm ERR! code ERESOLVE unable to resolve dependency tree",
                None,
            ),
        ];
        let rule = Coverage::CURRENT;
        assert_eq!(
            post_tool_pick("Error: database is locked", None, &cands, rule),
            ["busy"]
        );
        // строгий запрос: все слова ошибки есть в записи
        assert_eq!(
            post_tool_pick("Blocking waiting for file lock", None, &cands, rule),
            ["lock"]
        );
        // ослабленный: npm, err, code — 3 из 9 слов (нужно 3) — подсказка
        assert_eq!(
            post_tool_pick(
                "npm ERR! code ENOENT npm ERR! syscall open npm ERR! path package.json",
                None,
                &cands,
                rule
            ),
            ["npm"]
        );
        assert!(
            post_tool_pick(
                "npm ERR! code ENOENT npm ERR! syscall open npm ERR! path package.json",
                None,
                &cands,
                Coverage::Ceil { k: 2, lo: 2, hi: 4 }
            )
            .is_empty()
        );
        assert!(post_tool_pick("Segmentation fault", None, &cands, rule).is_empty());
    }

    #[test]
    fn refs_in_hook_text() {
        let t = "ZeroAIMemory: для **/*.sh: LF [gotcha] — i:01JABCDEFGHJKMNPQRSTVWXYZ0\n\
                 ZeroAIMemory: правило r — n:01JABCDEFGHJKMNPQRSTVWXYZ1 и снова i:01JABCDEFGHJKMNPQRSTVWXYZ0; \
                 xi:01JABCDEFGHJKMNPQRSTVWXYZ2";
        assert_eq!(
            refs_in_text(t),
            [
                ('i', "01JABCDEFGHJKMNPQRSTVWXYZ0".to_string()),
                ('n', "01JABCDEFGHJKMNPQRSTVWXYZ1".to_string())
            ]
        );
    }

    /// Набор `eval/hints.toml` проходит проверку на корпусе eval.
    #[test]
    fn hint_set_is_valid() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../eval");
        let mut corpus = Corpus::load(&root.join("corpus")).unwrap();
        corpus.insights = super::super::corpus::load_insights(&root.join("insights.toml")).unwrap();
        let (set, _) = load_hints(&root.join("hints.toml")).unwrap();
        let s = validate_hints(&set, &corpus).unwrap();
        assert!(s.cases >= MIN_CASES);
    }
}
