//! Ð Ð°Ð½Ð¶Ð¸Ñ€Ð¾Ð²Ð°Ð½Ð¸Ðµ ÐºÐ°Ñ€Ñ‚Ñ‹ ÐºÐ¾Ð´Ð° (ÑÐ¿ÐµÑ†Ð¸Ñ„Ð¸ÐºÐ°Ñ†Ð¸Ñ M5.6 Â§7): Ð³Ñ€Ð°Ñ„ Ñ„Ð°Ð¹Ð»Ð¾Ð², PageRank, Ð´Ð¾Ð»Ð¸ ÑÐ¸Ð¼Ð²Ð¾Ð»Ð¾Ð².

use std::collections::HashMap;

use proptest::prelude::*;
use zam_code::{
    Focus, Graph, RankFile, RankWeights, RefKind, SymbolKind, central, edges_among, focus_idents,
    focus_restart, ident_mult, pagerank, rank, rank_on_graph, rank_with,
};

fn file(id: i64, path: &str, defs: &[(&str, SymbolKind)], refs: &[(&str, u32)]) -> RankFile {
    RankFile {
        id,
        path: path.to_string(),
        defs: defs
            .iter()
            .map(|(n, k)| (n.to_string(), *k, None))
            .collect(),
        refs: refs
            .iter()
            .map(|(n, c)| (n.to_string(), RefKind::Call, *c, 0))
            .collect(),
    }
}

