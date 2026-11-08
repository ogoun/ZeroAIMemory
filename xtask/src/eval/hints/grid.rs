//! Сетки порогов подсказок M6 §9 (`cargo xtask eval --hints --grid <файл>`, задача 9).
//!
//! Файл сетки: `rule` — правило выбора и событие случаев, `[[point]] name, env = { ZAM_… }`
//! (формат точек — [`crate::eval::grid::Point`]); первая точка — база (текущие умолчания):
//! - `rule = "post_tool_coverage"` — `recall.post_tool_coverage` (делитель × `cap` × правило
//!   совпадения слов) на случаях `post_tool`; правило §9: (1) Δ `hint_recall` — нижняя граница
//!   ≥ −0,02; (2) среди прошедших — наибольшая Δ `hint_precision` с нижней границей > 0 (при
//!   равенстве — точка раньше в файле: «как есть» и `plain` идут первыми); (3) иначе база;
//! - `rule = "prompt_min_score_fts"` — порог FTS-only на случаях `prompt` (только `--mode fts`):
//!   наибольшее значение, при котором `hint_recall` ≥ 0,5 и `silence_rate` ≥ 0,7; иначе 0,6.
//!
//! Методика (план M6, «Методика замера при недетерминизме», правила 1–2): корпус загружается и
//! индексируется **один раз**, каждая точка — свежая копия каталога и сервер с `ZAM_*` точки;
//! первая точка снимается дважды (A/A-гейт): FTS обязан совпасть побайтово, у гибрида
//! расхождения A/A — порог шума в отчёте. Интервалы — парный бутстреп случаев, 10 000 выборок,
//! фиксированное зерно, односторонние 95 % границы (§3.5). Отчёт — по корзинам длины ошибки
//! ([`super::LEN_BUCKETS`]).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::{
    CaseRun, Deltas, Event, HintMetrics, LEN_BUCKETS, Outcome, RefMap, SOURCES, api_case,
    capped_hit, case_run, hint_metrics, len_bucket, load_hints, load_rules, recall_cap,
    validate_hints, with_hint_insights,
};
use crate::eval::corpus::Corpus;
use crate::eval::grid::{Point, validate_points};
use crate::eval::metrics::{
    BOOTSTRAP_M6, Estimate, SEED, paired_diff_weighted, paired_ratio_diff_one_sided, paired_ratio_q,
};
use crate::eval::run::{self, RunSpec, Session};
use crate::eval::{Device, EvalArgs, Mode, Paths, caps};

/// Порог неинфериорности Δ `hint_recall` (нижняя граница, §9 п. 1).
pub const RECALL_MARGIN: f64 = -0.02;
/// Условия порога FTS-only (§9): `hint_recall` и `silence_rate` не ниже.
pub const FTS_MIN_RECALL: f64 = 0.5;
pub const FTS_MIN_SILENCE: f64 = 0.7;
/// Значение порога FTS-only, если ни одна точка не прошла (§9).
pub const FTS_FALLBACK: f64 = 0.6;
/// Ключ порога FTS-only в точках.
const FTS_ENV: &str = "ZAM_RECALL__PROMPT_MIN_SCORE_FTS";
/// Префикс ключей правила покрытия в точках.
const COVERAGE_ENV: &str = "ZAM_RECALL__POST_TOOL_COVERAGE__";

/// Правило выбора сетки (§9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GridRule {
    PostToolCoverage,
    PromptMinScoreFts,
}

