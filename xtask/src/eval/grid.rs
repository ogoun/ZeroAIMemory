//! Подбор весов `[search]` и порогов чанкинга `[index]` по сетке (задача 14 M2, шаг 3).
//!
//! Файл сетки (`eval/grid.toml`): модель, устройство и до 12 конфигураций `[[config]]` —
//! переопределения `set = { "search.w_fts" = "1.5", "index.chunk_target_max" = "800" }` и,
//! при необходимости, свой исполняемый файл сервера `server_bin` (путь от корня репозитория:
//! база другой версии кода, например нарезка M2 для задачи 17 M3).
//! Первая конфигурация — базовая (обычно текущие умолчания). Конфигурации с одинаковыми
//! порогами чанкинга (`index.chunk_*`) делят один индекс: корпус загружается и индексируется
//! один раз, для остальных сервер перезапускается на том же каталоге с другими весами.
//!
//! Сравнение с базовой — парный бутстреп разности по запросам ([`paired_diff`]): выигрыш
//! считается устойчивым, если нижняя граница 95 % интервала разности nDCG@10 больше нуля.
//!
//! M6 (задача 10, методика замера при недетерминизме): `mode = "hybrid+rerank"` — сетка с
//! серверным реранком; `aa = true` — A/A-гейт: базовая конфигурация снимается дважды на том же
//! индексе, расхождение выдачи (путь и балл каждого попадания) и порог шума — в отчёте;
//! `[[part]] name, queries` — дополнительные наборы запросов на том же индексе: отчёт по частям
//! (основной набор — часть `main`) с односторонними границами Δ nDCG@10 (без поправки и с
//! поправкой Бонферрони на число сравнений с базой) и долей запросов с nDCG@10 < 1. Запросы
//! части с `area_kind` (набор областей, M6 §11) отчитываются частями `<часть>_<вид>`
//! (`area_same`, `area_other`, `area_none`).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::corpus::{Corpus, Query};
use super::metrics::{Estimate, paired_diff};
use super::report::{self, diff, est};
use super::run::{self, QueryRun, RunResult, RunSpec, Session};
use super::{Device, EvalArgs, Mode, Paths};

/// Предел числа конфигураций сетки.
pub const MAX_CONFIGS: usize = 12;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GridFile {
    model: Option<String>,
    device: Option<String>,
    /// Вторая база: имя конфигурации, с которой считаются дополнительные разности (вклад
    /// изменения поверх промежуточной конфигурации, ruling R11 задачи 17 M3).
    second_base: Option<String>,
    /// Режим поиска: `hybrid` (по умолчанию) | `hybrid+rerank`.
    mode: Option<String>,
    /// A/A-гейт: базовая конфигурация снимается дважды на том же индексе.
    #[serde(default)]
    aa: bool,
    /// Снятий базы в A/A-гейте (≥ 2; по умолчанию 2): каждое следующее сравнивается с первым.
    aa_k: Option<usize>,
    /// Дополнительные наборы запросов (части отчёта) поверх основного (`main`).
    #[serde(default)]
    part: Vec<PartFile>,
    #[serde(default)]
    config: Vec<GridConfig>,
}

/// Часть сетки: имя и файл запросов (от корня репозитория).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartFile {
    name: String,
    queries: String,
}

/// Имя основного набора в отчёте по частям.
pub const MAIN_PART: &str = "main";
/// Объединение дополнительных частей (если их больше одной).
pub const EXTRA_ALL: &str = "extra_all";

/// Итог части набора у конфигурации (M6, задача 10).
#[derive(Debug, Clone, Serialize)]
pub struct PartResult {
    pub n: usize,
    pub ndcg10: f64,
    /// Доля запросов с nDCG@10 < 1 (условие «не на потолке», §10).
    pub below1: f64,
    pub reliable_first: Option<f64>,
    pub n_reliable: usize,
    /// Δ nDCG@10 к базе: среднее и односторонние 95 % границы (`lo` — 5-й перцентиль).
    pub d_ndcg10: Estimate,
    /// Нижняя граница Δ nDCG@10 с поправкой Бонферрони (`0,05 / сравнений`).
    pub d_ndcg10_lo_bonf: f64,
    /// Запросов лучше / хуже / равно базе по nDCG@10.
    pub wins: usize,
    pub losses: usize,
    pub ties: usize,
}