/// Ð’ÐºÐ»Ð°Ð´ ÑÑÑ‹Ð»ÐºÐ¸ Ð¿Ð¾ Ñ‚Ð°Ð±Ð»Ð¸Ñ†Ðµ M6 Â§12Ð°, Ð·Ð°Ð¿Ð¸ÑÐ°Ð½Ð½Ð¾Ð¹ Ð·Ð´ÐµÑÑŒ Ð½ÐµÐ·Ð°Ð²Ð¸ÑÐ¸Ð¼Ð¾ Ð¾Ñ‚ `rank.rs`: Ñ‚Ð¸Ð¿ Ð¸ Ñ€ÐµÐ°Ð»Ð¸Ð·Ð°Ñ†Ð¸Ñ
/// 2,0, Ð¿Ñ€Ð¾Ñ‡Ð¸Ðµ 1,0; Ñƒ Ð²Ñ‹Ð·Ð¾Ð²Ð° `recv` (Ð½Ðµ Ð±Ð¾Ð»ÑŒÑˆÐµ `count`) â€” 0,25.
fn table_contribution(kind: RefKind, count: u32, recv: u32) -> f64 {
    let k = match kind {
        RefKind::Type | RefKind::Impl => 2.0,
        RefKind::Call | RefKind::Import | RefKind::Other => 1.0,
    };
    if kind == RefKind::Call {
        let r = recv.min(count);
        f64::from(count - r) + 0.25 * f64::from(r)
    } else {
        k * f64::from(count)
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn edge(edges: &[(usize, usize, f64)], s: usize, d: usize) -> Option<f64> {
    edges.iter().find(|e| e.0 == s && e.1 == d).map(|e| e.2)
}

/// Ð­Ñ‚Ð°Ð»Ð¾Ð½Ð½Ñ‹Ð¹ ÑÑ‚ÐµÐ¿ÐµÐ½Ð½Ð¾Ð¹ Ð¼ÐµÑ‚Ð¾Ð´ Ð±ÐµÐ· Ð¿Ñ€ÐµÐ´ÐµÐ»Ð° Ð¸Ñ‚ÐµÑ€Ð°Ñ†Ð¸Ð¹ (Ð´Ð»Ñ Ð¿Ñ€Ð¾Ð²ÐµÑ€ÐºÐ¸ ÑÑ…Ð¾Ð´Ð¸Ð¼Ð¾ÑÑ‚Ð¸ Ð·Ð° 50).
fn reference_pagerank(n: usize, edges: &[(usize, usize, f64)]) -> Vec<f64> {
    let mut out = vec![0.0; n];
    for &(s, _, w) in edges {
        out[s] += w;
    }
    let mut r = vec![1.0 / n as f64; n];
    for _ in 0..2000 {
        let dangling: f64 = (0..n).filter(|&i| out[i] == 0.0).map(|i| r[i]).sum();
        let mut next = vec![(0.15 + 0.85 * dangling) / n as f64; n];
        for &(s, d, w) in edges {
            next[d] += 0.85 * r[s] * w / out[s];
        }
        r = next;
    }
    r
}

#[test]
fn ident_mult_table() {
    let table = [
        ("getUserById", 1, 10.0),
        ("getUserById", 2, 10.0),
        ("open_store_file", 1, 10.0),
        ("_private_name", 1, 1.0),
        ("_priv", 1, 0.1),
        ("x", 1, 1.0),
        ("Store", 1, 1.0),
        ("getuserbyid", 1, 1.0),
        ("x", 6, 0.1),
        ("x", 5, 1.0),
        ("getUserById", 6, 1.0),
    ];
    for (name, d, want) in table {
        let got = ident_mult(name, d);
        assert!((got - want).abs() < 1e-12, "{name} d={d}: {got} != {want}");
    }
}

#[test]
fn edges_skip_self_definitions() {
    let files = [
        file(
            1,
            "a.rs",
            &[("local_helper", SymbolKind::Function)],
            &[("local_helper", 4)],
        ),
        file(2, "b.rs", &[], &[("local_helper", 1)]),
    ];
    let e = edges_among(&files, &[0, 1]);
    assert_eq!(
        edge(&e, 0, 0),
        None,
        "ÑÑÑ‹Ð»ÐºÐ° Ð½Ð° ÑÐ²Ð¾Ñ‘ Ð¾Ð¿Ñ€ÐµÐ´ÐµÐ»ÐµÐ½Ð¸Ðµ â€” Ð½Ðµ Ñ€ÐµÐ±Ñ€Ð¾"
    );
    assert!(close(edge(&e, 1, 0).unwrap(), 10.0));
    assert_eq!(e.len(), 1);
}

#[test]
fn edges_split_by_definers_count() {
    // `open_store` Ð¾Ð¿Ñ€ÐµÐ´ÐµÐ»Ñ‘Ð½ Ð² b Ð¸ c: Ð²ÐµÑ A â†’ B Ð¸ A â†’ C â€” âˆš4 Ã— 10 / 2 = 10; ÑÑÑ‹Ð»ÐºÐ¸ ÑÑƒÐ¼Ð¼Ð¸Ñ€ÑƒÑŽÑ‚ÑÑ
    let files = [
        file(1, "a.rs", &[], &[("open_store", 4), ("Db", 1)]),
        file(
            2,
            "b.rs",
            &[
                ("open_store", SymbolKind::Function),
                ("Db", SymbolKind::Struct),
            ],
            &[],
        ),
        file(3, "c.rs", &[("open_store", SymbolKind::Function)], &[]),
        file(4, "d.rs", &[], &[("unknown_name", 9)]),
    ];
    let e = edges_among(&files, &[0, 1, 2, 3]);
    assert!(close(edge(&e, 0, 1).unwrap(), 10.0 + 1.0));
    assert!(close(edge(&e, 0, 2).unwrap(), 10.0));
    assert_eq!(
        e.len(),
        2,
        "Ð½ÐµÐ¾Ð¿Ñ€ÐµÐ´ÐµÐ»Ñ‘Ð½Ð½Ð¾Ðµ Ð¸Ð¼Ñ Ñ€Ñ‘Ð±ÐµÑ€ Ð½Ðµ Ð´Ð°Ñ‘Ñ‚: {e:?}"
    );
}

#[test]
fn pagerank_sums_to_one_converges_50_iters() {
    // ÐºÐ¾Ð»ÑŒÑ†Ð¾ Ñ Ñ…Ð¾Ñ€Ð´Ð°Ð¼Ð¸ Ð¸ Ð²Ð¸ÑÑÑ‡Ð¸Ð¼ ÑƒÐ·Ð»Ð¾Ð¼
    let n = 12;
    let mut edges = Vec::new();
    for i in 0..n - 1 {
        edges.push((i, (i + 1) % (n - 1), 1.0 + i as f64));
        edges.push((i, (i * 7 + 3) % n, 0.5));
    }
    let r = pagerank(n, &edges, None);
    assert_eq!(r.len(), n);
    assert!((r.iter().sum::<f64>() - 1.0).abs() < 1e-9);
    let reference = reference_pagerank(n, &edges);
    let l1: f64 = r.iter().zip(&reference).map(|(a, b)| (a - b).abs()).sum();
    assert!(l1 < 1e-4, "L1 Ð´Ð¾ ÑÑ‚Ð°Ð»Ð¾Ð½Ð° {l1}");
    assert!(pagerank(0, &[], None).is_empty());
}

#[test]
fn pagerank_star_center_highest() {
    let n = 9;
    let edges: Vec<_> = (1..n).map(|i| (i, 0, 1.0)).collect();
    let r = pagerank(n, &edges, None);
    for (i, v) in r.iter().enumerate().skip(1) {
        assert!(r[0] > *v * 2.0, "Ñ†ÐµÐ½Ñ‚Ñ€ {} Ð»Ð¸ÑÑ‚ {i} {v}", r[0]);
    }
}

#[test]
fn pagerank_dangling_nodes_no_nan() {
    // Ñ‚Ð¾Ð»ÑŒÐºÐ¾ Ð²Ð¸ÑÑÑ‡Ð¸Ðµ ÑƒÐ·Ð»Ñ‹, Ð¼ÑƒÑÐ¾Ñ€Ð½Ñ‹Ðµ Ñ€Ñ‘Ð±Ñ€Ð° Ð¸ Ð½ÑƒÐ»ÐµÐ²Ð¾Ð¹ Ð²ÐµÐºÑ‚Ð¾Ñ€ Ð¿ÐµÑ€ÐµÐ·Ð°Ð¿ÑƒÑÐºÐ°
    let edges = [
        (0, 1, 1.0),
        (2, 9, 1.0),
        (1, 1, f64::NAN),
        (3, 0, -2.0),
        (3, 0, 0.0),
    ];
    for restart in [
        None,
        Some(vec![0.0; 5]),
        Some(vec![0.0, 0.0, 1.0, 0.0, 0.0]),
    ] {
        let r = pagerank(5, &edges, restart.as_deref());
        assert!(r.iter().all(|v| v.is_finite() && *v >= 0.0), "{r:?}");
        assert!((r.iter().sum::<f64>() - 1.0).abs() < 1e-9);
    }
    let r = pagerank(5, &edges, Some(&[0.0, 0.0, 1.0, 0.0, 0.0]));
    assert!(r[2] > r[3], "Ð¿ÐµÑ€ÐµÐ·Ð°Ð¿ÑƒÑÐº Ð² ÑƒÐ·ÐµÐ» 2: {r:?}");
}

/// Ð“Ñ€Ð°Ñ„ Ð¸Ð· 30 Ñ„Ð°Ð¹Ð»Ð¾Ð²: Ñ…Ð°Ð±, ÑÑ€ÐµÐ´Ð½Ð¸Ðµ ÑƒÐ·Ð»Ñ‹, Ñ„Ð¾ÐºÑƒÑÐ½Ñ‹Ð¹ Ñ„Ð°Ð¹Ð» Ð¸ Ñ‚Ñ€Ð¸ ÐµÐ³Ð¾ Ñ†ÐµÐ»Ð¸.
fn graph30() -> Vec<RankFile> {
    let mut files = vec![file(
        0,
        "src/hub.rs",
        &[("shared_service_registry", SymbolKind::Struct)],
        &[],
    )];
    for m in 1..=5 {
        let name = format!("middle_layer_{m}");
        files.push(RankFile {
            id: m,
            path: format!("src/mid{m}.rs"),
            defs: vec![(name, SymbolKind::Function, Some(true))],
            refs: vec![("shared_service_registry".into(), RefKind::Type, 2, 0)],
        });
    }
    for i in 6..30 {
        let mut refs = vec![("shared_service_registry".to_string(), RefKind::Type, 3, 0)];
        refs.push((format!("middle_layer_{}", i % 5 + 1), RefKind::Call, 2, 0));
        let mut defs = vec![];
        if (25..=27).contains(&i) {
            defs.push((
                format!("focus_target_{i}"),
                SymbolKind::Function,
                Some(true),
            ));
            refs.clear();
        }
        if i == 20 {
            // Ñ„Ð¾ÐºÑƒÑÐ½Ñ‹Ð¹ Ñ„Ð°Ð¹Ð» ÑÑÑ‹Ð»Ð°ÐµÑ‚ÑÑ Ð½Ð° Ñ…Ð°Ð± Ð¸ Ñ‚Ñ€Ð¸ Ñ†ÐµÐ»Ð¸: Ð²Ð¼ÐµÑÑ‚Ðµ Ñ Ð½Ð¸Ð¼ â€” Ñ€Ð¾Ð²Ð½Ð¾ Ð¿ÑÑ‚Ñ‘Ñ€ÐºÐ°
            refs.truncate(1);
            for t in 25..=27 {
                refs.push((format!("focus_target_{t}"), RefKind::Call, 1, 0));
            }
        }
        files.push(RankFile {
            id: i as i64,
            path: format!("src/f{i}.rs"),
            defs,
            refs,
        });
    }
    files
}

fn top(rank: &[f64], k: usize) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..rank.len()).collect();
    idx.sort_by(|a, b| rank[*b].total_cmp(&rank[*a]).then(a.cmp(b)));
    idx.truncate(k);
    idx
}

