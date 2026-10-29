//! Eval словаря M4.5 (задача 15, спецификация §2.2, §13): `cargo xtask eval --glossary`.
//!
//! **Набор** `eval/glossary.toml`: `[[term]]` — термины (`key`, `title`, `definition`,
//! `aliases`), `[[query]]` — запросы на псевдонимах (`id`, `text`, `lang`, `term`, `expect`).
//! Запрос содержит псевдоним своего термина и не содержит его имени; ожидаемые заметки корпуса
//! содержат имя (начало слова, без учёта регистра) и не содержат псевдонима запроса — по этому
//! слову их находит только канал `alias` (или векторный канал по смыслу). Проверка —
//! [`validate_glossary`].
//!
//! **Прогон** (один сервер и один индекс, корпус и insight набора M3 со связями `contradicts`):
//! 1. `base` — без словаря: запросы словаря и основной набор `eval/queries.toml`;
//! 2. термины словаря создаются в пространстве решения набора (`kind: term`);
//! 3. перезапуски сервера на том же индексе с `search.w_fts_alias` из [`ALIAS_WEIGHTS`] (0 —
//!    канал выключен, карточки терминов в выдаче остаются); последний — умолчание
//!    [`DEFAULT_ALIAS_WEIGHT`]: его основной набор — итоговый прогон M4.5 (`reliable_first` без
//!    бустов — ещё один перезапуск, как в [`super::run::run_modes`]).
//!
//! Карточки терминов в выдаче — `term:<key>`: для запросов словаря они не релевантны
//! (ожидаются заметки), справочно метрики считаются и без них. Сравнения — парный бутстреп по
//! запросам (95 %, как в M4): запросы словаря — вес `w` против `w = 0`, основной набор — вес `w`
//! против `base` (критерий §13: Δ nDCG@10 ≥ −0,005) и против `w = 0`.
//!
//! Итог: `<дата>-m45-glossary-<модель|fts>.{json,md}` и итоговые прогоны основного набора
//! `<дата>-m45-<модель>-<режим>.{json,md}` (FTS-only — `<дата>-m45-fts`) с `--compare` (по
//! файлу на режим).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::compare::{compare, compare_file};
use super::corpus::{Corpus, EVAL_SOLUTION, Kind, Lang, Query, fnv1a, valid_key};
use super::metrics::{Aggregate, Estimate, aggregate, paired_diff, score};
use super::report;
use super::run::{self, NO_CERTAINTY_BOOSTS, QueryRun, RunSpec, Session};
use super::{EvalArgs, Mode, Paths};

/// Веса канала `alias` прогона; последний — умолчание сервера (итоговый прогон).
pub const ALIAS_WEIGHTS: [f64; 4] = [0.0, 0.25, 0.75, DEFAULT_ALIAS_WEIGHT];
/// `search.w_fts_alias` по умолчанию (спецификация §2.2).
pub const DEFAULT_ALIAS_WEIGHT: f64 = 0.5;
/// Ключ веса канала.
const ALIAS_KEY: &str = "search.w_fts_alias";
/// Ссылка на карточку термина в выдаче.
pub const TERM_REF: &str = "term:";
/// Требования к полному набору (спецификация §13, бриф задачи 15).
const MIN_TERMS: usize = 15;
const MIN_QUERIES: usize = 20;
/// Предел основного набора по Δ nDCG@10 со словарём (§13).
pub const MAIN_NDCG_FLOOR: f64 = -0.005;

/// Термин словаря набора.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Term {
    pub key: String,
    pub title: String,
    pub definition: String,
    pub aliases: Vec<String>,
}

/// Запрос на псевдониме.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GlossaryQuery {
    pub id: String,
    pub text: String,
    pub lang: Lang,
    /// Ключ термина, чей псевдоним в запросе.
    pub term: String,
    pub expect: Vec<String>,
}

/// `eval/glossary.toml`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GlossarySet {
    #[serde(default)]
    pub term: Vec<Term>,
    #[serde(default)]
    pub query: Vec<GlossaryQuery>,
}

/// Читает набор; отпечаток — FNV-1a текста файла.
pub fn load_glossary(path: &Path) -> Result<(GlossarySet, String)> {
    let text = fs::read_to_string(path).with_context(|| format!("чтение {}", path.display()))?;
    let set: GlossarySet =
        toml::from_str(&text).with_context(|| format!("разбор {}", path.display()))?;
    Ok((set, fnv1a(text.into_bytes())))
}

/// Слова текста: буквы и цифры, нижний регистр (как разбивает запрос FTS5 `unicode61`).
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Последовательность слов `needle` встречается в `hay` подряд; `prefix` — последнее слово
/// `needle` — начало слова `hay` (префиксный поиск FTS5).
fn contains_seq(hay: &[String], needle: &[String], prefix: bool) -> bool {
    if needle.is_empty() || needle.len() > hay.len() {
        return false;
    }
    hay.windows(needle.len()).any(|w| {
        w.iter().zip(needle).enumerate().all(|(i, (h, n))| {
            if prefix && i + 1 == needle.len() {
                h.starts_with(n.as_str())
            } else {
                h == n
            }
        })
    })
}

impl GlossarySet {
    pub fn term(&self, key: &str) -> Option<&Term> {
        self.term.iter().find(|t| t.key == key)
    }

    /// Псевдоним термина `q.term` в тексте запроса (первый найденный).
    pub fn alias_in(&self, q: &GlossaryQuery) -> Option<&str> {
        let t = self.term(&q.term)?;
        let w = words(&q.text);
        t.aliases
            .iter()
            .find(|a| contains_seq(&w, &words(a), false))
            .map(String::as_str)
    }

