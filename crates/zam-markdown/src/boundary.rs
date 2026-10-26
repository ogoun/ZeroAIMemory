//! Выбор разреза листов с учётом силы границы (задача 17 M3, исследование
//! `docs/research/2026-10-21-chunking-semantic.md` §5, шаг 2).
//!
//! - [`choose_cuts`] — динамическое программирование по кандидатам разреза группы (по аналогии
//!   с переносом строк Knuth–Plass): минимум Σ((len − goal)/goal)² + λ·Σ(S_max − strength) при
//!   жёстком `hard_max` и штрафе за лист короче `target_min` (кроме хвоста; штраф, а не
//!   запрет: жёсткое ограничение снизу бывает недостижимо), O(n·k), где k — число единиц в окне
//!   `hard_max`. Слагаемое разреза — формула брифа `−λ·strength` плюс постоянная `λ·S_max` за
//!   каждый разрез **[решение исполнителя]**: с голым `−λ·strength` каждый лишний разрез
//!   уменьшал бы стоимость, и DP дробил бы группу на листы у `target_min` (число чанков росло
//!   бы с λ); со сдвигом число разрезов задаёт отклонение от цели, а сила — их место.
//!   Лист из нескольких единиц длиннее `target_max` штрафуется, как в жадной упаковке M2
//!   (за `target_max` растёт только лист, не добравший `target_min`).
//! - [`cohesion_bonus`] — лексическая связность соседних окон прозы (TextTiling, Hearst 1997):
//!   пересечение основ слов (Snowball, `rust-stemmers`), без инференса. Глубина провала
//!   связности добавляется к силе границы.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::OnceLock;

use rust_stemmers::{Algorithm, Stemmer};

/// Сила границы, при которой разрез запрещён: после заголовка и после вводной фразы на «:».
pub(crate) const FORBIDDEN: i32 = -5;

/// Цена разреза на запрещённой границе: запрет нарушается, только если иначе никак (лист
/// не уложится в `hard_max`).
const FORBIDDEN_COST: f64 = 1e6;
/// Цена листа короче `target_min` (кроме хвоста): мягче запрета, жёстче любой силы границы.
const SHORT_COST: f64 = 1e3;
/// Цена листа из нескольких единиц длиннее `target_max`: мягче короткого листа — как в
/// жадной упаковке, где лист, не добравший `target_min`, может вырасти до `hard_max`.
const OVER_COST: f64 = 5e2;
/// Сильнейшая граница (перед H4–H6 и `---`; абзац с провалом связности): разрез на ней
/// ничего не стоит.
pub(crate) const MAX_STRENGTH: f64 = 4.0;

/// Кандидат разреза — единица упаковки глазами DP.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Cand {
    /// Токены единицы.
    pub tokens: usize,
    /// Токены повторённого заголовка блока, если лист начнётся с этой единицы.
    pub header: usize,
    /// Лист может начаться с этой единицы (не пробельная).
    pub can_start: bool,
    /// Сила границы перед единицей (с бонусом связности).
    pub strength: f64,
    /// Разрез перед единицей запрещён.
    pub forbidden: bool,
}

/// Разбиение единиц группы на листы: диапазоны индексов единиц подряд, от первой единицы,
/// с которой может начаться лист, до конца. Единица, которая одна длиннее `hard_max`,
/// даёт отдельный лист (её заранее дробит чанкер).
pub(crate) struct Budget {
    pub goal: usize,
    pub target_min: usize,
    pub target_max: usize,
    pub hard_max: usize,
}