#[test]
fn focus_paths_and_their_targets_in_top5() {
    let files = graph30();
    let global = rank(&files, None);
    let g5 = top(&global.file_rank, 5);
    assert!(
        !g5.contains(&20) && !g5.contains(&25),
        "Ð±ÐµÐ· Ñ„Ð¾ÐºÑƒÑÐ°: {g5:?}"
    );
    let focus = Focus {
        paths: vec!["src/f20.rs".into()],
        idents: vec![],
        text: None,
    };
    let r = rank(&files, Some(&focus));
    let t5 = top(&r.file_rank, 5);
    for want in [20, 25, 26, 27] {
        assert!(t5.contains(&want), "{want} Ð½Ðµ Ð² Ð¿ÑÑ‚Ñ‘Ñ€ÐºÐµ {t5:?}");
    }
    assert!((r.file_rank.iter().sum::<f64>() - 1.0).abs() < 1e-9);
    // Ñ‚Ð¾Ñ‚ Ð¶Ðµ Ñ€ÐµÐ·ÑƒÐ»ÑŒÑ‚Ð°Ñ‚ Ð¿Ð¾ Ð³Ñ€Ð°Ñ„Ñƒ (Ð¿ÐµÑ€ÑÐ¾Ð½Ð°Ð»Ð¸Ð·Ð°Ñ†Ð¸Ñ Ð½Ð° ÑÐµÑ€Ð²ÐµÑ€Ðµ)
    let restart = focus_restart(&files, &focus).unwrap();
    assert_eq!(
        rank_on_graph(&Graph::new(&files), Some(&restart)),
        r.file_rank
    );
}

#[test]
fn focus_idents_and_text_give_restart_mass() {
    let files = graph30();
    let by_ident = Focus {
        paths: vec![],
        idents: vec!["focus_target_26".into()],
        text: None,
    };
    let m = focus_restart(&files, &by_ident).unwrap();
    assert!(m[26] > 0.0 && m.iter().enumerate().all(|(i, v)| i == 26 || *v == 0.0));
    let by_text = Focus {
        paths: vec![],
        idents: vec![],
        text: Some("Ð¿Ð¾Ñ‡ÐµÐ¼Ñƒ Ð¿Ð°Ð´Ð°ÐµÑ‚ focus_target_25 Ð¿Ñ€Ð¸ ÑÑ‚Ð°Ñ€Ñ‚Ðµ?".into()),
    };
    let m = focus_restart(&files, &by_text).unwrap();
    assert!(m[25] > 0.0 && m[26] == 0.0);
}

#[test]
fn focus_text_idents_min_len_3() {
    assert_eq!(
        focus_idents("fix Store::open in db_x, ab and a1b2 x 42 42abc Store _ab"),
        ["fix", "Store", "open", "db_x", "and", "a1b2", "_ab"]
    );
    assert!(focus_idents("").is_empty());
}

#[test]
fn focus_without_matches_is_global() {
    let files = graph30();
    let focus = Focus {
        paths: vec!["nope.rs".into()],
        idents: vec!["nothing_here".into()],
        text: Some("zzz qqq".into()),
    };
    assert_eq!(focus_restart(&files, &focus), None);
    assert_eq!(
        rank(&files, Some(&focus)).file_rank,
        rank(&files, None).file_rank
    );
}

#[test]
fn symbol_share_by_incoming_name() {
    let mut b = file(
        2,
        "b.rs",
        &[
            ("alpha_handler", SymbolKind::Function),
            ("unused_fn", SymbolKind::Function),
            ("Config", SymbolKind::Struct),
            ("exported_fn", SymbolKind::Function),
        ],
        &[],
    );
    b.defs[3].2 = Some(true);
    let files = [
        file(1, "a.rs", &[], &[("alpha_handler", 4)]),
        b,
        file(3, "c.rs", &[], &[]),
    ];
    let r = rank(&files, None);
    assert_eq!(r.symbol_share.len(), 3);
    assert!(r.symbol_share[0].is_empty() && r.symbol_share[2].is_empty());
    let s = &r.symbol_share[1];
    assert!((s.iter().sum::<f64>() - 1.0).abs() < 1e-9, "{s:?}");
    assert!(
        s[0] > s[1] + s[2] + s[3],
        "Ð²Ñ…Ð¾Ð´ÑÑ‰Ð¸Ð¹ Ð²ÐµÑ Ð¿Ð¾ Ð¸Ð¼ÐµÐ½Ð¸: {s:?}"
    );
    assert!(close(s[2], 2.0 * s[1]), "Ñ‚Ð¸Ð¿ Ã—2: {s:?}");
    assert!(
        close(s[3], 2.0 * s[1]),
        "ÑÐºÑÐ¿Ð¾Ñ€Ñ‚Ð¸Ñ€ÑƒÐµÐ¼Ñ‹Ð¹ Ã—2: {s:?}"
    );
    // Ñ„Ð°Ð¹Ð» Ð±ÐµÐ· Ð²Ñ…Ð¾Ð´ÑÑ‰Ð¸Ñ… ÑÑÑ‹Ð»Ð¾Ðº â€” Ð´Ð¾Ð»Ð¸ Ñ‚Ð¾Ð»ÑŒÐºÐ¾ Ð¿Ð¾ Ð²Ð¸Ð´Ñƒ
    let solo = [file(
        1,
        "a.rs",
        &[("f", SymbolKind::Function), ("T", SymbolKind::Trait)],
        &[],
    )];
    let s = &rank(&solo, None).symbol_share[0];
    assert!(close(s[0], 1.0 / 3.0) && close(s[1], 2.0 / 3.0), "{s:?}");
}