/// A/A-гейт: два снятия базовой конфигурации на одном индексе.
#[derive(Debug, Clone, Serialize)]
pub struct AaGate {
    /// Выдача совпала побайтово (путь и балл каждого попадания каждого запроса).
    pub equal: bool,
    /// Запросы с расхождением выдачи.
    pub differ: Vec<String>,
    /// 95-й перцентиль |Δ nDCG@10| запроса между снятиями — по частям.
    pub noise_p95: BTreeMap<String, f64>,
    /// Снятий базы (K).
    #[serde(default)]
    pub snapshots: usize,
    /// A/A по метрике: запросов с Δ nDCG@10 ≠ 0 хотя бы в одной паре снятий — по частям.
    #[serde(default)]
    pub ndcg_changed: BTreeMap<String, usize>,
    /// A/A по метрике: максимум |Δ nDCG@10| запроса по парам снятий — по частям.
    #[serde(default)]
    pub ndcg_max_abs: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GridConfig {
    pub name: String,
    #[serde(default)]
    pub set: BTreeMap<String, String>,
    /// `zam-server[.exe]` конфигурации (от корня репозитория); по умолчанию — `--server-bin`
    /// или собранный `target/release`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_bin: Option<String>,
}

/// Пороги чанкинга конфигурации: (ключ, значение).
type IndexKey = Vec<(String, String)>;

impl GridConfig {
    /// Ключ индекса: пороги чанкинга и исполняемый файл сервера (всё, что меняет чанки).
    fn index_key(&self) -> IndexKey {
        self.server_bin
            .iter()
            .map(|b| ("server_bin".to_string(), b.clone()))
            .chain(
                self.set
                    .iter()
                    .filter(|(k, _)| k.starts_with("index.chunk_"))
                    .map(|(k, v)| (k.clone(), v.clone())),
            )
            .collect()
    }
}

/// Итог конфигурации.
#[derive(Debug, Serialize)]
pub struct ConfigResult {
    pub config: GridConfig,
    pub index_secs: f64,
    pub metrics: run::Breakdown,
    /// Разности с базовой (парный бутстреп).
    pub d_recall5: Estimate,
    pub d_mrr10: Estimate,
    pub d_ndcg10: Estimate,
    pub d_ndcg10_disjoint: Estimate,
    /// Набор M3: разности по запросам `chunk` (листы) и по запросам к длинным заметкам.
    pub d_chunk_recall5: Option<Estimate>,
    /// Справочно: chunk-recall@5 с перекрытием (зонд сетки перекрытия: лист вместе с
    /// повторённым началом).
    pub d_chunk_recall5_with_overlap: Option<Estimate>,
    /// Разрезанные границами листов доказательства запросов `chunk`.
    pub split: Option<crate::eval::leaves::SplitStats>,
    pub d_chunk_ndcg10: Option<Estimate>,
    pub d_ndcg10_long: Option<Estimate>,
    /// Δ chunk-recall@5 запросов-сторожей «вводная фраза + код» (`evidence` с «:» и началом
    /// следующего блока, задача 17 M3): исправление «:» и λ не должны их ронять.
    #[serde(default)]
    pub d_chunk_recall5_intro: Option<Estimate>,
    /// Набор M3: Δ `reliable_first` (запросы `certainty`) и Δ `cancelled@5` (`superseded`).
    #[serde(default)]
    pub d_reliable_first: Option<Estimate>,
    #[serde(default)]
    pub d_cancelled5: Option<Estimate>,
    /// Листовых чанков заметок в индексе конфигурации (критерий ±5 % задачи нарезки).
    pub leaf_chunks: usize,
    /// Объём индексации: чанки и токены (критерий ±5 % задачи 17 M3).
    #[serde(default)]
    pub volume: crate::eval::leaves::IndexVolume,
    /// Разности со второй базой (`second_base` файла сетки), если она задана.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vs_second: Option<SecondDiff>,
    /// Устойчиво лучше базовой: нижняя граница интервала разности nDCG@10 > 0.
    pub better: bool,
    /// Устойчиво хуже: верхняя граница < 0.
    pub worse: bool,
    /// `(id, recall@5, nDCG@10, chunk-recall@5)` по запросам.
    pub per_query: Vec<(String, f64, f64, Option<f64>)>,
    /// Итоги по частям набора (`main` и `[[part]]`).
    #[serde(default)]
    pub parts: BTreeMap<String, PartResult>,
}

/// Разности конфигурации со второй базой сетки.
#[derive(Debug, Serialize)]
pub struct SecondDiff {
    pub base: String,
    pub d_recall5: Estimate,
    pub d_ndcg10: Estimate,
    pub d_chunk_recall5: Option<Estimate>,
    pub d_chunk_recall5_intro: Option<Estimate>,
    pub d_ndcg10_long: Option<Estimate>,
    /// Итоги частей против второй базы (поправка Бонферрони — на сравнения с ней).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parts: BTreeMap<String, PartResult>,
}

#[derive(Debug, Serialize)]
pub struct GridResult {
    pub date: String,
    pub git: String,
    pub model: String,
    pub device: String,
    pub notes: usize,
    pub queries: usize,
    pub mode: String,
    /// A/A-гейт базовой конфигурации (`aa = true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aa: Option<AaGate>,
    pub configs: Vec<ConfigResult>,
}

fn parse_mode(s: Option<&str>) -> Result<Mode> {
    Ok(match s {
        None | Some("hybrid") => Mode::Hybrid,
        Some("hybrid+rerank") => Mode::HybridRerank,
        Some(x) => bail!("mode = \"{x}\": hybrid | hybrid+rerank"),
    })
}

/// Части набора: основной (`main`) и `[[part]]` сетки; id запросов уникальны во всех частях.
fn load_parts(
    p: &Paths,
    corpus: &Corpus,
    main: &[Query],
    extra: &[PartFile],
) -> Result<(Vec<Query>, BTreeMap<String, String>)> {
    let mut all: Vec<Query> = main.to_vec();
    let mut part_of: BTreeMap<String, String> = main
        .iter()
        .map(|q| (q.id.clone(), MAIN_PART.to_string()))
        .collect();
    for pf in extra {
        if pf.name == MAIN_PART
            || pf.name == EXTRA_ALL
            || extra.iter().filter(|x| x.name == pf.name).count() > 1
        {
            bail!("часть «{}»: имя занято", pf.name);
        }
        let qs = super::corpus::load_queries(&p.repo.join(&pf.queries))?;
        super::corpus::validate(corpus, &qs, false)
            .with_context(|| format!("часть «{}» ({})", pf.name, pf.queries))?;
        for q in qs {
            // набор областей (§11): виды `same`/`other`/`none` — отдельные части отчёта
            let part = match q.area_kind {
                Some(k) => format!("{}_{}", pf.name, k.as_str()),
                None => pf.name.clone(),
            };
            if part_of.insert(q.id.clone(), part).is_some() {
                bail!("запрос {} повторяется в частях сетки", q.id);
            }
            all.push(q);
        }
    }
    Ok((all, part_of))
}

/// Выдача запроса в сравнимом виде (путь и балл попаданий) — A/A-гейт.
fn hits_key(q: &QueryRun) -> Vec<(String, u64)> {
    q.hits
        .iter()
        .map(|h| (h.path.clone(), h.score.to_bits()))
        .collect()
}

/// 95-й перцентиль (ближайший ранг сверху).
fn p95(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    let i = ((v.len() as f64) * 0.95).ceil() as usize;
    v[i.clamp(1, v.len()) - 1]
}

fn part_name(part_of: &BTreeMap<String, String>, id: &str) -> String {
    part_of.get(id).cloned().unwrap_or_else(|| MAIN_PART.into())
}

/// A/A-гейт по снятиям базы: `a` — первое, `rest` — следующие (каждое сравнивается с первым).
fn aa_gate(a: &[QueryRun], rest: &[Vec<QueryRun>], part_of: &BTreeMap<String, String>) -> AaGate {
    const EPS: f64 = 1e-12;
    let mut differ: Vec<String> = Vec::new();
    let mut by_part: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut changed: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
    let mut max_abs: BTreeMap<String, f64> = BTreeMap::new();
    for b in rest {
        for (x, y) in a.iter().zip(b) {
            if hits_key(x) != hits_key(y) && !differ.contains(&x.id) {
                differ.push(x.id.clone());
            }
            let part = part_name(part_of, &x.id);
            let d = (x.metrics.ndcg10 - y.metrics.ndcg10).abs();
            by_part.entry(part.clone()).or_default().push(d);
            let set = changed.entry(part.clone()).or_default();
            if d > EPS {
                set.insert(x.id.clone());
            }
            let m = max_abs.entry(part).or_insert(0.0);
            *m = m.max(d);
        }
    }
    AaGate {
        equal: differ.is_empty(),
        differ,
        noise_p95: by_part.into_iter().map(|(k, v)| (k, p95(v))).collect(),
        snapshots: rest.len() + 1,
        ndcg_changed: changed.into_iter().map(|(k, v)| (k, v.len())).collect(),
        ndcg_max_abs: max_abs,
    }
}

/// Итоги частей конфигурации `q` против базы `base`; `comparisons` — сравнений с базой
/// (поправка Бонферрони).
pub fn part_results(
    base: &[QueryRun],
    q: &[QueryRun],
    part_of: &BTreeMap<String, String>,
    comparisons: usize,
) -> BTreeMap<String, PartResult> {
    let mut groups: BTreeMap<String, Vec<(&QueryRun, &QueryRun)>> = BTreeMap::new();
    for (x, y) in base.iter().zip(q) {
        groups
            .entry(part_name(part_of, &x.id))
            .or_default()
            .push((x, y));
    }
    // несколько дополнительных частей — ещё и их объединение
    let extra: Vec<(&QueryRun, &QueryRun)> = groups
        .iter()
        .filter(|(k, _)| k.as_str() != MAIN_PART)
        .flat_map(|(_, v)| v.iter().copied())
        .collect();
    if groups.keys().filter(|k| k.as_str() != MAIN_PART).count() > 1 {
        groups.insert(EXTRA_ALL.to_string(), extra);
    }
    groups
        .into_iter()
        .map(|(name, pairs)| {
            let a: Vec<f64> = pairs.iter().map(|(x, _)| x.metrics.ndcg10).collect();
            let b: Vec<f64> = pairs.iter().map(|(_, y)| y.metrics.ndcg10).collect();
            let rel: Vec<f64> = pairs
                .iter()
                .filter_map(|(_, y)| flag(y.metrics.reliable_first))
                .collect();
            (name, part_result(&a, &b, &rel, comparisons))
        })
        .collect()
}

/// [`PartResult`] по nDCG@10 базы `a` и точки `b` (одни запросы) и флагам `reliable_first`
/// точки `rel`.
fn part_result(a: &[f64], b: &[f64], rel: &[f64], comparisons: usize) -> PartResult {
    use super::metrics::{BOOTSTRAP_M6, paired_ratio_q};
    const EPS: f64 = 1e-12;
    let ones = vec![1.0; a.len()];
    let d = |lo: f64| {
        paired_ratio_q(a, &ones, b, &ones, BOOTSTRAP_M6, (lo, 1.0 - lo)).unwrap_or_default()
    };
    let n = a.len().max(1) as f64;
    let count = |f: &dyn Fn(f64, f64) -> bool| a.iter().zip(b).filter(|(x, y)| f(**x, **y)).count();
    PartResult {
        n: a.len(),
        ndcg10: b.iter().sum::<f64>() / n,
        below1: b.iter().filter(|v| **v < 1.0 - EPS).count() as f64 / n,
        reliable_first: (!rel.is_empty()).then(|| rel.iter().sum::<f64>() / rel.len() as f64),
        n_reliable: rel.len(),
        d_ndcg10: d(0.05),
        d_ndcg10_lo_bonf: d(0.05 / comparisons.max(1) as f64).lo,
        wins: count(&|x, y| y > x + EPS),
        losses: count(&|x, y| y < x - EPS),
        ties: count(&|x, y| (y - x).abs() <= EPS),
    }
}

fn parse_device(s: Option<&str>, fallback: Device) -> Result<Device> {
    Ok(match s {
        None => fallback,
        Some("cpu") => Device::Cpu,
        Some("cuda") => Device::Cuda,
        Some(x) => bail!("device = \"{x}\": cpu | cuda"),
    })
}

/// `cargo xtask eval --grid <файл>`.
pub fn run(a: &EvalArgs, p: &Paths, corpus: &Corpus, queries: &[Query], file: &Path) -> Result<()> {
    let text =
        std::fs::read_to_string(file).with_context(|| format!("чтение {}", file.display()))?;
    let g: GridFile =
        toml::from_str(&text).with_context(|| format!("разбор {}", file.display()))?;
    if g.config.is_empty() || g.config.len() > MAX_CONFIGS {
        bail!(
            "в сетке от 1 до {MAX_CONFIGS} конфигураций, задано {}",
            g.config.len()
        );
    }
    let device = parse_device(g.device.as_deref(), a.device)?;
    let mode = parse_mode(g.mode.as_deref())?;
    let (all_queries, part_of) = load_parts(p, corpus, queries, &g.part)?;
    let queries = &all_queries[..];
    let mut aa: Option<AaGate> = None;
    let model = a
        .model
        .clone()
        .or(g.model)
        .unwrap_or_else(|| run::default_model(device).to_string());
    let server = run::server_dir(a, p)?;
    let server_of = |c: &GridConfig| -> Result<std::path::PathBuf> {
        match &c.server_bin {
            Some(b) => run::build_server(&p.repo, Some(&p.repo.join(b))),
            None => Ok(server.clone()),
        }
    };
    let base_sets = run::parse_sets(&a.sets)?;
    // группы по порогам чанкинга, в порядке первого появления
    let mut groups: Vec<(IndexKey, Vec<usize>)> = Vec::new();
    for (i, c) in g.config.iter().enumerate() {
        let key = c.index_key();
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => v.push(i),
            None => groups.push((key, vec![i])),
        }
    }
    let mut results: Vec<Option<(RunResult, f64)>> = (0..g.config.len()).map(|_| None).collect();
    let mut leaf_counts = vec![0usize; g.config.len()];
    let mut volumes = vec![crate::eval::leaves::IndexVolume::default(); g.config.len()];
    let mut splits = vec![None; g.config.len()];
    let mut failed: Vec<String> = Vec::new();
    for (gi, (_, members)) in groups.iter().enumerate() {
        let mut session: Option<Session> = None;
        let mut index_secs = 0.0;
        for &ci in members {
            let c = &g.config[ci];
            let mut sets = base_sets.clone();
            sets.extend(c.set.iter().map(|(k, v)| (k.clone(), v.clone())));
            let spec = RunSpec {
                name: format!("grid-{gi}"),
                model: Some(model.clone()),
                device,
                reranker: None,
                sets,
                keep_cuda_path: a.keep_cuda_path,
                ctrl_break: false,
            };
            let fresh = session.is_none();
            if let Some(s) = session.take() {
                s.stop()?;
            }
            let mut s = Session::start(p, &spec, &server_of(c)?, fresh)?;
            s.check_model()?;
            if fresh {
                s.load_corpus(corpus)?;
            }
            s.wait_indexed()?;
            if fresh {
                index_secs = s.index_secs;
            }
            let runs = s.run_queries(queries, mode)?;
            if g.aa && ci == 0 {
                // A/A-гейт: ещё K − 1 снятий базы на том же индексе и том же сервере
                let mut again = Vec::new();
                for _ in 1..g.aa_k.unwrap_or(2).max(2) {
                    again.push(s.run_queries(queries, mode)?);
                }
                let gate = aa_gate(&runs, &again, &part_of);
                if gate.equal {
                    println!("A/A {}: побайтово", c.name);
                } else {
                    println!(
                        "A/A {}: расходятся {} запросов: {:?}",
                        c.name,
                        gate.differ.len(),
                        gate.differ
                    );
                }
                aa = Some(gate);
            }
            let r = run::result(&s, p, corpus, queries, mode, Some(&c.name), runs);
            println!(
                "[{}/{}] {}: nDCG@10 {}, recall@5 {}",
                ci + 1,
                g.config.len(),
                c.name,
                est(&r.metrics.overall.ndcg10),
                est(&r.metrics.overall.recall5)
            );
            let leaves = crate::eval::leaves::leaf_cores(&s.data)?;
            leaf_counts[ci] = leaves.len();
            volumes[ci] = crate::eval::leaves::index_volume(&s.data)?;
            splits[ci] = run::chunk_split(&s, queries)?;
            // контроль устаревания — и в сетке: конфигурация, у которой запросы superseded
            // не находят замещённых даже с историей, не годится для сравнения
            failed.extend(run::history_control(&r).map(|e| format!("{}: {e}", c.name)));
            results[ci] = Some((r, index_secs));
            session = Some(s);
        }
        if let Some(s) = session.take() {
            s.stop()?;
        }
    }
    let results: Vec<(RunResult, f64)> = results
        .into_iter()
        .map(|r| r.expect("все конфигурации выполнены"))
        .collect();
    let col = |runs: &[QueryRun], f: fn(&QueryRun) -> f64| runs.iter().map(f).collect::<Vec<f64>>();
    let base = &results[0].0.queries;
    let second = match &g.second_base {
        Some(name) => Some(
            g.config
                .iter()
                .position(|c| &c.name == name)
                .with_context(|| format!("second_base = \"{name}\": нет такой конфигурации"))?,
        ),
        None => None,
    };
    let base_dis: Vec<&QueryRun> = base.iter().filter(|q| q.disjoint).collect();
    let intro: std::collections::HashSet<&str> = queries
        .iter()
        .filter(|q| q.evidence.as_deref().is_some_and(is_intro_evidence))
        .map(|q| q.id.as_str())
        .collect();
    let configs = results
        .iter()
        .zip(&g.config)
        .zip(&leaf_counts)
        .zip(&splits)
        .zip(&volumes)
        .map(|(((((r, secs), c), &leaf_chunks), split), &volume)| {
            let q = &r.queries;
            let dis: Vec<&QueryRun> = q.iter().filter(|x| x.disjoint).collect();
            let d_ndcg10 = paired_diff(
                &col(base, |x| x.metrics.ndcg10),
                &col(q, |x| x.metrics.ndcg10),
            );
            ConfigResult {
                config: c.clone(),
                index_secs: *secs,
                metrics: r.metrics.clone(),
                d_recall5: paired_diff(
                    &col(base, |x| x.metrics.recall5),
                    &col(q, |x| x.metrics.recall5),
                ),
                d_mrr10: paired_diff(
                    &col(base, |x| x.metrics.mrr10),
                    &col(q, |x| x.metrics.mrr10),
                ),
                d_ndcg10,
                d_ndcg10_disjoint: paired_diff(
                    &base_dis
                        .iter()
                        .map(|x| x.metrics.ndcg10)
                        .collect::<Vec<_>>(),
                    &dis.iter().map(|x| x.metrics.ndcg10).collect::<Vec<_>>(),
                ),
                d_chunk_recall5: subset_diff(base, q, |x| x.metrics.chunk_recall5),
                d_chunk_recall5_with_overlap: subset_diff(base, q, |x| {
                    (x.kind == crate::eval::corpus::Kind::Chunk).then(|| {
                        if x.leaves.iter().take(5).any(|l| l.evidence_with_overlap) {
                            1.0
                        } else {
                            0.0
                        }
                    })
                }),
                split: split.clone(),
                d_chunk_ndcg10: subset_diff(base, q, |x| x.metrics.chunk_ndcg10),
                d_ndcg10_long: subset_diff(base, q, |x| x.long.then_some(x.metrics.ndcg10)),
                d_chunk_recall5_intro: intro_diff(base, q, &intro),
                d_reliable_first: subset_diff(base, q, |x| flag(x.metrics.reliable_first)),
                d_cancelled5: subset_diff(base, q, |x| flag(x.metrics.cancelled5)),
                vs_second: second.map(|si| {
                    let b = &results[si].0.queries;
                    SecondDiff {
                        base: g.config[si].name.clone(),
                        d_recall5: paired_diff(
                            &col(b, |x| x.metrics.recall5),
                            &col(q, |x| x.metrics.recall5),
                        ),
                        d_ndcg10: paired_diff(
                            &col(b, |x| x.metrics.ndcg10),
                            &col(q, |x| x.metrics.ndcg10),
                        ),
                        d_chunk_recall5: subset_diff(b, q, |x| x.metrics.chunk_recall5),
                        d_chunk_recall5_intro: intro_diff(b, q, &intro),
                        d_ndcg10_long: subset_diff(b, q, |x| x.long.then_some(x.metrics.ndcg10)),
                        parts: part_results(b, q, &part_of, g.config.len().saturating_sub(1)),
                    }
                }),
                leaf_chunks,
                volume,
                better: d_ndcg10.lo > 0.0,
                worse: d_ndcg10.hi < 0.0,
                per_query: q
                    .iter()
                    .map(|x| {
                        (
                            x.id.clone(),
                            x.metrics.recall5,
                            x.metrics.ndcg10,
                            x.metrics.chunk_recall5,
                        )
                    })
                    .collect(),
                parts: part_results(base, q, &part_of, g.config.len().saturating_sub(1)),
            }
        })
        .collect();
    let gr = GridResult {
        date: run::today(),
        git: run::git_rev(&p.repo),
        model,
        device: device.as_str().into(),
        notes: corpus.notes.len(),
        queries: queries.len(),
        mode: mode.as_str().into(),
        aa,
        configs,
    };
    // eval/grid.toml → `<дата>-grid-<модель>`, eval/grid-confirm.toml → `<дата>-grid-confirm-<модель>`
    let name = file
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| s.starts_with("grid"))
        .unwrap_or("grid");
    // режим — в имени, кроме гибрида (имена прежних сеток не меняются)
    let stem = match mode {
        Mode::Hybrid => format!("{}-{name}-{}", gr.date, gr.model),
        m => format!("{}-{name}-{}-{}", gr.date, gr.model, m.file_tag()),
    };
    let md = grid_md(&gr);
    let (json, mdp) = report::write_pair(&p.results, &stem, &gr, &md)?;
    println!("{md}\n{}\n{}", json.display(), mdp.display());
    if !failed.is_empty() {
        bail!("{}", failed.join("; "));
    }
    Ok(())
}

