//! Метрики выдачи (ТЗ §18.2): recall@5, MRR@10, nDCG@10 с бинарной релевантностью и
//! доверительные интервалы бутстрепа по запросам.
//!
//! - **recall@5** — доля ожидаемых заметок в первых 5;
//! - **MRR@10** — 1 / место первой ожидаемой заметки в первых 10 (0 — не нашлась);
//! - **nDCG@10** — DCG = Σ 1/log2(место + 1) по ожидаемым в первых 10, делённый на DCG
//!   идеальной выдачи из min(|ожидаемых|, 10) заметок.
//!
//! **Шум.** 95 % интервал среднего — перцентили 2,5 и 97,5 среднего по 2000 выборкам
//! запросов с возвращением (фиксированное зерно, детерминированно). Разница двух
//! конфигураций на одних запросах — парный бутстреп разностей: выигрыш устойчив, если
//! нижняя граница интервала разности больше нуля.

use serde::{Deserialize, Serialize};

/// Число выборок бутстрепа.
pub const BOOTSTRAP: usize = 2000;
/// Число выборок бутстрепа замера M6 (спецификация M6 §3.5).
pub const BOOTSTRAP_M6: usize = 10_000;
/// Зерно генератора бутстрепа.
pub const SEED: u64 = 0x5eed_2026_1019;

/// Метрики одного запроса. Поля M3 — только у запросов своего вида (`None` у остальных):
/// `cancelled5` — `superseded`, `reliable_first` — `certainty`, `chunk_*` — `chunk`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct QueryMetrics {
    pub recall5: f64,
    pub mrr10: f64,
    pub ndcg10: f64,
    /// В top-5 есть замещённый или отозванный insight из `forbid`.
    pub cancelled5: Option<bool>,
    /// Надёжный ответ (`expect`) стоит выше противоречащей гипотезы (`weaker`).
    pub reliable_first: Option<bool>,
    /// Среди первых 5 листов есть лист, целиком содержащий `evidence`.
    pub chunk_recall5: Option<f64>,
    /// nDCG@10 по листам: релевантен лист, целиком содержащий `evidence`.
    pub chunk_ndcg10: Option<f64>,
}

/// `cancelled@5`: хоть одна цель из `forbid` среди первых 5 выдачи.
pub fn cancelled_at_5(ranked: &[String], forbid: &[String]) -> bool {
    ranked.iter().take(5).any(|p| forbid.contains(p))
}