#[test]
fn central_max_three_per_file() {
    let defs: Vec<(String, SymbolKind)> = (0..6)
        .map(|i| (format!("core_function_{i}"), SymbolKind::Function))
        .collect();
    let defs_ref: Vec<(&str, SymbolKind)> = defs.iter().map(|(n, k)| (n.as_str(), *k)).collect();
    let refs: Vec<(&str, u32)> = defs.iter().map(|(n, _)| (n.as_str(), 5)).collect();
    let files = [
        file(1, "core.rs", &defs_ref, &[]),
        file(
            2,
            "user.rs",
            &[("helper_fn_x", SymbolKind::Function)],
            &refs,
        ),
        file(3, "other.rs", &[], &[("helper_fn_x", 1)]),
    ];
    let r = rank(&files, None);
    let c = central(&files, &r, 10);
    assert_eq!(c.iter().filter(|(f, _)| *f == 0).count(), 3, "{c:?}");
    assert_eq!(c.len(), 4, "{c:?}");
    let score = |(f, s): (usize, usize)| r.file_rank[f] * r.symbol_share[f][s];
    for w in c.windows(2) {
        assert!(
            score(w[0]) >= score(w[1]),
            "Ð¿Ð¾ ÑƒÐ±Ñ‹Ð²Ð°Ð½Ð¸ÑŽ Ñ€Ð°Ð½Ð³Ð°: {c:?}"
        );
    }
    assert_eq!(central(&files, &r, 2).len(), 2);
    assert!(central(&files, &r, 0).is_empty());
}

#[test]
fn edges_sum_ref_kinds_of_one_name() {
    // `Store` ÐºÐ°Ðº Ñ‚Ð¸Ð¿ Ã—3 (Ð²ÐµÑ Ð²Ð¸Ð´Ð° 2,0, M6 Â§12Ð°) Ð¸ Ð²Ñ‹Ð·Ð¾Ð² Ã—1: âˆš(3 Ã— 2 + 1), Ð° Ð½Ðµ âˆš6 + âˆš1
    let mut a = file(1, "a.rs", &[], &[]);
    a.refs = vec![
        ("Store".into(), RefKind::Type, 3, 0),
        ("Store".into(), RefKind::Call, 1, 0),
    ];
    let files = [a, file(2, "b.rs", &[("Store", SymbolKind::Struct)], &[])];
    let e = edges_among(&files, &[0, 1]);
    assert!(close(edge(&e, 0, 1).unwrap(), 7f64.sqrt()), "{e:?}");
}

#[test]
fn edges_among_only_shown_sorted_by_weight() {
    let files = [
        file(1, "a.rs", &[], &[("Light", 1), ("Heavy", 9), ("Hidden", 4)]),
        file(2, "b.rs", &[("Light", SymbolKind::Struct)], &[]),
        file(3, "c.rs", &[("Heavy", SymbolKind::Struct)], &[]),
        file(4, "d.rs", &[("Hidden", SymbolKind::Struct)], &[]),
    ];
    let e = edges_among(&files, &[0, 1, 2]);
    assert_eq!(e, [(0, 2, 3.0), (0, 1, 1.0)]);
    // d(name) â€” Ð¿Ð¾ Ð²ÑÐµÐ¼ Ñ„Ð°Ð¹Ð»Ð°Ð¼, Ð½Ðµ Ñ‚Ð¾Ð»ÑŒÐºÐ¾ Ð¿Ð¾ÐºÐ°Ð·Ð°Ð½Ð½Ñ‹Ð¼
    let two = [
        file(1, "a.rs", &[], &[("Name", 4)]),
        file(2, "b.rs", &[("Name", SymbolKind::Struct)], &[]),
        file(3, "c.rs", &[("Name", SymbolKind::Struct)], &[]),
    ];
    assert_eq!(edges_among(&two, &[0, 1]), [(0, 1, 1.0)]);
    assert!(edges_among(&two, &[]).is_empty());
}

#[test]
fn focus_text_common_word_does_not_dilute() {
    // Â«newÂ» Ð¾Ð¿Ñ€ÐµÐ´ÐµÐ»Ñ‘Ð½ Ð² 20 Ñ„Ð°Ð¹Ð»Ð°Ñ…: ÑÐ»Ð¾Ð²Ð¾ Ð¸Ð· Ñ‚ÐµÐºÑÑ‚Ð° Ð½Ðµ Ð¾Ñ‚Ð½Ð¸Ð¼Ð°ÐµÑ‚ Ð¼Ð°ÑÑÑƒ Ñƒ Ñ€ÐµÐ´ÐºÐ¾Ð³Ð¾ Ð¸Ð¼ÐµÐ½Ð¸
    let mut files = graph30();
    for f in files.iter_mut().skip(6).take(20) {
        f.defs
            .push(("new".into(), SymbolKind::Function, Some(true)));
    }
    let focus = Focus {
        paths: vec![],
        idents: vec![],
        text: Some("add new check to focus_target_25".into()),
    };
    let m = focus_restart(&files, &focus).unwrap();
    let total: f64 = m.iter().sum();
    assert!(
        m[25] / total > 0.9,
        "Ð´Ð¾Ð»Ñ Ñ†ÐµÐ»Ð¸ {} Ð¸Ð· {total}",
        m[25]
    );
    assert!(
        close(m[6], 1.0 / 400.0),
        "1/dÂ² Ð¾Ð¿Ñ€ÐµÐ´ÐµÐ»Ð¸Ñ‚ÐµÐ»ÑŽ Ð¾Ð±Ñ‰ÐµÐ³Ð¾ ÑÐ»Ð¾Ð²Ð°: {}",
        m[6]
    );
    let r = rank(&files, Some(&focus));
    assert_eq!(top(&r.file_rank, 1), [25]);
    // ÑÐ²Ð½Ñ‹Ð¹ ident â€” Ð¼Ð°ÑÑÐ° 1 Ð´Ð°Ð¶Ðµ Ñƒ Ð¾Ð±Ñ‰ÐµÐ³Ð¾ Ð¸Ð¼ÐµÐ½Ð¸
    let explicit = Focus {
        idents: vec!["new".into()],
        ..Focus::default()
    };
    assert!(close(focus_restart(&files, &explicit).unwrap()[6], 1.0));
}

