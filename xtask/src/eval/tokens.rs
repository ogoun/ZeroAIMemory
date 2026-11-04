//! Расход токенов агента (`cargo xtask eval --tokens`, ТЗ §9.3, §18.2; задача 15 M4).
//!
//! Сценарии `eval/scenarios.toml` — «вопрос агента → источники, которые нужно прочитать». Для
//! каждого — три пути, у каждого своя сессия (`X-Zam-Session`):
//! - **(а) протокол §9.3**: `POST /search` `mode=brief` (md, бюджет по умолчанию) → агент
//!   выбирает из карточек ожидаемые источники (оракул: лучший возможный выбор) → `POST /read`
//!   выбранных (md, L2, бюджет [`READ_BUDGET`] — как у наивного пути) → `POST /cache` выжимки;
//! - **(б) повтор** того же вопроса другой сессией: `POST /search` `mode=brief` — выжимка кэша
//!   первым элементом, агент дальше не идёт;
//! - **(в) «наивный» путь**: `POST /search` `mode=chunks`, `budget_tokens = 4000`, `no_cache`.
//!
//! Метрика — токены ответов сервера, посчитанные токенизатором модели (bge-m3,
//! `tokenizer.json` из `target/models`), и полнота: (а) — все ожидаемые источники нашлись в
//! карточках и прочитаны, (б) — действующая выжимка кэша первым элементом, (в) — все ожидаемые
//! источники есть в выдаче. Выжимку (`summary`) пишет агент — её токены идут отдельно (расход
//! вывода агента, не ответ сервера). Итог — базовая линия M4 для цели M6 (экономия ≥ 30 %).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::corpus::{Corpus, EVAL_SOLUTION, INSIGHT_REF, fnv1a};
use super::hints::refs_in_text;
use super::metrics::{Estimate, estimate, ratio_estimate};
use super::nuggets::Nugget;
use super::run::{self, RunSpec, Session};
use super::{Device, EvalArgs, Mode, Paths};
use crate::smoke::api_text;

pub mod mcp;

/// Не меньше стольких сценариев.
pub const MIN_SCENARIOS: usize = 15;
/// Бюджет «наивного» пути (`mode=chunks`).
pub const NAIVE_BUDGET: usize = 4000;
/// Бюджет чтения пути (а): выбранные источники читаются целиком (L2) в том же бюджете, что у
/// наивного пути, — по умолчанию `read.budget_tokens` (1500) обрезал бы уже две заметки средней
/// длины, и полнота (а) мерила бы бюджет, а не протокол **[решение исполнителя]**.
pub const READ_BUDGET: usize = NAIVE_BUDGET;
/// Предел выжимки агента, символов (≈ 300–450 токенов bge-m3; сервер принимает до 600 токенов).
const SUMMARY_MAX_CHARS: usize = 1200;
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const SESSION_HEADER: &str = "X-Zam-Session";

/// Сценарий набора.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub id: String,
    /// Вид сценария (M6: длинные заметки, insight, вложения, много источников, один источник) —
    /// слой разбиения dev/holdout; без поля — [`DEFAULT_KIND`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub question: String,
    /// Пути заметок корпуса или `insight:<key>`.
    pub expect: Vec<String>,
    /// Нагеты M6 (спецификация M6 §3.2; разметка — задача 1b).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nuggets: Vec<Nugget>,
}

/// Вид сценария без поля `kind`.
pub const DEFAULT_KIND: &str = "general";

impl Scenario {
    pub fn kind(&self) -> &str {
        self.kind.as_deref().unwrap_or(DEFAULT_KIND)
    }

