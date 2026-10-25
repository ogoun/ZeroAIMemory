//! Гейт качества int8 (исследование 05 §3.3, задача 14 M2): `bge-m3-int8` против fp32-эталона
//! BAAI (`bge-m3-fp32`, `role = "reference"` в `deploy/models.toml`).
//!
//! Обе модели прогоняются полным eval (`hybrid`) на одном корпусе в отдельных каталогах;
//! затем эмбеддинги листов и сводок (`chunks` ⨝ `embeddings`, без служебных строк L0) читаются
//! из SQLite каждого прогона и сопоставляются по ключу эмбеддируемой строки (`embed_key`):
//! токенизатор у моделей один, поэтому чанки совпадают. Сравниваются ровно те векторы, что
//! лежат в индексе (dense — f16, sparse — пары u32 + f16).
//!
//! **Вердикт** (решение владельца 2026-10-21, ТЗ §8.4, §18.2): «пройден», если нижняя граница
//! 95 % интервала парного бутстрепа (10 000 выборок, фиксированное зерно) разности
//! кандидат − fp32 не ниже −0,01 и для nDCG@10, и для recall@5.
//!
//! **Справочно** (в отчёте, на вердикт не влияют — пороги M2):
//! - косинус dense: средний ≥ 0,99, минимальный ≥ 0,97;
//! - корреляция Спирмена весов sparse (объединение токенов, отсутствующий = 0): средняя ≥ 0,95.
//!
//! Если доступна CUDA (`target/deps/<платформа>/onnxruntime-gpu`), так же сравнивается fp16 —
//! для решения «одна модель CPU и GPU или разные `model_id`» (исследование 05 §3.3); на
//! вердикт гейта он не влияет.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use half::f16;
use serde::Serialize;

use super::corpus::{Corpus, Query};
use super::metrics::{QueryMetrics, cosine, paired_diff_with, quantile, spearman};
use super::report::{self, est};
use super::run::{self, RunResult, RunSpec, Session};
use super::{Device, EvalArgs, Mode, Paths};
use crate::manifest::{MODELS_MANIFEST, ModelsManifest};

pub const INT8: &str = "bge-m3-int8";
pub const FP16: &str = "bge-m3-fp16";
pub const FP32: &str = "bge-m3-fp32";

const DENSE_MEAN_MIN: f64 = 0.99;
const DENSE_MIN_MIN: f64 = 0.97;
const SPARSE_MEAN_MIN: f64 = 0.95;
/// Допустимое ухудшение recall@5 и nDCG@10 (доля, 1 п. п.).
const METRIC_DROP_MAX: f64 = 0.01;
/// Выборок парного бутстрепа разности метрик гейта (решение владельца 2026-10-21).
const GATE_BOOTSTRAP: usize = 10_000;

/// Эмбеддинг из базы прогона.
struct Vecs {
    dense: Vec<f32>,
    sparse: Vec<(u32, f32)>,
    level: String,
}

/// Разреженный вектор: (id токена, вес).
type Sparse = Vec<(u32, f32)>;

/// dense — подряд f16 LE; sparse — пары (u32 LE, f16 LE) (формат `zam_store::embeddings`).
fn decode(dense: &[u8], sparse: &[u8]) -> Result<(Vec<f32>, Sparse)> {
    let (d, d_rest) = dense.as_chunks::<2>();
    let (s, s_rest) = sparse.as_chunks::<6>();
    if !d_rest.is_empty() || !s_rest.is_empty() {
        bail!(
            "повреждённый эмбеддинг: dense {} Б, sparse {} Б",
            dense.len(),
            sparse.len()
        );
    }
    let d = d.iter().map(|c| f16::from_le_bytes(*c).to_f32()).collect();
    let s = s
        .iter()
        .map(|c| {
            (
                u32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                f16::from_le_bytes([c[4], c[5]]).to_f32(),
            )
        })
        .collect();
    Ok((d, s))
}