/// `reliable_first`: лучшая из `expect` найдена в top-10 и стоит выше лучшей из `weaker`
/// (гипотезы нет в top-10 — надёжное выше).
pub fn reliable_first(ranked: &[String], expect: &[String], weaker: &[String]) -> bool {
    let top: Vec<&String> = ranked.iter().take(10).collect();
    let pos = |set: &[String]| top.iter().position(|p| set.contains(p));
    match (pos(expect), pos(weaker)) {
        (Some(e), Some(w)) => e < w,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// Лист содержит `evidence` целиком (пробельные символы сравниваются как один пробел).
pub fn leaf_contains(leaf: &str, evidence: &str) -> bool {
    let ev = super::corpus::squash_ws(evidence);
    !ev.is_empty() && super::corpus::squash_ws(leaf).contains(&ev)
}

/// `chunk-recall@5` и `chunk-nDCG@10` по текстам листов выдачи (по убыванию балла, без
/// перекрытия — см. `run`): релевантен лист, целиком содержащий `evidence`. Идеальная
/// выдача — один такой лист на первом месте (собственные тексты листов не пересекаются).
pub fn chunk_scores(leaves: &[String], evidence: &str) -> (f64, f64) {
    let first = leaves
        .iter()
        .take(10)
        .position(|l| leaf_contains(l, evidence));
    match first {
        Some(i) => (if i < 5 { 1.0 } else { 0.0 }, 1.0 / (i as f64 + 2.0).log2()),
        None => (0.0, 0.0),
    }
}

/// Метрики выдачи `ranked` (пути заметок по убыванию балла) при ожидаемых `expect`.
pub fn score(ranked: &[String], expect: &[String]) -> QueryMetrics {
    if expect.is_empty() {
        return QueryMetrics::default();
    }
    let rel = |p: &String| expect.contains(p);
    let found5 = ranked.iter().take(5).filter(|p| rel(p)).count();
    let mrr10 = ranked
        .iter()
        .take(10)
        .position(rel)
        .map_or(0.0, |i| 1.0 / (i as f64 + 1.0));
    let dcg: f64 = ranked
        .iter()
        .take(10)
        .enumerate()
        .filter(|(_, p)| rel(p))
        .map(|(i, _)| 1.0 / (i as f64 + 2.0).log2())
        .sum();
    let ideal: f64 = (0..expect.len().min(10))
        .map(|i| 1.0 / (i as f64 + 2.0).log2())
        .sum();
    QueryMetrics {
        recall5: found5 as f64 / expect.len() as f64,
        mrr10,
        ndcg10: dcg / ideal,
        ..QueryMetrics::default()
    }
}

/// Среднее с 95 % интервалом.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    pub mean: f64,
    pub lo: f64,
    pub hi: f64,
}

/// Сводка метрик по набору запросов.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Aggregate {
    pub n: usize,
    pub recall5: Estimate,
    pub mrr10: Estimate,
    pub ndcg10: Estimate,
    /// Метрики M3 — по запросам, у которых они есть (`None` — таких запросов нет).
    pub cancelled5: Option<Estimate>,
    pub reliable_first: Option<Estimate>,
    pub chunk_recall5: Option<Estimate>,
    pub chunk_ndcg10: Option<Estimate>,
}

/// splitmix64: детерминированный генератор для бутстрепа (без внешних зависимостей).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

/// Перцентиль отсортированного массива (линейная интерполяция).
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let pos = q * (sorted.len() - 1) as f64;
    let (i, f) = (pos.floor() as usize, pos.fract());
    if i + 1 < sorted.len() {
        sorted[i] * (1.0 - f) + sorted[i + 1] * f
    } else {
        sorted[i]
    }
}

/// Среднее значений и 95 % интервал бутстрепа ([`BOOTSTRAP`] выборок).
pub fn estimate(values: &[f64]) -> Estimate {
    estimate_with(values, BOOTSTRAP)
}

/// Среднее значений и 95 % интервал бутстрепа из `samples` выборок (то же зерно).
pub fn estimate_with(values: &[f64], samples: usize) -> Estimate {
    let m = mean(values);
    if values.len() < 2 {
        return Estimate {
            mean: m,
            lo: m,
            hi: m,
        };
    }
    let mut rng = Rng(SEED);
    let mut means: Vec<f64> = (0..samples.max(1))
        .map(|_| {
            (0..values.len())
                .map(|_| values[rng.below(values.len())])
                .sum::<f64>()
                / values.len() as f64
        })
        .collect();
    means.sort_by(f64::total_cmp);
    Estimate {
        mean: m,
        lo: percentile(&means, 0.025),
        hi: percentile(&means, 0.975),
    }
}

/// Отношение сумм `Σ num / Σ den` по случаям с 95 % интервалом бутстрепа случаев (то же зерно,
/// [`BOOTSTRAP`] выборок; выборка с нулевым знаменателем пропускается). `None` — знаменатель
/// пуст (например, точность подсказок, когда не показано ни одной).
pub fn ratio_estimate(num: &[f64], den: &[f64]) -> Option<Estimate> {
    assert_eq!(
        num.len(),
        den.len(),
        "числитель и знаменатель по одним случаям"
    );
    let pairs: Vec<(f64, f64)> = num.iter().copied().zip(den.iter().copied()).collect();
    ratio_estimate_with(&pairs, BOOTSTRAP, SEED)
}