/// ÐŸÐ»Ð¾Ñ‚Ð½Ñ‹Ð¹ ÑÑ‚Ð°Ð»Ð¾Ð½ Ð³Ñ€Ð°Ñ„Ð°: Ñ€ÐµÐ±Ñ€Ð¾ A â†’ B Ð´Ð»Ñ ÐºÐ°Ð¶Ð´Ð¾Ð¹ ÑÑÑ‹Ð»ÐºÐ¸ A Ð¸ ÐºÐ°Ð¶Ð´Ð¾Ð³Ð¾ B â‰  A, Ð³Ð´Ðµ Ð¸Ð¼Ñ Ð¾Ð¿Ñ€ÐµÐ´ÐµÐ»ÐµÐ½Ð¾.
fn dense_named(files: &[RankFile]) -> Vec<(usize, usize, String, f64)> {
    let mut definers: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        for (n, _, _) in &f.defs {
            let v = definers.entry(n).or_default();
            if v.last() != Some(&i) {
                v.push(i);
            }
        }
    }
    let mut out = Vec::new();
    for (a, f) in files.iter().enumerate() {
        let mut agg: HashMap<&str, f64> = HashMap::new();
        for (n, k, c, r) in &f.refs {
            *agg.entry(n).or_default() += table_contribution(*k, *c, *r);
        }
        for (n, c) in agg {
            let Some(defs) = definers.get(n) else {
                continue;
            };
            if c == 0.0 {
                continue;
            }
            let d = defs.len();
            let w = c.sqrt() * ident_mult(n, d) / d as f64;
            for &b in defs.iter().filter(|&&b| b != a) {
                out.push((a, b, n.to_string(), w));
            }
        }
    }
    out
}

fn dense_edges(files: &[RankFile]) -> Vec<(usize, usize, f64)> {
    let mut sum: HashMap<(usize, usize), f64> = HashMap::new();
    for (a, b, _, w) in dense_named(files) {
        *sum.entry((a, b)).or_default() += w;
    }
    let mut e: Vec<_> = sum.into_iter().map(|((a, b), w)| (a, b, w)).collect();
    e.sort_by_key(|e| (e.0, e.1));
    e
}

/// Ð’ÐµÑ Ð²Ð¸Ð´Ð° ÑÐ¸Ð¼Ð²Ð¾Ð»Ð° Ð±ÐµÐ· Ð²Ñ…Ð¾Ð´ÑÑ‰Ð¸Ñ… ÑÑÑ‹Ð»Ð¾Ðº (ÑÑ‚Ð°Ð»Ð¾Ð½): Ñ‚Ð¸Ð¿Ñ‹ Ã—2, ÑÐºÑÐ¿Ð¾Ñ€Ñ‚Ð¸Ñ€ÑƒÐµÐ¼Ñ‹Ðµ Ã—2.
fn kind_w(k: SymbolKind, e: Option<bool>) -> f64 {
    let t = matches!(
        k,
        SymbolKind::Class
            | SymbolKind::Struct
            | SymbolKind::Enum
            | SymbolKind::Interface
            | SymbolKind::Trait
            | SymbolKind::Type
    );
    (if t { 2.0 } else { 1.0 }) * (if e == Some(true) { 2.0 } else { 1.0 })
}

/// ÐŸÐ»Ð¾Ñ‚Ð½Ñ‹Ð¹ ÑÑ‚Ð°Ð»Ð¾Ð½ Ð´Ð¾Ð»ÐµÐ¹: Ð²Ñ…Ð¾Ð´ÑÑ‰Ð¸Ð¹ Ð¿Ð¾Ñ‚Ð¾Ðº `r[A] Ã— w / out[A]` Ð¿Ð¾ Ð¸Ð¼ÐµÐ½Ð¸, Ð¾ÑÑ‚Ð°Ñ‚Ð¾Ðº 10 % Ð¿Ð¾ Ð²Ð¸Ð´Ñƒ.
fn dense_shares(files: &[RankFile], r: &[f64]) -> Vec<Vec<f64>> {
    let named = dense_named(files);
    let mut out = vec![0.0; files.len()];
    for (a, _, _, w) in &named {
        out[*a] += w;
    }
    let mut inflow: Vec<HashMap<String, f64>> = vec![HashMap::new(); files.len()];
    for (a, b, n, w) in named {
        *inflow[b].entry(n).or_default() += r[a] * w / out[a];
    }
    files
        .iter()
        .zip(&inflow)
        .map(|(f, inf)| {
            let mut same: HashMap<&str, usize> = HashMap::new();
            for (n, _, _) in &f.defs {
                *same.entry(n).or_default() += 1;
            }
            let fl = |n: &str| inf.get(n).copied().unwrap_or(0.0);
            let hot: f64 = same.keys().map(|n| fl(n)).sum();
            let cold: f64 = f
                .defs
                .iter()
                .filter(|(n, _, _)| fl(n) <= 0.0)
                .map(|(_, k, e)| kind_w(*k, *e))
                .sum();
            let (hp, cp) = match (hot > 0.0, cold > 0.0) {
                (true, true) => (0.9, 0.1),
                (true, false) => (1.0, 0.0),
                _ => (0.0, 1.0),
            };
            f.defs
                .iter()
                .map(|(n, k, e)| {
                    if fl(n) > 0.0 {
                        hp * fl(n) / hot / same[n.as_str()] as f64
                    } else {
                        cp * kind_w(*k, *e) / cold
                    }
                })
                .collect()
        })
        .collect()
}

