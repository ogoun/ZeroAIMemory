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
    #[serde(default)]
    config: Vec<GridConfig>,
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
        "# Сетка весов и чанкинга: {} ({})\n\n- дата {}, коммит `{}`, {} заметок, {} запросов\n\
         - разности — с первой (базовой) конфигурацией, парный бутстреп по запросам, 95 %\n\
         - «лучше»/«хуже» — интервал разности nDCG@10 целиком выше/ниже нуля\n\n",
        g.model, g.device, g.date, g.git, g.notes, g.queries
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

#[cfg(test)]
mod tests {
    use super::*;

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