pub(crate) fn choose_cuts(c: &[Cand], b: &Budget, lambda: f64) -> Vec<Range<usize>> {
    let n = c.len();
    let Some(first) = c.iter().position(|x| x.can_start) else {
        return Vec::new();
    };
    let mut pre = vec![0usize; n + 1];
    for (k, x) in c.iter().enumerate() {
        pre[k + 1] = pre[k] + x.tokens;
    }
    let goal = b.goal.max(1) as f64;
    let mut best = vec![f64::INFINITY; n + 1];
    let mut from = vec![usize::MAX; n + 1];
    best[first] = 0.0;
    for j in first + 1..=n {
        if j < n && !c[j].can_start {
            continue;
        }
        let cut = if j == n {
            0.0
        } else {
            let forbid = if c[j].forbidden { FORBIDDEN_COST } else { 0.0 };
            forbid + lambda * (MAX_STRENGTH - c[j].strength).max(0.0)
        };
        let mut nearest = true;
        for i in (first..j).rev() {
            if !c[i].can_start {
                continue;
            }
            let len = c[i].header + pre[j] - pre[i];
            // ближайший кандидат допустим всегда: единица уже уложена в hard_max чанкером
            if len > b.hard_max && !nearest {
                break;
            }
            let single = nearest;
            nearest = false;
            if !best[i].is_finite() {
                continue;
            }
            let dev = (len as f64 - goal) / goal;
            let short = if j < n && len < b.target_min {
                SHORT_COST
            } else {
                0.0
            };
            let over = if !single && len > b.target_max {
                OVER_COST
            } else {
                0.0
            };
            let total = best[i] + dev * dev + short + over + cut;
            if total < best[j] {
                best[j] = total;
                from[j] = i;
            }
        }
    }
    let mut out = Vec::new();
    let mut j = n;
    while j > first {
        let i = from[j];
        if i == usize::MAX {
            // недостижимо: ближайший кандидат допустим всегда; на всякий случай — один лист
            return std::iter::once(first..n).collect();
        }
        out.push(i..j);
        j = i;
    }
    out.reverse();
    out
}

/// Окно связности: единиц прозы с каждой стороны границы.
const COHESION_WINDOW: usize = 3;
/// Слова короче (в символах) в мешок основ не идут: служебные слова и предлоги.
const MIN_WORD_CHARS: usize = 4;
/// Предел бонуса связности (глубина провала TextTiling — от 0 до 2).
const MAX_BONUS: f64 = 2.0;

/// Стоп-слова ru/en через пробел (служебные слова от 4 символов).
const STOP_WORDS: &str = concat!(
    "that this with from have were which when they them their there also into been what ",
    "then than will would could should after before about because while where these those ",
    "only other этот этой этого этом эти этих который которая которое которые которых были ",
    "было была если когда тоже также чтобы потому только после через между перед более ",
    "менее очень даже уже ещё еще всех всего всё весь свой своих можно нужно есть ",
);

fn stemmer(cyrillic: bool) -> &'static Stemmer {
    static RU: OnceLock<Stemmer> = OnceLock::new();
    static EN: OnceLock<Stemmer> = OnceLock::new();
    if cyrillic {
        RU.get_or_init(|| Stemmer::create(Algorithm::Russian))
    } else {
        EN.get_or_init(|| Stemmer::create(Algorithm::English))
    }
}

/// Мешок основ слов текста: слова от [`MIN_WORD_CHARS`] символов без стоп-слов и чисел,
/// русские — стеммер Russian, остальные — English.
pub(crate) fn stems(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= MIN_WORD_CHARS && !w.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_lowercase)
        .filter(|w| !STOP_WORDS.split_whitespace().any(|s| s == w))
        .map(|w| {
            let cyr = w.chars().any(|c| matches!(c, 'а'..='я' | 'ё'));
            stemmer(cyr).stem(&w).into_owned()
        })
        .collect()
}

/// Косинус мешков (множеств) основ: |A ∩ B| / √(|A|·|B|).
fn similarity(a: &HashSet<&String>, b: &HashSet<&String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let common = a.intersection(b).count() as f64;
    common / ((a.len() * b.len()) as f64).sqrt()
}

/// Бонус связности к силе границы перед каждой единицей (индекс `j` — граница между `j − 1`
/// и `j`): глубина провала сходства соседних окон прозы. `bags[k]` — основы единицы прозы
/// (`None` — не проза: код, список, заголовок); `eligible[j]` — граница внутри прозы
/// (между предложениями или абзацами). Бонус в `0..=2`, у прочих границ — 0.
pub(crate) fn cohesion_bonus(bags: &[Option<HashSet<String>>], eligible: &[bool]) -> Vec<f64> {
    depth_bonus(&window_similarity(bags, eligible))
}

/// Сходство окон прозы слева и справа от каждой границы (`NaN` — граница не в прозе).
fn window_similarity(bags: &[Option<HashSet<String>>], eligible: &[bool]) -> Vec<f64> {
    let n = bags.len();
    let mut sim = vec![f64::NAN; n];
    for (j, slot) in sim.iter_mut().enumerate().skip(1) {
        if !eligible.get(j).copied().unwrap_or(false) {
            continue;
        }
        let left: HashSet<&String> = (j.saturating_sub(COHESION_WINDOW)..j)
            .rev()
            .map_while(|k| bags[k].as_ref())
            .flatten()
            .collect();
        let right: HashSet<&String> = (j..n.min(j + COHESION_WINDOW))
            .map_while(|k| bags[k].as_ref())
            .flatten()
            .collect();
        *slot = similarity(&left, &right);
    }
    sim
}