/// Δ chunk-recall@5 по запросам-сторожам `intro`.
fn intro_diff(
    base: &[QueryRun],
    q: &[QueryRun],
    intro: &std::collections::HashSet<&str>,
) -> Option<Estimate> {
    let (a, b): (Vec<f64>, Vec<f64>) = base
        .iter()
        .zip(q)
        .filter(|(x, _)| intro.contains(x.id.as_str()))
        .filter_map(|(x, y)| Some((x.metrics.chunk_recall5?, y.metrics.chunk_recall5?)))
        .unzip();
    (!a.is_empty()).then(|| paired_diff(&a, &b))
}

/// `evidence` запроса-сторожа «вводная фраза + блок»: фраза на «:», за ней через пустую
/// строку начало кода, списка или таблицы.
pub fn is_intro_evidence(ev: &str) -> bool {
    ev.split_once(":\n\n").is_some_and(|(_, rest)| {
        rest.starts_with("```")
            || rest.starts_with("- ")
            || rest.starts_with('|')
            || rest.starts_with(|c: char| c.is_ascii_digit())
    })
}

/// Парная разность по запросам, у которых метрика есть в обеих конфигурациях (одни id в
/// одном порядке); `None` — таких запросов нет.
fn subset_diff(
    base: &[QueryRun],
    q: &[QueryRun],
    f: fn(&QueryRun) -> Option<f64>,
) -> Option<Estimate> {
    let (a, b): (Vec<f64>, Vec<f64>) = base
        .iter()
        .zip(q)
        .filter_map(|(x, y)| Some((f(x)?, f(y)?)))
        .unzip();
    (!a.is_empty()).then(|| paired_diff(&a, &b))
}