    /// Нагеты источника.
    pub fn nuggets_of<'a>(&'a self, source: &'a str) -> impl Iterator<Item = &'a Nugget> + 'a {
        self.nuggets.iter().filter(move |n| n.source == source)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioFile {
    #[serde(default)]
    scenario: Vec<Scenario>,
}

/// Читает сценарии; второе — отпечаток файла.
pub fn load_scenarios(path: &Path) -> Result<(Vec<Scenario>, String)> {
    let text = fs::read_to_string(path).with_context(|| format!("чтение {}", path.display()))?;
    let f: ScenarioFile =
        toml::from_str(&text).with_context(|| format!("разбор {}", path.display()))?;
    Ok((f.scenario, fnv1a(text.into_bytes())))
}

/// Проверка полного набора: не меньше [`MIN_SCENARIOS`] сценариев, см.
/// [`validate_scenarios_with`].
pub fn validate_scenarios(sc: &[Scenario], corpus: &Corpus) -> Result<()> {
    validate_scenarios_with(sc, corpus, MIN_SCENARIOS)
}

/// Проверка: объём (не меньше `min`), уникальные id, ожидаемые источники существуют (заметка
/// корпуса с индексацией или действующий insight).
pub fn validate_scenarios_with(sc: &[Scenario], corpus: &Corpus, min: usize) -> Result<()> {
    let mut errs = Vec::new();
    let mut ids = BTreeSet::new();
    for s in sc {
        if !ids.insert(s.id.as_str()) {
            errs.push(format!("{}: id повторяется", s.id));
        }
        if s.question.trim().is_empty() || s.expect.is_empty() {
            errs.push(format!("{}: нужны question и expect", s.id));
        }
        for e in &s.expect {
            let ok = match e.strip_prefix(INSIGHT_REF) {
                Some(k) => corpus.insight_active(k),
                None => corpus.get(e).is_some(),
            };
            if !ok {
                errs.push(format!(
                    "{}: {e} — нет такой заметки или действующего insight",
                    s.id
                ));
            }
        }
    }
    if sc.len() < min {
        errs.push(format!("сценариев {} < {min}", sc.len()));
    }
    if !errs.is_empty() {
        bail!("сценарии расхода токенов:\n  {}", errs.join("\n  "));
    }
    Ok(())
}

/// Вызов пути: что вызвано и сколько токенов в ответе.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub call: String,
    pub tokens: usize,
    /// Служебная строка `~N tok` ответа (оценка сервера), если есть.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_estimate: Option<usize>,
    /// Ответ целиком (замер M6: отчёт хранит то, что видел агент).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Один путь сценария.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PathRun {
    pub steps: Vec<Step>,
    pub tokens: usize,
    pub complete: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl PathRun {
    fn push(&mut self, call: &str, text: &str, counter: &Counter) {
        self.push_step(call, text, counter, false);
    }

    /// Вызов пути; `keep` — сохранить ответ целиком ([`Step::text`]).
    pub fn push_step(&mut self, call: &str, text: &str, counter: &Counter, keep: bool) {
        self.steps.push(Step {
            call: call.into(),
            tokens: counter.count(text),
            server_estimate: tok_line(text),
            text: keep.then(|| text.to_string()),
        });
        self.tokens = scenario_tokens_sum(&self.steps);
    }

    /// Все ответы пути одним текстом (для поиска нагетов).
    pub fn delivered(&self) -> String {
        self.steps
            .iter()
            .filter_map(|s| s.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Токены пути — сумма токенов ответов его вызовов.
pub fn scenario_tokens_sum(steps: &[Step]) -> usize {
    steps.iter().map(|s| s.tokens).sum()
}

/// Итог сценария.
#[derive(Debug, Clone, Serialize)]
pub struct ScenarioRun {
    pub id: String,
    pub question: String,
    pub expect: Vec<String>,
    /// Ожидаемые, найденные в карточках (а) и прочитанные.
    pub selected: Vec<String>,
    /// Токены выжимки агента (а, `summary` в `POST /cache`) — вывод агента, не ответ сервера.
    pub summary_tokens: usize,
    pub protocol: PathRun,
    pub cached: PathRun,
    pub naive: PathRun,
}

/// Сводка пути по сценариям.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PathSummary {
    pub scenarios: usize,
    pub tokens_total: usize,
    pub tokens_mean: Option<Estimate>,
    pub complete: Option<Estimate>,
}

/// Сводка прогона.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TokenSummary {
    pub protocol: PathSummary,
    pub cached: PathSummary,
    pub naive: PathSummary,
    /// `Σ протокол / Σ наивный` и `Σ повтор / Σ наивный` по всем сценариям.
    pub protocol_to_naive: Option<f64>,
    pub cached_to_naive: Option<f64>,
    /// То же только по сценариям, где оба пути полны.
    pub protocol_to_naive_complete: Option<f64>,
    /// Отношения `Σ (а) / Σ (в)` и `Σ (б) / Σ (в)` с 95 % интервалом бутстрепа сценариев.
    pub protocol_to_naive_ci: Option<Estimate>,
    pub cached_to_naive_ci: Option<Estimate>,
    /// Сценариев, где путь (а) дешевле наивного.
    pub protocol_cheaper: usize,
    pub summary_tokens_total: usize,
}

fn path_summary(runs: &[&PathRun]) -> PathSummary {
    let tokens: Vec<f64> = runs.iter().map(|r| r.tokens as f64).collect();
    let complete: Vec<f64> = runs
        .iter()
        .map(|r| if r.complete { 1.0 } else { 0.0 })
        .collect();
    PathSummary {
        scenarios: runs.len(),
        tokens_total: runs.iter().map(|r| r.tokens).sum(),
        tokens_mean: (!tokens.is_empty()).then(|| estimate(&tokens)),
        complete: (!complete.is_empty()).then(|| estimate(&complete)),
    }
}

fn ratio(a: usize, b: usize) -> Option<f64> {
    (b > 0).then(|| a as f64 / b as f64)
}

/// Сводка по сценариям: суммы и средние токенов путей, полнота, отношения к наивному пути.
pub fn summarize(runs: &[ScenarioRun]) -> TokenSummary {
    let col = |f: fn(&ScenarioRun) -> &PathRun| runs.iter().map(f).collect::<Vec<_>>();
    let protocol = path_summary(&col(|r| &r.protocol));
    let cached = path_summary(&col(|r| &r.cached));
    let naive = path_summary(&col(|r| &r.naive));
    let both: Vec<&ScenarioRun> = runs
        .iter()
        .filter(|r| r.protocol.complete && r.naive.complete)
        .collect();
    TokenSummary {
        protocol_to_naive: ratio(protocol.tokens_total, naive.tokens_total),
        cached_to_naive: ratio(cached.tokens_total, naive.tokens_total),
        protocol_to_naive_complete: ratio(
            both.iter().map(|r| r.protocol.tokens).sum(),
            both.iter().map(|r| r.naive.tokens).sum(),
        ),
        protocol_to_naive_ci: ratio_estimate(
            &runs
                .iter()
                .map(|r| r.protocol.tokens as f64)
                .collect::<Vec<_>>(),
            &runs
                .iter()
                .map(|r| r.naive.tokens as f64)
                .collect::<Vec<_>>(),
        ),
        cached_to_naive_ci: ratio_estimate(
            &runs
                .iter()
                .map(|r| r.cached.tokens as f64)
                .collect::<Vec<_>>(),
            &runs
                .iter()
                .map(|r| r.naive.tokens as f64)
                .collect::<Vec<_>>(),
        ),
        protocol_cheaper: runs
            .iter()
            .filter(|r| r.protocol.tokens < r.naive.tokens)
            .count(),
        summary_tokens_total: runs.iter().map(|r| r.summary_tokens).sum(),
        protocol,
        cached,
        naive,
    }
}

/// Последняя строка `~N tok` ответа.
pub(super) fn tok_line(text: &str) -> Option<usize> {
    text.lines().rev().find_map(|l| {
        l.trim()
            .strip_prefix('~')?
            .strip_suffix(" tok")?
            .trim()
            .parse()
            .ok()
    })
}

/// Счётчик токенов модели (bge-m3).
pub struct Counter(tokenizers::Tokenizer);

impl Counter {
    /// `tokenizer.json` bge-m3 из `target/models` (`tokenizers/bge-m3-int8` или каталог модели).
    pub fn load(repo: &Path) -> Result<Counter> {
        let models = repo.join("target").join("models");
        let candidates = [
            models
                .join("tokenizers")
                .join("bge-m3-int8")
                .join("tokenizer.json"),
            models.join("bge-m3-int8").join("tokenizer.json"),
            models.join("bge-m3-fp16").join("tokenizer.json"),
        ];
        let path = candidates
            .iter()
            .find(|p| p.is_file())
            .context("нет tokenizer.json bge-m3 в target/models (cargo xtask fetch-models)")?;
        let t = tokenizers::Tokenizer::from_file(path)
            .map_err(|e| anyhow!("{}: {e}", path.display()))?;
        Ok(Counter(t))
    }

    pub fn count(&self, text: &str) -> usize {
        self.0
            .encode(text, false)
            .map(|e| e.len())
            .unwrap_or_else(|_| text.len().div_ceil(4))
    }
}

/// Клиент прогона: запросы с сессией.
struct Client<'a> {
    s: &'a Session,
}

impl Client<'_> {
    fn post(&self, path: &str, body: &Value, session: &str) -> Result<String> {
        let (st, text) = api_text(
            &self.s.base,
            self.s.token(),
            path,
            Some(body),
            &[(SESSION_HEADER, session)],
            CALL_TIMEOUT,
        )?;
        if !(200..300).contains(&st) {
            bail!("POST {path}: HTTP {st}: {text}");
        }
        Ok(text)
    }
}