/// [`ratio_estimate`] по парам `(числитель, знаменатель)` случаев: `n` выборок, зерно `seed`
/// (M6: `R = Σ M6 / Σ базы`, [`BOOTSTRAP_M6`] выборок, парный бутстреп сценариев).
pub fn ratio_estimate_with(pairs: &[(f64, f64)], n: usize, seed: u64) -> Option<Estimate> {
    let total: f64 = pairs.iter().map(|p| p.1).sum();
    if total <= 0.0 {
        return None;
    }
    let m = pairs.iter().map(|p| p.0).sum::<f64>() / total;
    if pairs.len() < 2 {
        return Some(Estimate {
            mean: m,
            lo: m,
            hi: m,
        });
    }
    let mut rng = Rng(seed);
    let mut ratios: Vec<f64> = Vec::with_capacity(n);
    for _ in 0..n.max(1) {
        let (mut a, mut d) = (0.0, 0.0);
        for _ in 0..pairs.len() {
            let (x, y) = pairs[rng.below(pairs.len())];
            a += x;
            d += y;
        }
        if d > 0.0 {
            ratios.push(a / d);
        }
    }
    ratios.sort_by(f64::total_cmp);
    Some(Estimate {
        mean: m,
        lo: percentile(&ratios, 0.025),
        hi: percentile(&ratios, 0.975),
    })
}

/// Взвешенная парная разность `Σ w (b − a) / Σ w` по одним случаям (M6: Δ микро-полноты —
/// доли найденных нагетов сценариев с весами числа нагетов) с **односторонними** 95 % границами
/// бутстрепа случаев: `lo` — 5-й перцентиль (правило неинфериорности «нижняя граница
/// одностороннего 95 % интервала ≥ −0,02», спецификация M6 §3.5), `hi` — 95-й. Пустой вес — 0.
pub fn paired_diff_weighted(a: &[f64], b: &[f64], w: &[f64], n: usize, seed: u64) -> Estimate {
    assert!(
        a.len() == b.len() && a.len() == w.len(),
        "парное сравнение на одних случаях"
    );
    let wmean = |idx: &mut dyn Iterator<Item = usize>| {
        let (mut s, mut ws) = (0.0, 0.0);
        for i in idx {
            s += w[i] * (b[i] - a[i]);
            ws += w[i];
        }
        (ws > 0.0).then(|| s / ws)
    };
    let m = wmean(&mut (0..a.len())).unwrap_or(0.0);
    if a.len() < 2 {
        return Estimate {
            mean: m,
            lo: m,
            hi: m,
        };
    }
    let mut rng = Rng(seed);
    let mut d: Vec<f64> = Vec::with_capacity(n);
    for _ in 0..n.max(1) {
        let mut idx = (0..a.len()).map(|_| rng.below(a.len()));
        if let Some(x) = wmean(&mut idx) {
            d.push(x);
        }
    }
    d.sort_by(f64::total_cmp);
    Estimate {
        mean: m,
        lo: percentile(&d, 0.05),
        hi: percentile(&d, 0.95),
    }
}

/// Микро-усреднённая полнота `Σ найденных / Σ всех` по `(найдено, всего)` сценариев; нагетов
/// нет — 0.
pub fn micro_recall(found_total: &[(usize, usize)]) -> f64 {
    let total: usize = found_total.iter().map(|p| p.1).sum();
    if total == 0 {
        return 0.0;
    }
    found_total.iter().map(|p| p.0).sum::<usize>() as f64 / total as f64
}

/// Чистые потери `L = #{полон в базе, неполон в плече} − #{неполон в базе, полон в плече}`
/// (спецификация M6 §3.2) по одним сценариям в одном порядке.
pub fn net_losses(base_full: &[bool], arm_full: &[bool]) -> i64 {
    assert_eq!(base_full.len(), arm_full.len(), "одни сценарии");
    base_full
        .iter()
        .zip(arm_full)
        .map(|(b, a)| match (b, a) {
            (true, false) => 1,
            (false, true) => -1,
            _ => 0,
        })
        .sum()
}

