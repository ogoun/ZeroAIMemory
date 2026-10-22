//! Поправки ранжирования (§9.2 пп. 4, 6).
//!
//! - затухание по свежести ([`recency_factor`], внутри слияния);
//! - бусты ([`apply_boosts`]): вид инсайта (`gotcha`, `failure`), надёжность (`certainty`
//!   заметки или записи: `search.boost_axiom` > `boost_theorem` > `boost_hypothesis`, §7.5) и
//!   текущая область (`X-Zam-Area`) — множители слитого балла;
//! - замена сводки листом и лимит на цель.
//!
//! `superseded`/`retracted` исключает фильтр статусов поиска (сервер), а не поправка.

use std::collections::HashMap;

use jiff::Timestamp;
use zam_core::TargetRef;

use crate::hybrid::{ChunkMeta, Level, Ranked};

/// Множители балла (§9.2 п. 4). Пустые таблицы и `area: None` — без бустов.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Boosts {
    /// Вид инсайта → множитель (`gotcha`, `failure` → `search.boost_gotcha`, `boost_failure`).
    pub insight_kinds: HashMap<String, f64>,
    /// `certainty` заметки или записи → множитель (`axiom > theorem > hypothesis`,
    /// `search.boost_axiom`/`boost_theorem`/`boost_hypothesis`); без статуса — 1.
    pub certainty: HashMap<String, f64>,
    /// Текущая область и её множитель (`search.boost_area`).
    pub area: Option<(String, f64)>,
}

impl Boosts {
    /// Итоговый множитель чанка: произведение подходящих бустов (нет подходящих — 1).
    fn factor(&self, m: &ChunkMeta) -> f64 {
        let kind = m
            .insight_kind
            .as_ref()
            .and_then(|k| self.insight_kinds.get(k))
            .copied()
            .unwrap_or(1.0);
        let certainty = m
            .certainty
            .as_ref()
            .and_then(|c| self.certainty.get(c))
            .copied()
            .unwrap_or(1.0);
        let area = match (&self.area, &m.area) {
            (Some((want, f)), Some(a)) if a == want => *f,
            _ => 1.0,
        };
        kind * certainty * area
    }
}

/// Умножает балл каждого кандидата на его бусты ([`Boosts`]) и пересортировывает по
/// убыванию балла (при равенстве — прежний порядок). Кандидат без метаданных не меняется.
/// Балл после бустов может превысить 1: порядок важнее шкалы, шкалу при выдаче ограничивает
/// вызывающий.
pub fn apply_boosts(r: &mut [Ranked], meta: &HashMap<String, ChunkMeta>, b: &Boosts) {
    for x in r.iter_mut() {
        if let Some(m) = meta.get(&x.chunk_id) {
            x.score *= b.factor(m);
        }
    }
    r.sort_by(|a, b| b.score.total_cmp(&a.score));
}

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

/// Подходит ли лист `leaf` на замену сводке `s`: та же цель (узел), для `section` — та же
/// секция (или вложенная).
fn leaf_fits(s: &Ranked, leaf: &Ranked) -> bool {
    leaf.level == Level::Chunk
        && leaf.target == s.target
        && (s.level == Level::Note || in_section(&leaf.heading_path, &s.heading_path))
}

/// Сводки из первых `window` элементов `r`, для которых во **всём** `r` нет подходящего
/// листа: для них вызывающий выбирает лист сам (`best_leaf` у [`note_to_leaf`]).
pub fn summaries_without_leaf(r: &[Ranked], window: usize) -> Vec<&Ranked> {
    r.iter()
        .take(window)
        .filter(|s| s.level.is_summary() && !r.iter().any(|l| leaf_fits(s, l)))
        .collect()
}