/// Ссылки ответа md → ссылки набора: путь заметки, `insight:<key>`; второе — исходные
/// `n:<id>` / `i:<id>` по порядку.
pub(super) fn mapped_refs(s: &Session, text: &str) -> Vec<(String, String)> {
    refs_in_text(text)
        .into_iter()
        .filter_map(|(k, id)| {
            let raw = format!("{k}:{id}");
            let r = match k {
                'n' => s.node_paths().get(&id).cloned()?,
                'i' | 'e' => format!("{INSIGHT_REF}{}", s.entry_keys().get(&id)?),
                _ => return None,
            };
            Some((r, raw))
        })
        .collect()
}

/// Выжимка «агента»: начало прочитанного текста без заголовков элементов и служебной строки.
pub(super) fn summary_of(question: &str, read_md: &str) -> String {
    let body: String = read_md
        .lines()
        .filter(|l| !l.starts_with("### ") && tok_line(l).is_none())
        .collect::<Vec<_>>()
        .join("\n");
    let mut s = format!("Выжимка: {question}\n\n");
    s.extend(body.chars().take(SUMMARY_MAX_CHARS));
    s
}

fn run_scenario(s: &Session, c: &Client, counter: &Counter, sc: &Scenario) -> Result<ScenarioRun> {
    let tag = format!("tok-{}-{}", run::today(), sc.id);
    let scope = json!({ "solution": EVAL_SOLUTION });
    let brief = json!({ "query": sc.question, "scope": scope, "mode": "brief", "format": "md" });
    // (а) протокол §9.3
    let mut protocol = PathRun::default();
    let sa = format!("{tag}-a");
    let cards = c.post("/api/v1/search", &brief, &sa)?;
    protocol.push("search brief", &cards, counter);
    let mut selected: Vec<(String, String)> = Vec::new();
    for (r, raw) in mapped_refs(s, &cards) {
        if sc.expect.contains(&r) && !selected.iter().any(|(x, _)| *x == r) {
            selected.push((r, raw));
        }
    }
    let mut summary_tokens = 0;
    if selected.is_empty() {
        protocol.note = "ни одного ожидаемого источника в карточках".into();
    } else {
        let items: Vec<Value> = selected
            .iter()
            .map(|(_, raw)| json!({ "id": raw }))
            .collect();
        let read = c.post(
            "/api/v1/read",
            &json!({ "items": items, "format": "md", "budget_tokens": READ_BUDGET }),
            &sa,
        )?;
        protocol.push("read", &read, counter);
        let truncated = read.contains("…ещё ") || read.contains("(budget)");
        let summary = summary_of(&sc.question, &read);
        summary_tokens = counter.count(&summary);
        let sources: Vec<Value> = selected
            .iter()
            .map(|(_, raw)| json!({ "id": raw }))
            .collect();
        let put = c.post(
            "/api/v1/cache",
            &json!({ "query": sc.question, "summary": summary, "sources": sources,
                     "solution": EVAL_SOLUTION }),
            &sa,
        )?;
        protocol.push("cache put", &put, counter);
        let missing: Vec<&String> = sc
            .expect
            .iter()
            .filter(|e| !selected.iter().any(|(r, _)| r == *e))
            .collect();
        protocol.complete = missing.is_empty() && !truncated;
        let mut notes = Vec::new();
        if !missing.is_empty() {
            notes.push(format!("нет в карточках: {missing:?}"));
        }
        if truncated {
            notes.push("чтение обрезано бюджетом".to_string());
        }
        protocol.note = notes.join("; ");
    }
    // (б) повтор другой сессией: выжимка кэша первым элементом
    let mut cached = PathRun::default();
    let again = c.post("/api/v1/search", &brief, &format!("{tag}-b"))?;
    cached.push("search brief (кэш)", &again, counter);
    let first = again
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default();
    let hit = first.contains("[cache]");
    cached.complete = protocol.complete && hit;
    if !hit {
        cached.note = format!(
            "первая строка: {}",
            first.chars().take(80).collect::<String>()
        );
    }
    // (в) наивный путь: листы с бюджетом 4000, без кэша
    let mut naive = PathRun::default();
    let chunks = c.post(
        "/api/v1/search",
        &json!({ "query": sc.question, "scope": scope, "mode": "chunks",
                 "budget_tokens": NAIVE_BUDGET, "no_cache": true, "format": "md" }),
        &format!("{tag}-c"),
    )?;
    naive.push("search chunks 4000", &chunks, counter);
    let found: BTreeSet<String> = mapped_refs(s, &chunks)
        .into_iter()
        .map(|(r, _)| r)
        .collect();
    let missing: Vec<&String> = sc
        .expect
        .iter()
        .filter(|e| !found.contains(*e) && !chunks.contains(e.as_str()))
        .collect();
    naive.complete = missing.is_empty();
    if !missing.is_empty() {
        naive.note = format!("нет в выдаче: {missing:?}");
    }
    Ok(ScenarioRun {
        id: sc.id.clone(),
        question: sc.question.clone(),
        expect: sc.expect.clone(),
        selected: selected.into_iter().map(|(r, _)| r).collect(),
        summary_tokens,
        protocol,
        cached,
        naive,
    })
}

