//! Оценка выдачи: метрики, разбиения наборов и доля слов заголовка — одни функции у замера
//! `cargo xtask eval` и у ночной проверки памяти сервера (M7.3 §6.4 п. 2, §6.5 п. 3).
//!
//! - **Разбиения** — два, по разным задачам **[решение координатора]**:
//!   [`probe_split`] — поэлементное, для растущего набора проб проверки (`test`, если
//!   `blake3("probes-v1" ‖ ссылка цели)` по модулю 3 равен нулю: ⅓ набора, принадлежность пробы не
//!   зависит от других проб); [`split_by_rank`] — ранговое разбиение сценариев замера внутри вида
//!   (перенесено из `xtask/src/eval/split.rs` без изменения поведения: сортировка по
//!   `blake3(SPLIT_SALT ‖ id)`, первые ⌈n/3⌉ — holdout).
//! - [`overlap`] — доля предметных слов вопроса среди слов заголовка цели: проверка пробы
//!   отказывает пересказу заголовка (> 0,5), метрика `hard` считает пробы с `overlap < 0,2`.
//! - [`score`] — метрики выдачи замера (recall@5, MRR@10, nDCG@10; перенесено из
//!   `xtask/src/eval/metrics.rs`).
//! - [`check_metrics`] — метрики прогона ночной проверки по частям `dev`, `test` и всем
//!   (§6.5 п. 3).

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::recall::{StemWord, error_terms, stem_words};

pub use zam_core::split::Split;

/// Соль поэлементного разбиения проб ([`probe_split`]).
pub const PROBE_SPLIT_SALT: &str = "probes-v1";

/// Часть пробы по ссылке её (первой) цели `target` (`n:<id>`, `i:<id>`; у посторонних
/// вопросов — их текст): `blake3(PROBE_SPLIT_SALT ‖ target)`, первые 8 байт как
/// `u64::from_le_bytes`, остаток от деления на 3 равен нулю — [`Split::Test`].
pub fn probe_split(target: &str) -> Split {
    let mut h = blake3::Hasher::new();
    h.update(PROBE_SPLIT_SALT.as_bytes());
    h.update(target.as_bytes());
    let mut first = [0u8; 8];
    first.copy_from_slice(&h.finalize().as_bytes()[..8]);
    if u64::from_le_bytes(first) % 3 == 0 {
        Split::Test
    } else {
        Split::Dev
    }
}

/// Соль рангового разбиения сценариев замера (спецификация M6 §3.5).
pub const SPLIT_SALT: &str = "m6-2026-11-04";

/// Ключ сортировки id внутри вида.
fn split_key(id: &str) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    h.update(SPLIT_SALT.as_bytes());
    h.update(id.as_bytes());
    h.finalize()
}

/// id holdout из пар `(id, вид)`: в каждом виде первые ⌈n/3⌉ по `blake3(SPLIT_SALT ‖ id)`.
pub fn split_by_rank(items: &[(String, String)]) -> BTreeSet<String> {
    let mut by_kind: BTreeMap<&str, Vec<(blake3::Hash, &str)>> = BTreeMap::new();
    for (id, kind) in items {
        by_kind.entry(kind).or_default().push((split_key(id), id));
    }
    let mut out = BTreeSet::new();
    for mut v in by_kind.into_values() {
        v.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()).then(a.1.cmp(b.1)));
        let n = v.len().div_ceil(3);
        out.extend(v.into_iter().take(n).map(|(_, id)| id.to_string()));
    }
    out
}

/// Предметные слова текста с основами: слова от трёх символов без повторов
/// ([`error_terms`]), без служебных слов ru/en, `ё` → `е` ([`stem_words`]) — та же
/// нормализация, что у слов подсказок.
fn subject_words(text: &str) -> Vec<StemWord> {
    stem_words(&error_terms(&text.to_lowercase()))
}

/// Доля предметных слов вопроса `question`, совпавших по основе со словом заголовка `title`
/// (0 — у вопроса нет предметных слов). Граница отказа «пересказ заголовка» — больше 0,5: 4 из 8
/// (0,5) принимается, 13 из 25 (0,52) — нет (§6.4 п. 2).
pub fn overlap(question: &str, title: &str) -> f64 {
    let q = subject_words(question);
    if q.is_empty() {
        return 0.0;
    }
    let t = subject_words(title);
    let hit = q.iter().filter(|w| t.iter().any(|x| w.matches(x))).count();
    hit as f64 / q.len() as f64
}

/// Метрики одного запроса замера. Поля M3 — только у запросов своего вида (`None` у остальных):
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

