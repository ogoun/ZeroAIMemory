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

/// Лист, которым заменяется сводка.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafRef {
    pub chunk_id: String,
    pub heading_path: String,
}

/// Лист с путём `leaf` лежит в секции `section` (равен ей или вложен: `A > B` в `A`).
/// Пустой путь секции — вся заметка.
pub fn in_section(leaf: &str, section: &str) -> bool {
    section.is_empty()
        || leaf == section
        || leaf
            .strip_prefix(section)
            .is_some_and(|rest| rest.starts_with(" > "))
}

/// Подходит ли лист `leaf` на замену сводке `s`: тот же узел, для `section` — та же
/// секция (или вложенная).
fn leaf_fits(s: &Ranked, leaf: &Ranked) -> bool {
    leaf.level == Level::Chunk
        && leaf.node_id == s.node_id
        && (s.level == Level::Note || in_section(&leaf.heading_path, &s.heading_path))
}

/// Сводки, для которых в `r` нет подходящего листа: для них вызывающий выбирает лист
/// сам (`best_leaf` у [`note_to_leaf`]).
pub fn summaries_without_leaf(r: &[Ranked]) -> Vec<&Ranked> {
    r.iter()
        .filter(|s| s.level.is_summary() && !r.iter().any(|l| leaf_fits(s, l)))
        .collect()
}

/// §9.2 п. 6: сводка заметки или секции (`note`/`section`) в выдаче заменяется листом.
///
/// - Если подходящий лист уже есть в выдаче (для `note` — любой лист узла, для `section` —
///   лист той же секции или вложенной), сводка с ним сливается: балл — больший из двух,
///   каналы — объединение, место — по новому баллу.
/// - Иначе вместо сводки берётся `best_leaf(сводка)` (выбирает вызывающий: например,
///   ближайший к запросу по косинусу лист секции); `None` — сводка остаётся как есть.
///
/// Повторы одного `chunk_id` после замены сливаются так же. Порядок — по убыванию балла,
/// при равенстве — прежний. Лимит на узел — после замены ([`cap_per_node`]).
pub fn note_to_leaf(r: Vec<Ranked>, best_leaf: impl Fn(&Ranked) -> Option<LeafRef>) -> Vec<Ranked> {
    // найденные листы в порядке выдачи: первый подходящий — лучший
    let leaves: Vec<Ranked> = r
        .iter()
        .filter(|x| x.level == Level::Chunk)
        .cloned()
        .collect();
    let mut out: Vec<Ranked> = Vec::with_capacity(r.len());
    let mut pos: HashMap<String, usize> = HashMap::new();
    for mut x in r {
        if x.level.is_summary() {
            let target = match leaves.iter().find(|l| leaf_fits(&x, l)) {
                Some(l) => Some(LeafRef {
                    chunk_id: l.chunk_id.clone(),
                    heading_path: l.heading_path.clone(),
                }),
                None => best_leaf(&x),
            };
            if let Some(l) = target {
                x.chunk_id = l.chunk_id;
                x.heading_path = l.heading_path;
                x.level = Level::Chunk;
            }
        }
        push_merged(&mut out, &mut pos, x);
    }
    // sort_by стабилен: при равном балле порядок прежний
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

/// Не больше `max` элементов с одного узла (§9.2 п. 6), порядок сохраняется.
pub fn cap_per_node(mut r: Vec<Ranked>, max: usize) -> Vec<Ranked> {
    let mut per_node: HashMap<Id, usize> = HashMap::new();
    r.retain(|x| {
        let n = per_node.entry(x.node_id).or_default();
        *n += 1;
        *n <= max.max(1)
    });
    r
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
        rh(chunk, node, level, "", score, src)
    }

    fn rh(chunk: &str, node: Id, level: Level, hp: &str, score: f64, src: &'static str) -> Ranked {
        Ranked {
            chunk_id: chunk.into(),
            node_id: node,
            level,
            heading_path: hp.into(),
            score,
            sources: vec![src],
        }
    }

    fn leaf(id: &str, hp: &str) -> Option<LeafRef> {
        Some(LeafRef {
            chunk_id: id.into(),
            heading_path: hp.into(),
        })
    }

    #[test]
    fn section_summary_merges_only_into_leaf_of_its_section() {
        // note 0.9, лист секции A 0.8, сводка секции B 0.7, лист секции B 0.6: обе сводки
        // не должны схлопнуться в лист A, лист B не теряется и лимит на узел — после замены
        let n = Id::new();
        let list = vec![
            rh("note", n, Level::Note, "", 0.9, "vector"),
            rh("leaf-a", n, Level::Chunk, "A", 0.8, "fts"),
            rh("sec-b", n, Level::Section, "B", 0.7, "vector"),
            rh("leaf-b", n, Level::Chunk, "B > C", 0.6, "fts"),
        ];
        assert!(summaries_without_leaf(&list).is_empty());
        let out = cap_per_node(note_to_leaf(list, |_| unreachable!()), 3);
        let got: Vec<(&str, f64)> = out.iter().map(|x| (x.chunk_id.as_str(), x.score)).collect();
        assert_eq!(got, vec![("leaf-a", 0.9), ("leaf-b", 0.7)]);
        assert_eq!(out[1].sources, vec!["vector", "fts"]);
        // без листа своей секции — best_leaf, а не лист чужой секции
        let list = vec![
            rh("leaf-a", n, Level::Chunk, "A", 0.8, "fts"),
            rh("sec-b", n, Level::Section, "B", 0.7, "vector"),
        ];
        assert_eq!(summaries_without_leaf(&list).len(), 1);
        let out = note_to_leaf(list, |x| {
            assert_eq!(x.chunk_id, "sec-b");
            leaf("leaf-b", "B")
        });
        assert_eq!(out[1].chunk_id, "leaf-b");
        assert_eq!(out[1].heading_path, "B");
    }

    #[test]
    fn section_paths() {
        assert!(in_section("A", "A") && in_section("A > B", "A") && in_section("x", ""));
        assert!(!in_section("AB", "A") && !in_section("B", "A") && !in_section("A", "A > B"));
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
        let out = note_to_leaf(list, |x| {
            if x.node_id == b {
                leaf("b-leaf", "")
            } else {
                None
            }
        });
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
            |_| leaf("leaf", ""),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].chunk_id, "leaf");
        assert_eq!(out[0].score, 0.6);
        assert_eq!(out[0].sources, vec!["vector", "fts"]);
    }
}
