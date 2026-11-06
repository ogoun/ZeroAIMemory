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
use super::nuggets::{DOC_REF, Nugget};
use super::run::{self, RunSpec, Session};
use super::{Device, EvalArgs, Mode, Paths};
use crate::smoke::api_text;

pub mod mcp;

/// Не меньше стольких сценариев (набор M4).
pub const MIN_SCENARIOS: usize = 15;
/// Не меньше стольких сценариев у набора ТС-1 M6 (спецификация M6 §3.1).
pub const MIN_SCENARIOS_M6: usize = 45;
/// Виды сценариев ТС-1 M6 — слои разбиения dev/holdout (§3.1, §3.5): длинная заметка, insight,
/// вложение, три и больше источников, один источник, два источника.
pub const KINDS_M6: [&str; 6] = ["long", "insight", "attachment", "multi", "single", "pair"];
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

/// Состав набора по признакам сценария (§3.1; сценарий может считаться в нескольких).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Composition {
    /// Ожидаемый источник — длинная заметка `<язык>/long/…`.
    pub long: usize,
    /// Ожидаемый источник — insight.
    pub insight: usize,
    /// Ожидаемый источник — документ набора (`doc:`).
    pub attachment: usize,
    /// Три и больше источников.
    pub multi: usize,
    /// Один источник.
    pub single: usize,
}

/// Минимальный состав набора ТС-1 M6 (спецификация M6 §3.1, «Состав пополнения»).
pub const MIN_COMPOSITION: Composition = Composition {
    long: 8,
    insight: 6,
    attachment: 4,
    multi: 6,
    single: 4,
};

/// Признаки сценария для состава и проверки вида.
fn traits(s: &Scenario) -> Composition {
    let has = |f: &dyn Fn(&str) -> bool| usize::from(s.expect.iter().any(|e| f(e)));
    Composition {
        long: has(&|e: &str| super::corpus::is_long_path(e)),
        insight: has(&|e: &str| e.starts_with(INSIGHT_REF)),
        attachment: has(&|e: &str| e.starts_with(DOC_REF)),
        multi: usize::from(s.expect.len() >= 3),
        single: usize::from(s.expect.len() == 1),
    }
}

/// Состав набора.
pub fn composition(sc: &[Scenario]) -> Composition {
    sc.iter()
        .map(traits)
        .fold(Composition::default(), |a, t| Composition {
            long: a.long + t.long,
            insight: a.insight + t.insight,
            attachment: a.attachment + t.attachment,
            multi: a.multi + t.multi,
            single: a.single + t.single,
        })
}

/// Проверки полного набора ТС-1 M6: объём, вид каждого сценария из [`KINDS_M6`] и согласован с
/// источниками (`long` — есть длинная заметка, `insight` — insight, `attachment` — документ,
/// `multi` — ≥ 3 источников, `single` — 1, `pair` — 2 заметки), состав не меньше
/// [`MIN_COMPOSITION`].
pub fn validate_m6_set(sc: &[Scenario]) -> Result<Composition> {
    let mut errs = Vec::new();
    if sc.len() < MIN_SCENARIOS_M6 {
        errs.push(format!("сценариев {} < {MIN_SCENARIOS_M6}", sc.len()));
    }
    for s in sc {
        let t = traits(s);
        let ok = match s.kind.as_deref() {
            Some("long") => t.long == 1,
            Some("insight") => t.insight == 1,
            Some("attachment") => t.attachment == 1,
            Some("multi") => t.multi == 1,
            Some("single") => t.single == 1,
            Some("pair") => s.expect.len() == 2 && t.long + t.insight + t.attachment == 0,
            Some(k) => {
                errs.push(format!("{}: вид {k} не из {KINDS_M6:?}", s.id));
                continue;
            }
            None => {
                errs.push(format!(
                    "{}: нет вида (kind), нужен один из {KINDS_M6:?}",
                    s.id
                ));
                continue;
            }
        };
        if !ok {
            errs.push(format!(
                "{}: вид {} не согласован с источниками {:?}",
                s.id,
                s.kind(),
                s.expect
            ));
        }
    }
    let c = composition(sc);
    let m = MIN_COMPOSITION;
    for (name, have, need) in [
        ("длинные заметки", c.long, m.long),
        ("insight", c.insight, m.insight),
        ("вложения", c.attachment, m.attachment),
        ("три и больше источников", c.multi, m.multi),
        ("один источник", c.single, m.single),
    ] {
        if have < need {
            errs.push(format!("сценариев «{name}» {have} < {need}"));
        }
    }
    if !errs.is_empty() {
        bail!("набор ТС-1 M6 (§3.1):\n  {}", errs.join("\n  "));
    }
    Ok(c)
}

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