    /// Запросы словаря в формате набора (вид — `cross-lang`, если язык ожидаемых заметок другой).
    pub fn queries(&self, corpus: &Corpus) -> Vec<Query> {
        self.query
            .iter()
            .map(|q| {
                let cross = q
                    .expect
                    .iter()
                    .filter_map(|e| corpus.get(e))
                    .all(|n| n.lang != q.lang);
                Query {
                    id: q.id.clone(),
                    text: q.text.clone(),
                    lang: q.lang,
                    expect: q.expect.clone(),
                    kind: if cross {
                        Kind::CrossLang
                    } else {
                        Kind::Paraphrase
                    },
                    disjoint: false,
                    forbid: Vec::new(),
                    weaker: Vec::new(),
                    evidence: None,
                    also: Vec::new(),
                }
            })
            .collect()
    }
}

/// Сводка проверки набора.
#[derive(Debug, Clone, Serialize)]
pub struct GlossarySummary {
    pub terms: usize,
    pub aliases: usize,
    pub queries: usize,
    /// Запросов к заметкам другого языка.
    pub cross_lang: usize,
}

/// Проверка набора: ключи, уникальность имён и псевдонимов (как `409 term-conflict` сервера),
/// пределы `data` термина (§2.1), псевдоним в запросе, имя — в ожидаемых заметках, псевдонима
/// запроса в них нет; `full` — требования полного набора (≥ 15 терминов, ≥ 20 запросов).
pub fn validate_glossary(
    set: &GlossarySet,
    corpus: &Corpus,
    full: bool,
) -> Result<GlossarySummary> {
    let mut errs: Vec<String> = Vec::new();
    let mut names: BTreeMap<String, &str> = BTreeMap::new();
    let mut keys = BTreeSet::new();
    for t in &set.term {
        let mut bad = |m: String| errs.push(format!("term {}: {m}", t.key));
        if !valid_key(&t.key) || !keys.insert(t.key.as_str()) {
            bad("ключ — [a-z0-9-], уникальный".into());
        }
        let title = t.title.trim().chars().count();
        if title == 0 || title > 200 {
            bad("имя 1–200 символов".into());
        }
        let def = t.definition.trim().chars().count();
        if def == 0 || def > 300 {
            bad("definition 1–300 символов".into());
        }
        if t.aliases.is_empty() || t.aliases.len() > 32 {
            bad("псевдонимов 1–32".into());
        }
        for n in std::iter::once(&t.title).chain(&t.aliases) {
            if n.trim().is_empty() || n.chars().count() > 200 {
                bad(format!("«{n}»: 1–200 символов"));
            }
            let norm = words(n).join(" ");
            if let Some(other) = names.insert(norm, &t.key) {
                bad(format!("«{n}» уже есть у термина {other}"));
            }
        }
    }
    let mut ids = BTreeSet::new();
    let mut cross = 0;
    for q in &set.query {
        let mut bad = |m: String| errs.push(format!("query {}: {m}", q.id));
        if !q.id.starts_with("g-") || !ids.insert(q.id.as_str()) {
            bad("id вида g-NN, уникальный".into());
        }
        if q.text.trim().is_empty() || q.text.chars().count() > 512 {
            bad("текст 1–512 символов".into());
        }
        let Some(t) = set.term(&q.term) else {
            bad(format!("нет термина {}", q.term));
            continue;
        };
        let qw = words(&q.text);
        if contains_seq(&qw, &words(&t.title), false) {
            bad(format!("в запросе имя термина «{}»", t.title));
        }
        let Some(alias) = set.alias_in(q) else {
            bad(format!("в запросе нет псевдонима термина {}", t.key));
            continue;
        };
        if q.expect.is_empty() {
            bad("пустой expect".into());
        }
        let mut other_lang = !q.expect.is_empty();
        for e in &q.expect {
            let Some(n) = corpus.get(e) else {
                bad(format!("нет заметки {e}"));
                continue;
            };
            other_lang &= n.lang != q.lang;
            let nw = words(&n.text);
            if !contains_seq(&nw, &words(&t.title), true) {
                bad(format!("{e}: нет имени «{}»", t.title));
            }
            if contains_seq(&nw, &words(alias), true) {
                bad(format!("{e}: есть псевдоним запроса «{alias}»"));
            }
        }
        cross += usize::from(other_lang);
    }
    if full {
        if set.term.len() < MIN_TERMS {
            errs.push(format!("терминов {} < {MIN_TERMS}", set.term.len()));
        }
        if set.query.len() < MIN_QUERIES {
            errs.push(format!("запросов {} < {MIN_QUERIES}", set.query.len()));
        }
        // каждый термин проверяется хотя бы одним запросом
        for t in &set.term {
            if !set.query.iter().any(|q| q.term == t.key) {
                errs.push(format!("term {}: нет запроса", t.key));
            }
        }
    }
    if !errs.is_empty() {
        bail!("набор словаря неверен:\n  {}", errs.join("\n  "));
    }
    Ok(GlossarySummary {
        terms: set.term.len(),
        aliases: set.term.iter().map(|t| t.aliases.len()).sum(),
        queries: set.query.len(),
        cross_lang: cross,
    })
}

// ---------------------------------------------------------------- прогон

/// Сервер прогона для [`evaluate`]: настоящий ([`Live`]) или модель в тесте.
pub trait Engine {
    /// Создаёт термины; возвращает id записи → ключ термина.
    fn load_terms(&mut self, terms: &[Term]) -> Result<BTreeMap<String, String>>;
    /// Перезапуск на том же индексе с дополнительными ключами конфигурации.
    fn restart(&mut self, sets: &[(String, String)]) -> Result<()>;
    /// Запросы в режиме `mode` (карточки терминов — `entry:<id>`).
    fn run(&mut self, queries: &[Query], mode: Mode) -> Result<Vec<QueryRun>>;
}