/// Флаг запроса как доля: `true` — 1, `false` — 0.
fn flag(b: Option<bool>) -> Option<f64> {
    b.map(|b| if b { 1.0 } else { 0.0 })
}

fn opt_diff(e: &Option<Estimate>) -> String {
    e.as_ref().map_or("—".into(), diff)
}

/// Markdown сетки.
pub fn grid_md(g: &GridResult) -> String {
    let mut s = format!(
        "# Сетка весов и чанкинга: {} ({}, {})\n\n- дата {}, коммит `{}`, {} заметок, {} запросов\n\
         - разности — с первой (базовой) конфигурацией, парный бутстреп по запросам, 95 %\n\
         - «лучше»/«хуже» — интервал разности nDCG@10 целиком выше/ниже нуля\n\n",
        g.model, g.device, g.mode, g.date, g.git, g.notes, g.queries
    );
    s.push_str(
        "| Конфигурация | переопределения | индексация, с | листов | чанков / токенов индексации | recall@5 | nDCG@10 | chunk-recall@5 | Δ recall@5 | Δ MRR@10 | Δ nDCG@10 | Δ nDCG@10 без общих слов | Δ chunk-recall@5 | Δ chunk-recall@5 с перекрытием | Δ chunk-recall@5 сторожей «:» | разрезано evidence | Δ chunk-nDCG@10 | Δ nDCG@10 длинных | итог |\n\
         |---|---|---:|---:|---:|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n",
    );
    for c in &g.configs {
        let sets: Vec<String> = c
            .config
            .set
            .iter()
            .map(|(k, v)| format!("`{k}={v}`"))
            .collect();
        let _ = writeln!(
            s,
            "| {} | {} | {:.0} | {} | {} / {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            c.config.name,
            match (sets.is_empty(), &c.config.server_bin) {
                (true, None) => "умолчания".into(),
                (_, Some(b)) => format!("`server_bin={b}` {}", sets.join(", ")),
                (false, None) => sets.join(", "),
            },
            c.index_secs,
            c.leaf_chunks,
            c.volume.chunks,
            c.volume.tokens(),
            est(&c.metrics.overall.recall5),
            est(&c.metrics.overall.ndcg10),
            c.metrics
                .overall
                .chunk_recall5
                .as_ref()
                .map_or("—".into(), est),
            diff(&c.d_recall5),
            diff(&c.d_mrr10),
            diff(&c.d_ndcg10),
            diff(&c.d_ndcg10_disjoint),
            opt_diff(&c.d_chunk_recall5),
            opt_diff(&c.d_chunk_recall5_with_overlap),
            opt_diff(&c.d_chunk_recall5_intro),
            c.split.as_ref().map_or("—".into(), |s| format!(
                "{} из {} ({} целиком с перекрытием): {}",
                s.split,
                s.queries,
                s.split_whole_with_overlap,
                s.split_ids.join(", ")
            )),
            opt_diff(&c.d_chunk_ndcg10),
            opt_diff(&c.d_ndcg10_long),
            if c.better {
                "лучше"
            } else if c.worse {
                "хуже"
            } else {
                "в пределах шума"
            }
        );
    }
    if g.configs.iter().any(|c| c.d_reliable_first.is_some()) {
        s.push_str(
            "\n## Метрики M3: надёжное выше гипотезы и отменённое\n\n\
             | Конфигурация | reliable_first | Δ reliable_first | cancelled@5 | Δ cancelled@5 | Δ nDCG@10 | Δ recall@5 |\n\
             |---|---|---|---|---|---|---|\n",
        );
        for c in &g.configs {
            let o = &c.metrics.overall;
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} | {} | {} |",
                c.config.name,
                o.reliable_first.as_ref().map_or("—".into(), est),
                opt_diff(&c.d_reliable_first),
                o.cancelled5.as_ref().map_or("—".into(), est),
                opt_diff(&c.d_cancelled5),
                diff(&c.d_ndcg10),
                diff(&c.d_recall5),
            );
        }
    }
    if let Some(base) = g
        .configs
        .iter()
        .find_map(|c| c.vs_second.as_ref().map(|v| v.base.clone()))
    {
        let _ = write!(
            s,
            "\n## Разности со второй базой «{base}»\n\n\
             | Конфигурация | Δ recall@5 | Δ nDCG@10 | Δ chunk-recall@5 | Δ chunk-recall@5 сторожей «:» | Δ nDCG@10 длинных |\n\
             |---|---|---|---|---|---|\n"
        );
        for c in &g.configs {
            if let Some(v) = &c.vs_second {
                let _ = writeln!(
                    s,
                    "| {} | {} | {} | {} | {} | {} |",
                    c.config.name,
                    diff(&v.d_recall5),
                    diff(&v.d_ndcg10),
                    opt_diff(&v.d_chunk_recall5),
                    opt_diff(&v.d_chunk_recall5_intro),
                    opt_diff(&v.d_ndcg10_long)
                );
            }
        }
    }
    if let Some(gate) = &g.aa {
        let _ = writeln!(
            s,
            "\n## A/A-гейт базы\n\n- снятий {}; выдача {}; порог шума (p95 |Δ nDCG@10| запроса): {}\n\
             - по метрике (запросов с Δ nDCG@10 ≠ 0 / максимум |Δ|): {}",
            gate.snapshots,
            if gate.equal {
                "побайтово равна".to_string()
            } else {
                format!(
                    "расходится у {} запросов: {}",
                    gate.differ.len(),
                    gate.differ.join(", ")
                )
            },
            gate.noise_p95
                .iter()
                .map(|(k, v)| format!("{k} {v:.3}"))
                .collect::<Vec<_>>()
                .join(", "),
            gate.ndcg_changed
                .iter()
                .map(|(k, n)| format!(
                    "{k} {n} / {:.3}",
                    gate.ndcg_max_abs.get(k).copied().unwrap_or(0.0)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let part_names: Vec<&String> = g
        .configs
        .first()
        .map(|c| c.parts.keys().collect())
        .unwrap_or_default();
    for pn in part_names {
        let _ = write!(
            s,
            "\n## Часть «{pn}»\n\nΔ — к первой конфигурации, парный бутстреп запросов 10 000, \
             односторонние 95 % (`lo`); «lo Бонф.» — с поправкой Бонферрони на {} сравнений.\n\n\
             | Конфигурация | n | nDCG@10 | доля nDCG@10 < 1 | reliable_first (n) | Δ nDCG@10 [lo…hi] | lo Бонф. | лучше / хуже / равно |\n\
             |---|---:|---:|---:|---|---|---:|---|\n",
            g.configs.len().saturating_sub(1)
        );
        for c in &g.configs {
            if let Some(r) = c.parts.get(pn) {
                let _ = writeln!(
                    s,
                    "| {} | {} | {:.3} | {:.3} | {} | {:+.4} [{:+.4}…{:+.4}] | {:+.4} | {} / {} / {} |",
                    c.config.name,
                    r.n,
                    r.ndcg10,
                    r.below1,
                    r.reliable_first
                        .map_or("—".into(), |v| format!("{v:.3} ({})", r.n_reliable)),
                    r.d_ndcg10.mean,
                    r.d_ndcg10.lo,
                    r.d_ndcg10.hi,
                    r.d_ndcg10_lo_bonf,
                    r.wins,
                    r.losses,
                    r.ties
                );
            }
        }
        // те же части против второй базы (`second_base`)
        for c in &g.configs {
            if let Some(r) = c
                .vs_second
                .as_ref()
                .and_then(|v| v.parts.get(pn).map(|r| (v.base.as_str(), r)))
            {
                let (b, r) = r;
                let _ = writeln!(
                    s,
                    "| {} − {b} | {} | {:.3} | {:.3} | — | {:+.4} [{:+.4}…{:+.4}] | {:+.4} | {} / {} / {} |",
                    c.config.name,
                    r.n,
                    r.ndcg10,
                    r.below1,
                    r.d_ndcg10.mean,
                    r.d_ndcg10.lo,
                    r.d_ndcg10.hi,
                    r.d_ndcg10_lo_bonf,
                    r.wins,
                    r.losses,
                    r.ties
                );
            }
        }
    }
    s.push_str("\n## nDCG@10 по видам запросов\n\n| Конфигурация |");
    let kinds: Vec<&String> = g
        .configs
        .first()
        .map(|c| c.metrics.by_kind.keys().collect())
        .unwrap_or_default();
    for k in &kinds {
        let _ = write!(s, " {k} |");
    }
    s.push_str("\n|---|");
    for _ in &kinds {
        s.push_str("---|");
    }
    s.push('\n');
    for c in &g.configs {
        let _ = write!(s, "| {} |", c.config.name);
        for k in &kinds {
            let v = c
                .metrics
                .by_kind
                .get(*k)
                .map(|a| a.ndcg10.mean)
                .unwrap_or(f64::NAN);
            let _ = write!(s, " {v:.3} |");
        }
        s.push('\n');
    }
    s
}

/// Точка сетки замера M6 (`eval --tokens --via mcp --grid <файл>`, спецификация M6 §3.5):
/// `[[point]] name = "…"; env = { ZAM_SEARCH__BUDGET_TOKENS = "600", … }`. Первая точка —
/// база сравнения точек. Переопределения отбираются по версии сервера
/// ([`crate::eval::caps::filter_overrides`]).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub name: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PointFile {
    #[serde(default)]
    point: Vec<Point>,
}

/// Сетка подбирается только на dev: приёмка (holdout) — одна конфигурация, выбранная до неё
/// (спецификация M6 §3.5, I1).
pub fn refuse_holdout(grid: Option<&Path>, part: Option<crate::eval::split::Part>) -> Result<()> {
    if let (Some(g), Some(crate::eval::split::Part::Holdout)) = (grid, part) {
        bail!(
            "сетка {} подбирается только на dev (--split dev); --split holdout — приёмка \
             выбранных значений, без сетки (спецификация M6 §3.5)",
            g.display()
        );
    }
    Ok(())
}

/// Точки сетки M6: непустой список, имена уникальны (в имени файла результата — буквы, цифры,
/// `-`, `_`, `.`), ключи — `ZAM_<РАЗДЕЛ>__<КЛЮЧ>`, не больше [`MAX_CONFIGS`].
pub fn load_points(path: &Path) -> Result<Vec<Point>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("чтение сетки {}", path.display()))?;
    let f: PointFile =
        toml::from_str(&text).with_context(|| format!("разбор сетки {}", path.display()))?;
    validate_points(&f.point, path)?;
    Ok(f.point)
}