const NAMES: &[&str] = &[
    "new",
    "Store",
    "open_store_file",
    "_priv",
    "x",
    "getUserById",
    "Db",
    "run",
    "_inner_helper",
    "parse",
];

fn arb_files() -> impl Strategy<Value = Vec<RankFile>> {
    let def = (
        0..NAMES.len(),
        0..SymbolKind::ALL.len(),
        any::<Option<bool>>(),
    );
    let rf = (0..NAMES.len(), 0..RefKind::ALL.len(), 0u32..6, 0u32..8);
    let one = (
        proptest::collection::vec(def, 0..5),
        proptest::collection::vec(rf, 0..8),
    );
    proptest::collection::vec(one, 1..25).prop_map(|files| {
        files
            .into_iter()
            .enumerate()
            .map(|(i, (defs, refs))| RankFile {
                id: i as i64,
                path: format!("f{i}.rs"),
                defs: defs
                    .into_iter()
                    .map(|(n, k, e)| (NAMES[n].to_string(), SymbolKind::ALL[k], e))
                    .collect(),
                refs: refs
                    .into_iter()
                    .map(|(n, k, c, r)| (NAMES[n].to_string(), RefKind::ALL[k], c, r))
                    .collect(),
            })
            .collect()
    })
}

proptest! {
    #[test]
    fn factorized_equals_dense_on_random_graphs(
        files in arb_files(),
        restart in proptest::collection::vec(0.0f64..1.0, 25),
        focused in any::<bool>(),
    ) {
        let n = files.len();
        let restart = focused.then(|| restart[..n].to_vec());
        let fact = rank_on_graph(&Graph::new(&files), restart.as_deref());
        let dense = pagerank(n, &dense_edges(&files), restart.as_deref());
        let l1: f64 = fact.iter().zip(&dense).map(|(a, b)| (a - b).abs()).sum();
        prop_assert!(l1 < 1e-9, "L1 {}", l1);
        let all: Vec<usize> = (0..n).rev().collect();
        let mut among = edges_among(&files, &all);
        among.sort_by_key(|e| (e.0, e.1));
        let de = dense_edges(&files);
        prop_assert_eq!(among.len(), de.len());
        for (x, y) in among.iter().zip(&de) {
            prop_assert!(x.0 == y.0 && x.1 == y.1 && (x.2 - y.2).abs() < 1e-12);
        }
        let r = rank(&files, None);
        let shares = dense_shares(&files, &r.file_rank);
        for (i, (s, d)) in r.symbol_share.iter().zip(&shares).enumerate() {
            prop_assert_eq!(s.len(), d.len());
            for (a, b) in s.iter().zip(d) {
                prop_assert!((a - b).abs() < 1e-9, "Ñ„Ð°Ð¹Ð» {}: {:?} != {:?}", i, s, d);
            }
        }
    }
}

/// Ð¡Ð¸Ð½Ñ‚ÐµÑ‚Ð¸ÐºÐ° Ð¿Ñ€Ð¾Ð¿Ð¾Ñ€Ñ†Ð¸Ð¹ Ñ€ÐµÐ¿Ð¾Ð·Ð¸Ñ‚Ð¾Ñ€Ð¸Ñ (Ð³ÐµÐ½ÐµÑ€Ð°Ñ‚Ð¾Ñ€ Ñ€ÐµÐ²ÑŒÑŽ Ð·Ð°Ð´Ð°Ñ‡Ð¸ 2): Ð¾Ð±Ñ‰Ð¸Ðµ Ð¸Ð¼ÐµÐ½Ð° Ð¼ÐµÑ‚Ð¾Ð´Ð¾Ð² Ð² Ñ‡Ð°ÑÑ‚Ð¸
/// Ñ„Ð°Ð¹Ð»Ð¾Ð², 12 ÑÐ²Ð¾Ð¸Ñ… ÑÐ¸Ð¼Ð²Ð¾Ð»Ð¾Ð², 40 ÑÑÑ‹Ð»Ð¾Ðº ÑÐ¾ ÑÑ‚ÐµÐ¿ÐµÐ½Ð½Ñ‹Ð¼ Ñ€Ð°ÑÐ¿Ñ€ÐµÐ´ÐµÐ»ÐµÐ½Ð¸ÐµÐ¼ Ñ†ÐµÐ»Ð¸.
fn synth(n: usize, common_def: f64, common_ref: f64) -> Vec<RankFile> {
    let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut rnd = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let common = [
        "new",
        "from",
        "default",
        "fmt",
        "clone",
        "drop",
        "get",
        "len",
        "run",
        "into",
        "as_ref",
        "eq",
        "hash",
        "build",
        "parse",
        "next",
        "deref",
        "try_from",
        "with_capacity",
        "is_empty",
    ];
    let mut files = Vec::with_capacity(n);
    for i in 0..n {
        let mut defs = Vec::new();
        for (ci, c) in common.iter().enumerate() {
            if (rnd() % 10_000) as f64 / 10_000.0 < common_def / (1.0 + ci as f64 * 0.3) {
                defs.push((c.to_string(), SymbolKind::Method, Some(true)));
            }
        }
        for k in 0..12 {
            let kind = if k == 0 {
                SymbolKind::Struct
            } else {
                SymbolKind::Function
            };
            defs.push((format!("Sym{i}x{k}"), kind, Some(k % 2 == 0)));
        }
        let mut refs = Vec::new();
        for (ci, c) in common.iter().enumerate() {
            if (rnd() % 10_000) as f64 / 10_000.0 < common_ref / (1.0 + ci as f64 * 0.3) {
                refs.push((c.to_string(), RefKind::Call, 1 + (rnd() % 8) as u32, 0));
            }
        }
        for _ in 0..40 {
            let u = (rnd() % 1_000_000) as f64 / 1_000_000.0;
            let t = ((n as f64).powf(u) as usize).min(n - 1);
            let k = rnd() % 12;
            refs.push((
                format!("Sym{t}x{k}"),
                RefKind::Call,
                1 + (rnd() % 4) as u32,
                0,
            ));
        }
        files.push(RankFile {
            id: i as i64,
            path: format!("src/f{i}.rs"),
            defs,
            refs,
        });
    }
    files
}

