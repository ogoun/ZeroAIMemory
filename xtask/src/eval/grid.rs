//! Подбор весов `[search]` и порогов чанкинга `[index]` по сетке (задача 14 M2, шаг 3).
//!
//! Файл сетки (`eval/grid.toml`): модель, устройство и до 12 конфигураций `[[config]]` —
//! переопределения `set = { "search.w_fts" = "1.5", "index.chunk_target_max" = "800" }`.
//! Первая конфигурация — базовая (обычно текущие умолчания). Конфигурации с одинаковыми
//! порогами чанкинга (`index.chunk_*`) делят один индекс: корпус загружается и индексируется
//! один раз, для остальных сервер перезапускается на том же каталоге с другими весами.
//!
//! Сравнение с базовой — парный бутстреп разности по запросам ([`paired_diff`]): выигрыш
//! считается устойчивым, если нижняя граница 95 % интервала разности nDCG@10 больше нуля.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::corpus::{Corpus, Query};
use super::metrics::{Estimate, paired_diff};
use super::report::{self, est};
use super::run::{self, QueryRun, RunResult, RunSpec, Session};
use super::{Device, EvalArgs, Mode, Paths};

/// Предел числа конфигураций сетки.
pub const MAX_CONFIGS: usize = 12;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GridFile {
    model: Option<String>,
    device: Option<String>,
    #[serde(default)]
    config: Vec<GridConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GridConfig {
    pub name: String,
    #[serde(default)]
    pub set: BTreeMap<String, String>,
}

/// Пороги чанкинга конфигурации: (ключ, значение).
type IndexKey = Vec<(String, String)>;

impl GridConfig {
    /// Ключ индекса: пороги чанкинга (всё, что меняет чанки).
    fn index_key(&self) -> IndexKey {
        self.set
            .iter()
            .filter(|(k, _)| k.starts_with("index.chunk_"))
            .map(|(k, v)| (k.clone(), v.clone()))
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
    /// Устойчиво лучше базовой: нижняя граница интервала разности nDCG@10 > 0.
    pub better: bool,
    /// Устойчиво хуже: верхняя граница < 0.
    pub worse: bool,
    pub per_query: Vec<(String, f64, f64)>,
}

#[derive(Debug, Serialize)]
pub struct GridResult {
    pub date: String,
    pub git: String,
    pub model: String,
    pub device: String,
    pub notes: usize,
    pub queries: usize,
    pub configs: Vec<ConfigResult>,
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
    let model = a
        .model
        .clone()
        .or(g.model)
        .unwrap_or_else(|| run::default_model(device).to_string());
    let server = run::server_dir(a, p)?;
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
            };
            let fresh = session.is_none();
            if let Some(s) = session.take() {
                s.stop()?;
            }
            let mut s = Session::start(p, &spec, &server, fresh)?;
            s.check_model()?;
            if fresh {
                s.load_corpus(corpus)?;
            }
            s.wait_indexed()?;
            if fresh {
                index_secs = s.index_secs;
            }
            let runs = s.run_queries(queries, Mode::Hybrid)?;
            let r = run::result(&s, p, corpus, queries, Mode::Hybrid, Some(&c.name), runs);
            println!(
                "[{}/{}] {}: nDCG@10 {}, recall@5 {}",
                ci + 1,
                g.config.len(),
                c.name,
                est(&r.metrics.overall.ndcg10),
                est(&r.metrics.overall.recall5)
            );
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
    let base_dis: Vec<&QueryRun> = base.iter().filter(|q| q.disjoint).collect();
    let configs = results
        .iter()
        .zip(&g.config)
        .map(|((r, secs), c)| {
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
                better: d_ndcg10.lo > 0.0,
                worse: d_ndcg10.hi < 0.0,
                per_query: q
                    .iter()
                    .map(|x| (x.id.clone(), x.metrics.recall5, x.metrics.ndcg10))
                    .collect(),
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
        configs,
    };
    // eval/grid.toml → `<дата>-grid-<модель>`, eval/grid-confirm.toml → `<дата>-grid-confirm-<модель>`
    let name = file
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| s.starts_with("grid"))
        .unwrap_or("grid");
    let stem = format!("{}-{name}-{}", gr.date, gr.model);
    let md = grid_md(&gr);
    let (json, mdp) = report::write_pair(&p.results, &stem, &gr, &md)?;
    println!("{md}\n{}\n{}", json.display(), mdp.display());
    Ok(())
}

fn diff(e: &Estimate) -> String {
    format!("{:+.3} [{:+.3}…{:+.3}]", e.mean, e.lo, e.hi)
}

/// Markdown сетки.
pub fn grid_md(g: &GridResult) -> String {
    let mut s = format!(
        "# Сетка весов и чанкинга: {} ({})\n\n- дата {}, коммит `{}`, {} заметок, {} запросов\n\
         - разности — с первой (базовой) конфигурацией, парный бутстреп по запросам, 95 %\n\
         - «лучше»/«хуже» — интервал разности nDCG@10 целиком выше/ниже нуля\n\n",
        g.model, g.device, g.date, g.git, g.notes, g.queries
    );
    s.push_str(
        "| Конфигурация | переопределения | индексация, с | recall@5 | nDCG@10 | Δ recall@5 | Δ MRR@10 | Δ nDCG@10 | Δ nDCG@10 без общих слов | итог |\n\
         |---|---|---:|---|---|---|---|---|---|---|\n",
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
            "| {} | {} | {:.0} | {} | {} | {} | {} | {} | {} | {} |",
            c.config.name,
            if sets.is_empty() {
                "умолчания".into()
            } else {
                sets.join(", ")
            },
            c.index_secs,
            est(&c.metrics.overall.recall5),
            est(&c.metrics.overall.ndcg10),
            diff(&c.d_recall5),
            diff(&c.d_mrr10),
            diff(&c.d_ndcg10),
            diff(&c.d_ndcg10_disjoint),
            if c.better {
                "лучше"
            } else if c.worse {
                "хуже"
            } else {
                "в пределах шума"
            }
        );
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