/// Выдачи фазы по режиму: основной набор и запросы словаря.
#[derive(Debug, Clone, Serialize)]
pub struct ModeRuns {
    pub main: Vec<QueryRun>,
    pub glossary: Vec<QueryRun>,
}

/// Фаза прогона.
#[derive(Debug, Clone, Serialize)]
pub struct Phase {
    /// `base` | `alias=<w>`.
    pub name: String,
    /// Термины словаря созданы.
    pub terms: bool,
    /// `search.w_fts_alias` (`None` — умолчание сервера).
    pub weight: Option<f64>,
    pub modes: BTreeMap<String, ModeRuns>,
}

/// Карточки терминов в выдаче: `entry:<id>` → `term:<key>`.
fn map_terms(runs: &mut [QueryRun], terms: &BTreeMap<String, String>) {
    for r in runs {
        for h in &mut r.hits {
            if let Some(id) = h.path.strip_prefix("entry:")
                && let Some(k) = terms.get(id)
            {
                h.path = format!("{TERM_REF}{k}");
            }
        }
    }
}

/// Пересчёт метрик по выдаче без карточек терминов.
fn without_cards(runs: &[QueryRun]) -> Vec<QueryRun> {
    runs.iter()
        .map(|r| {
            let mut r = r.clone();
            r.hits.retain(|h| !h.path.starts_with(TERM_REF));
            let ranked: Vec<String> = r.hits.iter().map(|h| h.path.clone()).collect();
            let rel: Vec<String> = r.expect.iter().chain(&r.also).cloned().collect();
            let m = score(&ranked, &rel);
            r.metrics.recall5 = m.recall5;
            r.metrics.mrr10 = m.mrr10;
            r.metrics.ndcg10 = m.ndcg10;
            r
        })
        .collect()
}

/// Фазы прогона: `base`, затем термины и веса `weights` (каждый — перезапуск).
pub fn evaluate(
    e: &mut dyn Engine,
    set: &GlossarySet,
    gq: &[Query],
    main: &[Query],
    modes: &[Mode],
    weights: &[f64],
) -> Result<Vec<Phase>> {
    let run_all = |e: &mut dyn Engine,
                   terms: &BTreeMap<String, String>|
     -> Result<BTreeMap<String, ModeRuns>> {
        let mut out = BTreeMap::new();
        for &m in modes {
            let mut main_runs = e.run(main, m)?;
            let mut g = e.run(gq, m)?;
            map_terms(&mut main_runs, terms);
            map_terms(&mut g, terms);
            out.insert(
                m.as_str().to_string(),
                ModeRuns {
                    main: main_runs,
                    glossary: g,
                },
            );
        }
        Ok(out)
    };
    let mut phases = Vec::new();
    let none = BTreeMap::new();
    println!("фаза base (без словаря)…");
    phases.push(Phase {
        name: "base".into(),
        terms: false,
        weight: None,
        modes: run_all(e, &none)?,
    });
    let terms = e.load_terms(&set.term)?;
    println!("словарь: {} терминов", terms.len());
    for &w in weights {
        println!("фаза alias={w}…");
        e.restart(&[(ALIAS_KEY.to_string(), w.to_string())])?;
        phases.push(Phase {
            name: format!("alias={w}"),
            terms: true,
            weight: Some(w),
            modes: run_all(e, &terms)?,
        });
    }
    Ok(phases)
}

/// Метрики фазы и режима.
#[derive(Debug, Clone, Serialize)]
pub struct PhaseRow {
    pub phase: String,
    pub weight: Option<f64>,
    pub glossary: Aggregate,
    /// Запросы словаря без карточек терминов в выдаче (справочно).
    pub glossary_no_cards: Aggregate,
    /// Доля запросов словаря, где ожидаемая заметка в top-10 найдена каналом `alias`.
    pub glossary_via_alias: f64,
    pub main: Aggregate,
}

/// Парная разность `b − a`.
#[derive(Debug, Clone, Serialize)]
pub struct DiffRow {
    pub set: String,
    pub a: String,
    pub b: String,
    pub n: usize,
    pub d_recall5: Estimate,
    pub d_ndcg10: Estimate,
}

/// Итог режима.
#[derive(Debug, Clone, Serialize)]
pub struct ModeReport {
    pub mode: Mode,
    pub rows: Vec<PhaseRow>,
    pub diffs: Vec<DiffRow>,
    /// Критерий §13: полнота@5 запросов словаря с каналом не ниже, чем без него (вес по
    /// умолчанию против 0).
    pub recall_not_lower: bool,
    /// Критерий §13: Δ nDCG@10 основного набора (вес по умолчанию против `base`) ≥ −0,005.
    pub main_ndcg_ok: bool,
    /// Лучший по nDCG@10 запросов словаря вес (> 0).
    pub best_weight: f64,
    /// Сменить умолчание: лучший вес достоверно лучше умолчания на запросах словаря (нижняя
    /// граница Δ nDCG@10 > 0) без регрессии основного набора (верхняя граница ≥ 0, Δ к `base`
    /// не ниже предела).
    pub change_default: bool,
}

fn col(runs: &[QueryRun], f: fn(&QueryRun) -> f64) -> Vec<f64> {
    runs.iter().map(f).collect()
}