/// Пометка явного фокуса (M6 §6): `[пропущено: <путь заголовков> · ~N t]`.
const SKIPPED_OPEN: &str = "[пропущено: ";

/// Элемент ответа `memory_read` в Markdown: ссылка цели и текст (заголовок `### <ref> …` и всё
/// до следующего заголовка элемента).
#[derive(Debug, Clone, PartialEq)]
pub struct ReadElement {
    pub raw: String,
    pub text: String,
}

/// Строка — заголовок элемента одной из `raws` (`### n:<id> …`)?
fn element_head<'a>(line: &str, raws: &'a [String]) -> Option<&'a String> {
    let rest = line.strip_prefix("### ")?;
    raws.iter().find(|r| {
        rest.strip_prefix(r.as_str())
            .is_some_and(|t| t.is_empty() || t.starts_with(' '))
    })
}

/// Разбор ответа `memory_read` (md) на элементы запрошенных целей `raws` (строки до первого
/// заголовка элемента — не элемент). Заголовки внутри текста заметки не начинаются со ссылки и
/// элемент не разрывают.
pub fn read_elements(md: &str, raws: &[String]) -> Vec<ReadElement> {
    let mut out: Vec<ReadElement> = Vec::new();
    for line in md.lines() {
        if let Some(r) = element_head(line, raws) {
            out.push(ReadElement {
                raw: r.clone(),
                text: String::new(),
            });
        }
        if let Some(e) = out.last_mut() {
            e.text.push_str(line);
            e.text.push('\n');
        }
    }
    out
}

