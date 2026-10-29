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
//!
//! **Канал папок** (§8.3, §9.2): точка папки, найденная векторным поиском, поднимает уже
//! найденные другими каналами чанки узлов своего поддерева — чанку прибавляется
//! `w_folder / (rrf_k + ранг папки)` один раз, по лучшей (с наименьшим рангом) папке-предку
//! того же пространства. В знаменатель нормировки канал не входит: прибавка — поправка поверх
//! слияния, как бусты; балл выше 1 после них приводит к шкале
//! [`rescale_overflow`](crate::rescale_overflow) (деление всех баллов на лучший, задача 18 M3).
//! При `w_folder = 0` (умолчание, сетка задачи 18 M3) выдача совпадает с M2.

use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use zam_core::{Id, TargetKind, TargetRef};

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
    /// FTS5 `bm25` по запросу, расширенному именами и псевдонимами терминов словаря (M4.5).
    Alias,
    /// Подъём потомков найденной папки (см. модуль).
    Folder,
}

impl Channel {
    /// Имя в `sources` ответа.
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Vector => "vector",
            Channel::Fts => "fts",
            Channel::Trigram => "trigram",
            Channel::Alias => "alias",
            Channel::Folder => "folder",
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
    /// Цель чанка (узел или запись журнала): по ней — лимит на цель и замена сводки листом.
    pub target: TargetRef,
    pub level: Level,
    /// Путь заголовков (`A > B`): секция сводки уровня `section` и листа.
    pub heading_path: String,
    /// Время последнего изменения узла — для затухания.
    pub updated_at: Timestamp,
    /// Вид инсайта (`data.kind` записи `insight`); у заметок и прочих записей — `None`.
    pub insight_kind: Option<String>,
    /// `certainty` записи (бусты надёжности).
    pub certainty: Option<String>,
    /// Область узла или записи (буст текущей области).
    pub area: Option<String>,
    /// Пространство цели (канал папок: папка поднимает только узлы своего пространства).
    pub space_id: Id,
    /// Путь узла; у записи журнала пуст (под папкой не лежит).
    pub path: String,
}

/// Точка папки векторного канала, прошедшая проверку по SQLite: жива, не Human, в области
/// цели. Ранг — среди живых папок, с 1, по порядку векторной выдачи.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderHit {
    pub folder: Id,
    /// Пространство папки: одинаковый путь в другом пространстве — не её поддерево.
    pub space_id: Id,
    /// Путь папки: её поддерево — узлы с этим путём или путём `<path_prefix>/…`.
    pub path_prefix: String,
    pub rank: usize,
}

impl FolderHit {
    /// Узел `path` пространства `space` лежит в поддереве папки (по целым сегментам).
    fn covers(&self, space: Id, path: &str) -> bool {
        space == self.space_id
            && !self.path_prefix.is_empty()
            && path
                .strip_prefix(self.path_prefix.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }
}

/// Входы слияния: каналы — `chunk_id` по убыванию релевантности канала.
#[derive(Debug, Clone, Default)]
pub struct SearchInputs {
    pub fts: Vec<String>,
    pub trigram: Vec<String>,
    /// Канал `alias` (словарь, M4.5); пустой — расширения не было или оно ничего не нашло.
    pub alias: Vec<String>,
    /// `None` — векторный канал выключен (деградация) или не запускался.
    pub vector: Option<Vec<String>>,
    /// Метаданные всех кандидатов. Кандидат без метаданных отбрасывается.
    pub chunks: HashMap<String, ChunkMeta>,
    /// `fts` получен ослабленным запросом (любое слово): строгий ничего не нашёл.
    pub fts_relaxed: bool,
    /// `alias` получен ослабленным запросом: вес — не выше `w_fts_relaxed` (при векторном канале).
    pub alias_relaxed: bool,
    /// Чанки, которых векторный канал не видит в принципе (Human, узел не проиндексирован):
    /// им ослабленный bm25 не понижается — соревноваться с векторными кандидатами им нечем.
    pub vector_blind: HashSet<String>,
    /// Канал папок (см. модуль); пустой — подъёма нет.
    pub folders: Vec<FolderHit>,
}

/// Параметры слияния (секция `[search]`). Умолчания библиотеки — веса `w_vector = w_fts = 1`,
/// `w_trigram = 0,7`, `w_fts_relaxed = w_fts` и `w_folder = 0` (на них держатся модульные
/// тесты слияния); рабочий `w_fts_relaxed` сервера подобран на eval.
#[derive(Debug, Clone, PartialEq)]
pub struct Weights {
    /// Константа RRF.
    pub rrf_k: f64,
    pub w_vector: f64,
    pub w_fts: f64,
    /// Вес `fts`, полученного ослабленным запросом (`SearchInputs::fts_relaxed`), — только
    /// при работающем векторном канале и только для чанков, которые он видит
    /// (не из `vector_blind`); иначе действует `w_fts`.
    pub w_fts_relaxed: f64,
    /// Вес канала `alias` (0 — канал выключен); ослабленный `alias` — не выше `w_fts_relaxed`.
    pub w_fts_alias: f64,
    pub w_trigram: f64,
    /// Вес канала папок (см. модуль); 0 — канал выключен (выдача M2).
    pub w_folder: f64,
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
            w_fts_relaxed: 1.0,
            w_fts_alias: 0.5,
            w_trigram: 0.7,
            w_folder: 0.0,
            recency_half_life_days: 180.0,
            max_per_node: 3,
        }
    }
}