fn diff_row(set: &str, a: (&str, &[QueryRun]), b: (&str, &[QueryRun])) -> DiffRow {
    DiffRow {
        set: set.into(),
        a: a.0.into(),
        b: b.0.into(),
        n: a.1.len(),
        d_recall5: paired_diff(
            &col(a.1, |r| r.metrics.recall5),
            &col(b.1, |r| r.metrics.recall5),
        ),
        d_ndcg10: paired_diff(
            &col(a.1, |r| r.metrics.ndcg10),
            &col(b.1, |r| r.metrics.ndcg10),
        ),
    }
}

fn runs_of<'p>(p: &'p Phase, key: &str) -> &'p ModeRuns {
    &p.modes[key]
}

/// Метрики и сравнения по режимам.
pub fn summarize(phases: &[Phase], modes: &[Mode]) -> Vec<ModeReport> {
    let mut out = Vec::new();
    for &m in modes {
        let key = m.as_str();
        let agg = |r: &[QueryRun]| aggregate(&r.iter().map(|x| x.metrics).collect::<Vec<_>>());
        let rows: Vec<PhaseRow> = phases
            .iter()
            .map(|p| {
                let g = &runs_of(p, key).glossary;
                let via = g
                    .iter()
                    .filter(|r| {
                        r.hits.iter().take(run::TOP_K).any(|h| {
                            r.expect.contains(&h.path) && h.sources.iter().any(|s| s == "alias")
                        })
                    })
                    .count() as f64
                    / g.len().max(1) as f64;
                PhaseRow {
                    phase: p.name.clone(),
                    weight: p.weight,
                    glossary: agg(g),
                    glossary_no_cards: agg(&without_cards(g)),
                    glossary_via_alias: via,
                    main: agg(&runs_of(p, key).main),
                }
            })
            .collect();
        let base = phases.iter().find(|p| !p.terms);
        let by_w = |w: f64| phases.iter().find(|p| p.weight == Some(w));
        let off = by_w(0.0);
        let def = by_w(DEFAULT_ALIAS_WEIGHT);
        let mut diffs = Vec::new();
        let mut weights: Vec<f64> = phases.iter().filter_map(|p| p.weight).collect();
        weights.sort_by(f64::total_cmp);
        for &w in &weights {
            let p = by_w(w).expect("фаза веса");
            if let Some(o) = off
                && w > 0.0
            {
                diffs.push(diff_row(
                    "glossary",
                    (&o.name, &runs_of(o, key).glossary),
                    (&p.name, &runs_of(p, key).glossary),
                ));
            }
            if let Some(b) = base {
                diffs.push(diff_row(
                    "main",
                    (&b.name, &runs_of(b, key).main),
                    (&p.name, &runs_of(p, key).main),
                ));
            }
        }
        if let (Some(b), Some(d)) = (base, def) {
            diffs.push(diff_row(
                "glossary",
                (&b.name, &runs_of(b, key).glossary),
                (&d.name, &runs_of(d, key).glossary),
            ));
        }
        let find = |set: &str, a: &str, b: &str| {
            diffs
                .iter()
                .find(|d| d.set == set && d.a == a && d.b == b)
                .cloned()
        };
        let def_name = def.map(|d| d.name.clone()).unwrap_or_default();
        let recall_not_lower = off
            .and_then(|o| find("glossary", &o.name, &def_name))
            .is_none_or(|d| d.d_recall5.mean >= 0.0);
        let main_ndcg_ok = base
            .and_then(|b| find("main", &b.name, &def_name))
            .is_none_or(|d| d.d_ndcg10.mean >= MAIN_NDCG_FLOOR);
        // лучший вес по nDCG@10 запросов словаря (равенство — ближе к умолчанию)
        let mut best = DEFAULT_ALIAS_WEIGHT;
        let mut best_v = f64::NEG_INFINITY;
        for (p, r) in phases.iter().zip(&rows) {
            if let Some(w) = p.weight.filter(|w| *w > 0.0) {
                let v = r.glossary.ndcg10.mean;
                let closer = (w - DEFAULT_ALIAS_WEIGHT).abs() < (best - DEFAULT_ALIAS_WEIGHT).abs();
                if v > best_v + 1e-12 || ((v - best_v).abs() <= 1e-12 && closer) {
                    best = w;
                    best_v = v;
                }
            }
        }
        let change_default = best != DEFAULT_ALIAS_WEIGHT
            && match (by_w(best), def, base) {
                (Some(bp), Some(d), Some(b)) => {
                    let g = diff_row(
                        "glossary",
                        (&d.name, &runs_of(d, key).glossary),
                        (&bp.name, &runs_of(bp, key).glossary),
                    );
                    let mm = diff_row(
                        "main",
                        (&d.name, &runs_of(d, key).main),
                        (&bp.name, &runs_of(bp, key).main),
                    );
                    let mb = diff_row(
                        "main",
                        (&b.name, &runs_of(b, key).main),
                        (&bp.name, &runs_of(bp, key).main),
                    );
                    let ok = g.d_ndcg10.lo > 0.0
                        && mm.d_ndcg10.hi >= 0.0
                        && mb.d_ndcg10.mean >= MAIN_NDCG_FLOOR;
                    diffs.push(g);
                    diffs.push(mm);
                    ok
                }
                _ => false,
            };
        out.push(ModeReport {
            mode: m,
            rows,
            diffs,
            recall_not_lower,
            main_ndcg_ok,
            best_weight: best,
            change_default,
        });
    }
    out
}

