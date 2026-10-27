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
use super::metrics::{Estimate, estimate};
use super::run::{self, RunSpec, Session};
use super::{Device, EvalArgs, Mode, Paths};
use crate::smoke::api_text;

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
    pub question: String,
    /// Пути заметок корпуса или `insight:<key>`.
    pub expect: Vec<String>,
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

/// Проверка: объём, уникальные id, ожидаемые источники существуют (заметка корпуса с
/// индексацией или действующий insight).
pub fn validate_scenarios(sc: &[Scenario], corpus: &Corpus) -> Result<()> {
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
    if sc.len() < MIN_SCENARIOS {
        errs.push(format!("сценариев {} < {MIN_SCENARIOS}", sc.len()));
    }
    if !errs.is_empty() {
        bail!("сценарии расхода токенов:\n  {}", errs.join("\n  "));
    }
    Ok(())
}

/// Вызов пути: что вызвано и сколько токенов в ответе.
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub call: String,
    pub tokens: usize,
    /// Служебная строка `~N tok` ответа (оценка сервера), если есть.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_estimate: Option<usize>,
}

/// Один путь сценария.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PathRun {
    pub steps: Vec<Step>,
    pub tokens: usize,
    pub complete: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl PathRun {
    fn push(&mut self, call: &str, text: &str, counter: &Counter) {
        self.steps.push(Step {
            call: call.into(),
            tokens: counter.count(text),
            server_estimate: tok_line(text),
        });
        self.tokens = scenario_tokens_sum(&self.steps);
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
        summary_tokens_total: runs.iter().map(|r| r.summary_tokens).sum(),
        protocol,
        cached,
        naive,
    }
}

/// Последняя строка `~N tok` ответа.
fn tok_line(text: &str) -> Option<usize> {
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
fn mapped_refs(s: &Session, text: &str) -> Vec<(String, String)> {
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
fn summary_of(question: &str, read_md: &str) -> String {
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
    s.push_str(&format!(
        "\n(а) / (в): {}; по сценариям, где оба пути полны: {}; (б) / (в): {}. Выжимки агента \
         (вывод, не ответ сервера): {} токенов всего.\n\n",
        pct(u.protocol_to_naive),
        pct(u.protocol_to_naive_complete),
        pct(u.cached_to_naive),
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
        assert!((u.protocol.tokens_mean.unwrap().mean - 770.0).abs() < 1e-12);
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