/// Кандидат после слияния.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    pub chunk_id: String,
    pub target: TargetRef,
    pub level: Level,
    /// Путь заголовков чанка.
    pub heading_path: String,
    /// Нормированный балл, 0..=1.
    pub score: f64,
    /// Каналы, нашедшие чанк, в порядке `vector`, `fts`, `alias`, `trigram`, `folder`.
    pub sources: Vec<&'static str>,
}

/// Weighted RRF + затухание, без лимита на узел (см. модуль).
pub fn fuse(inp: &SearchInputs, w: &Weights, now: Timestamp) -> Vec<Ranked> {
    fuse_normed(inp, w, now).0
}

/// [`fuse`] и знаменатель нормировки балла (наибольший возможный сырой балл; 0 — нет
/// ни одного кандидата): по нему сервер ставит балл карточкам папок ([`folder_score`]).
pub fn fuse_normed(inp: &SearchInputs, w: &Weights, now: Timestamp) -> (Vec<Ranked>, f64) {
    let empty = Vec::new();
    let channels = [
        (
            Channel::Vector,
            w.w_vector,
            inp.vector.as_ref().unwrap_or(&empty),
        ),
        (Channel::Fts, w.w_fts, &inp.fts),
        (Channel::Alias, w.w_fts_alias, &inp.alias),
        (Channel::Trigram, w.w_trigram, &inp.trigram),
    ];
    // ослабленный bm25 понижается, только если есть с кем соревноваться: векторный канал
    // работает, а чанк ему виден (FTS-only и Human — с обычным весом)
    let relaxed = inp.fts_relaxed && inp.vector.is_some();
    let mut by_chunk: HashMap<&str, (f64, Vec<&'static str>)> = HashMap::new();
    let mut max = 0.0;
    for (ch, weight, list) in channels {
        // наибольший применённый вес канала — для нормировки балла в 0..=1
        let mut top: Option<f64> = None;
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
            let weight = if ch == Channel::Fts && relaxed && !inp.vector_blind.contains(id) {
                w.w_fts_relaxed
            } else if ch == Channel::Alias
                && inp.alias_relaxed
                && inp.vector.is_some()
                && !inp.vector_blind.contains(id)
            {
                weight.min(w.w_fts_relaxed)
            } else {
                weight
            };
            top = Some(top.map_or(weight, |t: f64| t.max(weight)));
            e.0 += weight / (w.rrf_k + rank as f64);
            e.1.push(ch.as_str());
        }
        if let Some(t) = top {
            max += t / (w.rrf_k + 1.0);
        }
    }
    // канал папок: прибавка по лучшей папке-предку, один раз на чанк
    if w.w_folder > 0.0 && !inp.folders.is_empty() {
        for (id, e) in by_chunk.iter_mut() {
            let m = &inp.chunks[*id];
            if m.target.kind != TargetKind::Node {
                continue;
            }
            let best = inp
                .folders
                .iter()
                .filter(|f| f.covers(m.space_id, &m.path))
                .map(|f| f.rank.max(1))
                .min();
            if let Some(rank) = best {
                e.0 += w.w_folder / (w.rrf_k + rank as f64);
                e.1.push(Channel::Folder.as_str());
            }
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
                target: m.target,
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
    (all, max)
}

/// Балл карточки папки ранга `rank` (с 1) в шкале выдачи: как у кандидата только
/// векторного канала того же места — `w_vector / (rrf_k + rank)`, делённый на знаменатель
/// нормировки слияния `max` ([`fuse_normed`]); без кандидатов — на первое место векторного
/// канала. Затухание к папке не применяется.
pub fn folder_score(rank: usize, w: &Weights, max: f64) -> f64 {
    let raw = w.w_vector / (w.rrf_k + rank.max(1) as f64);
    let max = if max > 0.0 {
        max
    } else {
        w.w_vector / (w.rrf_k + 1.0)
    };
    if max > 0.0 { raw / max } else { 0.0 }
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
                        target: TargetRef {
                            kind: TargetKind::Node,
                            id: *n,
                        },
                        level: Level::Chunk,
                        heading_path: String::new(),
                        updated_at: now(),
                        insight_kind: None,
                        certainty: None,
                        area: None,
                        space_id: Id::new(),
                        path: String::new(),
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
    fn relaxed_fts_weight_only_for_vector_visible_chunks() {
        let ids3 = (Id::new(), Id::new(), Id::new());
        let mk = |relaxed: bool, vector: bool, blind: &[&str]| {
            let inp = SearchInputs {
                // «h» — Human или не проиндексирован, «f» — обычный: оба только в ослабленном bm25
                fts: list(&["h", "f"]),
                vector: vector.then(|| list(&["v"])),
                chunks: meta(&[("h", ids3.0), ("f", ids3.1), ("v", ids3.2)]),
                fts_relaxed: relaxed,
                vector_blind: blind.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            };
            let w = Weights {
                w_fts_relaxed: 0.25,
                ..Weights::default()
            };
            fuse(&inp, &w, now())
        };
        // строгий запрос — обычный вес: «h» на равных с векторным первым местом
        assert_eq!(ids(&mk(false, true, &["h"]))[..2], ["h", "v"]);
        // ослабленный: видимый вектору «f» понижен ниже векторного кандидата, «h» — нет
        let r = mk(true, true, &["h"]);
        assert_eq!(ids(&r), vec!["h", "v", "f"]);
        assert!(r.iter().all(|x| x.score <= 1.0));
        // без векторного канала (FTS-only) вес не меняется: порядок и баллы как у строгого
        let (a, b) = (mk(true, false, &[]), mk(false, false, &[]));
        assert_eq!(ids(&a), ids(&b));
        assert_eq!(
            a.iter().map(|x| x.score).collect::<Vec<_>>(),
            b.iter().map(|x| x.score).collect::<Vec<_>>()
        );
    }

    #[test]
    fn alias_channel_weight_and_relaxed_cap() {
        let (a, b, c) = (Id::new(), Id::new(), Id::new());
        let mk = |alias: &[&str], w_alias: f64, relaxed: bool, vector: bool| {
            let inp = SearchInputs {
                fts: list(&["a"]),
                alias: list(alias),
                alias_relaxed: relaxed,
                vector: vector.then(|| list(&["c"])),
                chunks: meta(&[("a", a), ("b", b), ("c", c)]),
                ..Default::default()
            };
            let w = Weights {
                w_fts_alias: w_alias,
                w_fts_relaxed: 0.25,
                ..Weights::default()
            };
            fuse(&inp, &w, now())
        };
        // «a» найден двумя каналами и первый; «b» — только alias (в sources виден канал)
        let r = mk(&["b", "a"], 0.5, false, false);
        assert_eq!(ids(&r), vec!["a", "b"]);
        assert_eq!(r[0].sources, vec!["fts", "alias"]);
        assert_eq!(r[1].sources, vec!["alias"]);
        // вес alias против базового канала: ниже — «a» первым, выше — «b»
        assert_eq!(ids(&mk(&["b"], 0.5, false, false)), vec!["a", "b"]);
        assert_eq!(ids(&mk(&["b"], 5.0, false, false)), vec!["b", "a"]);
        // ослабленный alias с векторным каналом не выше `w_fts_relaxed`; без него — как есть
        let strong = mk(&["b"], 0.5, false, true);
        let capped = mk(&["b"], 0.5, true, true);
        let score = |r: &[Ranked], id: &str| r.iter().find(|x| x.chunk_id == id).unwrap().score;
        assert!(score(&capped, "b") < score(&strong, "b"));
        let (x, y) = (mk(&["b"], 0.5, true, false), mk(&["b"], 0.5, false, false));
        assert_eq!(score(&x, "b"), score(&y, "b"));
        assert!(strong.iter().chain(&capped).all(|x| x.score <= 1.0));
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

    /// Чанк узла `node` с путём `path` в пространстве `space`.
    fn at(node: Id, space: Id, path: &str) -> ChunkMeta {
        ChunkMeta {
            target: TargetRef {
                kind: TargetKind::Node,
                id: node,
            },
            level: Level::Chunk,
            heading_path: String::new(),
            updated_at: now(),
            insight_kind: None,
            certainty: None,
            area: None,
            space_id: space,
            path: path.into(),
        }
    }

    fn folder(space: Id, path: &str, rank: usize) -> FolderHit {
        FolderHit {
            folder: Id::new(),
            space_id: space,
            path_prefix: path.into(),
            rank,
        }
    }

    /// Каналы: «c» и «a1» в обоих, «a2», «d», «x» и запись «e» — только в FTS.
    fn folder_inputs(s: Id, other: Id) -> SearchInputs {
        let entry = ChunkMeta {
            target: TargetRef {
                kind: TargetKind::Entry,
                id: Id::new(),
            },
            path: String::new(),
            ..at(Id::new(), s, "")
        };
        let chunks: HashMap<String, ChunkMeta> = [
            ("c", at(Id::new(), s, "other/c")),
            ("a1", at(Id::new(), s, "ops/a")),
            ("a2", at(Id::new(), s, "ops/sub/b")),
            // сегмент «opsx» — не потомок «ops»
            ("d", at(Id::new(), s, "opsx/d")),
            // тот же путь в другом пространстве
            ("x", at(Id::new(), other, "ops/x")),
            ("e", entry),
        ]
        .into_iter()
        .map(|(k, m)| (k.to_string(), m))
        .collect();
        SearchInputs {
            fts: list(&["c", "a1", "d", "x", "e", "a2"]),
            vector: Some(list(&["c", "a1"])),
            chunks,
            ..Default::default()
        }
    }

    #[test]
    fn folder_channel_boosts_descendants_once() {
        let (s, other) = (Id::new(), Id::new());
        let base = folder_inputs(s, other);
        let w = Weights {
            w_folder: 1.0,
            ..Weights::default()
        };
        // «ops» — второе место канала папок, «ops/sub» — первое: «a2» лежит под обеими
        let mut inp = base.clone();
        inp.folders = vec![folder(s, "ops/sub", 1), folder(s, "ops", 2)];
        let r = fuse(&inp, &w, now());
        let plain = fuse(&base, &w, now());
        let score = |r: &[Ranked], id: &str| r.iter().find(|x| x.chunk_id == id).unwrap().score;
        let sources =
            |r: &[Ranked], id: &str| r.iter().find(|x| x.chunk_id == id).unwrap().sources.clone();
        // нормировка без канала папок — прибавка в долях того же максимума
        let max = 1.0 / 61.0 + 1.0 / 61.0;
        let lift = |id: &str| (score(&r, id) - score(&plain, id)) * max;
        // лучшая папка-предок «a2» — «ops/sub» (ранг 1), прибавка одна
        assert!((lift("a2") - 1.0 / 61.0).abs() < 1e-12, "{}", lift("a2"));
        assert!((lift("a1") - 1.0 / 62.0).abs() < 1e-12, "{}", lift("a1"));
        assert_eq!(sources(&r, "a2"), vec!["fts", "folder"]);
        assert_eq!(sources(&r, "a1"), vec!["vector", "fts", "folder"]);
        // не потомки: другой сегмент, другое пространство, запись журнала, чужой путь
        for id in ["d", "x", "e", "c"] {
            assert_eq!(score(&r, id), score(&plain, id), "{id}");
            assert!(!sources(&r, id).contains(&"folder"), "{id}");
        }
        // «a2» из хвоста FTS поднимается над «d», «x» и «e»
        let order = ids(&r);
        let pos = |id: &str| order.iter().position(|x| *x == id).unwrap();
        assert!(pos("a2") < pos("d") && pos("a2") < pos("x"), "{order:?}");
        // папка совпадает с путём узла целиком — узел тоже её потомок
        let mut inp = base.clone();
        inp.folders = vec![folder(s, "ops/sub/b", 1)];
        let r2 = fuse(&inp, &w, now());
        assert!(sources(&r2, "a2").contains(&"folder"));
    }

    #[test]
    fn no_folder_hits_equals_m2_order() {
        let (s, other) = (Id::new(), Id::new());
        let base = folder_inputs(s, other);
        // умолчание библиотеки — канал выключен (сетка задачи 18)
        assert_eq!(Weights::default().w_folder, 0.0);
        let m2 = fuse(&base, &Weights::default(), now());
        // нет хитов папок при любом весе — выдача M2
        let w = Weights {
            w_folder: 1.0,
            ..Weights::default()
        };
        assert_eq!(fuse(&base, &w, now()), m2);
        // хиты есть, но вес 0 — тоже M2 (ни балла, ни источника `folder`)
        let mut inp = base.clone();
        inp.folders = vec![folder(s, "ops", 1)];
        assert_eq!(fuse(&inp, &Weights::default(), now()), m2);
    }
}
