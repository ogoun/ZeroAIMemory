//! Слияние каналов FTS нескольких баз (§9.2 п. 2b): FTS идёт по каждой базе отдельно в
//! пределах пространств её цели, затем каждый канал (`bm25`, `trigram`) сливается по
//! сырому баллу FTS5 (`FtsHit::raw_score`, меньше — лучше) и обрезается до своего предела
//! (`bm25_top`, `trigram_top` — как у канала одной базы); балл
//! хита — `1/место` в слитом канале.
//!
//! - Порядок при равном `raw_score` — порядок целей, внутри цели — порядок канала (сортировка
//!   стабильна), поэтому для одной базы выдача совпадает с её каналами (поведение M2).
//! - Строгий `bm25` (все слова запроса) хотя бы в одной базе — ослабленные выдачи (любое
//!   слово) других баз отбрасываются: в одной общей базе ослабленный запрос не выполнился бы
//!   вовсе, а баллы запросов `AND` и `OR` несравнимы. `relaxed` — все непустые выдачи `bm25`
//!   ослабленные.
//! - Канал `alias` (словарь, M4.5) сливается так же, как `bm25`, со своим признаком `alias_relaxed`
//!   и пределом `bm25_top`.
//! - Канал `trigram` берётся из всех баз (в том числе фраза при пустом `bm25` своей базы).
//!
//! Баллы `bm25` разных баз считаются по статистике своей базы (IDF): сравнение приближённое,
//! но это лучший доступный без общего индекса порядок, а дальше его выравнивает weighted RRF
//! с векторным каналом.
use zam_store::{FtsChannels, FtsHit};

/// Слитые каналы: `(индекс цели, хит)` по убыванию релевантности.
#[derive(Debug, Default)]
pub struct MergedChannels {
    pub bm25: Vec<(usize, FtsHit)>,
    pub trigram: Vec<(usize, FtsHit)>,
    /// Канал `alias` (словарь): пуст без расширений запроса.
    pub alias: Vec<(usize, FtsHit)>,
    /// `bm25` получен ослабленным запросом (см. модуль).
    pub relaxed: bool,
    /// `alias` получен ослабленным запросом.
    pub alias_relaxed: bool,
}

/// Слияние каналов баз `per_db` (`(индекс цели, каналы её базы)`, в порядке целей).
pub fn merge_channels(
    per_db: Vec<(usize, FtsChannels)>,
    bm25_top: usize,
    trigram_top: usize,
) -> MergedChannels {
    let strict = per_db.iter().any(|(_, c)| !c.relaxed && !c.bm25.is_empty());
    let strict_alias = per_db
        .iter()
        .any(|(_, c)| !c.alias_relaxed && !c.alias.is_empty());
    let mut bm25 = Vec::new();
    let mut trigram = Vec::new();
    let mut alias = Vec::new();
    for (t, ch) in per_db {
        if !(strict && ch.relaxed) {
            bm25.extend(ch.bm25.into_iter().map(|h| (t, h)));
        }
        if !(strict_alias && ch.alias_relaxed) {
            alias.extend(ch.alias.into_iter().map(|h| (t, h)));
        }
        trigram.extend(ch.trigram.into_iter().map(|h| (t, h)));
    }
    MergedChannels {
        relaxed: !strict && !bm25.is_empty(),
        alias_relaxed: !strict_alias && !alias.is_empty(),
        bm25: rank(bm25, bm25_top),
        trigram: rank(trigram, trigram_top),
        alias: rank(alias, bm25_top),
    }
}