impl GridRule {
    /// Событие случаев правила.
    pub fn event(self) -> Event {
        match self {
            GridRule::PostToolCoverage => Event::PostTool,
            GridRule::PromptMinScoreFts => Event::Prompt,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GridFile {
    rule: GridRule,
    #[serde(default)]
    point: Vec<Point>,
}

/// Читает и проверяет сетку: точки ([`validate_points`]) и ключи правила (покрытие — только
/// `ZAM_RECALL__POST_TOOL_COVERAGE__*`; порог FTS-only — число 0..=1 в каждой точке).
pub fn load(path: &Path) -> Result<(GridRule, Vec<Point>)> {
    let text =
        fs::read_to_string(path).with_context(|| format!("чтение сетки {}", path.display()))?;
    let f: GridFile =
        toml::from_str(&text).with_context(|| format!("разбор сетки {}", path.display()))?;
    validate_points(&f.point, path)?;
    let mut errs = Vec::new();
    for p in &f.point {
        let env: Vec<(String, String)> =
            p.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        if let Err(e) = caps::check_known(&env) {
            errs.push(format!("{}: {e}", p.name));
        }
        match f.rule {
            GridRule::PostToolCoverage => {
                for k in p.env.keys().filter(|k| !k.starts_with(COVERAGE_ENV)) {
                    errs.push(format!(
                        "{}: {k} — у сетки покрытия только {COVERAGE_ENV}*",
                        p.name
                    ));
                }
            }
            GridRule::PromptMinScoreFts => {
                let ok = p.env.len() == 1 && fts_value(p).is_some_and(|v| (0.0..=1.0).contains(&v));
                if !ok {
                    errs.push(format!("{}: нужен ровно {FTS_ENV} в 0..=1", p.name));
                }
            }
        }
    }
    if !errs.is_empty() {
        bail!("сетка {}:\n  {}", path.display(), errs.join("\n  "));
    }
    Ok((f.rule, f.point))
}

/// Значение порога FTS-only точки.
fn fts_value(p: &Point) -> Option<f64> {
    p.env.get(FTS_ENV).and_then(|v| v.trim().parse().ok())
}

/// Происхождения случаев, по которым принимается решение (ревью задачи 9, I-1): старые случаи
/// M4 и оба независимых набора (`x-*`, `y-*`); случаи исполнителя задачи 9 (`task9`) — только
/// справочно.
pub const DECISION_SOURCES: [&str; 3] = ["m4", "independent", "independent_b"];
/// Части, в каждой из которых точность не должна падать (п. 2): старые M4 и независимые x + y.
pub const STABLE_PARTS: [&str; 2] = ["m4", "independent_all"];
/// Допуск дискретности п. 1 (ревью задачи 9, I-1): при `N` релевантных случаях одна потерянная
/// ожидаемая подсказка — шаг `1/N` (при 31 — 0,032 > 0,02), и граница бутстрепа ниже −0,02 при
/// любой потере: правило вырождается в «ни одной потери». Поэтому п. 1 выполнен и при чистой
/// потере не больше [`LOSS_TOLERANCE`] ожидаемой подсказки, если `N ≥`
/// [`LOSS_TOLERANCE_MIN_N`] (не больше 4 % полноты).
pub const LOSS_TOLERANCE: f64 = 1.0;
pub const LOSS_TOLERANCE_MIN_N: usize = 25;

/// Метрики части набора.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PartResult {
    pub cases: usize,
    pub metrics: HintMetrics,
    /// Парные разности с базой на тех же случаях.
    pub delta: Deltas,
    /// Чистая потеря ожидаемых подсказок к базе (Σ попаданий базы − Σ точки; < 0 — прирост).
    pub lost: f64,
    /// Релевантных случаев.
    pub relevant: usize,
}

/// Итог точки сетки.
#[derive(Debug, Clone, Serialize)]
pub struct PointResult {
    pub name: String,
    pub env: BTreeMap<String, String>,
    /// Метрики решающей части ([`DECISION_SOURCES`]).
    pub metrics: HintMetrics,
    /// `post_tool` — по корзинам длины ошибки (все случаи).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub by_bucket: BTreeMap<String, HintMetrics>,
    /// Парные разности с базой (первой точкой) на решающей части, односторонние границы.
    pub delta: Deltas,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub delta_by_bucket: BTreeMap<String, Deltas>,
    /// Нижняя граница Δ `hint_precision` решающей части с поправкой Бонферрони на число
    /// сравнений (`0,05 / k`, `k` — точек кроме базы).
    pub precision_lo_bonf: Option<f64>,
    /// Решающая часть: чистая потеря ожидаемых подсказок и релевантных случаев.
    pub lost: f64,
    pub relevant: usize,
    /// Части набора по происхождению (`m4`, `task9`, `independent`) и `all`.
    pub parts: BTreeMap<String, PartResult>,
    /// Прошла условия правила (у базы покрытия — `false`).
    pub passes: bool,
    /// Показанные подсказки по случаям: `(id, ссылки)`.
    pub shown: Vec<(String, Vec<String>)>,
}

/// A/A-гейт: первая точка дважды на одном индексе.
#[derive(Debug, Clone, Serialize)]
pub struct AaGate {
    /// Все случаи совпали побайтово (порядок подсказок тоже).
    pub identical: bool,
    /// Случаи с расхождением.
    pub differing: Vec<String>,
    /// Порог шума: |Δ| метрик A/A (`hint_precision`, `hint_recall`, `silence_rate`), наибольший.
    pub noise: f64,
}

/// Сведения о прогоне сетки.
#[derive(Debug, Clone, Serialize)]
pub struct GridInfo {
    pub date: String,
    pub git: String,
    pub grid: String,
    pub rule: GridRule,
    pub model: Option<String>,
    pub device: Option<String>,
    pub server_version: String,
    pub corpus_fingerprint: String,
    pub hints_fingerprint: String,
    pub cases: usize,
    pub index_secs: f64,
}

/// Итог `eval --hints --grid`.
#[derive(Debug, Clone, Serialize)]
pub struct GridResult {
    pub info: GridInfo,
    pub aa: AaGate,
    pub points: Vec<PointResult>,
    /// Выбранная точка и почему.
    pub choice: String,
    pub reason: String,
}

/// Парные разности `b − a` (одни случаи в одном порядке): отношения — односторонний бутстреп
/// [`BOOTSTRAP_M6`] выборок, тишина — то же по нерелевантным случаям.
pub fn deltas_one_sided(a: &[Outcome], b: &[Outcome]) -> Deltas {
    assert_eq!(a.len(), b.len(), "парное сравнение на одних случаях");
    let hit = |o: &Outcome| o.shown.iter().filter(|s| o.expected.contains(s)).count() as f64;
    let shown = |o: &Outcome| o.shown.len() as f64;
    let col =
        |v: &[&Outcome], f: &dyn Fn(&Outcome) -> f64| v.iter().map(|o| f(o)).collect::<Vec<_>>();
    let all_a: Vec<&Outcome> = a.iter().collect();
    let all_b: Vec<&Outcome> = b.iter().collect();
    fn rel(v: &[Outcome]) -> Vec<&Outcome> {
        v.iter().filter(|o| !o.expected.is_empty()).collect()
    }
    let silent = |v: &[Outcome]| -> Vec<f64> {
        v.iter()
            .filter(|o| o.expected.is_empty())
            .map(|o| if o.shown.is_empty() { 1.0 } else { 0.0 })
            .collect()
    };
    let (ra, rb) = (rel(a), rel(b));
    let (sa, sb) = (silent(a), silent(b));
    let ones = vec![1.0; sa.len()];
    Deltas {
        hint_precision: paired_ratio_diff_one_sided(
            &col(&all_a, &hit),
            &col(&all_a, &shown),
            &col(&all_b, &hit),
            &col(&all_b, &shown),
            BOOTSTRAP_M6,
        ),
        hint_recall: paired_ratio_diff_one_sided(
            &col(&ra, &capped_hit),
            &col(&ra, &recall_cap),
            &col(&rb, &capped_hit),
            &col(&rb, &recall_cap),
            BOOTSTRAP_M6,
        ),
        silence_rate: (!sa.is_empty())
            .then(|| paired_diff_weighted(&sa, &sb, &ones, BOOTSTRAP_M6, SEED)),
    }
}

/// Выбор по правилу покрытия (§9 с правками ревью задачи 9): индекс точки и пояснение.
pub fn choose_coverage(points: &[PointResult]) -> (usize, String) {
    let mut best: Option<(usize, f64)> = None;
    for (i, p) in points.iter().enumerate().skip(1) {
        if !p.passes {
            continue;
        }
        let gain = p.delta.hint_precision.map_or(f64::NEG_INFINITY, |e| e.mean);
        if best.is_none_or(|(_, g)| gain > g) {
            best = Some((i, gain));
        }
    }
    match best {
        Some((i, g)) => (
            i,
            format!(
                "решающая часть {DECISION_SOURCES:?}: п. 1 — Δ hint_recall ≥ {RECALL_MARGIN} или \
                 потеря ≤ {LOSS_TOLERANCE} подсказки при N ≥ {LOSS_TOLERANCE_MIN_N}; п. 2 — \
                 наибольшая Δ hint_precision ({g:+.3}) с нижней границей (Бонферрони) > 0 и Δ ≥ 0 \
                 в каждой части {STABLE_PARTS:?}"
            ),
        ),
        None => (
            0,
            format!(
                "ни одна точка не прошла п. 1–2 на решающей части {DECISION_SOURCES:?} — как есть \
                 (база)"
            ),
        ),
    }
}

/// П. 1 (неинфериорность полноты) с допуском дискретности.
fn recall_ok(d: &Deltas, lost: f64, relevant: usize) -> bool {
    d.hint_recall.is_none_or(|e| e.lo >= RECALL_MARGIN)
        || (lost <= LOSS_TOLERANCE && relevant >= LOSS_TOLERANCE_MIN_N)
}

/// Точка покрытия прошла п. 1–2: полнота решающей части, рост точности с границей Бонферрони
/// больше 0 и без ухудшения точности в каждой решающей части.
fn coverage_passes(p: &PointResult) -> bool {
    let precision_up = p.precision_lo_bonf.is_some_and(|lo| lo > 0.0);
    let parts_ok = STABLE_PARTS.iter().all(|s| {
        p.parts
            .get(*s)
            .is_none_or(|r| r.delta.hint_precision.is_none_or(|e| e.mean >= 0.0))
    });
    recall_ok(&p.delta, p.lost, p.relevant) && precision_up && parts_ok
}

/// Точка порога FTS-only прошла условия §9.
fn fts_passes(m: &HintMetrics) -> bool {
    let r = m.hint_recall.map_or(0.0, |e| e.mean);
    let s = m.silence_rate.map_or(1.0, |e| e.mean);
    r >= FTS_MIN_RECALL && s >= FTS_MIN_SILENCE
}

/// Выбор порога FTS-only (§9): наибольшее прошедшее значение, иначе точка с 0,6 (или база).
pub fn choose_fts(points: &[PointResult]) -> (usize, String) {
    let value = |p: &PointResult| p.env.get(FTS_ENV).and_then(|v| v.parse::<f64>().ok());
    let best = points
        .iter()
        .enumerate()
        .filter(|(_, p)| p.passes)
        .filter_map(|(i, p)| value(p).map(|v| (i, v)))
        .max_by(|a, b| a.1.total_cmp(&b.1));
    match best {
        Some((i, v)) => (
            i,
            format!(
                "наибольшее значение ({v:.2}) с hint_recall ≥ {FTS_MIN_RECALL} и silence_rate ≥ \
                 {FTS_MIN_SILENCE}"
            ),
        ),
        None => {
            let i = points
                .iter()
                .position(|p| value(p).is_some_and(|v| (v - FTS_FALLBACK).abs() < 1e-9))
                .unwrap_or(0);
            (
                i,
                format!("ни одно значение не прошло условия — {FTS_FALLBACK} (§9)"),
            )
        }
    }
}

/// Исходы запусков.
fn outcomes_of(rs: &[&CaseRun]) -> Vec<Outcome> {
    rs.iter()
        .map(|r| Outcome {
            shown: r.shown.clone(),
            expected: r.expected.clone(),
        })
        .collect()
}

/// Часть набора: метрики, разности и потеря к базе.
fn part(base: &[&CaseRun], runs: &[&CaseRun]) -> PartResult {
    let (a, b) = (outcomes_of(base), outcomes_of(runs));
    let hits = |v: &[Outcome]| {
        v.iter()
            .filter(|o| !o.expected.is_empty())
            .map(capped_hit)
            .sum::<f64>()
    };
    PartResult {
        cases: runs.len(),
        metrics: hint_metrics(&b),
        delta: deltas_one_sided(&a, &b),
        lost: hits(&a) - hits(&b),
        relevant: b.iter().filter(|o| !o.expected.is_empty()).count(),
    }
}

/// Запуски из частей `src`.
fn of_sources<'a>(v: &[&'a CaseRun], src: &[&str]) -> Vec<&'a CaseRun> {
    v.iter()
        .copied()
        .filter(|r| src.contains(&r.source.as_str()))
        .collect()
}