/// Ð—Ð°Ð¼ÐµÑ€: `cargo test -p zam-code --release --test rank -- --ignored --nocapture`.
#[test]
#[ignore = "Ð·Ð°Ð¼ÐµÑ€ Ð² release: 50 000 Ñ„Ð°Ð¹Ð»Ð¾Ð² ÑÐ¸Ð½Ñ‚ÐµÑ‚Ð¸ÐºÐ¸, rank() < 1 Ñ"]
fn rank_50k_files_under_1s_release() {
    for (def, rf) in [(0.01, 0.1), (0.1, 0.3)] {
        let files = synth(50_000, def, rf);
        let t = std::time::Instant::now();
        let r = rank(&files, None);
        let el = t.elapsed();
        let refs: usize = files.iter().map(|f| f.refs.len()).sum();
        println!("50 000 Ñ„Ð°Ð¹Ð»Ð¾Ð² (def {def}, ref {rf}), ÑÑÑ‹Ð»Ð¾Ðº {refs}: rank() {el:?}");
        assert!((r.file_rank.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        if !cfg!(debug_assertions) {
            assert!(el.as_secs_f64() < 1.0, "{el:?}");
        }
    }
}

/// M6 Â§12Ð°: Ñ‚Ð°Ð±Ð»Ð¸Ñ†Ð° Ð²ÐµÑÐ¾Ð² Ð²Ð¸Ð´Ð° Ð¾Ð±ÑŠÑÐ²Ð»ÐµÐ½Ð° Ð´Ð¾ Ð·Ð°Ð¼ÐµÑ€Ð° â€” Ñ‚Ð¸Ð¿ Ð¸ Ñ€ÐµÐ°Ð»Ð¸Ð·Ð°Ñ†Ð¸Ñ 2,0, Ð¿Ñ€ÑÐ¼Ð¾Ð¹ Ð²Ñ‹Ð·Ð¾Ð², Ð¸Ð¼Ð¿Ð¾Ñ€Ñ‚ Ð¸
/// Ð¿Ñ€Ð¾Ñ‡ÐµÐµ 1,0, Ð²Ñ‹Ð·Ð¾Ð² Ð¿Ð¾ Ð¿Ð¾Ð»ÑƒÑ‡Ð°Ñ‚ÐµÐ»ÑŽ 0,25: Ð²ÐºÐ»Ð°Ð´ Ð²Ñ‹Ð·Ð¾Ð²Ð° `(count âˆ’ recv) Ã— 1,0 + recv Ã— 0,25`
/// (`recv` Ð±Ð¾Ð»ÑŒÑˆÐµ `count` â€” Ð½Ðµ Ð±Ð¾Ð»ÑŒÑˆÐµ `count`). Ð’ÐµÑ Ñ€ÐµÐ±Ñ€Ð° â€” `âˆšc` (Ð¸Ð¼Ñ `Store`: Ð¼Ð½Ð¾Ð¶Ð¸Ñ‚ÐµÐ»ÑŒ 1, d = 1).
#[test]
fn weights_table_fixed() {
    let weight = |kind: RefKind, count: u32, recv: u32| {
        let mut a = file(1, "a.rs", &[], &[]);
        a.refs = vec![("Store".into(), kind, count, recv)];
        let files = [a, file(2, "b.rs", &[("Store", SymbolKind::Struct)], &[])];
        edge(&edges_among(&files, &[0, 1]), 0, 1).unwrap()
    };
    for (kind, w) in [
        (RefKind::Type, 2.0f64),
        (RefKind::Impl, 2.0),
        (RefKind::Call, 1.0),
        (RefKind::Import, 1.0),
        (RefKind::Other, 1.0),
    ] {
        assert!(close(weight(kind, 4, 0), (4.0 * w).sqrt()), "{kind}");
    }
    assert!(close(weight(RefKind::Call, 4, 4), 1.0));
    assert!(close(weight(RefKind::Call, 4, 2), 2.5f64.sqrt()));
    assert!(close(weight(RefKind::Call, 4, 9), 1.0), "recv â‰¤ count");
    // ÑÑ‡Ñ‘Ñ‚Ñ‡Ð¸Ðº Ð¿Ð¾Ð»ÑƒÑ‡Ð°Ñ‚ÐµÐ»Ñ Ñƒ Ð½Ðµ-Ð²Ñ‹Ð·Ð¾Ð²Ð° Ð½Ðµ Ð´ÐµÐ¹ÑÑ‚Ð²ÑƒÐµÑ‚
    assert!(close(weight(RefKind::Type, 4, 4), 8f64.sqrt()));
    // Ð¿Ñ€Ð°Ð²Ð¸Ð»Ð° M5.7 Ð´Ð»Ñ Ð°Ð±Ð»ÑÑ†Ð¸Ð¸: Ð²Ð¸Ð´Ñ‹ Ñ€Ð°Ð²Ð½Ñ‹, Ð¿Ð¾Ð»ÑƒÑ‡Ð°Ñ‚ÐµÐ»ÑŒ Ð½Ðµ Ñ€Ð°Ð·Ð»Ð¸Ñ‡Ð°ÐµÑ‚ÑÑ
    let mut a = file(1, "a.rs", &[], &[]);
    a.refs = vec![
        ("Store".into(), RefKind::Type, 3, 0),
        ("Store".into(), RefKind::Call, 4, 2),
    ];
    let files = [a, file(2, "b.rs", &[("Store", SymbolKind::Struct)], &[])];
    // M5.7: √(3 + 4); M6: √(3 × 2 + 2 + 2 × 0,25)
    assert!(close(
        edge(&edges_among(&files, &[0, 1]), 0, 1).unwrap(),
        8.5f64.sqrt()
    ));
    let m57 = Graph::with_weights(&files, RankWeights::M57);
    let m6 = Graph::new(&files);
    assert_eq!(m6, Graph::with_weights(&files, RankWeights::M6));
    assert_ne!(m57, m6);
    let r57 = rank_with(&files, None, RankWeights::M57);
    let uniform: Vec<RankFile> = files
        .iter()
        .map(|f| RankFile {
            refs: f
                .refs
                .iter()
                .map(|(n, _, c, _)| (n.clone(), RefKind::Call, *c, 0))
                .collect(),
            ..f.clone()
        })
        .collect();
    assert_eq!(
        r57,
        rank(&uniform, None),
        "M5.7: Ð²ÑÐµ Ð²Ð¸Ð´Ñ‹ ÐºÐ°Ðº Ð¿Ñ€ÑÐ¼Ð¾Ð¹ Ð²Ñ‹Ð·Ð¾Ð²"
    );
}

/// Входы ранжирования из JSON эталона шага 0 задачи 14 M6 (`files[].defs [[name, kind,
/// exported]]`, `refs [[name, kind, count]]` — данные клиента до M6, без `recv_count`).
fn m57_inputs(v: &serde_json::Value) -> Vec<RankFile> {
    v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| RankFile {
            id: f["id"].as_i64().unwrap(),
            path: f["path"].as_str().unwrap().to_string(),
            defs: f["defs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| {
                    (
                        d[0].as_str().unwrap().to_string(),
                        SymbolKind::parse(d[1].as_str().unwrap()).unwrap(),
                        d[2].as_bool(),
                    )
                })
                .collect(),
            // старый клиент: поля нет — сервер M6 хранит `recv_count = 0`
            refs: f["refs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    (
                        r[0].as_str().unwrap().to_string(),
                        RefKind::parse(r[1].as_str().unwrap()).unwrap(),
                        u32::try_from(r[2].as_u64().unwrap()).unwrap(),
                        0,
                    )
                })
                .collect(),
        })
        .collect()
}