/// Глубина провала сходства у каждой границы: вершины слева и справа — подъём, пока сходство
/// соседних границ не убывает. Два линейных прохода: подъём из `j` первым шагом переходит в
/// `j − 1` (при `sim[j − 1] ≥ sim[j]`) и дальше совпадает с подъёмом из `j − 1`, поэтому
/// вершина слева `top_l[j] = top_l[j − 1]`, иначе `sim[j]` (справа — зеркально). Значения
/// копируются, а не вычисляются — результат бит в бит как у прямого подъёма из каждой границы
/// (O(n²) на монотонных участках; тест эквивалентности — `depth_bonus_matches_naive`).
fn depth_bonus(sim: &[f64]) -> Vec<f64> {
    let n = sim.len();
    let mut top_l = vec![f64::NAN; n];
    for j in 1..n {
        if sim[j].is_nan() {
            continue;
        }
        top_l[j] = if j > 1 && !sim[j - 1].is_nan() && sim[j - 1] >= sim[j] {
            top_l[j - 1]
        } else {
            sim[j]
        };
    }
    let mut top_r = vec![f64::NAN; n];
    for j in (1..n).rev() {
        if sim[j].is_nan() {
            continue;
        }
        top_r[j] = if j + 1 < n && !sim[j + 1].is_nan() && sim[j + 1] >= sim[j] {
            top_r[j + 1]
        } else {
            sim[j]
        };
    }
    let mut out = vec![0.0; n];
    for (j, &s) in sim.iter().enumerate().skip(1) {
        if s.is_nan() {
            continue;
        }
        out[j] = ((top_l[j] - s) + (top_r[j] - s)).clamp(0.0, MAX_BONUS);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Прямой подъём из каждой границы (реализация задачи 17, O(n²)) — эталон для
    /// [`depth_bonus`].
    fn depth_bonus_naive(sim: &[f64]) -> Vec<f64> {
        let n = sim.len();
        let mut out = vec![0.0; n];
        for (j, &s) in sim.iter().enumerate().skip(1) {
            if s.is_nan() {
                continue;
            }
            let (mut hl, mut k) = (s, j);
            while k > 1 && !sim[k - 1].is_nan() && sim[k - 1] >= hl {
                hl = sim[k - 1];
                k -= 1;
            }
            let (mut hr, mut k) = (s, j);
            while k + 1 < n && !sim[k + 1].is_nan() && sim[k + 1] >= hr {
                hr = sim[k + 1];
                k += 1;
            }
            out[j] = ((hl - s) + (hr - s)).clamp(0.0, MAX_BONUS);
        }
        out
    }

    fn bits(v: &[f64]) -> Vec<u64> {
        v.iter().map(|x| x.to_bits()).collect()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        /// Сходства из малого набора значений (частые равенства — ветка `>=`) и `NaN`.
        #[test]
        fn depth_bonus_matches_naive(
            sim in prop::collection::vec(
                prop_oneof![
                    1 => Just(f64::NAN),
                    4 => (0u8..6).prop_map(|k| f64::from(k) / 5.0),
                    2 => 0.0f64..1.0,
                ],
                0..60,
            )
        ) {
            prop_assert_eq!(bits(&depth_bonus(&sim)), bits(&depth_bonus_naive(&sim)));
        }

        /// Весь путь `cohesion_bonus` на случайных мешках из маленького словаря.
        #[test]
        fn cohesion_bonus_matches_naive(
            units in prop::collection::vec(
                (any::<bool>(), prop::collection::hash_set(0u8..8, 0..5), any::<bool>()),
                0..40,
            )
        ) {
            let bags: Vec<Option<HashSet<String>>> = units
                .iter()
                .map(|(prose, words, _)| {
                    prose.then(|| words.iter().map(|w| format!("w{w}")).collect())
                })
                .collect();
            let eligible: Vec<bool> = units.iter().map(|u| u.2).collect();
            let sim = window_similarity(&bags, &eligible);
            prop_assert_eq!(
                bits(&cohesion_bonus(&bags, &eligible)),
                bits(&depth_bonus_naive(&sim))
            );
        }
    }

    #[test]
    fn depth_bonus_linear_on_long_monotone_run() {
        // 200 000 границ с неубывающим сходством: прямой подъём — O(n²) (≈ 2·10¹⁰ шагов)
        let n = 200_000;
        let sim: Vec<f64> = (0..n)
            .map(|j| {
                if j == 0 {
                    f64::NAN
                } else {
                    j as f64 / n as f64
                }
            })
            .collect();
        let started = std::time::Instant::now();
        let b = depth_bonus(&sim);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(b[1], (sim[n - 1] - sim[1]).clamp(0.0, MAX_BONUS));
    }

    fn cand(tokens: usize, strength: f64) -> Cand {
        Cand {
            tokens,
            header: 0,
            can_start: true,
            strength,
            forbidden: false,
        }
    }

    fn budget(goal: usize, target_min: usize, hard_max: usize) -> Budget {
        Budget {
            goal,
            target_min,
            target_max: hard_max,
            hard_max,
        }
    }

    #[test]
    fn dp_balances_and_prefers_strong_cut() {
        // 6 единиц по 100: без силы — два листа по 300
        let c: Vec<Cand> = (0..6).map(|_| cand(100, 1.0)).collect();
        assert_eq!(choose_cuts(&c, &budget(300, 200, 800), 0.0), [0..3, 3..6]);
        // сильная граница перед единицей 4 перевешивает отклонение от цели
        let mut c = c;
        c[4].strength = 4.0;
        assert_eq!(choose_cuts(&c, &budget(300, 200, 800), 1.0), [0..4, 4..6]);
        // сила не дробит группу: при любой λ два листа, а не три у target_min
        for lambda in [0.5, 1.0, 2.0, 8.0] {
            assert_eq!(choose_cuts(&c, &budget(300, 200, 800), lambda).len(), 2);
        }
        // лист из нескольких единиц не растёт за target_max без нужды
        let c: Vec<Cand> = (0..7).map(|_| cand(100, 1.0)).collect();
        let b = Budget {
            goal: 350,
            target_min: 300,
            target_max: 500,
            hard_max: 800,
        };
        for r in choose_cuts(&c, &b, 1.0) {
            assert!(r.len() * 100 <= 500, "{r:?}");
        }
    }

    #[test]
    fn dp_respects_forbidden_and_hard_max() {
        let mut c: Vec<Cand> = (0..6).map(|_| cand(100, 1.0)).collect();
        c[3].forbidden = true;
        let r = choose_cuts(&c, &budget(300, 200, 800), 1.0);
        assert!(r.iter().all(|x| x.start != 3), "{r:?}");
        // все листы в пределах hard_max
        let c: Vec<Cand> = (0..20).map(|_| cand(100, 1.0)).collect();
        for r in choose_cuts(&c, &budget(300, 200, 450), 2.0) {
            assert!(r.len() * 100 <= 450, "{r:?}");
        }
        // единица больше hard_max — отдельный лист, без паники
        let c = [cand(900, 1.0), cand(10, 1.0)];
        assert_eq!(choose_cuts(&c, &budget(300, 200, 800), 1.0), [0..1, 1..2]);
        assert!(choose_cuts(&[], &budget(300, 200, 800), 1.0).is_empty());
    }

    #[test]
    fn stems_merge_word_forms_and_skip_stop_words() {
        let a = stems("Репликация базы отстаёт, репликации нет; это было потому");
        let b = stems("replication lags; the replicas were lagging");
        assert!(a.contains(&stemmer(true).stem("репликация").into_owned()));
        assert!(!a.iter().any(|s| s.starts_with("потом")));
        assert!(b.contains("lag"));
        assert!(!b.contains("were"));
    }

    #[test]
    fn cohesion_peaks_at_topic_change() {
        let bag = |s: &str| Some(stems(s));
        let bags = vec![
            bag("репликация базы отстаёт журнал растёт"),
            bag("репликация базы отстаёт журнал растёт"),
            bag("репликация базы отстаёт журнал растёт"),
            bag("сертификаты шлюза истекают обновление падает"),
            bag("сертификаты шлюза истекают обновление падает"),
            bag("сертификаты шлюза истекают обновление падает"),
        ];
        let eligible = vec![false, true, true, true, true, true];
        let b = cohesion_bonus(&bags, &eligible);
        let max = (1..6).max_by(|&x, &y| b[x].total_cmp(&b[y])).unwrap();
        assert_eq!(max, 3, "{b:?}");
        assert!(b[3] > 1.0 && b[1] < 0.5, "{b:?}");
        assert_eq!(b[0], 0.0);
    }
}
