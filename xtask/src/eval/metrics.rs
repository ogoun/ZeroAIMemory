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

use serde::Serialize;

/// Число выборок бутстрепа.
pub const BOOTSTRAP: usize = 2000;
/// Зерно генератора бутстрепа.
const SEED: u64 = 0x5eed_2026_1019;

/// Метрики одного запроса.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct QueryMetrics {
    pub recall5: f64,
    pub mrr10: f64,
    pub ndcg10: f64,
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
    }
}

/// Среднее с 95 % интервалом.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
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

/// Среднее значений и 95 % интервал бутстрепа.
pub fn estimate(values: &[f64]) -> Estimate {
    let m = mean(values);
    if values.len() < 2 {
        return Estimate {
            mean: m,
            lo: m,
            hi: m,
        };
    }
    let mut rng = Rng(SEED);
    let mut means: Vec<f64> = (0..BOOTSTRAP)
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

/// Сводка по метрикам запросов.
pub fn aggregate(per_query: &[QueryMetrics]) -> Aggregate {
    let col = |f: fn(&QueryMetrics) -> f64| per_query.iter().map(f).collect::<Vec<_>>();
    Aggregate {
        n: per_query.len(),
        recall5: estimate(&col(|m| m.recall5)),
        mrr10: estimate(&col(|m| m.mrr10)),
        ndcg10: estimate(&col(|m| m.ndcg10)),
    }
}

/// Парная разница `b − a` (одни запросы в одном порядке) с 95 % интервалом бутстрепа.
pub fn paired_diff(a: &[f64], b: &[f64]) -> Estimate {
    assert_eq!(a.len(), b.len(), "парное сравнение на одних запросах");
    let d: Vec<f64> = a.iter().zip(b).map(|(x, y)| y - x).collect();
    estimate(&d)
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