fn f64s(v: &serde_json::Value) -> Vec<f64> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect()
}

/// Ранги кода M6 на входах `files` с правилами `w` совпадают с рангами библиотеки M5.7 `want`
/// (`file_rank`, `symbol_share`) до последних знаков: допуск 1e-13 относительно — разбор чисел
/// `serde_json` без `float_roundtrip` ошибается на единицу последнего разряда (≈ 2e-16).
fn assert_same_ranks(files: &[RankFile], want: &serde_json::Value, w: RankWeights) {
    let got = rank_with(files, None, w);
    let same = |a: &[f64], b: &[f64]| {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .all(|(x, y)| (x - y).abs() <= 1e-13 * x.abs().max(y.abs()))
    };
    let want_rank = f64s(&want["file_rank"]);
    assert!(same(&got.file_rank, &want_rank), "{w:?}: ранги файлов");
    let shares: Vec<Vec<f64>> = want["symbol_share"]
        .as_array()
        .unwrap()
        .iter()
        .map(f64s)
        .collect();
    assert_eq!(got.symbol_share.len(), shares.len());
    for (g, s) in got.symbol_share.iter().zip(&shares) {
        assert!(same(g, s), "{w:?}: доли символов");
    }
}

/// M6 §12а: данные старого клиента (`recv_count` нет → 0) — вызовы прямые, вес вызова как в
/// v0.0.10: ранги кода M6 при равных весах видов совпадают с рангами M5.7 (до 1e-13). Эталон —
/// `tests/fixtures/rank_m57/`: входы — подмножество карты репозитория шага 0 (база прогона
/// `code-bench` бинарниками `v0.0.10-m5.7` на снимке `045ef40`, файлы `crates/zam-code/**` и
/// `crates/zam-cli/src/cmd/code/**`), ранги посчитаны библиотекой `zam-code` тега
/// `v0.0.10-m5.7` (журнал этапа, задача 14). Таблица весов вида M6 — отдельное объявленное
/// изменение (`weights_table_fixed`); с ней ранги другие.
#[test]
fn ranks_unchanged_when_recv_count_zero() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rank_m57");
    let read = |f: &str| -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(dir.join(f)).unwrap()).unwrap()
    };
    let files = m57_inputs(&read("inputs.json"));
    assert!(files.len() >= 20, "{}", files.len());
    let want = read("ranks.json");
    // получатель учитывается, но счётчиков нет — как M5.7
    let recv_only = RankWeights {
        kind_table: false,
        recv: true,
    };
    assert_same_ranks(&files, &want, recv_only);
    assert_same_ranks(&files, &want, RankWeights::M57);
    // правила M6 с таблицей видов меняют ранги (иначе эталон ничего не проверял бы)
    let m6 = rank(&files, None).file_rank;
    let diff = m6
        .iter()
        .zip(f64s(&want["file_rank"]))
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f64::max);
    assert!(diff > 1e-6, "{diff}");
}

/// То же на всей карте репозитория шага 0 (не в наборе: эталон — `.tmp`, ≈ мегабайты):
/// `ZAM_RANKS_M57=<ranks-m57.json> cargo test -p zam-code --test rank -- --ignored full_repo`.
/// Сверка — с рангами библиотеки M5.7 (до 1e-13) и с сохранёнными сервером M5.7 (`f32`
/// долей, перезапись при изменении больше 1e-4 — допуск 1e-3 относительно).
#[test]
#[ignore = "эталон всей карты — .tmp/m6/task14/ranks-m57.json (ZAM_RANKS_M57)"]
fn ranks_unchanged_when_recv_count_zero_full_repo() {
    let path = std::env::var("ZAM_RANKS_M57").expect("ZAM_RANKS_M57");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let files = m57_inputs(&v);
    let recv_only = RankWeights {
        kind_table: false,
        recv: true,
    };
    assert_same_ranks(&files, &v["lib_ranks"], recv_only);
    let got = rank_with(&files, None, recv_only);
    let mut worst = 0.0f64;
    for (i, f) in v["files"].as_array().unwrap().iter().enumerate() {
        let stored = f["stored_rank"].as_f64().unwrap();
        let rel = (got.file_rank[i] - stored).abs() / stored.max(1e-12);
        worst = worst.max(rel);
    }
    println!(
        "файлов {}, наибольшее относительное расхождение с рангами сервера M5.7: {worst:.2e}",
        files.len()
    );
    assert!(worst < 1e-3, "{worst}");
}