/// Сведения о прогоне.
#[derive(Debug, Clone, Serialize)]
pub struct GlossaryInfo {
    pub date: String,
    pub git: String,
    pub model: Option<String>,
    pub model_id: Option<String>,
    pub device: Option<String>,
    pub sets: BTreeMap<String, String>,
    pub corpus_fingerprint: String,
    pub glossary_fingerprint: String,
    pub queries_fingerprint: String,
    pub summary: GlossarySummary,
    pub weights: Vec<f64>,
}

/// Итог `eval --glossary`.
#[derive(Debug, Clone, Serialize)]
pub struct GlossaryResult {
    pub info: GlossaryInfo,
    pub modes: Vec<ModeReport>,
    /// Выдачи запросов словаря по фазам и режимам (основной набор — в итоговых прогонах).
    pub glossary_runs: BTreeMap<String, BTreeMap<String, Vec<QueryRun>>>,
}

fn est(e: &Estimate) -> String {
    format!("{:.3} [{:.3}…{:.3}]", e.mean, e.lo, e.hi)
}

/// Markdown итога.
pub fn render_md(r: &GlossaryResult) -> String {
    let i = &r.info;
    let mut s = format!(
        "# Eval M4.5: словарь RU↔EN, {}\n\n",
        i.model.as_deref().unwrap_or("без моделей (FTS-only)")
    );
    let _ = writeln!(s, "- дата {}, коммит `{}`", i.date, i.git);
    if let Some(m) = &i.model_id {
        let _ = writeln!(
            s,
            "- модель `{m}`, устройство {}",
            i.device.as_deref().unwrap_or("?")
        );
    }
    let _ = writeln!(
        s,
        "- словарь: {} терминов, {} псевдонимов, {} запросов (к заметкам другого языка — {}), \
         отпечаток `{}`; корпус `{}`, основной набор `{}`",
        i.summary.terms,
        i.summary.aliases,
        i.summary.queries,
        i.summary.cross_lang,
        i.glossary_fingerprint,
        i.corpus_fingerprint,
        i.queries_fingerprint
    );
    if !i.sets.is_empty() {
        let _ = writeln!(s, "- переопределения: {:?}", i.sets);
    }
    let _ = writeln!(
        s,
        "- интервалы — 95 % бутстреп по запросам; разности — парные (`b − a`)\n"
    );
    for m in &r.modes {
        let _ = writeln!(s, "## Режим {}\n", m.mode.as_str());
        s.push_str(
            "| Фаза | словарь recall@5 | словарь nDCG@10 | без карточек nDCG@10 | через alias | \
             основной recall@5 | основной nDCG@10 |\n|---|---|---|---|---:|---|---|\n",
        );
        for row in &m.rows {
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {:.0} % | {} | {} |",
                row.phase,
                est(&row.glossary.recall5),
                est(&row.glossary.ndcg10),
                est(&row.glossary_no_cards.ndcg10),
                row.glossary_via_alias * 100.0,
                est(&row.main.recall5),
                est(&row.main.ndcg10)
            );
        }
        s.push_str(
            "\n| Набор | a | b | n | Δ recall@5 | Δ nDCG@10 |\n|---|---|---|---:|---|---|\n",
        );
        for d in &m.diffs {
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} | {} |",
                d.set,
                d.a,
                d.b,
                d.n,
                report::diff(&d.d_recall5),
                report::diff(&d.d_ndcg10)
            );
        }
        let yes = |b: bool| if b { "да" } else { "нет" };
        let _ = writeln!(
            s,
            "\n- полнота@5 словаря с каналом (вес {DEFAULT_ALIAS_WEIGHT}) не ниже, чем без него: {}\n\
             - основной набор: Δ nDCG@10 (вес {DEFAULT_ALIAS_WEIGHT} против base) ≥ {MAIN_NDCG_FLOOR}: {}\n\
             - лучший вес по nDCG@10 словаря: {}; сменить умолчание: {}\n",
            yes(m.recall_not_lower),
            yes(m.main_ndcg_ok),
            m.best_weight,
            yes(m.change_default)
        );
    }
    s
}

/// Имя файла итогового прогона M4.5: `<дата>-m45-<модель>-<режим>` (FTS-only — `<дата>-m45-fts`).
pub fn final_stem(date: &str, model: Option<&str>, mode: Mode) -> String {
    match model {
        Some(m) => format!("{date}-m45-{m}-{}", mode.file_tag()),
        None => format!("{date}-m45-fts"),
    }
}

/// Сервер прогона: [`Session`] на `target/eval/<прогон>`.
struct Live<'a> {
    p: &'a Paths,
    server: PathBuf,
    spec: RunSpec,
    s: Option<Session>,
    terms: BTreeMap<String, String>,
}

impl Live<'_> {
    fn session(&mut self) -> &mut Session {
        self.s.as_mut().expect("сервер запущен")
    }

    fn start(&mut self, extra: &[(String, String)]) -> Result<()> {
        if let Some(s) = self.s.take() {
            s.stop()?;
        }
        let mut spec = self.spec.clone();
        spec.sets.extend(extra.iter().cloned());
        let mut s = Session::start(self.p, &spec, &self.server, false)?;
        s.check_model()?;
        if spec.model.is_some() {
            s.wait_indexed()?;
        }
        self.s = Some(s);
        Ok(())
    }
}