/// Запуски корзины длины `b`.
fn of_bucket<'a>(v: &[&'a CaseRun], b: &str) -> Vec<&'a CaseRun> {
    v.iter()
        .copied()
        .filter(|r| len_bucket(&r.input) == b)
        .collect()
}

/// Итоги точки по запускам случаев; `comparisons` — точек кроме базы (поправка Бонферрони).
fn point_result(
    rule: GridRule,
    p: &Point,
    runs: &[CaseRun],
    base: Option<&[CaseRun]>,
    comparisons: usize,
) -> PointResult {
    let all: Vec<&CaseRun> = runs.iter().collect();
    let base_all: Vec<&CaseRun> = base.unwrap_or(runs).iter().collect();
    let mut parts = BTreeMap::new();
    for s in SOURCES {
        let (pa, pb) = (of_sources(&base_all, &[s]), of_sources(&all, &[s]));
        if !pb.is_empty() {
            parts.insert(s.to_string(), part(&pa, &pb));
        }
    }
    let ind = ["independent", "independent_b"];
    let (ia, ib) = (of_sources(&base_all, &ind), of_sources(&all, &ind));
    if !ib.is_empty() {
        parts.insert("independent_all".to_string(), part(&ia, &ib));
    }
    parts.insert("all".to_string(), part(&base_all, &all));
    let da = of_sources(&base_all, &DECISION_SOURCES);
    let db = of_sources(&all, &DECISION_SOURCES);
    let decision = part(&da, &db);
    let precision_lo_bonf = {
        let (a, b) = (outcomes_of(&da), outcomes_of(&db));
        let hit = |o: &Outcome| o.shown.iter().filter(|s| o.expected.contains(s)).count() as f64;
        let shown = |o: &Outcome| o.shown.len() as f64;
        let col = |v: &[Outcome], f: &dyn Fn(&Outcome) -> f64| v.iter().map(f).collect::<Vec<_>>();
        let q = 0.05 / comparisons.max(1) as f64;
        paired_ratio_q(
            &col(&a, &hit),
            &col(&a, &shown),
            &col(&b, &hit),
            &col(&b, &shown),
            BOOTSTRAP_M6,
            (q, 1.0 - q),
        )
        .map(|e| e.lo)
    };
    let (mut by_bucket, mut delta_by_bucket) = (BTreeMap::new(), BTreeMap::new());
    if rule == GridRule::PostToolCoverage {
        for b in LEN_BUCKETS {
            let (pa, pb) = (of_bucket(&base_all, b), of_bucket(&all, b));
            by_bucket.insert(b.to_string(), hint_metrics(&outcomes_of(&pb)));
            delta_by_bucket.insert(
                b.to_string(),
                deltas_one_sided(&outcomes_of(&pa), &outcomes_of(&pb)),
            );
        }
    }
    let mut r = PointResult {
        name: p.name.clone(),
        env: p.env.clone(),
        metrics: decision.metrics.clone(),
        by_bucket,
        delta: decision.delta.clone(),
        delta_by_bucket,
        precision_lo_bonf,
        lost: decision.lost,
        relevant: decision.relevant,
        parts,
        passes: false,
        shown: runs
            .iter()
            .map(|r| (r.id.clone(), r.shown.clone()))
            .collect(),
    };
    r.passes = match rule {
        GridRule::PostToolCoverage => base.is_some() && coverage_passes(&r),
        GridRule::PromptMinScoreFts => fts_passes(&r.metrics),
    };
    r
}