/// Парная разность отношений сумм `b − a` по одним случаям (`Σ num / Σ den` у каждого) с 95 %
/// интервалом бутстрепа случаев (выборка случаев общая для обоих; выборка с нулевым знаменателем
/// у одного из них пропускается). `None` — знаменатель одного из отношений пуст.
pub fn paired_ratio_diff(
    a_num: &[f64],
    a_den: &[f64],
    b_num: &[f64],
    b_den: &[f64],
) -> Option<Estimate> {
    let n = a_num.len();
    assert!(
        a_den.len() == n && b_num.len() == n && b_den.len() == n,
        "парное сравнение на одних случаях"
    );
    let sum = |v: &[f64]| v.iter().sum::<f64>();
    if sum(a_den) <= 0.0 || sum(b_den) <= 0.0 {
        return None;
    }
    let m = sum(b_num) / sum(b_den) - sum(a_num) / sum(a_den);
    if n < 2 {
        return Some(Estimate {
            mean: m,
            lo: m,
            hi: m,
        });
    }
    let mut rng = Rng(SEED);
    let mut d: Vec<f64> = Vec::with_capacity(BOOTSTRAP);
    for _ in 0..BOOTSTRAP {
        let (mut an, mut ad, mut bn, mut bd) = (0.0, 0.0, 0.0, 0.0);
        for _ in 0..n {
            let i = rng.below(n);
            an += a_num[i];
            ad += a_den[i];
            bn += b_num[i];
            bd += b_den[i];
        }
        if ad > 0.0 && bd > 0.0 {
            d.push(bn / bd - an / ad);
        }
    }
    d.sort_by(f64::total_cmp);
    Some(Estimate {
        mean: m,
        lo: percentile(&d, 0.025),
        hi: percentile(&d, 0.975),
    })
}

/// Доля: `true` — 1, `false` — 0.
fn flag(b: Option<bool>) -> Option<f64> {
    b.map(|b| if b { 1.0 } else { 0.0 })
}

/// Сводка по метрикам запросов.
pub fn aggregate(per_query: &[QueryMetrics]) -> Aggregate {
    let col = |f: fn(&QueryMetrics) -> f64| per_query.iter().map(f).collect::<Vec<_>>();
    let opt = |f: fn(&QueryMetrics) -> Option<f64>| {
        let v: Vec<f64> = per_query.iter().filter_map(f).collect();
        (!v.is_empty()).then(|| estimate(&v))
    };
    Aggregate {
        n: per_query.len(),
        recall5: estimate(&col(|m| m.recall5)),
        mrr10: estimate(&col(|m| m.mrr10)),
        ndcg10: estimate(&col(|m| m.ndcg10)),
        cancelled5: opt(|m| flag(m.cancelled5)),
        reliable_first: opt(|m| flag(m.reliable_first)),
        chunk_recall5: opt(|m| m.chunk_recall5),
        chunk_ndcg10: opt(|m| m.chunk_ndcg10),
    }
}

/// Парная разница `b − a` (одни запросы в одном порядке) с 95 % интервалом бутстрепа.
pub fn paired_diff(a: &[f64], b: &[f64]) -> Estimate {
    paired_diff_with(a, b, BOOTSTRAP)
}

/// [`paired_diff`] с `samples` выборками бутстрепа (гейт int8 — 10 000).
pub fn paired_diff_with(a: &[f64], b: &[f64], samples: usize) -> Estimate {
    assert_eq!(a.len(), b.len(), "парное сравнение на одних запросах");
    let d: Vec<f64> = a.iter().zip(b).map(|(x, y)| y - x).collect();
    estimate_with(&d, samples)
}

/// Ранговая корреляция Спирмена (средние ранги при равенствах; Пирсон рангов).
/// Меньше двух точек или нулевая дисперсия — `None`.
pub fn spearman(x: &[f64], y: &[f64]) -> Option<f64> {
    assert_eq!(x.len(), y.len());
    if x.len() < 2 {
        return None;
    }
    let (rx, ry) = (ranks(x), ranks(y));
    pearson(&rx, &ry)
}