/// Метрики выдачи `ranked` (пути заметок по убыванию балла) при ожидаемых `expect`: recall@5 —
/// доля ожидаемых в первых 5; MRR@10 — 1 / место первой ожидаемой в первых 10 (0 — не нашлась);
/// nDCG@10 с бинарной релевантностью к идеальной выдаче из min(|ожидаемых|, 10).
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

/// Порог `overlap` «трудной» пробы (метрика `hard`, §6.5 п. 3).
pub const HARD_OVERLAP: f64 = 0.2;

/// Наблюдение одной пробы в прогоне проверки (строка `check_results` и свойства пробы).
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeObs {
    pub split: Split,
    /// Посторонний вопрос (`negative`): цели нет, считается только показ подсказок.
    pub negative: bool,
    /// Цель может прийти подсказкой к запросу (insight, заметка Rules — ТЗ §10.3): только такие
    /// пробы входят в знаменатель `hit_prompt`.
    pub hintable: bool,
    /// Доля слов вопроса среди слов заголовка цели ([`overlap`]).
    pub overlap: Option<f64>,
    /// Место цели среди карточек основного поиска в бюджете (с 1); `None` — не в бюджете.
    pub search_rank: Option<u32>,
    /// Место цели в глубоком поиске (только у проб вне бюджета); `None` — не нашлась.
    pub deep_rank: Option<u32>,
    /// Место цели среди подсказок к запросу (с 1; `None` — нет среди них); у постороннего
    /// вопроса — число показанных подсказок.
    pub prompt_rank: Option<u32>,
    pub search_ms: Option<f64>,
    pub prompt_ms: Option<f64>,
}

/// Метрики части набора (`dev`, `test` или все пробы с целью).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PartMetrics {
    /// Проб с целью в части.
    pub n: usize,
    /// Доля проб с целью в бюджете основного поиска.
    pub hit_search: Option<f64>,
    /// Среднее 1 / место цели в основном поиске (место до 10).
    pub mrr10: Option<f64>,
    /// Доля проб, цель которых не нашлась и в глубоком поиске.
    pub none: Option<f64>,
    /// Доля проб, цель которых нашлась в глубоком поиске за границей бюджета основного.
    pub beyond: Option<f64>,
    /// Доля проб с целью-подсказкой, цель которых среди подсказок к запросу; `None` — подсказки
    /// не считались (`hooks.prompt = false`) или таких проб нет.
    pub hit_prompt: Option<f64>,
    /// Проб в знаменателе `hit_prompt`.
    pub prompt_n: usize,
    /// `hit_search` проб с `overlap <` [`HARD_OVERLAP`].
    pub hard: Option<f64>,
    pub hard_n: usize,
}

/// Метрики прогона проверки (§6.5 п. 3).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CheckMetrics {
    pub dev: PartMetrics,
    pub test: PartMetrics,
    pub all: PartMetrics,
    /// Доля посторонних запросов, к которым подсказки к запросу показали хоть одну подсказку;
    /// `None` — подсказки не считались или посторонних запросов не было.
    pub negatives_shown: Option<f64>,
    pub negatives_n: usize,
    /// Задержки основного поиска (глубокий не входит) и подсказок, мс.
    pub search_p50_ms: Option<f64>,
    pub search_p95_ms: Option<f64>,
    pub prompt_p50_ms: Option<f64>,
    pub prompt_p95_ms: Option<f64>,
}

fn share(hit: usize, n: usize) -> Option<f64> {
    (n > 0).then(|| hit as f64 / n as f64)
}

/// Перцентиль `q` (0..=1) по ближайшему рангу: `⌈q·n⌉`-е значение по возрастанию.
pub fn percentile(values: &[f64], q: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let k = ((q * v.len() as f64).ceil() as usize).clamp(1, v.len());
    Some(v[k - 1])
}

/// Медиана (у чётного числа — среднее двух средних).
pub fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let m = v.len() / 2;
    Some(if v.len().is_multiple_of(2) {
        (v[m - 1] + v[m]) / 2.0
    } else {
        v[m]
    })
}