/// Проверка точек сетки M6 ([`load_points`]; у сетки подсказок — свой файл с правилом).
pub fn validate_points(points: &[Point], path: &Path) -> Result<()> {
    let mut errs = Vec::new();
    if points.is_empty() {
        errs.push("нет ни одной [[point]]".to_string());
    }
    if points.len() > MAX_CONFIGS {
        errs.push(format!("точек {} > {MAX_CONFIGS}", points.len()));
    }
    let mut names = std::collections::BTreeSet::new();
    for p in points {
        let ok_name = !p.name.is_empty()
            && p.name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !ok_name {
            errs.push(format!("имя точки «{}»: буквы, цифры, -, _, .", p.name));
        }
        if !names.insert(p.name.as_str()) {
            errs.push(format!("точка {} повторяется", p.name));
        }
        for k in p.env.keys() {
            if crate::eval::caps::env_to_key(k).is_none() {
                errs.push(format!("{}: {k} — ожидается ZAM_<РАЗДЕЛ>__<КЛЮЧ>", p.name));
            }
        }
    }
    if !errs.is_empty() {
        bail!("сетка {}:\n  {}", path.display(), errs.join("\n  "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_file_format() {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/xtask/grid-points");
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("g.toml");
        std::fs::write(
            &f,
            "[[point]]\nname = \"base\"\n\n[[point]]\nname = \"b600\"\nenv = { ZAM_SEARCH__BUDGET_TOKENS = \"600\" }\n",
        )
        .unwrap();
        let p = load_points(&f).unwrap();
        assert_eq!(p.len(), 2);
        assert!(p[0].env.is_empty());
        assert_eq!(p[1].env["ZAM_SEARCH__BUDGET_TOKENS"], "600");
        std::fs::write(&f, "[[point]]\nname = \"x y\"\nenv = { SEARCH = \"1\" }\n").unwrap();
        let e = load_points(&f).unwrap_err().to_string();
        assert!(e.contains("x y") && e.contains("SEARCH"), "{e}");
        use crate::eval::split::Part;
        assert!(refuse_holdout(Some(&f), Some(Part::Holdout)).is_err());
        assert!(refuse_holdout(Some(&f), Some(Part::Dev)).is_ok());
        assert!(refuse_holdout(None, Some(Part::Holdout)).is_ok());
    }

    #[test]
    fn intro_evidence_is_colon_phrase_and_block_start() {
        assert!(is_intro_evidence("Пример:\n\n```yaml\nkey: v"));
        assert!(is_intro_evidence("Steps:\n\n- stop writes"));
        assert!(is_intro_evidence("Таблица:\n\n| a | b |"));
        assert!(!is_intro_evidence(
            "Обычная фраза: продолжение в той же строке."
        ));
        assert!(!is_intro_evidence(
            "##### Заголовок\n\nТекст после заголовка."
        ));
    }

    #[test]
    fn part_result_one_sided_and_counts() {
        // 10 запросов: 4 лучше на 0,5, 1 хуже на 0,5, 5 равно
        let a = vec![0.5; 10];
        let mut b = vec![0.5; 10];
        for v in b.iter_mut().take(4) {
            *v = 1.0;
        }
        b[9] = 0.0;
        let r = part_result(&a, &b, &[1.0, 0.0], 8);
        assert_eq!((r.wins, r.losses, r.ties), (4, 1, 5));
        assert!((r.d_ndcg10.mean - 0.15).abs() < 1e-12);
        assert!(r.d_ndcg10_lo_bonf <= r.d_ndcg10.lo, "{r:?}");
        assert!((r.below1 - 0.6).abs() < 1e-12);
        assert_eq!((r.reliable_first, r.n_reliable), (Some(0.5), 2));
        assert_eq!(p95(vec![0.0; 19].into_iter().chain([1.0]).collect()), 0.0);
        assert_eq!(p95(vec![0.0, 1.0]), 1.0);
        assert!(parse_mode(Some("hybrid+rerank")).is_ok() && parse_mode(Some("fts")).is_err());
    }

    /// A/A-гейт по K снятиям: выдача (путь и балл) и метрика по частям — число запросов с
    /// Δ nDCG@10 ≠ 0 хотя бы в одной паре и максимум |Δ| (задача 11).
    #[test]
    fn aa_gate_k_snapshots_by_metric() {
        let run = |id: &str, ndcg: f64, score: f64| QueryRun {
            id: id.into(),
            text: String::new(),
            kind: crate::eval::corpus::Kind::Paraphrase,
            lang: crate::eval::corpus::Lang::En,
            disjoint: false,
            expect: Vec::new(),
            also: Vec::new(),
            forbid: Vec::new(),
            weaker: Vec::new(),
            long: false,
            fts_only: false,
            metrics: crate::eval::metrics::QueryMetrics {
                ndcg10: ndcg,
                ..Default::default()
            },
            cancelled5_history: None,
            reliable_first_no_boost: None,
            leaves: Vec::new(),
            hits: vec![run::Hit {
                path: "p".into(),
                score,
                sources: Vec::new(),
                reranked: false,
            }],
            ms: 0.0,
            server_ms: serde_json::Value::Null,
        };
        let part_of: BTreeMap<String, String> = [("a", "main"), ("b", "area_same")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let first = vec![run("a", 1.0, 0.5), run("b", 0.5, 0.4)];
        let g = aa_gate(&first, &[first.clone(), first.clone()], &part_of);
        assert!(g.equal && g.differ.is_empty(), "{g:?}");
        assert_eq!(g.snapshots, 3);
        assert_eq!(g.ndcg_changed["area_same"], 0);
        // третье снятие: у «a» сдвинут балл (метрика та же), у «b» nDCG@10 другой
        let third = vec![run("a", 1.0, 0.51), run("b", 0.25, 0.4)];
        let g = aa_gate(&first, &[first.clone(), third], &part_of);
        assert!(!g.equal);
        assert_eq!(g.differ, vec!["a".to_string()]);
        assert_eq!(
            (g.ndcg_changed["main"], g.ndcg_changed["area_same"]),
            (0, 1)
        );
        assert!((g.ndcg_max_abs["area_same"] - 0.25).abs() < 1e-12);
    }

    #[test]
    fn server_bin_splits_index_groups() {
        let c = |bin: Option<&str>| GridConfig {
            name: "x".into(),
            set: BTreeMap::new(),
            server_bin: bin.map(str::to_string),
        };
        assert_ne!(
            c(None).index_key(),
            c(Some("m2/zam-server.exe")).index_key()
        );
        assert_eq!(c(None).index_key(), c(None).index_key());
    }
}