impl Engine for Live<'_> {
    fn load_terms(&mut self, terms: &[Term]) -> Result<BTreeMap<String, String>> {
        let s = self.session();
        let mut out = BTreeMap::new();
        for t in terms {
            let body = json!({
                "kind": "term",
                "title": t.title,
                "body": "",
                "data": {"definition": t.definition, "aliases": t.aliases},
            });
            let (st, v) = s.api(
                &format!("/api/v1/spaces/{EVAL_SOLUTION}/entries"),
                Some(&body),
            )?;
            if st != 201 {
                bail!("термин {}: HTTP {st}: {v}", t.key);
            }
            let id = v["id"].as_str().context("нет id термина")?.to_string();
            out.insert(id, t.key.clone());
        }
        if s.spec.model.is_some() {
            s.wait_indexed()?;
        }
        self.terms = out.clone();
        Ok(out)
    }

    fn restart(&mut self, sets: &[(String, String)]) -> Result<()> {
        self.start(sets)
    }

    fn run(&mut self, queries: &[Query], mode: Mode) -> Result<Vec<QueryRun>> {
        self.session().run_queries(queries, mode)
    }
}

/// `cargo xtask eval --glossary`.
pub fn run(a: &EvalArgs, p: &Paths, corpus: &Corpus, queries: &[Query]) -> Result<()> {
    let (set, fingerprint) = load_glossary(&p.glossary)?;
    let summary = validate_glossary(&set, corpus, true)?;
    let gq = set.queries(corpus);
    println!(
        "словарь: {} терминов, {} псевдонимов, {} запросов",
        summary.terms, summary.aliases, summary.queries
    );
    let server = run::server_dir(a, p)?;
    let only_fts = a.modes.iter().all(|m| *m == Mode::Fts);
    if a.modes.contains(&Mode::Fts) && !only_fts {
        bail!("fts запускается отдельно: сервер без моделей (--mode fts)");
    }
    let model = (!only_fts).then(|| {
        a.model
            .clone()
            .unwrap_or_else(|| run::default_model(a.device).to_string())
    });
    let sets = run::parse_sets(&a.sets)?;
    if sets.iter().any(|(k, _)| k == ALIAS_KEY) {
        bail!("--set {ALIAS_KEY}: вес канала перебирает сам прогон");
    }
    let spec = RunSpec {
        name: run::run_name(model.as_deref(), Some("m45-glossary")),
        model: model.clone(),
        device: a.device,
        reranker: a.reranker.clone(),
        sets,
        keep_cuda_path: a.keep_cuda_path,
        ctrl_break: false,
    };
    let mut s = Session::start(p, &spec, &server, true)?;
    s.check_model()?;
    println!(
        "сервер {}: модель {}, устройство {}",
        s.base, s.status["model"]["id"], s.status["device"]
    );
    s.load_corpus(corpus)?;
    if model.is_some() {
        s.wait_indexed()?;
        println!("индексация: {:.1} с", s.index_secs);
    }
    let split = run::chunk_split(&s, queries)?;
    let mut live = Live {
        p,
        server,
        spec: spec.clone(),
        s: Some(s),
        terms: BTreeMap::new(),
    };
    let phases = evaluate(&mut live, &set, &gq, queries, &a.modes, &ALIAS_WEIGHTS)?;
    let last = phases.last().context("нет фаз")?;
    // итоговые прогоны основного набора: последняя фаза (вес по умолчанию)
    let date = run::today();
    let mut finals = Vec::new();
    {
        let s = live.session();
        for &m in &a.modes {
            let runs = last.modes[m.as_str()].main.clone();
            let mut r = run::result(s, p, corpus, queries, m, Some("m45"), runs);
            r.split = split.clone();
            finals.push(r);
        }
    }
    // reliable_first без бустов certainty — перезапуск на том же индексе
    let certainty: Vec<Query> = queries
        .iter()
        .filter(|q| q.kind == Kind::Certainty)
        .cloned()
        .collect();
    if !certainty.is_empty() {
        let mut extra = vec![(ALIAS_KEY.to_string(), DEFAULT_ALIAS_WEIGHT.to_string())];
        extra.extend(
            NO_CERTAINTY_BOOSTS
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string())),
        );
        live.start(&extra)?;
        for r in &mut finals {
            let mut runs = live.session().run_queries(&certainty, r.info.mode)?;
            map_terms(&mut runs, &live.terms);
            run::apply_no_boost(r, &runs);
        }
    }
    let st = live.session().status.clone();
    let model_id = model
        .as_ref()
        .map(|_| st["model"]["id"].as_str().unwrap_or_default().to_string());
    let device = model.as_ref().map(|_| {
        st["device"]["kind"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    });
    if let Some(s) = live.s.take() {
        s.stop()?;
    }
    let mut failed = Vec::new();
    for mut r in finals {
        if let Some(prev) = compare_file(&a.compare, r.info.mode)? {
            r.compare = Some(compare(&p.repo, &prev, &r)?);
        }
        let stem = final_stem(&date, model.as_deref(), r.info.mode);
        let (json_path, md_path) = report::write_run_as(&p.results, &stem, &r)?;
        report::print_run(&r);
        println!("  {}\n  {}", json_path.display(), md_path.display());
        failed.extend(run::history_control(&r));
    }
    let modes = summarize(&phases, &a.modes);
    let glossary_runs = phases
        .iter()
        .map(|ph| {
            (
                ph.name.clone(),
                ph.modes
                    .iter()
                    .map(|(m, r)| (m.clone(), r.glossary.clone()))
                    .collect(),
            )
        })
        .collect();
    let result = GlossaryResult {
        info: GlossaryInfo {
            date: date.clone(),
            git: run::git_rev(&p.repo),
            model: model.clone(),
            model_id,
            device,
            sets: spec.sets.iter().cloned().collect(),
            corpus_fingerprint: corpus.fingerprint(),
            glossary_fingerprint: fingerprint,
            queries_fingerprint: run::queries_fingerprint(queries),
            summary,
            weights: ALIAS_WEIGHTS.to_vec(),
        },
        modes,
        glossary_runs,
    };
    let stem = format!("{date}-m45-glossary-{}", model.as_deref().unwrap_or("fts"));
    fs::create_dir_all(&p.results)?;
    let json_path = p.results.join(format!("{stem}.json"));
    let md_path = p.results.join(format!("{stem}.md"));
    fs::write(&json_path, serde_json::to_string(&result)? + "\n")?;
    let md = render_md(&result);
    fs::write(&md_path, &md)?;
    print!("{md}");
    println!("  {}\n  {}", json_path.display(), md_path.display());
    if !failed.is_empty() {
        bail!("{}", failed.join("; "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::corpus::Note;
    use crate::eval::metrics::QueryMetrics;
    use crate::eval::run::Hit;

    fn repo_eval() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../eval")
    }

    #[test]
    fn glossary_set_is_valid() {
        let e = repo_eval();
        let corpus = Corpus::load(&e.join("corpus")).unwrap();
        let (set, _) = load_glossary(&e.join("glossary.toml")).unwrap();
        let s = validate_glossary(&set, &corpus, true).unwrap();
        assert!(s.terms >= MIN_TERMS && s.queries >= MIN_QUERIES, "{s:?}");
    }

    #[test]
    fn words_and_sequences() {
        assert_eq!(
            words("Тайм-аут k8s, PowerShell!"),
            ["тайм", "аут", "k8s", "powershell"]
        );
        let h = words("таймауты upstream в nginx");
        assert!(contains_seq(&h, &words("таймаут"), true));
        assert!(!contains_seq(&h, &words("таймаут"), false));
        assert!(contains_seq(&h, &words("в nginx"), false));
        assert!(!contains_seq(&h, &words("nginx в"), false));
    }

    /// Мини-корпус: две заметки, термин «выжимка» с псевдонимом «summary».
    fn mini() -> (Corpus, GlossarySet) {
        let mut c = Corpus::default();
        for (path, lang, text) in [
            (
                "ru/zam/cache",
                Lang::Ru,
                "# Кэш\n\nВыжимка ответа хранится в кэше сервера.",
            ),
            (
                "en/ops/other",
                Lang::En,
                "# Other\n\nNothing about caching here at all.",
            ),
        ] {
            c.notes.insert(
                path.into(),
                Note {
                    path: path.into(),
                    lang,
                    text: text.into(),
                },
            );
        }
        let set: GlossarySet = toml::from_str(
            r#"
[[term]]
key = "summary"
title = "Выжимка"
definition = "Краткий ответ агента по источникам."
aliases = ["summary", "дайджест"]

[[query]]
id = "g-01"
text = "какой summary у агента"
lang = "ru"
term = "summary"
expect = ["ru/zam/cache"]
"#,
        )
        .unwrap();
        (c, set)
    }

    #[test]
    fn validate_rejects_bad_sets() {
        let (c, set) = mini();
        validate_glossary(&set, &c, false).unwrap();
        // полный набор требует ≥ 15 терминов и ≥ 20 запросов
        assert!(validate_glossary(&set, &c, true).is_err());
        // имя термина в запросе
        let mut s = set.clone();
        s.query[0].text = "какая выжимка и summary".into();
        assert!(validate_glossary(&s, &c, false).is_err());
        // нет псевдонима
        let mut s = set.clone();
        s.query[0].text = "где хранится ответ".into();
        assert!(validate_glossary(&s, &c, false).is_err());
        // псевдоним совпадает с именем другого термина
        let mut s = set.clone();
        let mut t = s.term[0].clone();
        t.key = "other".into();
        t.title = "Дайджест".into();
        s.term.push(t);
        assert!(validate_glossary(&s, &c, false).is_err());
        // ожидаемая заметка без имени
        let mut s = set.clone();
        s.query[0].expect = vec!["en/ops/other".into()];
        assert!(validate_glossary(&s, &c, false).is_err());
    }

    /// Модель сервера: заметка находится, если содержит слово запроса (начало слова); с
    /// терминами и весом канала > 0 — ещё и по имени термина вместо псевдонима (балл × вес);
    /// карточки терминов — тоже документы.
    struct Fake {
        notes: Vec<(String, String)>,
        set: GlossarySet,
        terms: bool,
        weight: f64,
        restarts: Vec<Vec<(String, String)>>,
    }

    impl Fake {
        fn search(&self, q: &str) -> Vec<Hit> {
            let qw = words(q);
            let mut docs = self.notes.clone();
            if self.terms {
                for (i, t) in self.set.term.iter().enumerate() {
                    docs.push((
                        format!("entry:t{i}"),
                        format!("{} {} {}", t.title, t.aliases.join(" "), t.definition),
                    ));
                }
            }
            let mut scored: Vec<(f64, Hit)> = Vec::new();
            for (path, text) in docs {
                let tw = words(&text);
                let own = qw
                    .iter()
                    .filter(|w| tw.iter().any(|t| t.starts_with(w.as_str())))
                    .count() as f64;
                let mut alias = 0.0;
                let mut sources = vec!["fts".to_string()];
                if self.terms && self.weight > 0.0 {
                    for t in &self.set.term {
                        let hit_alias = t
                            .aliases
                            .iter()
                            .any(|a| contains_seq(&qw, &words(a), false));
                        if hit_alias && contains_seq(&tw, &words(&t.title), true) {
                            alias += self.weight * 2.0;
                            sources.push("alias".into());
                        }
                    }
                }
                let sc = own + alias;
                if sc > 0.0 {
                    scored.push((
                        sc,
                        Hit {
                            path,
                            score: sc,
                            sources,
                            reranked: false,
                        },
                    ));
                }
            }
            scored.sort_by(|a, b| b.0.total_cmp(&a.0));
            scored.into_iter().map(|x| x.1).collect()
        }
    }

    impl Engine for Fake {
        fn load_terms(&mut self, terms: &[Term]) -> Result<BTreeMap<String, String>> {
            self.terms = true;
            Ok(terms
                .iter()
                .enumerate()
                .map(|(i, t)| (format!("t{i}"), t.key.clone()))
                .collect())
        }

        fn restart(&mut self, sets: &[(String, String)]) -> Result<()> {
            self.weight = sets
                .iter()
                .find(|(k, _)| k == ALIAS_KEY)
                .map_or(DEFAULT_ALIAS_WEIGHT, |(_, v)| v.parse().unwrap());
            self.restarts.push(sets.to_vec());
            Ok(())
        }

        fn run(&mut self, queries: &[Query], _mode: Mode) -> Result<Vec<QueryRun>> {
            Ok(queries
                .iter()
                .map(|q| {
                    let hits = self.search(&q.text);
                    let ranked: Vec<String> = hits.iter().map(|h| h.path.clone()).collect();
                    let metrics: QueryMetrics = score(&ranked, &q.relevant());
                    QueryRun {
                        id: q.id.clone(),
                        text: q.text.clone(),
                        kind: q.kind,
                        lang: q.lang,
                        disjoint: false,
                        expect: q.expect.clone(),
                        also: Vec::new(),
                        forbid: Vec::new(),
                        weaker: Vec::new(),
                        long: false,
                        fts_only: false,
                        metrics,
                        cancelled5_history: None,
                        reliable_first_no_boost: None,
                        leaves: Vec::new(),
                        hits,
                        ms: 0.0,
                        server_ms: serde_json::Value::Null,
                    }
                })
                .collect())
        }
    }

    /// Раннер на мини-корпусе: фазы base и весов, карточки терминов `term:<key>`, канал
    /// `alias` поднимает заметку с каноническим словом, основной набор не страдает, отчёт.
    #[test]
    fn glossary_eval_smoke() {
        let (c, set) = mini();
        validate_glossary(&set, &c, false).unwrap();
        let gq = set.queries(&c);
        let main: Vec<Query> = vec![Query {
            id: "m-01".into(),
            text: "caching here".into(),
            lang: Lang::En,
            expect: vec!["en/ops/other".into()],
            kind: Kind::Exact,
            disjoint: false,
            forbid: Vec::new(),
            weaker: Vec::new(),
            evidence: None,
            also: Vec::new(),
        }];
        let mut e = Fake {
            notes: c
                .notes
                .values()
                .map(|n| (n.path.clone(), n.text.clone()))
                .collect(),
            set: set.clone(),
            terms: false,
            weight: DEFAULT_ALIAS_WEIGHT,
            restarts: Vec::new(),
        };
        let modes = [Mode::Fts];
        let phases = evaluate(&mut e, &set, &gq, &main, &modes, &ALIAS_WEIGHTS).unwrap();
        assert_eq!(
            phases.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["base", "alias=0", "alias=0.25", "alias=0.75", "alias=0.5"]
        );
        assert_eq!(e.restarts.len(), ALIAS_WEIGHTS.len());
        // без канала заметка с «выжимкой» не находится (в запросе только «summary»), с ним — да
        let g =
            |name: &str| &phases.iter().find(|p| p.name == name).unwrap().modes["fts"].glossary[0];
        assert_eq!(g("base").metrics.recall5, 0.0);
        assert_eq!(g("alias=0").metrics.recall5, 0.0);
        let on = g("alias=0.5");
        assert_eq!(on.metrics.recall5, 1.0);
        // карточка термина — `term:<key>`, нерелевантна
        assert!(
            g("alias=0").hits.iter().any(|h| h.path == "term:summary"),
            "{:?}",
            g("alias=0").hits
        );
        let rep = summarize(&phases, &modes);
        let m = &rep[0];
        assert!(m.recall_not_lower && m.main_ndcg_ok, "{m:?}");
        let off_to_def = m
            .diffs
            .iter()
            .find(|d| d.set == "glossary" && d.a == "alias=0" && d.b == "alias=0.5")
            .unwrap();
        assert!(off_to_def.d_recall5.mean > 0.0);
        let via = m.rows.iter().find(|r| r.phase == "alias=0.5").unwrap();
        assert_eq!(via.glossary_via_alias, 1.0);
        // все веса > 0 дают одно и то же — умолчание не меняется
        assert_eq!(m.best_weight, DEFAULT_ALIAS_WEIGHT);
        assert!(!m.change_default);
        let md = render_md(&GlossaryResult {
            info: GlossaryInfo {
                date: "2026-10-29".into(),
                git: "x".into(),
                model: None,
                model_id: None,
                device: None,
                sets: BTreeMap::new(),
                corpus_fingerprint: c.fingerprint(),
                glossary_fingerprint: "f".into(),
                queries_fingerprint: "q".into(),
                summary: validate_glossary(&set, &c, false).unwrap(),
                weights: ALIAS_WEIGHTS.to_vec(),
            },
            modes: rep,
            glossary_runs: BTreeMap::new(),
        });
        assert!(
            md.contains("## Режим fts") && md.contains("alias=0.75"),
            "{md}"
        );
        assert_eq!(
            final_stem("2026-10-29", None, Mode::Fts),
            "2026-10-29-m45-fts"
        );
        assert_eq!(
            final_stem("2026-10-29", Some("bge-m3-fp16"), Mode::HybridRerank),
            "2026-10-29-m45-bge-m3-fp16-hybrid-rerank"
        );
    }
}