fn ranks(v: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..v.len()).collect();
    idx.sort_by(|&a, &b| v[a].total_cmp(&v[b]));
    let mut r = vec![0.0; v.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && v[idx[j + 1]] == v[idx[i]] {
            j += 1;
        }
        // места i..=j (с 1) делят средний ранг
        let avg = (i + j) as f64 / 2.0 + 1.0;
        for &k in &idx[i..=j] {
            r[k] = avg;
        }
        i = j + 1;
    }
    r
}

fn pearson(x: &[f64], y: &[f64]) -> Option<f64> {
    let (mx, my) = (mean(x), mean(y));
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (a, b) in x.iter().zip(y) {
        sxy += (a - mx) * (b - my);
        sxx += (a - mx) * (a - mx);
        syy += (b - my) * (b - my);
    }
    (sxx > 0.0 && syy > 0.0).then(|| sxy / (sxx * syy).sqrt())
}

/// Косинус двух векторов.
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let (mut ab, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x), f64::from(*y));
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    if aa == 0.0 || bb == 0.0 {
        0.0
    } else {
        ab / (aa * bb).sqrt()
    }
}

/// Перцентиль (0..1) неотсортированных значений.
pub fn quantile(values: &[f64], q: f64) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    percentile(&v, q)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn metrics_of_a_ranking() {
        let m = score(&s(&["x", "a", "y", "b"]), &s(&["a", "b"]));
        assert_eq!(m.recall5, 1.0);
        assert_eq!(m.mrr10, 0.5);
        let dcg = 1.0 / 3f64.log2() + 1.0 / 5f64.log2();
        let ideal = 1.0 + 1.0 / 3f64.log2();
        assert!((m.ndcg10 - dcg / ideal).abs() < 1e-12);
        // идеальная выдача
        let p = score(&s(&["a", "b"]), &s(&["a", "b"]));
        assert_eq!((p.recall5, p.mrr10, p.ndcg10), (1.0, 1.0, 1.0));
        // не нашлось / нашлось за пятым местом
        let late = score(&s(&["1", "2", "3", "4", "5", "a"]), &s(&["a"]));
        assert_eq!((late.recall5, late.mrr10), (0.0, 1.0 / 6.0));
        assert_eq!(score(&[], &s(&["a"])), QueryMetrics::default());
    }

    #[test]
    fn cancelled_at_5_counts_forbidden_in_top5() {
        let ranked = s(&["a", "b", "c", "d", "e", "old"]);
        // запрещённый на шестом месте — не в top-5
        assert!(!cancelled_at_5(&ranked, &s(&["old"])));
        assert!(cancelled_at_5(&ranked, &s(&["zzz", "c"])));
        assert!(!cancelled_at_5(&[], &s(&["old"])));
        assert!(!cancelled_at_5(&ranked, &[]));
    }

    #[test]
    fn reliable_first_compares_ranks() {
        let ranked = s(&["x", "proof", "guess"]);
        assert!(reliable_first(&ranked, &s(&["proof"]), &s(&["guess"])));
        assert!(!reliable_first(&ranked, &s(&["guess"]), &s(&["proof"])));
        // гипотезы нет в top-10, надёжное есть — выше
        assert!(reliable_first(&ranked, &s(&["proof"]), &s(&["none"])));
        // надёжного нет — не выше
        assert!(!reliable_first(&ranked, &s(&["none"]), &s(&["guess"])));
        // за десятым местом не считается
        let mut long: Vec<String> = (0..10).map(|i| format!("n{i}")).collect();
        long.push("proof".into());
        assert!(!reliable_first(&long, &s(&["proof"]), &s(&["guess"])));
        // лучшая из гипотез решает
        let r = s(&["g2", "proof", "g1"]);
        assert!(!reliable_first(&r, &s(&["proof"]), &s(&["g1", "g2"])));
    }

    #[test]
    fn chunk_recall_requires_whole_evidence_in_leaf() {
        let ev = "worker retries a batch four times";
        // доказательство разрезано между листами — не засчитывается
        let split = s(&["intro. The worker retries", "a batch four times. Then"]);
        assert_eq!(chunk_scores(&split, ev), (0.0, 0.0));
        // целиком во втором листе; переводы строк и лишние пробелы не мешают
        let whole = s(&["intro", "The worker  retries a\nbatch four times. Then"]);
        let (r, n) = chunk_scores(&whole, ev);
        assert_eq!(r, 1.0);
        assert!((n - 1.0 / 3f64.log2()).abs() < 1e-12);
        // за пятым листом: recall 0, nDCG по десяти
        let mut late: Vec<String> = (0..5).map(|i| format!("leaf {i}")).collect();
        late.push(format!("x {ev} y"));
        let (r, n) = chunk_scores(&late, ev);
        assert_eq!(r, 0.0);
        assert!((n - 1.0 / 7f64.log2()).abs() < 1e-12);
        // повтор доказательства ниже не добавляет (идеальная выдача — один лист)
        let twice = s(&[ev, ev]);
        assert_eq!(chunk_scores(&twice, ev), (1.0, 1.0));
    }

    #[test]
    fn bootstrap_interval_contains_mean_and_is_deterministic() {
        let v: Vec<f64> = (0..100).map(|i| f64::from(i % 2)).collect();
        let e = estimate(&v);
        assert!((e.mean - 0.5).abs() < 1e-12);
        assert!(e.lo < 0.5 && e.hi > 0.5 && e.hi - e.lo < 0.25);
        assert_eq!(estimate(&v), e);
        // одинаковые значения — нулевой интервал
        let c = estimate(&[0.7; 10]);
        assert!((c.hi - c.lo).abs() < 1e-12);
        // парная разница: b стабильно лучше на 0.1
        let a: Vec<f64> = (0..50).map(|i| f64::from(i) / 50.0).collect();
        let b: Vec<f64> = a.iter().map(|x| x + 0.1).collect();
        let d = paired_diff(&a, &b);
        assert!((d.mean - 0.1).abs() < 1e-12 && d.lo > 0.09);
    }

    /// Одинаковые плечи: R = 1, интервал его содержит.
    #[test]
    fn ratio_identity_is_one() {
        let pairs: Vec<(f64, f64)> = (1..=40)
            .map(|i| (f64::from(i * 37 % 900), f64::from(i * 37 % 900)))
            .collect();
        let e = ratio_estimate_with(&pairs, BOOTSTRAP_M6, 7).unwrap();
        assert!((e.mean - 1.0).abs() < 1e-12);
        assert!(e.lo <= 1.0 && 1.0 <= e.hi, "{e:?}");
        assert!(ratio_estimate_with(&[(1.0, 0.0)], BOOTSTRAP_M6, 7).is_none());
    }

    /// Одно зерно — один интервал; другое зерно — другие границы.
    #[test]
    fn ratio_seed_reproducible() {
        let pairs: Vec<(f64, f64)> = (0..30)
            .map(|i| (f64::from(500 + i * 13 % 170), f64::from(800 + i * 29 % 310)))
            .collect();
        let a = ratio_estimate_with(&pairs, 2000, 42).unwrap();
        assert_eq!(ratio_estimate_with(&pairs, 2000, 42).unwrap(), a);
        let b = ratio_estimate_with(&pairs, 2000, 43).unwrap();
        assert_eq!(a.mean, b.mean);
        assert!(a.lo != b.lo || a.hi != b.hi);
        // старый интерфейс — тот же расчёт с BOOTSTRAP выборок и прежним зерном
        let (num, den): (Vec<f64>, Vec<f64>) = pairs.iter().copied().unzip();
        assert_eq!(
            ratio_estimate(&num, &den).unwrap(),
            ratio_estimate_with(&pairs, BOOTSTRAP, SEED).unwrap()
        );
    }

    /// `[1,1,0]` против `[0,1,1]`: одна потеря и одна находка — L = 0.
    #[test]
    fn net_losses_counts_both_ways() {
        assert_eq!(net_losses(&[true, true, false], &[false, true, true]), 0);
        assert_eq!(net_losses(&[true, true, true], &[false, false, true]), 2);
        assert_eq!(net_losses(&[false, false], &[true, true]), -2);
    }

    /// Микро-полнота — Σ найденных / Σ всех; взвешенная разность — разность микро-полнот.
    #[test]
    fn micro_recall_and_weighted_diff() {
        assert!((micro_recall(&[(1, 2), (3, 3), (0, 1)]) - 4.0 / 6.0).abs() < 1e-12);
        assert_eq!(micro_recall(&[]), 0.0);
        let base = [0.5, 1.0, 0.0];
        let arm = [1.0, 1.0, 0.0];
        let w = [2.0, 3.0, 1.0];
        let d = paired_diff_weighted(&base, &arm, &w, BOOTSTRAP_M6, 1);
        assert!((d.mean - 1.0 / 6.0).abs() < 1e-12, "{d:?}");
        assert!(d.lo <= d.mean && d.mean <= d.hi);
        let same = paired_diff_weighted(&base, &base, &w, BOOTSTRAP_M6, 1);
        assert_eq!((same.mean, same.lo, same.hi), (0.0, 0.0, 0.0));
        // границы — 5-й и 95-й перцентили выборок (односторонние 95 %), не 2,5 / 97,5
        let a: Vec<f64> = (0..20).map(|i| f64::from(i % 3) / 2.0).collect();
        let b: Vec<f64> = (0..20).map(|i| f64::from((i + 1) % 3) / 2.0).collect();
        let w: Vec<f64> = (0..20).map(|i| f64::from(1 + i % 4)).collect();
        let d = paired_diff_weighted(&a, &b, &w, 500, 9);
        let mut rng = Rng(9);
        let mut s: Vec<f64> = (0..500)
            .map(|_| {
                let (mut x, mut ws) = (0.0, 0.0);
                for _ in 0..20 {
                    let i = rng.below(20);
                    x += w[i] * (b[i] - a[i]);
                    ws += w[i];
                }
                x / ws
            })
            .collect();
        s.sort_by(f64::total_cmp);
        assert_eq!((d.lo, d.hi), (percentile(&s, 0.05), percentile(&s, 0.95)));
    }

    /// Бутстреп парный: сценарий берётся в выборку обоими плечами сразу. У коррелированных
    /// плеч (тяжёлый сценарий тяжёл в обоих) интервал R уже, чем у тех же чисел без пар.
    #[test]
    fn paired_bootstrap_uses_pairs() {
        let base: Vec<f64> = (0..40).map(|i| f64::from(200 + (i * 97) % 3000)).collect();
        let arm: Vec<f64> = base.iter().map(|x| x * 0.7).collect();
        let paired: Vec<(f64, f64)> = arm.iter().copied().zip(base.iter().copied()).collect();
        // те же значения, пары перемешаны (сдвиг на 17)
        let shuffled: Vec<(f64, f64)> = (0..40).map(|i| (arm[(i + 17) % 40], base[i])).collect();
        let p = ratio_estimate_with(&paired, BOOTSTRAP_M6, SEED).unwrap();
        let u = ratio_estimate_with(&shuffled, BOOTSTRAP_M6, SEED).unwrap();
        assert!((p.mean - 0.7).abs() < 1e-12 && (u.mean - 0.7).abs() < 1e-12);
        assert!(p.hi - p.lo < 1e-9, "{p:?}");
        assert!(u.hi - u.lo > 0.05, "{u:?}");
    }

    #[test]
    fn spearman_and_cosine() {
        assert_eq!(spearman(&[1.0, 2.0, 3.0], &[10.0, 20.0, 30.0]), Some(1.0));
        assert_eq!(spearman(&[1.0, 2.0, 3.0], &[3.0, 2.0, 1.0]), Some(-1.0));
        // равные значения получают средний ранг
        assert_eq!(ranks(&[5.0, 1.0, 5.0]), [2.5, 1.0, 2.5]);
        assert_eq!(spearman(&[1.0, 1.0], &[1.0, 2.0]), None);
        assert!((cosine(&[1.0, 0.0], &[1.0, 1.0]) - 0.5f64.sqrt()).abs() < 1e-9);
        assert_eq!(quantile(&[3.0, 1.0, 2.0], 0.5), 2.0);
    }
}