/// Что дочитать по пометке пропуска элемента (спецификация M6 §3.4, п. 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reread {
    /// Названные пометками `[пропущено: …]` секции.
    Sections(Vec<String>),
    /// L2 источника целиком (обрезка `…ещё N токенов` или `(budget)`).
    Whole,
}

/// `…ещё N токенов` — пометка обрезки элемента `/read`.
fn is_cut_marker(line: &str) -> bool {
    line.trim()
        .strip_prefix("…ещё ")
        .and_then(|r| r.strip_suffix(" токенов"))
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// Пути секций пометок `[пропущено: <путь> · ~N t]` в тексте.
fn skipped_sections(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find(SKIPPED_OPEN) {
        let after = &rest[i + SKIPPED_OPEN.len()..];
        let Some(end) = after.find(']') else {
            break;
        };
        let inner = &after[..end];
        let path = inner.rsplit_once(" · ~").map_or(inner, |(p, _)| p).trim();
        if !path.is_empty() && !out.iter().any(|p| p == path) {
            out.push(path.to_string());
        }
        rest = &after[end..];
    }
    out
}

/// Пометка пропуска в элементе: секции явного фокуса важнее обрезки; пометок нет — `None`.
pub fn reread_need(element_text: &str) -> Option<Reread> {
    let sections = skipped_sections(element_text);
    if !sections.is_empty() {
        return Some(Reread::Sections(sections));
    }
    element_text
        .lines()
        .any(|l| l.trim() == "(budget)" || is_cut_marker(l))
        .then_some(Reread::Whole)
}

/// Цели дочитывания одного источника: секции из пометки или L2 целиком (`tier` по умолчанию).
pub fn reread_items(raw: &str, need: &Reread) -> Vec<Value> {
    match need {
        Reread::Sections(s) => s
            .iter()
            .map(|p| json!({ "id": raw, "section": p }))
            .collect(),
        Reread::Whole => vec![json!({ "id": raw })],
    }
}

/// Сведения о прогоне.
#[derive(Debug, Clone, Serialize)]
pub struct TokensInfo {
    pub date: String,
    pub git: String,
    pub label: Option<String>,
    pub model: Option<String>,
    pub model_id: Option<String>,
    pub device: Option<String>,
    pub sets: std::collections::BTreeMap<String, String>,
    pub corpus_fingerprint: String,
    pub scenarios_fingerprint: String,
    pub counter: String,
}

/// Итог `eval --tokens`.
#[derive(Debug, Clone, Serialize)]
pub struct TokensResult {
    pub info: TokensInfo,
    pub summary: TokenSummary,
    pub scenarios: Vec<ScenarioRun>,
}

/// `cargo xtask eval --tokens`.
pub fn run(a: &EvalArgs, p: &Paths, corpus: &Corpus) -> Result<()> {
    let (sc, fingerprint) = load_scenarios(&p.scenarios)?;
    validate_scenarios(&sc, corpus)?;
    let counter = Counter::load(&p.repo)?;
    let server = run::server_dir(a, p)?;
    let only_fts = a.modes.iter().all(|m| *m == Mode::Fts);
    let model = (!only_fts).then(|| {
        a.model
            .clone()
            .unwrap_or_else(|| run::default_model(a.device).to_string())
    });
    let label = match &a.label {
        Some(l) => format!("tokens-{l}"),
        None => "tokens".to_string(),
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
    // каталог всегда свежий: кэш выжимок прошлого прогона исказил бы путь (а)
    let mut s = Session::start(p, &spec, &server, true)?;
    s.check_model()?;
    s.load_corpus(corpus)?;
    if model.is_some() {
        s.wait_indexed()?;
    }
    let client = Client { s: &s };
    let mut runs = Vec::with_capacity(sc.len());
    for x in &sc {
        runs.push(run_scenario(&s, &client, &counter, x)?);
    }
    let st = &s.status;
    let r = TokensResult {
        info: TokensInfo {
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
            corpus_fingerprint: corpus.fingerprint(),
            scenarios_fingerprint: fingerprint,
            counter: "bge-m3 tokenizer.json".into(),
        },
        summary: summarize(&runs),
        scenarios: runs,
    };
    s.stop()?;
    let (j, m) = write(&p.results, &r)?;
    print!("{}", render_md(&r));
    println!("  {}\n  {}", j.display(), m.display());
    Ok(())
}

fn est(e: &Option<Estimate>, digits: usize) -> String {
    match e {
        Some(e) => format!("{:.d$} [{:.d$}…{:.d$}]", e.mean, e.lo, e.hi, d = digits),
        None => "—".into(),
    }
}

/// Markdown-отчёт.
pub fn render_md(r: &TokensResult) -> String {
    let i = &r.info;
    let mut s = format!(
        "# Расход токенов: {} {} ({})\n\nКоммит `{}`, модель `{}`, отпечаток корпуса `{}`, сценариев `{}`; \
         токены — {}.\n\n",
        i.model.as_deref().unwrap_or("fts"),
        i.device.as_deref().unwrap_or("—"),
        i.date,
        i.git,
        i.model_id.as_deref().unwrap_or("—"),
        i.corpus_fingerprint,
        i.scenarios_fingerprint,
        i.counter
    );
    let u = &r.summary;
    s.push_str("| Путь | сценариев | токенов всего | в среднем на сценарий | полнота |\n|---|---|---|---|---|\n");
    for (name, p) in [
        ("(а) протокол §9.3: brief → read → cache put", &u.protocol),
        ("(б) повтор вопроса: кэш", &u.cached),
        ("(в) наивный: chunks, бюджет 4000", &u.naive),
    ] {
        s.push_str(&format!(
            "| {name} | {} | {} | {} | {} |\n",
            p.scenarios,
            p.tokens_total,
            est(&p.tokens_mean, 0),
            est(&p.complete, 3)
        ));
    }
    let pct = |x: Option<f64>| x.map_or("—".into(), |v| format!("{:.1} %", v * 100.0));
    let ci = |e: &Option<Estimate>| {
        e.map_or("—".into(), |e| {
            format!("{:.1}…{:.1} %", e.lo * 100.0, e.hi * 100.0)
        })
    };
    s.push_str(&format!(
        "\n(а) / (в): {} (95 % бутстреп сценариев: {}); по сценариям, где оба пути полны: {}; \
         (а) дешевле (в) в {} из {} сценариев; (б) / (в): {} ({}). Выжимки агента \
         (вывод, не ответ сервера): {} токенов всего.\n\n",
        pct(u.protocol_to_naive),
        ci(&u.protocol_to_naive_ci),
        pct(u.protocol_to_naive_complete),
        u.protocol_cheaper,
        r.scenarios.len(),
        pct(u.cached_to_naive),
        ci(&u.cached_to_naive_ci),
        u.summary_tokens_total
    ));
    s.push_str("| id | (а) токены | полно | (б) | полно | (в) | полно | примечание |\n|---|---|---|---|---|---|---|---|\n");
    let yes = |b: bool| if b { "да" } else { "нет" };
    for x in &r.scenarios {
        let note: Vec<&str> = [&x.protocol.note, &x.cached.note, &x.naive.note]
            .into_iter()
            .filter(|n| !n.is_empty())
            .map(String::as_str)
            .collect();
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
            x.id,
            x.protocol.tokens,
            yes(x.protocol.complete),
            x.cached.tokens,
            yes(x.cached.complete),
            x.naive.tokens,
            yes(x.naive.complete),
            note.join("; ")
        ));
    }
    s
}