/// База пользователя прогона (единственный пользователь — администратор).
pub(crate) fn user_db(data: &Path) -> Result<PathBuf> {
    let dir = data.join("users");
    let dbs: Vec<PathBuf> = fs::read_dir(&dir)
        .with_context(|| format!("чтение {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("db"))
        .collect();
    match dbs.as_slice() {
        [one] => Ok(one.clone()),
        _ => bail!(
            "в {} ожидалась одна база пользователя: {dbs:?}",
            dir.display()
        ),
    }
}

/// Эмбеддинги чанков прогона по `embed_key`.
fn load_vectors(data: &Path) -> Result<HashMap<String, Vecs>> {
    let c = rusqlite::Connection::open(user_db(data)?)?;
    let mut st = c.prepare(
        "SELECT e.text_hash, e.dense, e.sparse, MIN(c.level)
         FROM embeddings e JOIN chunks c ON c.embed_key = e.text_hash
         WHERE e.owner IS NULL
         GROUP BY e.text_hash",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Vec<u8>>(1)?,
            r.get::<_, Vec<u8>>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (k, d, s, level) = row?;
        let (dense, sparse) = decode(&d, &s)?;
        out.insert(
            k,
            Vecs {
                dense,
                sparse,
                level,
            },
        );
    }
    Ok(out)
}

/// Сравнение векторов двух моделей.
#[derive(Debug, Clone, Serialize)]
pub struct VectorCompare {
    pub a: String,
    pub b: String,
    /// Общих чанков (по `embed_key`).
    pub chunks: usize,
    pub chunks_by_level: BTreeMap<String, usize>,
    pub dense_mean: f64,
    pub dense_min: f64,
    pub dense_p01: f64,
    pub sparse_spearman_mean: f64,
    pub sparse_spearman_min: f64,
    pub sparse_spearman_p05: f64,
    /// Чанков без sparse-весов у обеих моделей (Спирмен не определён).
    pub sparse_skipped: usize,
}

fn compare(
    a_name: &str,
    a: &HashMap<String, Vecs>,
    b_name: &str,
    b: &HashMap<String, Vecs>,
) -> Result<VectorCompare> {
    let mut dense = Vec::new();
    let mut sp = Vec::new();
    let mut skipped = 0;
    let mut by_level: BTreeMap<String, usize> = BTreeMap::new();
    for (k, va) in a {
        let Some(vb) = b.get(k) else { continue };
        dense.push(cosine(&va.dense, &vb.dense));
        *by_level.entry(va.level.clone()).or_default() += 1;
        // объединение токенов; отсутствующий вес — 0
        let mut ids: BTreeMap<u32, (f64, f64)> = BTreeMap::new();
        for (id, w) in &va.sparse {
            ids.entry(*id).or_default().0 = f64::from(*w);
        }
        for (id, w) in &vb.sparse {
            ids.entry(*id).or_default().1 = f64::from(*w);
        }
        let (x, y): (Vec<f64>, Vec<f64>) = ids.values().copied().unzip();
        match spearman(&x, &y) {
            Some(r) => sp.push(r),
            None => skipped += 1,
        }
    }
    if dense.is_empty() {
        bail!("у прогонов {a_name} и {b_name} нет общих чанков");
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
    let min = |v: &[f64]| v.iter().copied().fold(f64::INFINITY, f64::min);
    Ok(VectorCompare {
        a: a_name.into(),
        b: b_name.into(),
        chunks: dense.len(),
        chunks_by_level: by_level,
        dense_mean: mean(&dense),
        dense_min: min(&dense),
        dense_p01: quantile(&dense, 0.01),
        sparse_spearman_mean: mean(&sp),
        sparse_spearman_min: if sp.is_empty() { f64::NAN } else { min(&sp) },
        sparse_spearman_p05: quantile(&sp, 0.05),
        sparse_skipped: skipped,
    })
}

/// Проверка порога.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub value: f64,
    pub threshold: String,
    pub pass: bool,
    /// 95 % интервал парного бутстрепа разности по запросам (только для метрик поиска).
    pub ci: Option<[f64; 2]>,
    /// Справочная проверка: в отчёте, на вердикт не влияет.
    #[serde(default)]
    pub informational: bool,
}

fn check(name: &str, value: f64, threshold: String, pass: bool) -> Check {
    Check {
        name: name.into(),
        value,
        threshold,
        pass,
        ci: None,
        informational: false,
    }
}

/// Разность метрики кандидата и эталона по одним запросам (`reference`, `candidate` — значения
/// метрики по запросам в одном порядке): среднее и 95 % интервал парного бутстрепа
/// ([`GATE_BOOTSTRAP`] выборок, фиксированное зерно). Пройдено, если **нижняя граница**
/// интервала не ниже −[`METRIC_DROP_MAX`] (решение владельца 2026-10-21): среднее и верхняя
/// граница на вердикт не влияют.
fn metric_check_values(name: &str, reference: &[f64], candidate: &[f64]) -> Check {
    let d = paired_diff_with(reference, candidate, GATE_BOOTSTRAP);
    Check {
        ci: Some([d.lo, d.hi]),
        ..check(
            name,
            d.mean,
            format!("нижняя граница 95 % ≥ −{METRIC_DROP_MAX}"),
            d.lo >= -METRIC_DROP_MAX,
        )
    }
}

/// [`metric_check_values`] по результатам прогонов кандидата `m` и эталона `r`.
fn metric_check(name: &str, m: &RunResult, r: &RunResult, f: fn(&QueryMetrics) -> f64) -> Check {
    let col = |x: &RunResult| x.queries.iter().map(|q| f(&q.metrics)).collect::<Vec<_>>();
    metric_check_values(name, &col(r), &col(m))
}

/// Вердикт гейта: все проверки, кроме справочных (векторы), пройдены.
fn verdict(checks: &[Check]) -> bool {
    checks.iter().filter(|c| !c.informational).all(|c| c.pass)
}

/// Проверки одной модели против эталона.
fn checks(v: &VectorCompare, m: &RunResult, r: &RunResult) -> Vec<Check> {
    let mut out = vector_checks(v);
    out.extend([
        metric_check("recall@5: разница с fp32", m, r, |q| q.recall5),
        metric_check("nDCG@10: разница с fp32", m, r, |q| q.ndcg10),
    ]);
    out
}

/// Проверки векторов (косинус dense, Спирмен sparse) — справочные: на вердикт не влияют
/// (решение владельца 2026-10-21), пороги M2 остаются в отчёте для сравнения.
fn vector_checks(v: &VectorCompare) -> Vec<Check> {
    let mut out = vec![
        check(
            "dense: средний косинус",
            v.dense_mean,
            format!("≥ {DENSE_MEAN_MIN}"),
            v.dense_mean >= DENSE_MEAN_MIN,
        ),
        check(
            "dense: минимальный косинус",
            v.dense_min,
            format!("≥ {DENSE_MIN_MIN}"),
            v.dense_min >= DENSE_MIN_MIN,
        ),
        check(
            "sparse: средний Спирмен",
            v.sparse_spearman_mean,
            format!("≥ {SPARSE_MEAN_MIN}"),
            v.sparse_spearman_mean >= SPARSE_MEAN_MIN,
        ),
    ];
    for c in &mut out {
        c.informational = true;
    }
    out
}

/// Итог гейта.
#[derive(Debug, Serialize)]
pub struct GateResult {
    pub date: String,
    pub git: String,
    pub reference: String,
    pub candidate: String,
    pub pass: bool,
    pub checks: Vec<Check>,
    pub vectors: Vec<VectorCompare>,
    /// Метрики `hybrid` по моделям: overall.
    pub runs: BTreeMap<String, RunSummary>,
    /// fp16: проверки против эталона (на вердикт не влияют) или причина пропуска.
    pub fp16_checks: Vec<Check>,
    pub fp16_note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunSummary {
    pub model_id: Option<String>,
    pub device: Option<String>,
    pub index_secs: f64,
    pub overall: super::metrics::Aggregate,
    pub disjoint: super::metrics::Aggregate,
    pub result_file: String,
}

/// Устанавливает fp32-эталон в `target/models`, если его нет (≈ 2,3 ГБ).
fn ensure_reference(p: &Paths) -> Result<()> {
    let manifest = ModelsManifest::load(&p.repo.join("deploy").join(MODELS_MANIFEST))?;
    let m = manifest.get(FP32)?;
    let models = p.repo.join("target").join("models");
    if zam_fetch::model_state(m, &models, false) != zam_fetch::ModelState::Installed {
        println!(
            "загрузка эталона {FP32} ({:.0} МБ) в {}",
            m.total_size() as f64 / 1e6,
            models.join(FP32).display()
        );
        zam_fetch::fetch_model(&manifest, FP32, &models, false)?;
    }
    Ok(())
}

/// Общее для прогонов гейта.
#[derive(Clone, Copy)]
struct Ctx<'a> {
    a: &'a EvalArgs,
    p: &'a Paths,
    server: &'a Path,
    corpus: &'a Corpus,
    queries: &'a [Query],
}

/// Полный прогон `hybrid` модели в каталоге `gate-<модель>`: результат и векторы.
fn eval_model(
    c: &Ctx,
    model: &str,
    device: Device,
    reuse: bool,
) -> Result<(RunResult, HashMap<String, Vecs>, PathBuf)> {
    let Ctx {
        a,
        p,
        server,
        corpus,
        queries,
    } = *c;
    let spec = RunSpec {
        name: format!("gate-{model}"),
        model: Some(model.into()),
        device,
        reranker: None,
        sets: run::parse_sets(&a.sets)?,
        keep_cuda_path: a.keep_cuda_path,
        ctrl_break: false,
    };
    let t0 = Instant::now();
    // --reuse: прогон эталона (и fp16), уже проиндексированный в target/eval/gate-<модель>
    let reuse = reuse && p.work.join(&spec.name).join("data").is_dir();
    let mut s = Session::start(p, &spec, server, !reuse)?;
    s.check_model()?;
    if !reuse {
        s.load_corpus(corpus)?;
    }
    s.wait_indexed()?;
    let runs = s.run_queries(queries, Mode::Hybrid)?;
    let r = run::result(&s, p, corpus, queries, Mode::Hybrid, Some("gate"), runs);
    let data = s.data.clone();
    s.stop()?;
    // повторный прогон на готовом индексе не затирает файл первого (с временем индексации)
    let stem = report::file_stem(&r);
    let json = if reuse {
        p.results.join(format!("{stem}.json"))
    } else {
        report::write_run(&p.results, &r)?.0
    };
    report::print_run(&r);
    println!(
        "  {model}: индексация {:.0} с, всего {:.0} с",
        r.info.index_secs,
        t0.elapsed().as_secs_f64()
    );
    let v = load_vectors(&data)?;
    Ok((r, v, json))
}

fn summary(r: &RunResult, file: &Path) -> RunSummary {
    RunSummary {
        model_id: r.info.model_id.clone(),
        device: r.info.device.clone(),
        index_secs: r.info.index_secs,
        overall: r.metrics.overall,
        disjoint: r.metrics.disjoint,
        result_file: file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

/// `cargo xtask eval --quality-gate`.
pub fn run(a: &EvalArgs, p: &Paths, corpus: &Corpus, queries: &[Query]) -> Result<()> {
    ensure_reference(p)?;
    let server = run::server_dir(a, p)?;
    let ctx = Ctx {
        a,
        p,
        server: &server,
        corpus,
        queries,
    };
    // кандидат: --model (собственное квантование и т. п.), по умолчанию bge-m3-int8
    let cand = a.model.clone().unwrap_or_else(|| INT8.to_string());
    let (r32, v32, f32_file) = eval_model(&ctx, FP32, Device::Cpu, a.reuse)?;
    let (r8, v8, f8_file) = eval_model(&ctx, &cand, Device::Cpu, false)?;
    let cmp8 = compare(&cand, &v8, FP32, &v32)?;
    let checks8 = checks(&cmp8, &r8, &r32);
    let mut vectors = vec![cmp8];
    let mut runs = BTreeMap::new();
    runs.insert(FP32.to_string(), summary(&r32, &f32_file));
    runs.insert(cand.clone(), summary(&r8, &f8_file));
    // fp16 на CUDA — справочно
    let (mut fp16_checks, mut fp16_note) = (Vec::new(), None);
    match eval_model(&ctx, FP16, Device::Cuda, a.reuse) {
        Ok((r16, v16, f16_file)) => {
            let c16 = compare(FP16, &v16, FP32, &v32)?;
            fp16_checks = checks(&c16, &r16, &r32);
            vectors.push(c16);
            vectors.push(compare(&cand, &v8, FP16, &v16)?);
            runs.insert(FP16.to_string(), summary(&r16, &f16_file));
        }
        Err(e) => {
            println!("fp16 на CUDA пропущен: {e:#}");
            fp16_note = Some(format!("пропущен: {e:#}"));
        }
    }
    let pass = verdict(&checks8);
    let g = GateResult {
        date: run::today(),
        git: run::git_rev(&p.repo),
        reference: FP32.into(),
        candidate: cand.clone(),
        pass,
        checks: checks8,
        vectors,
        runs,
        fp16_checks,
        fp16_note,
    };
    let stem = if cand == INT8 {
        format!("{}-quality-gate", g.date)
    } else {
        format!("{}-quality-gate-{cand}", g.date)
    };
    let (json, md) = report::write_pair(&p.results, &stem, &g, &gate_md(&g))?;
    println!("{}\n{}\n{}", gate_md(&g), json.display(), md.display());
    if !pass {
        bail!("гейт качества int8 не пройден (см. {})", md.display());
    }
    println!("QUALITY-GATE OK");
    Ok(())
}

fn checks_table(c: &[Check]) -> String {
    let mut s = String::from(
        "| Проверка | Значение | 95 % интервал (парный бутстреп) | Порог | Итог |\n\
         |---|---:|---|---|---|\n",
    );
    for x in c {
        let ci = x.ci.map_or("—".to_string(), |[lo, hi]| {
            format!("{:+.4} … {:+.4}", lo + 0.0, hi + 0.0)
        });
        s.push_str(&format!(
            "| {} | {:.4} | {ci} | {} | {} |\n",
            x.name,
            x.value + 0.0,
            x.threshold,
            if x.pass { "да" } else { "**нет**" }
        ));
    }
    s
}

/// Markdown гейта.
pub fn gate_md(g: &GateResult) -> String {
    let mut s = format!(
        "# Гейт качества int8: {} против {}\n\n- дата {}, коммит `{}`\n- итог: **{}**\n\n",
        g.candidate,
        g.reference,
        g.date,
        g.git,
        if g.pass {
            "пройден"
        } else {
            "не пройден"
        }
    );
    let (info, verdict): (Vec<Check>, Vec<Check>) =
        g.checks.iter().cloned().partition(|c| c.informational);
    s.push_str(&format!(
        "Вердикт — по нижней границе 95 % интервала парного бутстрепа разности {} − {} \
         ({GATE_BOOTSTRAP} выборок, фиксированное зерно): не ниже −{METRIC_DROP_MAX} и для \
         recall@5, и для nDCG@10.\n\n",
        g.candidate, g.reference
    ));
    s.push_str(&checks_table(&verdict));
    if !info.is_empty() {
        s.push_str("\n## Справочно: векторы (на вердикт не влияют)\n\n");
        s.push_str(&checks_table(&info));
    }
    s.push_str("\n## Векторы (чанки корпуса, общие по embed_key)\n\n");
    s.push_str(
        "| Пара | чанков | dense: средний | dense: мин. | dense: 1 % | sparse ρ: средний | sparse ρ: мин. | sparse ρ: 5 % |\n\
         |---|---:|---:|---:|---:|---:|---:|---:|\n",
    );
    for v in &g.vectors {
        s.push_str(&format!(
            "| {} / {} | {} | {:.4} | {:.4} | {:.4} | {:.4} | {:.4} | {:.4} |\n",
            v.a,
            v.b,
            v.chunks,
            v.dense_mean,
            v.dense_min,
            v.dense_p01,
            v.sparse_spearman_mean,
            v.sparse_spearman_min,
            v.sparse_spearman_p05
        ));
    }
    s.push_str("\n## Поиск (hybrid, все запросы)\n\n");
    s.push_str(
        "| Модель | устройство | индексация, с | recall@5 | MRR@10 | nDCG@10 | nDCG@10 без общих слов |\n\
         |---|---|---:|---|---|---|---|\n",
    );
    for (m, r) in &g.runs {
        s.push_str(&format!(
            "| {m} | {} | {:.0} | {} | {} | {} | {} |\n",
            r.device.as_deref().unwrap_or("?"),
            r.index_secs,
            est(&r.overall.recall5),
            est(&r.overall.mrr10),
            est(&r.overall.ndcg10),
            est(&r.disjoint.ndcg10)
        ));
    }
    if !g.fp16_checks.is_empty() {
        s.push_str("\n## fp16 (CUDA) против эталона — справочно\n\n");
        s.push_str(&checks_table(&g.fp16_checks));
    }
    if let Some(n) = &g.fp16_note {
        s.push_str(&format!("\nfp16: {n}\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Значения метрики по 200 запросам: эталон — 0,5, у кандидата к первым прибавлены `deltas`.
    fn cols(deltas: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let reference = vec![0.5; 200];
        let mut candidate = reference.clone();
        for (c, d) in candidate.iter_mut().zip(deltas) {
            *c += d;
        }
        (reference, candidate)
    }

    fn vc(dense_mean: f64, dense_min: f64, sparse: f64) -> VectorCompare {
        VectorCompare {
            a: INT8.into(),
            b: FP32.into(),
            chunks: 10,
            chunks_by_level: BTreeMap::new(),
            dense_mean,
            dense_min,
            dense_p01: dense_min,
            sparse_spearman_mean: sparse,
            sparse_spearman_min: sparse,
            sparse_spearman_p05: sparse,
            sparse_skipped: 0,
        }
    }

    #[test]
    fn verdict_uses_lower_bound_only() {
        // среднее −0,005 (не хуже порога), но разброс велик: нижняя граница < −0,01 — провал
        let mut d = vec![-0.5; 6];
        d.extend([0.5; 5]);
        let (r, c) = cols(&d);
        let ch = metric_check_values("nDCG@10", &r, &c);
        let [lo, hi] = ch.ci.unwrap();
        assert!(
            ch.value > -METRIC_DROP_MAX && lo < -METRIC_DROP_MAX,
            "{ch:?}"
        );
        assert!(hi > 0.0);
        assert!(
            !ch.pass,
            "среднее выше порога, но нижняя граница ниже: {ch:?}"
        );
        assert!(!verdict(&[ch]));
        // ровный сдвиг −0,005 на всех запросах: интервал вырожден, нижняя граница −0,005
        let (r, c) = cols(&[-0.005; 200]);
        let ch = metric_check_values("recall@5", &r, &c);
        assert!(ch.pass, "{ch:?}");
        // ровный сдвиг −0,011 — нижняя граница ниже порога
        let (r, c) = cols(&[-0.011; 200]);
        assert!(!metric_check_values("recall@5", &r, &c).pass);
        // широкая верхняя граница не мешает: кандидат лучше — пройдено
        let (r, c) = cols(&[0.5; 10]);
        let ch = metric_check_values("recall@5", &r, &c);
        assert!(ch.pass && ch.ci.unwrap()[0] >= 0.0, "{ch:?}");
        // интервал — 10 000 выборок парного бутстрепа с фиксированным зерном
        let (r, c) = cols(&d);
        let e = paired_diff_with(&r, &c, GATE_BOOTSTRAP);
        assert_eq!(GATE_BOOTSTRAP, 10_000);
        assert_eq!(metric_check_values("x", &r, &c).ci, Some([e.lo, e.hi]));
        assert_eq!(metric_check_values("x", &r, &c).ci, Some([e.lo, e.hi]));
        // порог в отчёте называет нижнюю границу
        assert!(ch.threshold.contains("нижняя граница"), "{}", ch.threshold);
    }

    #[test]
    fn cosine_is_informational() {
        // косинус и Спирмен ниже порогов, поиск не хуже — гейт пройден
        let v = vc(0.978, 0.90, 0.90);
        let vecs = vector_checks(&v);
        assert_eq!(vecs.len(), 3);
        assert!(vecs.iter().all(|c| c.informational && !c.pass), "{vecs:?}");
        let (r, c) = cols(&[]);
        let mut all = vecs.clone();
        all.push(metric_check_values("recall@5", &r, &c));
        all.push(metric_check_values("nDCG@10", &r, &c));
        assert!(all[3..].iter().all(|c| !c.informational));
        assert!(verdict(&all), "{all:?}");
        // поиск хуже — не пройден, как бы хороши ни были векторы
        let mut all = vector_checks(&vc(0.999, 0.99, 0.99));
        let (r, c) = cols(&[-0.02; 200]);
        all.push(metric_check_values("nDCG@10", &r, &c));
        assert!(!verdict(&all));
        // в отчёте векторные проверки — в разделе «справочно», вердикт — «пройден»
        let g = GateResult {
            date: "2026-10-25".into(),
            git: "x".into(),
            reference: FP32.into(),
            candidate: INT8.into(),
            pass: true,
            checks: {
                let mut c = vector_checks(&v);
                let (r, q) = cols(&[]);
                c.push(metric_check_values("nDCG@10: разница с fp32", &r, &q));
                c
            },
            vectors: vec![v],
            runs: BTreeMap::new(),
            fp16_checks: Vec::new(),
            fp16_note: None,
        };
        let md = gate_md(&g);
        assert!(md.contains("итог: **пройден**"), "{md}");
        let (verdict_part, info_part) = md.split_once("Справочно").expect("раздел «Справочно»");
        assert!(verdict_part.contains("nDCG@10: разница с fp32"), "{md}");
        assert!(!verdict_part.contains("dense: средний косинус"), "{md}");
        assert!(info_part.contains("dense: средний косинус"), "{md}");
    }
}
