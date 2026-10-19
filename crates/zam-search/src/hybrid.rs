//! Слияние каналов weighted RRF (§9.2 пп. 3–4, 6).
//!
//! Балл чанка — `Σ w_канала / (rrf_k + ранг)` по каналам, где он найден (ранг с 1),
//! умноженный на [`recency_factor`] узла и нормированный на наибольший возможный балл
//! (первое место во всех непустых каналах у свежей заметки = 1,0). Затем — порядок по
//! убыванию балла (при равенстве — по `chunk_id`, чтобы выдача была детерминированной).
//!
//! Лимит `max_per_node` чанков с узла здесь не применяется: сводки ещё не заменены листами
//! ([`note_to_leaf`](crate::note_to_leaf)), и ранний лимит отбрасывал бы листы, в которые
//! сводки потом сливаются. Порядок вызовов: `fuse` → `note_to_leaf` →
//! [`cap_per_node`](crate::cap_per_node).

use std::collections::HashMap;

use jiff::Timestamp;
use zam_core::Id;

use crate::adjust::recency_factor;

/// Канал кандидатов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    /// Qdrant: dense + sparse по листам и dense по сводкам, слитые RRF в Qdrant.
    Vector,
    /// FTS5 `bm25` (unicode61).
    Fts,
    /// FTS5 `trigram`: идентификаторы и код.
    Trigram,
}

impl Channel {
    /// Имя в `sources` ответа.
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Vector => "vector",
            Channel::Fts => "fts",
            Channel::Trigram => "trigram",
        }
    }
}

/// Уровень чанка (§8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Лист — текст заметки.
    Chunk,
    /// L1 секции (сводка).
    Section,
    /// L1 заметки (сводка).
    Note,
}

impl Level {
    /// `chunk` / `section` / `note`; прочее — `None` (папки и вложения в M2 не ищутся).
    pub fn parse(s: &str) -> Option<Level> {
        match s {
            "chunk" => Some(Level::Chunk),
            "section" => Some(Level::Section),
            "note" => Some(Level::Note),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Chunk => "chunk",
            Level::Section => "section",
            Level::Note => "note",
        }
    }

    /// Сводка (L1), а не текст.
    pub fn is_summary(self) -> bool {
        self != Level::Chunk
    }
}

/// Метаданные чанка из SQLite (только живые чанки области).
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkMeta {
    pub node_id: Id,
    pub level: Level,
    /// Путь заголовков (`A > B`): секция сводки уровня `section` и листа.
    pub heading_path: String,
    /// Время последнего изменения узла — для затухания.
    pub updated_at: Timestamp,
}

/// Входы слияния: каналы — `chunk_id` по убыванию релевантности канала.
#[derive(Debug, Clone, Default)]
pub struct SearchInputs {
    pub fts: Vec<String>,
    pub trigram: Vec<String>,
    /// `None` — векторный канал выключен (деградация) или не запускался.
    pub vector: Option<Vec<String>>,
    /// Метаданные всех кандидатов. Кандидат без метаданных отбрасывается.
    pub chunks: HashMap<String, ChunkMeta>,
}

/// Параметры слияния (секция `[search]`).
#[derive(Debug, Clone, PartialEq)]
pub struct Weights {
    /// Константа RRF.
    pub rrf_k: f64,
    pub w_vector: f64,
    pub w_fts: f64,
    pub w_trigram: f64,
    /// Полупериод затухания по свежести, дней; 0 — без затухания.
    pub recency_half_life_days: f64,
    /// Не больше чанков с одного узла.
    pub max_per_node: usize,
}

impl Default for Weights {
    fn default() -> Self {
        Weights {
            rrf_k: 60.0,
            w_vector: 1.0,
            w_fts: 1.0,
            w_trigram: 0.7,
            recency_half_life_days: 180.0,
            max_per_node: 3,
        }
    }
}

/// Кандидат после слияния.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    pub chunk_id: String,
    pub node_id: Id,
    pub level: Level,
    /// Путь заголовков чанка.
    pub heading_path: String,
    /// Нормированный балл, 0..=1.
    pub score: f64,
    /// Каналы, нашедшие чанк, в порядке `vector`, `fts`, `trigram`.
    pub sources: Vec<&'static str>,
}