/// Проверка полного набора M4 (без документов): не меньше [`MIN_SCENARIOS`] сценариев, см.
/// [`validate_scenarios_with`].
pub fn validate_scenarios(sc: &[Scenario], corpus: &Corpus) -> Result<()> {
    validate_scenarios_with(sc, corpus, None, MIN_SCENARIOS)
}

/// Сценарии без документов-источников (`doc:`): путь M4 (REST, `eval --tokens` без `--via
/// mcp`) вложения не грузит; второе — id отброшенных.
pub fn without_docs(sc: Vec<Scenario>) -> (Vec<Scenario>, Vec<String>) {
    let (docs, rest): (Vec<Scenario>, Vec<Scenario>) = sc
        .into_iter()
        .partition(|s| s.expect.iter().any(|e| e.starts_with(DOC_REF)));
    (rest, docs.into_iter().map(|s| s.id).collect())
}

/// Проверка: объём (не меньше `min`), уникальные id, ожидаемые источники существуют (заметка
/// корпуса, действующий insight или — при `docs` — файл набора документов `doc:<путь>`).
pub fn validate_scenarios_with(
    sc: &[Scenario],
    corpus: &Corpus,
    docs: Option<&Path>,
    min: usize,
) -> Result<()> {
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
            let ok = if let Some(k) = e.strip_prefix(INSIGHT_REF) {
                corpus.insight_active(k)
            } else if let Some(rel) = e.strip_prefix(DOC_REF) {
                docs.is_some_and(|d| d.join(rel).is_file())
            } else {
                corpus.get(e).is_some()
            };
            if !ok {
                errs.push(format!(
                    "{}: {e} — нет такой заметки, действующего insight или файла набора \
                     документов (eval/docs: cargo xtask docs-corpus)",
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

/// Длина id фрагмента вложения в ссылке `a:<chunk>` (32 шестнадцатеричных знака).
const CHUNK_REF_HEX: usize = 32;

/// Ссылки фрагментов вложений `a:<chunk>` в md (карточки поиска, заголовки чтения): позиция,
/// ссылка и путь заметки-владельца — слово после ссылки без первого компонента (пространства):
/// `1. a:<chunk> knowledge/eval-docs/files-x-docx › x.docx` → `eval-docs/files-x-docx`.
pub(super) fn attachment_refs(text: &str) -> Vec<(usize, String, String)> {
    let b = text.as_bytes();
    let mut out: Vec<(usize, String, String)> = Vec::new();
    let mut i = 0;
    while i + 2 + CHUNK_REF_HEX <= b.len() {
        let prev_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        let end = i + 2 + CHUNK_REF_HEX;
        let ok = prev_ok
            && &b[i..i + 2] == b"a:"
            && b[i + 2..end]
                .iter()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
            && b.get(end).is_none_or(|c| !c.is_ascii_alphanumeric());
        if !ok {
            i += 1;
            continue;
        }
        let raw = text[i..end].to_string();
        let path = text[end..]
            .lines()
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .next()
            .and_then(|w| w.split_once('/'))
            .map(|(_, p)| p.to_string())
            .unwrap_or_default();
        if !out.iter().any(|(_, r, _)| *r == raw) {
            out.push((i, raw, path));
        }
        i = end;
    }
    out
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

/// Пометка явного фокуса (M6 §6): `[пропущено: <адрес> (<подпись>) · ~N t]`.
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
    /// Адреса пометок `[пропущено: …]` явного фокуса: `(поле элемента, значение)` — `range`
    /// (байты заметки), `locator` (часть документа) или `section` (пометка без адреса).
    Parts(Vec<(String, String)>),
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

/// Предел элементов дочитывания по одному диапазону частей документа.
const REREAD_LOCATORS_MAX: u32 = 20;

/// Адрес части документа `page:3` → `("page", 3)`.
fn numbered(loc: &str) -> Option<(&str, u32)> {
    let (k, n) = loc.split_once(':')?;
    matches!(k, "page" | "slide" | "para")
        .then(|| n.parse().ok())
        .flatten()
        .map(|n| (k, n))
}

/// Элементы дочитывания по адресу пометки: `range:S-E` → `range`; `page:3 — page:7` → `locator`
/// каждой части (не больше [`REREAD_LOCATORS_MAX`]; не нумерованные — первая и последняя);
/// `оглавление` — L2; иначе — путь секции (пометка без адреса).
fn address_parts(addr: &str) -> Vec<(String, String)> {
    if let Some(r) = addr.strip_prefix("range:") {
        return vec![("range".into(), r.to_string())];
    }
    if addr == "оглавление" {
        return Vec::new();
    }
    let (first, last) = addr.split_once(" — ").unwrap_or((addr, addr));
    if first.contains(':') {
        if let (Some((k1, a)), Some((k2, b))) = (numbered(first), numbered(last))
            && k1 == k2
            && a <= b
        {
            return (a..=b.min(a + REREAD_LOCATORS_MAX - 1))
                .map(|n| ("locator".to_string(), format!("{k1}:{n}")))
                .collect();
        }
        let mut v = vec![("locator".to_string(), first.to_string())];
        if last != first {
            v.push(("locator".to_string(), last.to_string()));
        }
        return v;
    }
    vec![("section".into(), addr.to_string())]
}

/// Адреса пометок `[пропущено: <адрес> (<подпись>) · ~N t]` в тексте (подпись — справочно).
fn skipped_parts(text: &str) -> (Vec<(String, String)>, bool) {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut whole = false;
    let mut rest = text;
    while let Some(i) = rest.find(SKIPPED_OPEN) {
        let after = &rest[i + SKIPPED_OPEN.len()..];
        let Some(end) = after.find(" t]") else {
            break;
        };
        let inner = &after[..end];
        let head = inner.rsplit_once(" · ~").map_or(inner, |(p, _)| p);
        let addr = head.split_once(" (").map_or(head, |(a, _)| a).trim();
        if !addr.is_empty() {
            let parts = address_parts(addr);
            if parts.is_empty() {
                whole = true;
            }
            for p in parts {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
        rest = &after[end..];
    }
    (out, whole)
}

/// Пометка пропуска в элементе: адреса явного фокуса важнее обрезки; пометок нет — `None`.
pub fn reread_need(element_text: &str) -> Option<Reread> {
    let (parts, whole) = skipped_parts(element_text);
    if !parts.is_empty() && !whole {
        return Some(Reread::Parts(parts));
    }
    (whole
        || element_text
            .lines()
            .any(|l| l.trim() == "(budget)" || is_cut_marker(l)))
    .then_some(Reread::Whole)
}

/// Цели дочитывания одного источника: адреса пометок или L2 целиком (`tier` по умолчанию).
pub fn reread_items(raw: &str, need: &Reread) -> Vec<Value> {
    match need {
        Reread::Parts(p) => p
            .iter()
            .map(|(k, v)| {
                let mut o = json!({ "id": raw });
                o[k.as_str()] = json!(v);
                o
            })
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
    let (sc, skipped) = without_docs(sc);
    if !skipped.is_empty() {
        println!(
            "сценарии на вложения пропущены (путь M4 документов не грузит; ТС-1 M6 — --via mcp): \
             {skipped:?}"
        );
    }
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
        let t = "### n:x a\nлист\n[пропущено: range:0-120 (начало — A > B, секций: 2) · ~40 t]\n\
                 [пропущено: range:300-420 (FAQ) · ~80 t]\n…ещё 10 токенов\n";
        let need = reread_need(t).unwrap();
        assert_eq!(
            need,
            Reread::Parts(vec![
                ("range".into(), "0-120".into()),
                ("range".into(), "300-420".into())
            ])
        );
        assert_eq!(
            reread_items("n:x", &need),
            [
                json!({"id": "n:x", "range": "0-120"}),
                json!({"id": "n:x", "range": "300-420"})
            ]
        );
        assert_eq!(reread_items("n:x", &Reread::Whole), [json!({"id": "n:x"})]);
        // документ: диапазон нумерованных частей — каждая; листы — первая и последняя
        let d = "[пропущено: page:3 — page:5 (стр. 3 — стр. 5) · ~90 t]\n\
                 [пропущено: sheet:#1!A1:B2 — sheet:#1!A9:B9 (лист) · ~9 t]\n";
        assert_eq!(
            reread_items("d:x", &reread_need(d).unwrap()),
            [
                json!({"id": "d:x", "locator": "page:3"}),
                json!({"id": "d:x", "locator": "page:4"}),
                json!({"id": "d:x", "locator": "page:5"}),
                json!({"id": "d:x", "locator": "sheet:#1!A1:B2"}),
                json!({"id": "d:x", "locator": "sheet:#1!A9:B9"}),
            ]
        );
        // часть без адреса — L2
        let w = "[пропущено: оглавление (оглавление) · ~9 t]\n";
        assert_eq!(reread_need(w), Some(Reread::Whole));
    }

    /// Markdown v2 (M6 §4.1, §4.3, §13): ссылки карточек и подсказок с новым хвостом.
    #[test]
    fn refs_in_text_parses_v2() {
        let (n, i) = ("01M5ZW9X5TEXXA5Z35ZWAXPHBK", "01M60A7Q4T7QK2M9ZWAXPHBK9C");
        let a = format!("a:{}", "0123456789abcdef".repeat(2));
        let md = format!(
            "1. n:{n} Knowledge/ru/windows/crlf-bash-scripts › Причина  [.82] r7 · 2026-10-10 \
             14:03:22 · 340t\n   Скрипт с CRLF падает в bash…\n\
             2. i:{i} insight/gotcha «Bash-скрипты должны иметь LF»  [.77] hyp · 2026-09-28 \
             08:00:01 · 120t   (seen)\n\
             3. {a} Knowledge/eval-docs/files-roadmap-pptx › roadmap.pptx · слайд 4  [1.0] \
             2026-10-10 14:03:22 · 88t\n\
             ZeroAIMemory: LF в скриптах [gotcha·hyp] i:{i}\n\
             ZeroAIMemory: правило Rules/x [th] n:{n}\n"
        );
        assert_eq!(
            refs_in_text(&md),
            [('n', n.to_string()), ('i', i.to_string())]
        );
        let r = attachment_refs(&md);
        assert_eq!(r.len(), 1);
        assert_eq!(
            (r[0].1.as_str(), r[0].2.as_str()),
            (a.as_str(), "eval-docs/files-roadmap-pptx")
        );
        // хвост `r7` и `340t` — не ссылки
        assert!(
            !refs_in_text("[.82] r7 · 2026-10-10 14:03:22 · 340t")
                .iter()
                .any(|(k, _)| *k == 'r')
        );
    }

    #[test]
    fn attachment_refs_with_owner_path() {
        let a = format!("a:{}", "0123456789abcdef".repeat(2));
        let b = format!("a:{}", "fedcba9876543210".repeat(2));
        let md = format!(
            "1. n:{n} knowledge/ru/git/x  [.8] rev1
   текст
             2. {a} knowledge/eval-docs/files-roadmap-pptx › roadmap.pptx · слайд 4  [.7]
             3. {b} knowledge/eval-docs/files-onboarding-en-docx › onboarding-en.docx
             4. {a} knowledge/eval-docs/files-roadmap-pptx › roadmap.pptx · слайд 5
             ### {b} knowledge/eval-docs/files-onboarding-en-docx › onboarding-en.docx
             a:{bad} knowledge/z
",
            n = "N".repeat(26),
            bad = "0123456789ABCDEF".repeat(2)
        );
        let r = attachment_refs(&md);
        let got: Vec<(&str, &str)> = r.iter().map(|(_, x, p)| (x.as_str(), p.as_str())).collect();
        assert_eq!(
            got,
            [
                (a.as_str(), "eval-docs/files-roadmap-pptx"),
                (b.as_str(), "eval-docs/files-onboarding-en-docx")
            ]
        );
        assert!(r[0].0 < r[1].0 && md[r[0].0..].starts_with(&a));
    }

    #[test]
    fn tok_line_parses_service_line() {
        assert_eq!(tok_line("### n:x a\nтекст\n~523 tok\n"), Some(523));
        assert_eq!(tok_line("нет строки"), None);
    }

    /// Набор документов для проверки сценариев: `eval/docs` (не в git), а если его нет (CI) —
    /// полный набор генератора с сидом по умолчанию в `target/test-tmp` (один раз).
    fn docs_for_tests() -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let eval_docs = root.join("eval").join("docs");
        if eval_docs.join("manifest.json").is_file() {
            return eval_docs;
        }
        let d = root.join("target/test-tmp/xtask/docs-full");
        if !d.join("manifest.json").is_file() {
            let p = crate::docs_corpus::Profile {
                small: false,
                large: false,
            };
            crate::docs_corpus::write(&d, crate::docs_corpus::DEFAULT_SEED, p).unwrap();
        }
        d
    }

    /// Сценарии `eval/scenarios.toml` проходят проверку на корпусе eval: путь M4 (без
    /// документов) и полный набор ТС-1 M6 — состав §3.1 и нагеты §3.2.
    #[test]
    fn scenarios_are_valid() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../eval");
        let mut corpus = Corpus::load(&root.join("corpus")).unwrap();
        corpus.insights = super::super::corpus::load_insights(&root.join("insights.toml")).unwrap();
        let (sc, _) = load_scenarios(&root.join("scenarios.toml")).unwrap();
        let docs = docs_for_tests();
        validate_scenarios_with(&sc, &corpus, Some(&docs), MIN_SCENARIOS_M6).unwrap();
        validate_m6_set(&sc).unwrap();
        let checks: Vec<super::super::nuggets::Check> = sc
            .iter()
            .map(|s| super::super::nuggets::Check {
                id: &s.id,
                question: &s.question,
                expect: &s.expect,
                nuggets: &s.nuggets,
            })
            .collect();
        let n = super::super::nuggets::validate(&checks, &corpus, &docs).unwrap();
        assert_eq!(n.scenarios_without, 0);
        let (m4, skipped) = without_docs(sc);
        assert!(!skipped.is_empty());
        validate_scenarios(&m4, &corpus).unwrap();
    }

    #[test]
    fn m6_set_kinds_and_composition() {
        let s = |id: &str, kind: Option<&str>, expect: &[&str]| Scenario {
            id: id.into(),
            kind: kind.map(String::from),
            question: "q".into(),
            expect: expect.iter().map(|e| e.to_string()).collect(),
            nuggets: Vec::new(),
        };
        let one = [
            s("a", Some("long"), &["ru/long/x", "ru/git/y"]),
            s("b", Some("insight"), &["insight:k"]),
            s("c", Some("attachment"), &["doc:files/x.docx"]),
            s("d", Some("multi"), &["ru/a/b", "ru/a/c", "ru/a/d"]),
            s("e", Some("single"), &["ru/a/b"]),
            s("f", Some("pair"), &["ru/a/b", "ru/a/c"]),
        ];
        let c = composition(&one);
        assert_eq!(
            (c.long, c.insight, c.attachment, c.multi, c.single),
            (1, 1, 1, 1, 3)
        );
        // вид не согласован с источниками, вида нет, мало сценариев и состав
        let bad = [
            s("x", Some("long"), &["ru/a/b"]),
            s("y", None, &["ru/a/b"]),
            s("z", Some("pair"), &["insight:k", "ru/a/b"]),
            s("w", Some("other"), &["ru/a/b"]),
        ];
        let e = validate_m6_set(&bad).unwrap_err().to_string();
        for want in [
            "x: вид long не согласован",
            "y: нет вида",
            "z: вид pair не согласован",
            "w: вид other не из",
            "сценариев 4 < 45",
            "«длинные заметки» 0 < 8",
            "«вложения» 0 < 4",
        ] {
            assert!(e.contains(want), "{want}: {e}");
        }
        // M4: сценарии на вложения отбрасываются
        let (rest, skipped) = without_docs(one.to_vec());
        assert_eq!((rest.len(), skipped), (5, vec!["c".to_string()]));
    }
}