fn write(dir: &Path, r: &TokensResult) -> Result<(PathBuf, PathBuf)> {
    let mut stem = format!(
        "{}-{}-tokens",
        r.info.date,
        r.info.model.as_deref().unwrap_or("fts")
    );
    if let Some(l) = &r.info.label {
        stem.push('-');
        stem.push_str(l);
    }
    fs::create_dir_all(dir)?;
    let j = dir.join(format!("{stem}.json"));
    let m = dir.join(format!("{stem}.md"));
    fs::write(&j, serde_json::to_string(r)? + "\n")?;
    fs::write(&m, render_md(r))?;
    Ok((j, m))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(t: usize) -> Step {
        Step {
            call: "x".into(),
            tokens: t,
            server_estimate: None,
            text: None,
        }
    }

    fn path(tokens: &[usize], complete: bool) -> PathRun {
        let steps: Vec<Step> = tokens.iter().map(|t| step(*t)).collect();
        PathRun {
            tokens: super::scenario_tokens_sum(&steps),
            steps,
            complete,
            note: String::new(),
        }
    }

    fn sc(id: &str, a: PathRun, b: PathRun, c: PathRun) -> ScenarioRun {
        ScenarioRun {
            id: id.into(),
            question: String::new(),
            expect: Vec::new(),
            selected: Vec::new(),
            summary_tokens: 10,
            protocol: a,
            cached: b,
            naive: c,
        }
    }

    /// Токены пути — сумма ответов его вызовов; сводка — суммы по сценариям и отношения к
    /// наивному пути (в том числе только по сценариям, где оба пути полны).
    #[test]
    fn scenario_tokens_sum() {
        assert_eq!(
            super::scenario_tokens_sum(&[step(300), step(900), step(40)]),
            1240
        );
        assert_eq!(super::scenario_tokens_sum(&[]), 0);
        let runs = [
            sc(
                "s1",
                path(&[300, 900, 40], true),
                path(&[350], true),
                path(&[3900], true),
            ),
            sc(
                "s2",
                path(&[300], false),
                path(&[310], false),
                path(&[3800], true),
            ),
        ];
        let u = summarize(&runs);
        assert_eq!(u.protocol.tokens_total, 1540);
        assert_eq!(u.cached.tokens_total, 660);
        assert_eq!(u.naive.tokens_total, 7700);
        assert!((u.protocol_to_naive.unwrap() - 1540.0 / 7700.0).abs() < 1e-12);
        assert!((u.protocol_to_naive_complete.unwrap() - 1240.0 / 3900.0).abs() < 1e-12);
        assert!((u.protocol.complete.unwrap().mean - 0.5).abs() < 1e-12);
        assert_eq!(u.summary_tokens_total, 20);
        assert_eq!(u.protocol_cheaper, 2);
        let ci = u.protocol_to_naive_ci.unwrap();
        assert!((ci.mean - 1540.0 / 7700.0).abs() < 1e-12 && ci.lo <= ci.mean && ci.mean <= ci.hi);
        assert!((u.protocol.tokens_mean.unwrap().mean - 770.0).abs() < 1e-12);
    }

    /// Элементы `memory_read` разбираются по заголовкам запрошенных целей; заголовок внутри
    /// текста заметки элемент не рвёт.
    #[test]
    fn read_elements_split_by_requested_refs() {
        let a = format!("n:{}", "A".repeat(26));
        let b = format!("n:{}", "B".repeat(26));
        let md = format!(
            "### {a} knowledge/x  rev1 · ~900 tok\nначало\n### Раздел заметки\nтекст\n…ещё 500 токенов\n\n\
             ### {b} knowledge/y  rev2 · ~300 tok\n(budget)\n\n~1200 tok\n"
        );
        let raws = vec![a.clone(), b.clone()];
        let el = read_elements(&md, &raws);
        assert_eq!(el.len(), 2);
        assert_eq!(el[0].raw, a);
        assert!(el[0].text.contains("### Раздел заметки") && el[0].text.contains("…ещё 500"));
        assert_eq!(reread_need(&el[0].text), Some(Reread::Whole));
        assert_eq!(reread_need(&el[1].text), Some(Reread::Whole));
        assert_eq!(reread_need("### n:x\nполный текст\n"), None);
        // «ещё» в тексте заметки — не пометка
        assert_eq!(reread_need("осталось …ещё много токенов\n"), None);
    }

    /// Пометки явного фокуса называют секции: дочитываются они, а не L2 целиком.
    #[test]
    fn reread_prefers_named_sections() {
        let t = "### n:x a\nлист\n[пропущено: Установка › Linux · ~420 t]\n[пропущено: FAQ · ~80 t]\n…ещё 10 токенов\n";
        let need = reread_need(t).unwrap();
        assert_eq!(
            need,
            Reread::Sections(vec!["Установка › Linux".into(), "FAQ".into()])
        );
        assert_eq!(
            reread_items("n:x", &need),
            [
                json!({"id": "n:x", "section": "Установка › Linux"}),
                json!({"id": "n:x", "section": "FAQ"})
            ]
        );
        assert_eq!(reread_items("n:x", &Reread::Whole), [json!({"id": "n:x"})]);
    }

    #[test]
    fn tok_line_parses_service_line() {
        assert_eq!(tok_line("### n:x a\nтекст\n~523 tok\n"), Some(523));
        assert_eq!(tok_line("нет строки"), None);
    }

    /// Сценарии `eval/scenarios.toml` проходят проверку на корпусе eval.
    #[test]
    fn scenarios_are_valid() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../eval");
        let mut corpus = Corpus::load(&root.join("corpus")).unwrap();
        corpus.insights = super::super::corpus::load_insights(&root.join("insights.toml")).unwrap();
        let (sc, _) = load_scenarios(&root.join("scenarios.toml")).unwrap();
        validate_scenarios(&sc, &corpus).unwrap();
    }
}