/// Метрики части `obs` (пробы с целью); `prompt_on` — подсказки к запросу считались.
pub fn part_metrics<'a>(
    obs: impl IntoIterator<Item = &'a ProbeObs>,
    prompt_on: bool,
) -> PartMetrics {
    let obs: Vec<&ProbeObs> = obs.into_iter().filter(|o| !o.negative).collect();
    let n = obs.len();
    let found = obs.iter().filter(|o| o.search_rank.is_some()).count();
    let rr: f64 = obs
        .iter()
        .filter_map(|o| o.search_rank.filter(|r| *r <= 10))
        .map(|r| 1.0 / f64::from(r))
        .sum();
    let lost: Vec<&&ProbeObs> = obs.iter().filter(|o| o.search_rank.is_none()).collect();
    let none = lost.iter().filter(|o| o.deep_rank.is_none()).count();
    let beyond = lost.len() - none;
    let hints: Vec<&&ProbeObs> = obs.iter().filter(|o| o.hintable).collect();
    let hint_hit = hints.iter().filter(|o| o.prompt_rank.is_some()).count();
    let hard: Vec<&&ProbeObs> = obs
        .iter()
        .filter(|o| o.overlap.is_some_and(|x| x < HARD_OVERLAP))
        .collect();
    let hard_hit = hard.iter().filter(|o| o.search_rank.is_some()).count();
    PartMetrics {
        n,
        hit_search: share(found, n),
        mrr10: (n > 0).then(|| rr / n as f64),
        none: share(none, n),
        beyond: share(beyond, n),
        hit_prompt: if prompt_on {
            share(hint_hit, hints.len())
        } else {
            None
        },
        prompt_n: if prompt_on { hints.len() } else { 0 },
        hard: share(hard_hit, hard.len()),
        hard_n: hard.len(),
    }
}