/// A/A: два снятия первой точки.
fn aa_gate(a: &[CaseRun], b: &[CaseRun]) -> AaGate {
    let differing: Vec<String> = a
        .iter()
        .zip(b)
        .filter(|(x, y)| x.shown != y.shown)
        .map(|(x, _)| x.id.clone())
        .collect();
    let out = |v: &[CaseRun]| -> Vec<Outcome> {
        v.iter()
            .map(|r| Outcome {
                shown: r.shown.clone(),
                expected: r.expected.clone(),
            })
            .collect()
    };
    let d = deltas_one_sided(&out(a), &out(b));
    let noise = [d.hint_precision, d.hint_recall, d.silence_rate]
        .iter()
        .flatten()
        .map(|e| e.mean.abs())
        .fold(0.0, f64::max);
    AaGate {
        identical: differing.is_empty(),
        differing,
        noise,
    }
}

/// Срок сбора подсказок сетки, мс: срок 200 мс по умолчанию на длинных ошибках (FTS по 100
/// находкам) отдаёт неполный ответ с `recall timeout`, и A/A расходится (ревью задачи 9);
/// сетка мерит отбор, а не срок, — как протокол эталона идей (`timeout_ms = 60000`).
const GRID_TIMEOUT_MS: &str = "60000";

/// Переопределения `--set` и срок сбора [`GRID_TIMEOUT_MS`] (если `--set` его не задал).
fn grid_sets(raw: &[String]) -> Result<Vec<(String, String)>> {
    let mut sets = run::parse_sets(raw)?;
    if !sets.iter().any(|(k, _)| k == "recall.timeout_ms") {
        sets.push(("recall.timeout_ms".into(), GRID_TIMEOUT_MS.into()));
    }
    Ok(sets)
}