/// Weighted RRF + затухание, без лимита на узел (см. модуль).
pub fn fuse(inp: &SearchInputs, w: &Weights, now: Timestamp) -> Vec<Ranked> {
    let empty = Vec::new();
    let channels = [
        (
            Channel::Vector,
            w.w_vector,
            inp.vector.as_ref().unwrap_or(&empty),
        ),
        (Channel::Fts, w.w_fts, &inp.fts),
        (Channel::Trigram, w.w_trigram, &inp.trigram),
    ];
    let mut by_chunk: HashMap<&str, (f64, Vec<&'static str>)> = HashMap::new();
    let mut max = 0.0;
    for (ch, weight, list) in channels {
        let mut seen = false;
        // ранг — среди живых кандидатов канала: отброшенные не сдвигают остальных вниз
        let mut rank = 0usize;
        for id in list {
            if !inp.chunks.contains_key(id.as_str()) {
                continue;
            }
            let e = by_chunk.entry(id.as_str()).or_insert((0.0, Vec::new()));
            if e.1.contains(&ch.as_str()) {
                continue;
            }
            rank += 1;
            seen = true;
            e.0 += weight / (w.rrf_k + rank as f64);
            e.1.push(ch.as_str());
        }
        if seen {
            max += weight / (w.rrf_k + 1.0);
        }
    }
    let mut all: Vec<Ranked> = by_chunk
        .into_iter()
        .map(|(id, (raw, sources))| {
            let m = &inp.chunks[id];
            let decay = recency_factor(m.updated_at, now, w.recency_half_life_days);
            let score = if max > 0.0 { raw * decay / max } else { 0.0 };
            Ranked {
                chunk_id: id.to_string(),
                node_id: m.node_id,
                level: m.level,
                heading_path: m.heading_path.clone(),
                score,
                sources,
            }
        })
        .collect();
    all.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
    });
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Timestamp {
        "2026-10-19T12:00:00Z".parse().unwrap()
    }

    /// Чанки `c<i>` на узлах из `nodes[i]`, все обновлены сейчас.
    fn meta(pairs: &[(&str, Id)]) -> HashMap<String, ChunkMeta> {
        pairs
            .iter()
            .map(|(c, n)| {
                (
                    c.to_string(),
                    ChunkMeta {
                        node_id: *n,
                        level: Level::Chunk,
                        heading_path: String::new(),
                        updated_at: now(),
                    },
                )
            })
            .collect()
    }

    fn ids(r: &[Ranked]) -> Vec<&str> {
        r.iter().map(|x| x.chunk_id.as_str()).collect()
    }

    fn list(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn both_channels_beat_one() {
        let (a, b, c) = (Id::new(), Id::new(), Id::new());
        let inp = SearchInputs {
            // «b» второй в обоих каналах, «a» и «c» — первые, но в одном
            fts: list(&["a", "b"]),
            vector: Some(list(&["c", "b"])),
            chunks: meta(&[("a", a), ("b", b), ("c", c)]),
            ..Default::default()
        };
        let r = fuse(&inp, &Weights::default(), now());
        assert_eq!(r[0].chunk_id, "b");
        assert_eq!(r[0].sources, vec!["vector", "fts"]);
        // равные баллы «a» и «c» — по chunk_id
        assert_eq!((r[1].chunk_id.as_str(), &r[1].sources), ("a", &vec!["fts"]));
        assert!(r.iter().all(|x| x.score > 0.0 && x.score <= 1.0));
    }

    #[test]
    fn weights_change_order() {
        let (a, b) = (Id::new(), Id::new());
        let mk = |wv: f64, wf: f64| {
            let inp = SearchInputs {
                fts: list(&["a"]),
                vector: Some(list(&["b"])),
                chunks: meta(&[("a", a), ("b", b)]),
                ..Default::default()
            };
            let w = Weights {
                w_vector: wv,
                w_fts: wf,
                ..Weights::default()
            };
            ids(&fuse(&inp, &w, now()))
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(mk(2.0, 1.0), vec!["b", "a"]);
        assert_eq!(mk(1.0, 2.0), vec!["a", "b"]);
        // trigram со своим весом
        let (x, y) = (Id::new(), Id::new());
        let inp = SearchInputs {
            fts: list(&["x"]),
            trigram: list(&["y"]),
            chunks: meta(&[("x", x), ("y", y)]),
            ..Default::default()
        };
        assert_eq!(ids(&fuse(&inp, &Weights::default(), now())), vec!["x", "y"]);
        let w = Weights {
            w_trigram: 1.5,
            ..Weights::default()
        };
        assert_eq!(ids(&fuse(&inp, &w, now())), vec!["y", "x"]);
    }

    #[test]
    fn recency_decay_at_equal_rank_scores() {
        let (old, fresh) = (Id::new(), Id::new());
        let mut chunks = meta(&[("old", old), ("fresh", fresh)]);
        chunks.get_mut("old").unwrap().updated_at =
            now() - jiff::SignedDuration::from_hours(24 * 365);
        chunks.get_mut("fresh").unwrap().updated_at = now() - jiff::SignedDuration::from_hours(24);
        // одинаковый ранговый балл: первые места в каналах с равными весами
        let inp = SearchInputs {
            fts: list(&["old"]),
            vector: Some(list(&["fresh"])),
            chunks,
            ..Default::default()
        };
        let r = fuse(&inp, &Weights::default(), now());
        assert_eq!(ids(&r), vec!["fresh", "old"]);
        assert!(r[1].score < r[0].score);
        // без затухания — равные баллы, порядок по chunk_id
        let w = Weights {
            recency_half_life_days: 0.0,
            ..Weights::default()
        };
        let r = fuse(&inp, &w, now());
        assert_eq!(r[0].score, r[1].score);
    }

    #[test]
    fn at_most_three_per_node() {
        let n = Id::new();
        let m = Id::new();
        let pairs: Vec<(String, Id)> = (0..6)
            .map(|i| (format!("n{i}"), n))
            .chain([("m0".to_string(), m)])
            .collect();
        let refs: Vec<(&str, Id)> = pairs.iter().map(|(c, i)| (c.as_str(), *i)).collect();
        let inp = SearchInputs {
            fts: pairs.iter().map(|(c, _)| c.clone()).collect(),
            chunks: meta(&refs),
            ..Default::default()
        };
        // fuse лимит не применяет: его применяет cap_per_node после note_to_leaf
        let all = fuse(&inp, &Weights::default(), now());
        assert_eq!(all.len(), 7);
        let r = crate::cap_per_node(all.clone(), Weights::default().max_per_node);
        assert_eq!(ids(&r), vec!["n0", "n1", "n2", "m0"]);
        assert_eq!(ids(&crate::cap_per_node(all, 1)), vec!["n0", "m0"]);
    }

    #[test]
    fn empty_vector_channel_keeps_fts_order() {
        let ns: Vec<Id> = (0..5).map(|_| Id::new()).collect();
        let names = ["e", "a", "d", "b", "c"];
        let pairs: Vec<(&str, Id)> = names.iter().copied().zip(ns.iter().copied()).collect();
        for vector in [None, Some(Vec::new())] {
            let inp = SearchInputs {
                fts: list(&names),
                vector,
                chunks: meta(&pairs),
                ..Default::default()
            };
            let r = fuse(&inp, &Weights::default(), now());
            assert_eq!(ids(&r), names.to_vec());
            assert!((r[0].score - 1.0).abs() < 1e-12, "{}", r[0].score);
            assert!(r.iter().all(|x| x.sources == vec!["fts"]));
        }
    }

    #[test]
    fn hits_without_meta_are_dropped_and_do_not_take_rank() {
        // точка удалённого в SQLite узла осталась в Qdrant: метаданных нет
        let (a, b) = (Id::new(), Id::new());
        let inp = SearchInputs {
            fts: list(&["b"]),
            vector: Some(list(&["ghost", "a"])),
            chunks: meta(&[("a", a), ("b", b)]),
            ..Default::default()
        };
        let r = fuse(&inp, &Weights::default(), now());
        assert_eq!(r.len(), 2);
        assert!(!ids(&r).contains(&"ghost"));
        // «a» — первый живой в векторном канале: балл как у первого места в FTS
        assert_eq!(r[0].score, r[1].score);
        // пустые каналы — пустая выдача
        assert!(fuse(&SearchInputs::default(), &Weights::default(), now()).is_empty());
    }
}