/// Порядок по `raw_score` (стабильно), `top_k`, балл `1/место`.
fn rank(mut v: Vec<(usize, FtsHit)>, top_k: usize) -> Vec<(usize, FtsHit)> {
    v.sort_by(|a, b| a.1.raw_score.total_cmp(&b.1.raw_score));
    v.truncate(top_k);
    for (i, (_, h)) in v.iter_mut().enumerate() {
        h.score = 1.0 / (i + 1) as f64;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use zam_core::{Id, TargetKind, TargetRef};

    fn hit(chunk: &str, raw: f64) -> FtsHit {
        FtsHit {
            chunk_id: chunk.into(),
            target: TargetRef {
                kind: TargetKind::Node,
                id: Id::new(),
            },
            space_id: Id::new(),
            path: "p".into(),
            heading_path: String::new(),
            level: "chunk".into(),
            rev: 1,
            token_count: 1,
            score: 0.0,
            raw_score: raw,
            updated_at: jiff::Timestamp::now(),
            snippet: String::new(),
            text: String::new(),
            index_state: "indexed".into(),
            entry: None,
            area: None,
            certainty: None,
            verified_at: None,
        }
    }

    fn ch(bm25: &[(&str, f64)], trigram: &[(&str, f64)], relaxed: bool) -> FtsChannels {
        let mk = |l: &[(&str, f64)]| l.iter().map(|(c, r)| hit(c, *r)).collect();
        FtsChannels {
            bm25: mk(bm25),
            trigram: mk(trigram),
            relaxed,
            ..Default::default()
        }
    }

    fn ids(v: &[(usize, FtsHit)]) -> Vec<(usize, &str)> {
        v.iter().map(|(t, h)| (*t, h.chunk_id.as_str())).collect()
    }

    #[test]
    fn merge_orders_by_raw_score_across_dbs() {
        let m = merge_channels(
            vec![
                (0, ch(&[("a1", -5.0), ("a2", -1.0)], &[("ta", -2.0)], false)),
                (1, ch(&[("b1", -3.0), ("b2", -0.5)], &[("tb", -4.0)], false)),
            ],
            50,
            20,
        );
        assert_eq!(
            ids(&m.bm25),
            vec![(0, "a1"), (1, "b1"), (0, "a2"), (1, "b2")]
        );
        assert_eq!(ids(&m.trigram), vec![(1, "tb"), (0, "ta")]);
        // балл — 1/место в слитом канале
        let s: Vec<f64> = m.bm25.iter().map(|(_, h)| h.score).collect();
        assert_eq!(s, vec![1.0, 0.5, 1.0 / 3.0, 0.25]);
        assert!(!m.relaxed);
        // равные баллы: порядок целей, внутри цели — порядок канала
        let m = merge_channels(
            vec![
                (0, ch(&[("x", -1.0), ("y", -1.0)], &[], false)),
                (1, ch(&[("z", -1.0)], &[], false)),
            ],
            50,
            20,
        );
        assert_eq!(ids(&m.bm25), vec![(0, "x"), (0, "y"), (1, "z")]);
    }

    #[test]
    fn merge_keeps_top_k_per_channel() {
        let many = |p: &str, n: usize| -> Vec<(String, f64)> {
            (0..n).map(|i| (format!("{p}{i}"), -(i as f64))).collect()
        };
        let a = many("a", 30);
        let b = many("b", 30);
        fn r(v: &[(String, f64)]) -> Vec<(&str, f64)> {
            v.iter().map(|(c, s)| (c.as_str(), *s)).collect()
        }
        let m = merge_channels(
            vec![
                (0, ch(&r(&a), &r(&a), false)),
                (1, ch(&r(&b), &r(&b), false)),
            ],
            50,
            20,
        );
        assert_eq!(m.bm25.len(), 50);
        // trigram — свой предел, как у канала одной базы
        assert_eq!(m.trigram.len(), 20);
        // лучшие (самые отрицательные) попали, худшие отброшены
        assert_eq!(m.bm25[0].1.raw_score, -29.0);
        assert!(m.bm25.iter().all(|(_, h)| h.raw_score <= -5.0));
        let m = merge_channels(vec![(0, ch(&r(&a), &[], false))], 3, 3);
        assert_eq!(ids(&m.bm25), vec![(0, "a29"), (0, "a28"), (0, "a27")]);
    }

    #[test]
    fn alias_channel_merges_like_bm25() {
        let mut a = ch(&[], &[], false);
        a.alias = vec![hit("a1", -3.0), hit("a2", -1.0)];
        a.alias_relaxed = true;
        let mut b = ch(&[], &[], false);
        b.alias = vec![hit("b1", -2.0)];
        // строгий alias одной базы вытесняет ослабленный другой
        let m = merge_channels(vec![(0, a), (1, b)], 50, 20);
        assert_eq!(ids(&m.alias), vec![(1, "b1")]);
        assert!(!m.alias_relaxed);
        let mut only = ch(&[], &[], false);
        only.alias = vec![hit("x", -1.0)];
        only.alias_relaxed = true;
        let m = merge_channels(vec![(0, only)], 50, 20);
        assert_eq!(ids(&m.alias), vec![(0, "x")]);
        assert!(m.alias_relaxed);
        assert!(m.bm25.is_empty() && !m.relaxed);
    }

    #[test]
    fn one_db_keeps_channel_order() {
        // одна база: порядок и баллы как у её каналов (M2)
        let m = merge_channels(
            vec![(3, ch(&[("a", -9.0), ("b", -4.0)], &[("t", -1.0)], true))],
            50,
            20,
        );
        assert_eq!(ids(&m.bm25), vec![(3, "a"), (3, "b")]);
        assert_eq!(ids(&m.trigram), vec![(3, "t")]);
        assert!(m.relaxed);
        assert!(merge_channels(Vec::new(), 50, 20).bm25.is_empty());
    }

    #[test]
    fn strict_bm25_drops_relaxed_of_other_dbs() {
        let m = merge_channels(
            vec![
                (0, ch(&[("relaxed", -20.0)], &[("t0", -1.0)], true)),
                (1, ch(&[("strict", -1.0)], &[], false)),
            ],
            50,
            20,
        );
        assert_eq!(ids(&m.bm25), vec![(1, "strict")]);
        assert_eq!(ids(&m.trigram), vec![(0, "t0")]);
        assert!(!m.relaxed);
        // только ослабленные — сливаются, признак сохраняется; пустой bm25 не строгий
        let m = merge_channels(
            vec![
                (0, ch(&[("r0", -2.0)], &[], true)),
                (1, ch(&[], &[], false)),
                (2, ch(&[("r2", -3.0)], &[], true)),
            ],
            50,
            20,
        );
        assert_eq!(ids(&m.bm25), vec![(2, "r2"), (0, "r0")]);
        assert!(m.relaxed);
    }
}
