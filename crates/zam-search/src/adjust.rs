//! Поправки ранжирования (§9.2 пп. 4, 6).
//!
//! Бусты `gotcha`/`failure` и текущей области — в M3 (журналов и областей ещё нет);
//! `superseded`/`retracted` — тоже в M3.

use std::collections::HashMap;

use jiff::Timestamp;
use zam_core::Id;

use crate::hybrid::{Level, Ranked};

/// Нижняя граница множителя свежести **[решение исполнителя]**: память хранит и давние
/// правила, и решения, поэтому старая заметка теряет не больше половины балла и
/// по-прежнему обгоняет свежую с заметно худшим рангом.
pub const RECENCY_FLOOR: f64 = 0.5;

const SECS_PER_DAY: f64 = 86_400.0;

/// Множитель свежести: `floor + (1 − floor) · 2^(−возраст / полупериод)`, где
/// `floor` = [`RECENCY_FLOOR`]. Свежая заметка — 1,0, на возрасте полупериода — 0,75,
/// дальше стремится к `floor`. Полупериод ≤ 0 или изменение «в будущем» — 1,0.
pub fn recency_factor(updated_at: Timestamp, now: Timestamp, half_life_days: f64) -> f64 {
    if half_life_days.is_nan() || half_life_days <= 0.0 {
        return 1.0;
    }
    let age_days = (now.as_second() - updated_at.as_second()).max(0) as f64 / SECS_PER_DAY;
    RECENCY_FLOOR + (1.0 - RECENCY_FLOOR) * (-age_days / half_life_days).exp2()
}

/// §9.2 п. 6: сводка заметки или секции (`note`/`section`) в выдаче заменяется лучшим
/// листом узла.
///
/// - Если лист того же узла уже есть в выдаче, сводка с ним сливается: балл — больший из
///   двух, каналы — объединение, место — по новому баллу.
/// - Иначе вместо сводки берётся лист `best_leaf(сводка)` (выбирает вызывающий: например,
///   ближайший к запросу по косинусу лист секции); `None` — сводка остаётся как есть.
///
/// Повторы одного `chunk_id` после замены сливаются так же. Порядок — по убыванию балла,
/// при равенстве — прежний.
pub fn note_to_leaf(r: Vec<Ranked>, best_leaf: impl Fn(&Ranked) -> Option<String>) -> Vec<Ranked> {
    // лучший (первый по месту) лист каждого узла среди уже найденных
    let mut leaf_of: HashMap<Id, String> = HashMap::new();
    for x in &r {
        if x.level == Level::Chunk {
            leaf_of
                .entry(x.node_id)
                .or_insert_with(|| x.chunk_id.clone());
        }
    }
    let mut out: Vec<Ranked> = Vec::with_capacity(r.len());
    let mut pos: HashMap<String, usize> = HashMap::new();
    for mut x in r {
        if x.level.is_summary() {
            let target = match leaf_of.get(&x.node_id) {
                Some(id) => Some(id.clone()),
                None => best_leaf(&x),
            };
            if let Some(id) = target {
                x.chunk_id = id;
                x.level = Level::Chunk;
            }
        }
        push_merged(&mut out, &mut pos, x);
    }
    // sort_by стабилен: при равном балле порядок прежний
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

/// Добавляет `x` или сливает с уже добавленным тем же `chunk_id`.
fn push_merged(out: &mut Vec<Ranked>, pos: &mut HashMap<String, usize>, x: Ranked) {
    match pos.get(&x.chunk_id) {
        Some(&k) => merge(&mut out[k], x),
        None => {
            pos.insert(x.chunk_id.clone(), out.len());
            out.push(x);
        }
    }
}

fn merge(into: &mut Ranked, x: Ranked) {
    into.score = into.score.max(x.score);
    for s in x.sources {
        if !into.sources.contains(&s) {
            into.sources.push(s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::SignedDuration;

    fn r(chunk: &str, node: Id, level: Level, score: f64, src: &'static str) -> Ranked {
        Ranked {
            chunk_id: chunk.into(),
            node_id: node,
            level,
            score,
            sources: vec![src],
        }
    }

    #[test]
    fn recency_factor_curve() {
        let now: Timestamp = "2026-10-19T00:00:00Z".parse().unwrap();
        let day = SignedDuration::from_hours(24);
        assert_eq!(recency_factor(now, now, 180.0), 1.0);
        let half = recency_factor(now - day * 180, now, 180.0);
        assert!((half - 0.75).abs() < 1e-9, "{half}");
        let old = recency_factor(now - day * 3650, now, 180.0);
        assert!(old > RECENCY_FLOOR && old < RECENCY_FLOOR + 1e-5, "{old}");
        // выключено и «будущее»
        assert_eq!(recency_factor(now - day * 3650, now, 0.0), 1.0);
        assert_eq!(recency_factor(now + day, now, 180.0), 1.0);
    }

    #[test]
    fn summary_merges_into_found_leaf_or_takes_best_leaf() {
        let (a, b, c) = (Id::new(), Id::new(), Id::new());
        let list = vec![
            // сводка a выше своего листа: лист поднимается на её место
            r("a-note", a, Level::Note, 0.9, "vector"),
            r("b-sec", b, Level::Section, 0.8, "vector"),
            r("x", c, Level::Chunk, 0.7, "fts"),
            r("a-leaf", a, Level::Chunk, 0.5, "fts"),
            // сводка без листа и без замены остаётся
            r("c-none", Id::new(), Level::Note, 0.4, "fts"),
        ];
        let out = note_to_leaf(list, |x| (x.node_id == b).then(|| "b-leaf".to_string()));
        let got: Vec<(&str, f64, Level)> = out
            .iter()
            .map(|x| (x.chunk_id.as_str(), x.score, x.level))
            .collect();
        assert_eq!(
            got,
            vec![
                ("a-leaf", 0.9, Level::Chunk),
                ("b-leaf", 0.8, Level::Chunk),
                ("x", 0.7, Level::Chunk),
                ("c-none", 0.4, Level::Note),
            ]
        );
        assert_eq!(out[0].sources, vec!["vector", "fts"]);
    }

    #[test]
    fn two_summaries_to_same_leaf_merge() {
        let a = Id::new();
        let out = note_to_leaf(
            vec![
                r("n", a, Level::Note, 0.6, "vector"),
                r("s", a, Level::Section, 0.3, "fts"),
            ],
            |_| Some("leaf".into()),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].chunk_id, "leaf");
        assert_eq!(out[0].score, 0.6);
        assert_eq!(out[0].sources, vec!["vector", "fts"]);
    }
}
