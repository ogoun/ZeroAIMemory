//! Ранжирование карты кода (спецификация M5.6 §7): граф файлов, PageRank, доли символов.

use zam_code::{
    Focus, RankFile, SymbolKind, central, file_edges, focus_idents, focus_restart, ident_mult,
    pagerank, rank, rank_on_edges,
};

fn file(id: i64, path: &str, defs: &[(&str, SymbolKind)], refs: &[(&str, u32)]) -> RankFile {
    RankFile {
        id,
        path: path.to_string(),
        defs: defs
            .iter()
            .map(|(n, k)| (n.to_string(), *k, None))
            .collect(),
        refs: refs.iter().map(|(n, c)| (n.to_string(), *c)).collect(),
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn edge(edges: &[(usize, usize, f64)], s: usize, d: usize) -> Option<f64> {
    edges.iter().find(|e| e.0 == s && e.1 == d).map(|e| e.2)
}

/// Эталонный степенной метод без предела итераций (для проверки сходимости за 50).
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
        ("open_store_file", 1, 10.0),
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
    let e = file_edges(&files);
    assert_eq!(
        edge(&e, 0, 0),
        None,
        "ссылка на своё определение — не ребро"
    );
    assert!(close(edge(&e, 1, 0).unwrap(), 10.0));
    assert_eq!(e.len(), 1);
}

#[test]
fn edges_split_by_definers_count() {
    // `open_store` определён в b и c: вес A → B и A → C — √4 × 10 / 2 = 10; ссылки суммируются
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
    let e = file_edges(&files);
    assert!(close(edge(&e, 0, 1).unwrap(), 10.0 + 1.0));
    assert!(close(edge(&e, 0, 2).unwrap(), 10.0));
    assert_eq!(e.len(), 2, "неопределённое имя рёбер не даёт: {e:?}");
}

#[test]
fn pagerank_sums_to_one_converges_50_iters() {
    // кольцо с хордами и висячим узлом
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
    assert!(l1 < 1e-4, "L1 до эталона {l1}");
    assert_eq!(rank_on_edges(n, &edges, None), r);
    assert!(pagerank(0, &[], None).is_empty());
}

#[test]
fn pagerank_star_center_highest() {
    let n = 9;
    let edges: Vec<_> = (1..n).map(|i| (i, 0, 1.0)).collect();
    let r = pagerank(n, &edges, None);
    for (i, v) in r.iter().enumerate().skip(1) {
        assert!(r[0] > *v * 2.0, "центр {} лист {i} {v}", r[0]);
    }
}

#[test]
fn pagerank_dangling_nodes_no_nan() {
    // только висячие узлы, мусорные рёбра и нулевой вектор перезапуска
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
    assert!(r[2] > r[3], "перезапуск в узел 2: {r:?}");
}

/// Граф из 30 файлов: хаб, средние узлы, фокусный файл и три его цели.
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
            refs: vec![("shared_service_registry".into(), 2)],
        });
    }
    for i in 6..30 {
        let mut refs = vec![("shared_service_registry".to_string(), 3)];
        refs.push((format!("middle_layer_{}", i % 5 + 1), 2));
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
            // фокусный файл ссылается на хаб и три цели: вместе с ним — ровно пятёрка
            refs.truncate(1);
            for t in 25..=27 {
                refs.push((format!("focus_target_{t}"), 1));
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
    assert!(!g5.contains(&20) && !g5.contains(&25), "без фокуса: {g5:?}");
    let focus = Focus {
        paths: vec!["src/f20.rs".into()],
        idents: vec![],
        text: None,
    };
    let r = rank(&files, Some(&focus));
    let t5 = top(&r.file_rank, 5);
    for want in [20, 25, 26, 27] {
        assert!(t5.contains(&want), "{want} не в пятёрке {t5:?}");
    }
    assert!((r.file_rank.iter().sum::<f64>() - 1.0).abs() < 1e-9);
    // тот же результат по сохранённым рёбрам
    let restart = focus_restart(&files, &focus).unwrap();
    assert_eq!(
        rank_on_edges(files.len(), &r.edges, Some(&restart)),
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
        text: Some("почему падает focus_target_25 при старте?".into()),
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
    assert!(s[0] > s[1] + s[2] + s[3], "входящий вес по имени: {s:?}");
    assert!(close(s[2], 2.0 * s[1]), "тип ×2: {s:?}");
    assert!(close(s[3], 2.0 * s[1]), "экспортируемый ×2: {s:?}");
    // файл без входящих ссылок — доли только по виду
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
        assert!(score(w[0]) >= score(w[1]), "по убыванию ранга: {c:?}");
    }
    assert_eq!(central(&files, &r, 2).len(), 2);
    assert!(central(&files, &r, 0).is_empty());
}