/// Метрики прогона по наблюдениям `obs` (§6.5 п. 3): части `dev`, `test` и все пробы с целью,
/// показ подсказок к посторонним вопросам и задержки.
pub fn check_metrics(obs: &[ProbeObs], prompt_on: bool) -> CheckMetrics {
    let of = |s: Split| part_metrics(obs.iter().filter(|o| o.split == s), prompt_on);
    let neg: Vec<&ProbeObs> = obs.iter().filter(|o| o.negative).collect();
    let shown = neg
        .iter()
        .filter(|o| o.prompt_rank.is_some_and(|n| n > 0))
        .count();
    let search_ms: Vec<f64> = obs
        .iter()
        .filter(|o| !o.negative)
        .filter_map(|o| o.search_ms)
        .collect();
    let prompt_ms: Vec<f64> = obs.iter().filter_map(|o| o.prompt_ms).collect();
    CheckMetrics {
        dev: of(Split::Dev),
        test: of(Split::Test),
        all: part_metrics(obs, prompt_on),
        negatives_shown: if prompt_on {
            share(shown, neg.len())
        } else {
            None
        },
        negatives_n: if prompt_on { neg.len() } else { 0 },
        search_p50_ms: percentile(&search_ms, 0.5),
        search_p95_ms: percentile(&search_ms, 0.95),
        prompt_p50_ms: percentile(&prompt_ms, 0.5),
        prompt_p95_ms: percentile(&prompt_ms, 0.95),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §6.4 п. 2: часть пробы зависит только от ссылки цели — та же ссылка всегда в той же
    /// части, на большом наборе `test` — около трети.
    #[test]
    fn probes_split_by_target_stable() {
        let refs: Vec<String> = (0..3000).map(|i| format!("n:{i:026}")).collect();
        let first: Vec<Split> = refs.iter().map(|r| probe_split(r)).collect();
        let again: Vec<Split> = refs.iter().rev().map(|r| probe_split(r)).collect();
        assert_eq!(first, again.into_iter().rev().collect::<Vec<_>>());
        let test = first.iter().filter(|s| **s == Split::Test).count();
        assert!((900..=1100).contains(&test), "test {test} из 3000");
        // формула: первые 8 байт blake3("probes-v1" ‖ ссылка) как u64 LE, mod 3 == 0
        for r in refs.iter().take(50) {
            let h = blake3::hash(format!("{PROBE_SPLIT_SALT}{r}").as_bytes());
            let v = u64::from_le_bytes(h.as_bytes()[..8].try_into().unwrap());
            let want = if v % 3 == 0 { Split::Test } else { Split::Dev };
            assert_eq!(probe_split(r), want, "{r}");
        }
        assert_eq!(PROBE_SPLIT_SALT, "probes-v1");
    }

    /// Граница пересказа заголовка (§6.4 п. 2): 4 из 8 предметных слов — 0,5, 13 из 25 — 0,52;
    /// служебные слова в знаменатель не входят, сравнение — по основам.
    #[test]
    fn overlap_cases() {
        let title = "alpha bravo charlie delta";
        let q = "alpha bravo charlie delta echoes foxtrot golfing hotels";
        assert!(
            (overlap(q, title) - 0.5).abs() < 1e-9,
            "{}",
            overlap(q, title)
        );
        // основы — первые 4 символа: у каждого слова своя («k00a», «k01a», …)
        let words: Vec<String> = (0..25).map(|i| format!("k{i:02}abcd")).collect();
        let title13 = words[..13].join(" ");
        let q25 = words.join(" ");
        assert!((overlap(&q25, &title13) - 0.52).abs() < 1e-9);
        // основы совпадают («отчёты» ~ «Отчётов»), слова короче трёх символов не считаются
        assert!((overlap("отчёты", "Отчётов по проекту") - 1.0).abs() < 1e-9);
        assert_eq!(overlap("и на по", "что угодно"), 0.0);
        assert_eq!(overlap("", "заголовок"), 0.0);
    }

    fn obs(split: Split, rank: Option<u32>, deep: Option<u32>) -> ProbeObs {
        ProbeObs {
            split,
            negative: false,
            hintable: false,
            overlap: None,
            search_rank: rank,
            deep_rank: deep,
            prompt_rank: None,
            search_ms: Some(10.0),
            prompt_ms: None,
        }
    }

    /// §6.5 п. 3: `hit_search`, `mrr10`, `none`, `beyond`, `hit_prompt` только по пробам с
    /// целью-подсказкой, `negatives_shown`, `hard`, задержки; подсказки выключены — `None`.
    #[test]
    fn check_metrics_cases() {
        let mut v = vec![
            obs(Split::Test, Some(1), None),
            obs(Split::Test, Some(4), None),
            obs(Split::Test, None, Some(12)),
            obs(Split::Test, None, None),
            obs(Split::Dev, Some(2), None),
        ];
        v[0].hintable = true;
        v[0].prompt_rank = Some(1);
        v[1].hintable = true;
        v[2].overlap = Some(0.1);
        v[3].overlap = Some(0.6);
        v[0].search_ms = Some(100.0);
        v.push(ProbeObs {
            negative: true,
            prompt_rank: Some(2),
            search_ms: None,
            prompt_ms: Some(5.0),
            ..obs(Split::Dev, None, None)
        });
        v.push(ProbeObs {
            negative: true,
            prompt_rank: Some(0),
            search_ms: None,
            prompt_ms: Some(7.0),
            ..obs(Split::Test, None, None)
        });
        let m = check_metrics(&v, true);
        assert_eq!(m.test.n, 4);
        assert_eq!(m.test.hit_search, Some(0.5));
        assert!((m.test.mrr10.unwrap() - (1.0 + 0.25) / 4.0).abs() < 1e-9);
        assert_eq!(m.test.none, Some(0.25));
        assert_eq!(m.test.beyond, Some(0.25));
        assert_eq!(m.test.hit_prompt, Some(0.5));
        assert_eq!(m.test.prompt_n, 2);
        assert_eq!(m.test.hard, Some(0.0));
        assert_eq!(m.test.hard_n, 1);
        assert_eq!(m.dev.n, 1);
        assert_eq!(m.all.n, 5);
        assert_eq!(m.all.hit_search, Some(0.6));
        assert_eq!(m.negatives_shown, Some(0.5));
        assert_eq!(m.negatives_n, 2);
        assert_eq!(m.search_p50_ms, Some(10.0));
        assert_eq!(m.search_p95_ms, Some(100.0));
        assert_eq!(m.prompt_p95_ms, Some(7.0));
        let off = check_metrics(&v, false);
        assert_eq!(off.test.hit_prompt, None);
        assert_eq!(off.negatives_shown, None);
        assert_eq!(off.negatives_n, 0);
        let empty = check_metrics(&[], true);
        assert_eq!(empty.test.hit_search, None);
        assert_eq!(empty.search_p95_ms, None);
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.95), Some(4.0));
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.5), Some(2.0));
    }

    /// Метрики замера перенесены без изменения поведения.
    #[test]
    fn score_cases() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(score(&[], &s(&["a"])), QueryMetrics::default());
        let m = score(&s(&["x", "a", "y"]), &s(&["a"]));
        assert_eq!(m.recall5, 1.0);
        assert_eq!(m.mrr10, 0.5);
        assert!((m.ndcg10 - 1.0 / 3f64.log2()).abs() < 1e-12);
        assert_eq!(score(&s(&["a"]), &[]), QueryMetrics::default());
    }

    /// Ранговое разбиение — как в xtask: по ⌈n/3⌉ каждого вида, детерминированно.
    #[test]
    fn split_by_rank_third_per_kind() {
        let mut items = Vec::new();
        for kind in ["long", "insight"] {
            for i in 0..30 {
                items.push((format!("{kind}-{i:02}"), kind.to_string()));
            }
        }
        let h = split_by_rank(&items);
        assert_eq!(h.len(), 20);
        assert_eq!(split_by_rank(&items), h);
    }
}