/// `cargo xtask eval --hints --grid <файл>`.
pub fn run(a: &EvalArgs, p: &Paths, corpus: &Corpus, grid_path: &Path) -> Result<()> {
    let (rule, points) = load(grid_path)?;
    let (set, fingerprint) = load_hints(&p.hints)?;
    validate_hints(&set, corpus)?;
    let full = with_hint_insights(corpus, &set);
    let only_fts = a.modes.iter().all(|m| *m == Mode::Fts);
    if rule == GridRule::PromptMinScoreFts && !only_fts {
        bail!("сетка порога FTS-only — только --mode fts (поиск без модели деградирует до FTS)");
    }
    let server = run::server_dir(a, p)?;
    let model = (!only_fts).then(|| {
        a.model
            .clone()
            .unwrap_or_else(|| run::default_model(a.device).to_string())
    });
    let stem = grid_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "grid".into());
    let spec = RunSpec {
        name: run::run_name(model.as_deref(), Some(&format!("hints-{stem}"))),
        model: model.clone(),
        device: if only_fts { Device::Cpu } else { a.device },
        reranker: None,
        sets: grid_sets(&a.sets)?,
        keep_cuda_path: a.keep_cuda_path,
        ctrl_break: false,
    };
    // один индекс на все точки (правило 1 методики)
    let mut s = Session::start(p, &spec, &server, true)?;
    s.check_model()?;
    s.load_corpus(&full)?;
    load_rules(&mut s, &set.rule)?;
    if model.is_some() {
        s.wait_indexed()?;
        println!("индексация: {:.1} с", s.index_secs);
    }
    let caps = caps::probe(&s.base)?;
    if !caps.features.iter().any(|f| f == "hint_thresholds") {
        bail!("сервер {}: нет hint_thresholds (M6, задача 9)", caps.tag());
    }
    let index_secs = s.index_secs;
    let index_dir = s.data.parent().context("каталог прогона")?.to_path_buf();
    s.stop()?;
    let event = rule.event();
    let cases: Vec<&super::Case> = set.cases.iter().filter(|c| c.event == event).collect();
    println!(
        "сетка {} ({:?}): точек {}, случаев {} ({})",
        grid_path.display(),
        rule,
        points.len(),
        cases.len(),
        event.as_str()
    );
    // первая точка — дважды (A/A-гейт)
    let schedule: Vec<usize> = std::iter::once(0).chain(0..points.len()).collect();
    let mut runs: Vec<Vec<CaseRun>> = Vec::with_capacity(schedule.len());
    for (k, &pi) in schedule.iter().enumerate() {
        let pt = &points[pi];
        let name = format!("{}-{k:02}", spec.name);
        let dir: PathBuf = p.work.join(&name);
        if dir.exists() {
            fs::remove_dir_all(&dir).with_context(|| format!("очистка {}", dir.display()))?;
        }
        fs::create_dir_all(&dir)?;
        run::copy_dir(&index_dir, &dir)?;
        let mut sets = spec.sets.clone();
        sets.extend(
            pt.env
                .iter()
                .filter_map(|(k, v)| caps::env_to_key(k).map(|key| (key, v.clone()))),
        );
        let ps = RunSpec {
            name: name.clone(),
            sets,
            ..spec.clone()
        };
        let mut s = Session::start(p, &ps, &server, false)?;
        s.check_model()?;
        if model.is_some() {
            // индекс тот же: ожидание лишь подтверждает, что переиндексации нет
            s.wait_indexed()?;
        }
        let refs = RefMap::of(&s);
        let mut rs = Vec::with_capacity(cases.len());
        for c in &cases {
            let mut r = case_run(c);
            api_case(&s, &refs, c, &mut r).with_context(|| format!("{}: {}", pt.name, c.id))?;
            if r.warnings.iter().any(|w| w == super::WARN_RECALL_TIMEOUT) {
                bail!(
                    "{}: {}: {} — сбор подсказок не уложился в recall.timeout_ms, точка неполна",
                    pt.name,
                    c.id,
                    super::WARN_RECALL_TIMEOUT
                );
            }
            rs.push(r);
        }
        s.stop()?;
        fs::remove_dir_all(&dir).ok();
        println!("  точка {} ({k}): готово", pt.name);
        runs.push(rs);
    }
    let aa = aa_gate(&runs[0], &runs[1]);
    if only_fts && !aa.identical {
        let detail: Vec<String> = runs[0]
            .iter()
            .zip(&runs[1])
            .filter(|(x, y)| x.shown != y.shown)
            .map(|(x, y)| format!("{}: {:?} / {:?}", x.id, x.shown, y.shown))
            .collect();
        bail!(
            "A/A-гейт FTS не пройден: первая точка дважды разошлась — {} — FTS обязан совпасть \
             побайтово (методика, правило 2)",
            detail.join("; ")
        );
    }
    let base_runs = runs[1].clone();
    let results: Vec<PointResult> = points
        .iter()
        .enumerate()
        .map(|(i, pt)| {
            let base = (i > 0).then_some(base_runs.as_slice());
            point_result(rule, pt, &runs[i + 1], base, points.len().saturating_sub(1))
        })
        .collect();
    let (ci, reason) = match rule {
        GridRule::PostToolCoverage => choose_coverage(&results),
        GridRule::PromptMinScoreFts => choose_fts(&results),
    };
    let result = GridResult {
        info: GridInfo {
            date: run::today(),
            git: run::git_rev(&p.repo),
            grid: grid_path.display().to_string(),
            rule,
            model: model.clone(),
            device: model.as_ref().map(|_| a.device.as_str().to_string()),
            server_version: caps.tag(),
            corpus_fingerprint: corpus.fingerprint(),
            hints_fingerprint: fingerprint,
            cases: cases.len(),
            index_secs,
        },
        aa,
        choice: results[ci].name.clone(),
        reason,
        points: results,
    };
    let dir = &p.results;
    fs::create_dir_all(dir)?;
    let file = format!(
        "{}-{}-hints-{stem}",
        result.info.date,
        model.as_deref().unwrap_or("fts")
    );
    let json_path = dir.join(format!("{file}.json"));
    let md_path = dir.join(format!("{file}.md"));
    fs::write(&json_path, serde_json::to_string(&result)? + "\n")?;
    let md = render_md(&result);
    fs::write(&md_path, &md)?;
    print!("{md}");
    println!("  {}\n  {}", json_path.display(), md_path.display());
    Ok(())
}

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