/// Убирает сводки за окном (`window` первых элементов), у которых в `r` нет подходящего
/// листа: лист им не выбирался ([`summaries_without_leaf`] берёт только окно), а после
/// слияний они могли бы подняться в выдачу как сводки. Сводки со своим листом в `r`
/// остаются — [`note_to_leaf`] сольёт их с ним.
pub fn drop_unresolved_tail(r: Vec<Ranked>, window: usize) -> Vec<Ranked> {
    let keep: Vec<bool> = r
        .iter()
        .enumerate()
        .map(|(i, s)| i < window || !s.level.is_summary() || r.iter().any(|l| leaf_fits(s, l)))
        .collect();
    r.into_iter()
        .zip(keep)
        .filter_map(|(x, k)| k.then_some(x))
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

/// Не больше `max` элементов с одной цели — узла или записи журнала (§9.2 п. 6), порядок
/// сохраняется.
pub fn cap_per_node(mut r: Vec<Ranked>, max: usize) -> Vec<Ranked> {
    let mut per_node: HashMap<TargetRef, usize> = HashMap::new();
    r.retain(|x| {
        let n = per_node.entry(x.target).or_default();
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
    use zam_core::Id;

    fn r(chunk: &str, node: Id, level: Level, score: f64, src: &'static str) -> Ranked {
        rh(chunk, node, level, "", score, src)
    }

    fn rh(chunk: &str, node: Id, level: Level, hp: &str, score: f64, src: &'static str) -> Ranked {
        Ranked {
            chunk_id: chunk.into(),
            target: TargetRef {
                kind: zam_core::TargetKind::Node,
                id: node,
            },
            level,
            heading_path: hp.into(),
            score,
            sources: vec![src],
        }
    }

    fn meta(insight_kind: Option<&str>, area: Option<&str>) -> ChunkMeta {
        ChunkMeta {
            target: TargetRef {
                kind: zam_core::TargetKind::Entry,
                id: Id::new(),
            },
            level: Level::Chunk,
            heading_path: String::new(),
            updated_at: Timestamp::UNIX_EPOCH,
            insight_kind: insight_kind.map(String::from),
            certainty: None,
            area: area.map(String::from),
            space_id: Id::new(),
            path: String::new(),
        }
    }

    fn boosts() -> Boosts {
        Boosts {
            insight_kinds: [("gotcha".to_string(), 1.15), ("failure".to_string(), 1.15)]
                .into_iter()
                .collect(),
            certainty: HashMap::new(),
            area: None,
        }
    }

    fn metas(v: Vec<(&str, ChunkMeta)>) -> HashMap<String, ChunkMeta> {
        v.into_iter().map(|(k, m)| (k.to_string(), m)).collect()
    }

    #[test]
    fn boost_gotcha_orders_above_equal_note() {
        // равный балл: заметка раньше по порядку, gotcha и failure поднимаются над ней,
        // discovery (без буста) остаётся на месте
        let n = Id::new();
        let mut list = vec![
            r("note", n, Level::Chunk, 0.5, "fts"),
            r("discovery", Id::new(), Level::Chunk, 0.5, "fts"),
            r("gotcha", Id::new(), Level::Chunk, 0.5, "fts"),
            r("failure", Id::new(), Level::Chunk, 0.5, "fts"),
        ];
        let meta = metas(vec![
            ("note", meta(None, None)),
            ("discovery", meta(Some("discovery"), None)),
            ("gotcha", meta(Some("gotcha"), None)),
            ("failure", meta(Some("failure"), None)),
        ]);
        apply_boosts(&mut list, &meta, &boosts());
        let ids: Vec<&str> = list.iter().map(|x| x.chunk_id.as_str()).collect();
        assert_eq!(ids, vec!["gotcha", "failure", "note", "discovery"]);
        assert!((list[0].score - 0.575).abs() < 1e-12);
        assert_eq!(list[2].score, 0.5);
        // буст — множитель, а не подмена ранга: заметно лучшая заметка остаётся выше
        let mut list = vec![
            r("note", n, Level::Chunk, 0.9, "fts"),
            r("gotcha", Id::new(), Level::Chunk, 0.6, "fts"),
        ];
        apply_boosts(&mut list, &meta, &boosts());
        assert_eq!(list[0].chunk_id, "note");
        // без бустов порядок и баллы прежние
        let before = list.clone();
        apply_boosts(&mut list, &meta, &Boosts::default());
        assert_eq!(list, before);
    }

    #[test]
    fn certainty_boost_orders_reliable_first() {
        let with_c = |c: Option<&str>| ChunkMeta {
            certainty: c.map(String::from),
            ..meta(None, None)
        };
        // равный балл: гипотеза и заметка без статуса раньше по порядку
        let mut list = vec![
            r("hyp", Id::new(), Level::Chunk, 0.5, "fts"),
            r("plain", Id::new(), Level::Chunk, 0.5, "fts"),
            r("thm", Id::new(), Level::Chunk, 0.5, "fts"),
            r("ax", Id::new(), Level::Chunk, 0.5, "fts"),
        ];
        let meta = metas(vec![
            ("hyp", with_c(Some("hypothesis"))),
            ("plain", with_c(None)),
            ("thm", with_c(Some("theorem"))),
            ("ax", with_c(Some("axiom"))),
        ]);
        let b = Boosts {
            certainty: [("axiom", 1.20), ("theorem", 1.10), ("hypothesis", 1.00)]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            ..Boosts::default()
        };
        apply_boosts(&mut list, &meta, &b);
        let ids: Vec<&str> = list.iter().map(|x| x.chunk_id.as_str()).collect();
        assert_eq!(ids, vec!["ax", "thm", "hyp", "plain"]);
        assert!((list[0].score - 0.6).abs() < 1e-12);
    }

    #[test]
    fn area_boost_multiplies_only_matching() {
        let mut list = vec![
            r("other", Id::new(), Level::Chunk, 0.5, "fts"),
            r("none", Id::new(), Level::Chunk, 0.5, "fts"),
            r("web", Id::new(), Level::Chunk, 0.5, "fts"),
            r("gotcha-web", Id::new(), Level::Chunk, 0.5, "fts"),
            r("unknown", Id::new(), Level::Chunk, 0.5, "fts"),
        ];
        let meta = metas(vec![
            ("other", meta(None, Some("api"))),
            ("none", meta(None, None)),
            ("web", meta(None, Some("web"))),
            ("gotcha-web", meta(Some("gotcha"), Some("web"))),
        ]);
        let b = Boosts {
            area: Some(("web".into(), 1.10)),
            ..boosts()
        };
        apply_boosts(&mut list, &meta, &b);
        let got: Vec<(&str, f64)> = list
            .iter()
            .map(|x| (x.chunk_id.as_str(), (x.score * 1e6).round() / 1e6))
            .collect();
        // бусты перемножаются; область — точное совпадение; без метаданных — без изменений
        assert_eq!(
            got,
            vec![
                ("gotcha-web", 0.6325),
                ("web", 0.55),
                ("other", 0.5),
                ("none", 0.5),
                ("unknown", 0.5),
            ]
        );
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
        assert!(summaries_without_leaf(&list, 10).is_empty());
        let out = cap_per_node(note_to_leaf(list, |_| unreachable!()), 3);
        let got: Vec<(&str, f64)> = out.iter().map(|x| (x.chunk_id.as_str(), x.score)).collect();
        assert_eq!(got, vec![("leaf-a", 0.9), ("leaf-b", 0.7)]);
        assert_eq!(out[1].sources, vec!["vector", "fts"]);
        // без листа своей секции — best_leaf, а не лист чужой секции
        let list = vec![
            rh("leaf-a", n, Level::Chunk, "A", 0.8, "fts"),
            rh("sec-b", n, Level::Section, "B", 0.7, "vector"),
        ];
        assert_eq!(summaries_without_leaf(&list, 10).len(), 1);
        let out = note_to_leaf(list, |x| {
            assert_eq!(x.chunk_id, "sec-b");
            leaf("leaf-b", "B")
        });
        assert_eq!(out[1].chunk_id, "leaf-b");
        assert_eq!(out[1].heading_path, "B");
    }

    #[test]
    fn window_summaries_and_tail() {
        let (n, m, k) = (Id::new(), Id::new(), Id::new());
        let list = vec![
            // в окне: сводка n, её лист — за окном; сводка m без листа
            rh("n-note", n, Level::Note, "", 0.9, "vector"),
            rh("m-sec", m, Level::Section, "S", 0.8, "vector"),
            rh("x", k, Level::Chunk, "", 0.7, "fts"),
            // за окном: лист n и сводка k-секции без листа своей секции
            rh("n-leaf", n, Level::Chunk, "A", 0.5, "fts"),
            rh("k-sec", k, Level::Section, "B", 0.4, "fts"),
        ];
        // лист ищется по всему списку: сводке n лист не нужен, сводке m — нужен
        let need: Vec<&str> = summaries_without_leaf(&list, 3)
            .iter()
            .map(|r| r.chunk_id.as_str())
            .collect();
        assert_eq!(need, vec!["m-sec"]);
        // сводка за окном без своего листа отбрасывается, сводка в окне и лист — нет
        let pruned = drop_unresolved_tail(list, 3);
        let ids: Vec<&str> = pruned.iter().map(|r| r.chunk_id.as_str()).collect();
        assert_eq!(ids, vec!["n-note", "m-sec", "x", "n-leaf"]);
        // итог: ни одной сводки, у которой есть подходящий лист
        let out = note_to_leaf(pruned, |r| {
            assert_eq!(r.chunk_id, "m-sec");
            None
        });
        let ids: Vec<(&str, Level)> = out.iter().map(|r| (r.chunk_id.as_str(), r.level)).collect();
        assert_eq!(
            ids,
            vec![
                ("n-leaf", Level::Chunk),
                ("m-sec", Level::Section),
                ("x", Level::Chunk)
            ]
        );
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
            if x.target.id == b {
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