fn row(name: &str, m: &HintMetrics, d: &Deltas, mark: &str) -> String {
    format!(
        "| {name}{mark} | {}/{} | {} | {} | {} | {} | {} | {} |\n",
        m.shown_expected,
        m.shown,
        est(&m.hint_precision),
        est(&m.hint_recall),
        est(&m.silence_rate),
        delta(&d.hint_precision),
        delta(&d.hint_recall),
        delta(&d.silence_rate)
    )
}

const HEAD: &str = "| точка | ожидаемых/показано | hint_precision | hint_recall | silence_rate | Δ precision | Δ recall | Δ silence |\n|---|---|---|---|---|---|---|---|\n";

/// Markdown-отчёт сетки.
pub fn render_md(r: &GridResult) -> String {
    let i = &r.info;
    let mut s = format!(
        "# Сетка подсказок {:?}: {} ({})\n\nКоммит `{}`, сервер {}, сетка `{}`, случаев {} \
         ({}), отпечаток корпуса `{}`, набора `{}`, индексация {:.0} с. Интервалы — парный \
         бутстреп случаев, 10 000 выборок, односторонние 95 % границы; Δ — к первой точке.\n\n",
        i.rule,
        i.model.as_deref().unwrap_or("fts"),
        i.date,
        i.git,
        i.server_version,
        i.grid,
        i.cases,
        i.rule.event().as_str(),
        i.corpus_fingerprint,
        i.hints_fingerprint,
        i.index_secs
    );
    s.push_str(&format!(
        "**A/A-гейт** (первая точка дважды на одном индексе): {}; порог шума |Δ| метрик — \
         {:.3}.\n\n**Выбор: `{}`** — {}.\n\n## Точки\n\n",
        if r.aa.identical {
            "побайтово совпали".to_string()
        } else {
            format!("расходятся случаи {:?}", r.aa.differing)
        },
        r.aa.noise,
        r.choice,
        r.reason
    ));
    s.push_str(HEAD);
    for p in &r.points {
        let mark = match (p.name == r.choice, p.passes) {
            (true, _) => " **(выбор)**",
            (false, true) => " (прошла)",
            _ => "",
        };
        s.push_str(&row(&format!("`{}`", p.name), &p.metrics, &p.delta, mark));
    }
    s.push_str(
        "\nРешающая часть: потеря ожидаемых подсказок к базе / релевантных; нижняя граница Δ \
         precision с поправкой Бонферрони:\n\n",
    );
    for p in r.points.iter().skip(1) {
        s.push_str(&format!(
            "- `{}`: потеря {:+.0} из {}, Δ precision lo (Бонферрони) {}\n",
            p.name,
            p.lost,
            p.relevant,
            p.precision_lo_bonf
                .map_or("—".to_string(), |v| format!("{v:+.3}"))
        ));
    }
    let names: Vec<&String> = r
        .points
        .first()
        .map(|p| p.parts.keys().collect())
        .unwrap_or_default();
    for part in names {
        s.push_str(&format!("\n## Часть «{part}»\n\n{HEAD}"));
        for p in &r.points {
            if let Some(x) = p.parts.get(part) {
                let mark = format!(" ({} сл., потеря {:+.0})", x.cases, x.lost);
                s.push_str(&row(&format!("`{}`", p.name), &x.metrics, &x.delta, &mark));
            }
        }
    }
    if r.points.iter().any(|p| !p.by_bucket.is_empty()) {
        for b in LEN_BUCKETS {
            s.push_str(&format!("\n## Корзина «слов {b}»\n\n{HEAD}"));
            for p in &r.points {
                if let (Some(m), Some(d)) = (p.by_bucket.get(b), p.delta_by_bucket.get(b)) {
                    s.push_str(&row(&format!("`{}`", p.name), m, d, ""));
                }
            }
        }
    }
    s.push_str("\n## Точки: env\n\n");
    for p in &r.points {
        let env: Vec<String> = p.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        s.push_str(&format!("- `{}`: {}\n", p.name, env.join(", ")));
    }
    // случаи, где точки разошлись с базой
    s.push_str("\n## Расхождения с первой точкой\n\n");
    let base = &r.points[0].shown;
    let mut any = false;
    for p in r.points.iter().skip(1) {
        let diff: Vec<String> = p
            .shown
            .iter()
            .zip(base)
            .filter(|(x, y)| x.1 != y.1)
            .map(|(x, y)| format!("{}: {:?} → {:?}", x.0, y.1, x.1))
            .collect();
        if !diff.is_empty() {
            any = true;
            s.push_str(&format!("- `{}`: {}\n", p.name, diff.join("; ")));
        }
    }
    if !any {
        s.push_str("нет\n");
    }
    s
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

    fn point(name: &str, env: &[(&str, &str)]) -> Point {
        Point {
            name: name.into(),
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn est(mean: f64, lo: f64) -> Option<Estimate> {
        Some(Estimate { mean, lo, hi: mean })
    }

    fn pr(name: &str, env: &[(&str, &str)], d: Deltas, passes: bool) -> PointResult {
        PointResult {
            name: name.into(),
            env: point(name, env).env,
            metrics: HintMetrics::default(),
            by_bucket: BTreeMap::new(),
            precision_lo_bonf: d.hint_precision.map(|e| e.lo),
            delta: d,
            delta_by_bucket: BTreeMap::new(),
            lost: 0.0,
            relevant: 40,
            parts: BTreeMap::new(),
            passes,
            shown: Vec::new(),
        }
    }

    /// Правило покрытия §9: полнота не хуже −0,02, затем наибольший рост точности с нижней
    /// границей > 0; при равенстве — точка раньше в файле; никто не прошёл — база.
    #[test]
    fn coverage_choice_follows_rule() {
        let d = |p: Option<Estimate>, r: Option<Estimate>| Deltas {
            hint_precision: p,
            hint_recall: r,
            silence_rate: None,
        };
        let cp = |dd: Deltas, lost: f64, relevant: usize| {
            let mut p = pr("p", &[], dd, false);
            p.lost = lost;
            p.relevant = relevant;
            coverage_passes(&p)
        };
        assert!(cp(d(est(0.05, 0.01), est(0.0, -0.01)), 0.0, 40));
        // потеря двух подсказок — п. 1 не выполнен
        assert!(!cp(d(est(0.05, 0.01), est(-0.05, -0.1)), 2.0, 40));
        // допуск дискретности: одна потеря при N ≥ 25 — выполнен, при N < 25 — нет
        assert!(cp(d(est(0.05, 0.01), est(-0.03, -0.09)), 1.0, 31));
        assert!(!cp(d(est(0.05, 0.01), est(-0.05, -0.15)), 1.0, 20));
        assert!(!cp(d(est(0.05, 0.0), est(0.0, 0.0)), 0.0, 40));
        // точность падает в одной из устойчивых частей — не проходит
        let mut p = pr("p", &[], d(est(0.05, 0.01), est(0.0, 0.0)), false);
        p.parts.insert(
            "m4".into(),
            PartResult {
                delta: d(est(-0.01, -0.05), None),
                ..PartResult::default()
            },
        );
        assert!(!coverage_passes(&p));
        let pts = vec![
            pr("base", &[], Deltas::default(), false),
            pr("a", &[], d(est(0.04, 0.01), est(0.0, 0.0)), true),
            pr("b", &[], d(est(0.06, 0.02), est(0.0, 0.0)), true),
            pr("c", &[], d(est(0.06, 0.03), est(0.0, 0.0)), true),
            pr("d", &[], d(est(0.09, -0.01), est(0.0, 0.0)), false),
        ];
        assert_eq!(choose_coverage(&pts).0, 2, "равенство — раньше в файле");
        assert_eq!(choose_coverage(&pts[..1]).0, 0);
        let none = vec![pts[0].clone(), pts[4].clone()];
        assert_eq!(choose_coverage(&none).0, 0);
    }

    /// Порог FTS-only §9: наибольшее значение с полнотой ≥ 0,5 и тишиной ≥ 0,7, иначе 0,6.
    #[test]
    fn fts_choice_follows_rule() {
        let m = |r: f64, s: f64| HintMetrics {
            hint_recall: est(r, r),
            silence_rate: est(s, s),
            ..HintMetrics::default()
        };
        assert!(fts_passes(&m(0.5, 0.7)));
        assert!(!fts_passes(&m(0.49, 0.9)));
        assert!(!fts_passes(&m(0.9, 0.69)));
        let p = |v: &str, ok: bool| pr(v, &[(FTS_ENV, v)], Deltas::default(), ok);
        let pts = vec![
            p("0.6", false),
            p("0.2", true),
            p("0.4", true),
            p("0.5", false),
        ];
        assert_eq!(choose_fts(&pts).0, 2);
        let none = vec![p("0.2", false), p("0.6", false)];
        assert_eq!(choose_fts(&none).0, 1, "иначе 0,6");
    }

    /// A/A: одинаковые снятия — гейт пройден, шум 0; расхождение называет случай.
    #[test]
    fn aa_gate_detects_difference() {
        let run = |id: &str, shown: &[&str]| CaseRun {
            shown: shown.iter().map(|s| s.to_string()).collect(),
            expected: vec!["x".into()],
            ..case_run(&super::super::Case {
                id: id.into(),
                event: Event::PostTool,
                input: "e".into(),
                tool: None,
                expected: vec!["x".into()],
                note: String::new(),
                source: "m4".into(),
            })
        };
        let a = vec![run("e-1", &["x"]), run("e-2", &[])];
        let g = aa_gate(&a, &a.clone());
        assert!(g.identical && g.noise == 0.0);
        let b = vec![run("e-1", &["x"]), run("e-2", &["x"])];
        let g = aa_gate(&a, &b);
        assert_eq!(g.differing, ["e-2"]);
        assert!(g.noise > 0.0);
    }

    /// Разности односторонние: нижняя граница не ниже двусторонней.
    #[test]
    fn one_sided_deltas_tighter_than_two_sided() {
        let a: Vec<Outcome> = (0..20)
            .map(|i| o(if i % 3 == 0 { &["x"] } else { &[] }, &["x"]))
            .collect();
        let b: Vec<Outcome> = (0..20)
            .map(|i| o(if i % 2 == 0 { &["x"] } else { &[] }, &["x"]))
            .collect();
        let one = deltas_one_sided(&a, &b).hint_recall.unwrap();
        let two = super::super::deltas(&a, &b).hint_recall.unwrap();
        assert!((one.mean - two.mean).abs() < 1e-12);
        assert!(one.lo >= two.lo - 1e-12, "{one:?} {two:?}");
    }

    /// Файлы сеток eval проходят проверку.
    #[test]
    fn hint_grid_files_are_valid() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../eval");
        let (rule, pts) = load(&root.join("grid-m6-hints.toml")).unwrap();
        assert_eq!(rule, GridRule::PostToolCoverage);
        assert_eq!(
            pts.len(),
            12,
            "делитель {{3, 2}} × cap {{3, 4, 6}} × {{plain, stems}}"
        );
        assert!(pts[0].env.values().any(|v| v == "plain"));
        let (rule, pts) = load(&root.join("grid-m6-hints-fts.toml")).unwrap();
        assert_eq!(rule, GridRule::PromptMinScoreFts);
        let vals: Vec<f64> = pts.iter().filter_map(fts_value).collect();
        assert_eq!(vals, [0.6, 0.2, 0.3, 0.4, 0.5]);
        let bad = root.join("../target/test-tmp/xtask/hint-grid");
        fs::create_dir_all(&bad).unwrap();
        let f = bad.join("g.toml");
        fs::write(
            &f,
            "rule = \"prompt_min_score_fts\"\n[[point]]\nname = \"a\"\nenv = { ZAM_SEARCH__BUDGET_TOKENS = \"600\" }\n",
        )
        .unwrap();
        assert!(load(&f).is_err());
    }
}
