//! Ранжирование карты кода (спецификация M5.6 §7): граф файлов, PageRank, доли символов;
//! связывание ссылок с определениями (M7.5 §8.6).

use std::collections::HashMap;

use proptest::prelude::*;
use zam_code::{
    Focus, Graph, RankFile, RankWeights, RefKind, SymbolKind, central, edges_among,
    edges_among_with, focus_idents, focus_restart, ident_mult, pagerank, rank, rank_on_graph,
    rank_with,
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

/// Вклад ссылки по таблице M6 §12а, записанной здесь независимо от `rank.rs`: тип и реализация
/// 2,0, прочие 1,0; у вызова `recv` (не больше `count`) — 0,25.
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
    let e = edges_among(&files, &[0, 1, 2, 3]);
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
            // фокусный файл ссылается на хаб и три цели: вместе с ним — ровно пятёрка
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
    // тот же результат по графу (персонализация на сервере)
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

#[test]
fn edges_sum_ref_kinds_of_one_name() {
    // `Store` как тип ×3 (вес вида 2,0, M6 §12а) и вызов ×1: √(3 × 2 + 1), а не √6 + √1
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
    // d(name) — по всем файлам, не только показанным
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
    // «new» определён в 20 файлах: слово из текста не отнимает массу у редкого имени
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
    assert!(m[25] / total > 0.9, "доля цели {} из {total}", m[25]);
    assert!(
        close(m[6], 1.0 / 400.0),
        "1/d² определителю общего слова: {}",
        m[6]
    );
    let r = rank(&files, Some(&focus));
    assert_eq!(top(&r.file_rank, 1), [25]);
    // явный ident — масса 1 даже у общего имени
    let explicit = Focus {
        idents: vec!["new".into()],
        ..Focus::default()
    };
    assert!(close(focus_restart(&files, &explicit).unwrap()[6], 1.0));
}

/// Плотный эталон графа: ребро A → B для каждой ссылки A и каждого B ≠ A, где имя определено.
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

/// Вес вида символа без входящих ссылок (эталон): типы ×2, экспортируемые ×2.
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

/// Плотный эталон долей: входящий поток `r[A] × w / out[A]` по имени, остаток 10 % по виду.
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
                prop_assert!((a - b).abs() < 1e-9, "файл {}: {:?} != {:?}", i, s, d);
            }
        }
    }
}

/// Синтетика пропорций репозитория (генератор ревью задачи 2): общие имена методов в части
/// файлов, 12 своих символов, 40 ссылок со степенным распределением цели.
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

/// Замер: `cargo test -p zam-code --release --test rank -- --ignored --nocapture`. Правила M6 и
/// со связыванием (В1 + В2, M7.5 §8.6) — по < 1 с.
#[test]
#[ignore = "замер в release: 50 000 файлов синтетики, rank() < 1 с"]
fn rank_50k_files_under_1s_release() {
    for (def, rf) in [(0.01, 0.1), (0.1, 0.3)] {
        let files = synth(50_000, def, rf);
        for (name, w) in [("M6", RankWeights::M6), ("В1 + В2", W12)] {
            let t = std::time::Instant::now();
            let r = rank_with(&files, None, w);
            let el = t.elapsed();
            let refs: usize = files.iter().map(|f| f.refs.len()).sum();
            println!("50 000 файлов (def {def}, ref {rf}), ссылок {refs}, {name}: rank() {el:?}");
            assert!((r.file_rank.iter().sum::<f64>() - 1.0).abs() < 1e-9);
            if !cfg!(debug_assertions) {
                assert!(el.as_secs_f64() < 1.0, "{name}: {el:?}");
            }
        }
    }
}

/// M6 §12а: таблица весов вида объявлена до замера — тип и реализация 2,0, прямой вызов, импорт и
/// прочее 1,0, вызов по получателю 0,25: вклад вызова `(count − recv) × 1,0 + recv × 0,25`
/// (`recv` больше `count` — не больше `count`). Вес ребра — `√c` (имя `Store`: множитель 1, d = 1).
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
    // счётчик получателя у не-вызова не действует
    assert!(close(weight(RefKind::Type, 4, 4), 8f64.sqrt()));
    // правила M5.7 для абляции: виды равны, получатель не различается
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
    assert_eq!(r57, rank(&uniform, None), "M5.7: все виды как прямой вызов");
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
        recv: true,
        ..RankWeights::M57
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
        recv: true,
        ..RankWeights::M57
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

// ---- M7.5 §8.6: связывание ссылки с определением ----

/// В1 «по виду» на весах M6.
const W1: RankWeights = RankWeights {
    bind_kind: true,
    ..RankWeights::M6
};
/// В2 «экспорт» на весах M6.
const W2: RankWeights = RankWeights {
    bind_export: true,
    ..RankWeights::M6
};
/// В1 + В2.
const W12: RankWeights = RankWeights {
    bind_kind: true,
    bind_export: true,
    ..RankWeights::M6
};

/// Файл с определениями `(имя, вид, exported)` и ссылками `(имя, вид, count)` (`recv` = 0).
fn bfile(
    path: &str,
    defs: &[(&str, SymbolKind, Option<bool>)],
    refs: &[(&str, RefKind, u32)],
) -> RankFile {
    RankFile {
        id: 0,
        path: path.to_string(),
        defs: defs
            .iter()
            .map(|(n, k, e)| (n.to_string(), *k, *e))
            .collect(),
        refs: refs
            .iter()
            .map(|(n, k, c)| (n.to_string(), *k, *c, 0))
            .collect(),
    }
}

fn all_edges(files: &[RankFile], w: RankWeights) -> Vec<(usize, usize, f64)> {
    let all: Vec<usize> = (0..files.len()).collect();
    edges_among_with(files, &all, w)
}

/// В1: ссылка `type`/`impl` идёт только к определениям-типам, `call` — только к функциям и
/// методам; `d(n)` — число подходящих (не всех одноимённых); `import` — ко всем, как в M6.
#[test]
fn bind_by_kind_type_refs_to_types_only() {
    use SymbolKind as S;
    let files = [
        bfile("a.ts", &[], &[("Node", RefKind::Type, 4)]),
        bfile("b.ts", &[("Node", S::Class, None)], &[]),
        bfile("c.ts", &[("Node", S::Function, None)], &[]),
        bfile("d.ts", &[], &[("Node", RefKind::Call, 9)]),
        bfile("e.ts", &[], &[("Node", RefKind::Impl, 1)]),
        bfile("f.ts", &[], &[("Node", RefKind::Import, 4)]),
    ];
    let e = all_edges(&files, W1);
    // тип: √(4 × 2) к классу, d = 1; к функции — нет
    assert!(close(edge(&e, 0, 1).unwrap(), 8f64.sqrt()), "{e:?}");
    assert_eq!(edge(&e, 0, 2), None, "{e:?}");
    // вызов: √9 к функции, d = 1; к классу — нет
    assert!(close(edge(&e, 3, 2).unwrap(), 3.0), "{e:?}");
    assert_eq!(edge(&e, 3, 1), None, "{e:?}");
    // реализация — к типу
    assert!(close(edge(&e, 4, 1).unwrap(), 2f64.sqrt()), "{e:?}");
    assert_eq!(edge(&e, 4, 2), None, "{e:?}");
    // импорт — ко всем одноимённым, d = 2
    assert!(close(edge(&e, 5, 1).unwrap(), 1.0) && close(edge(&e, 5, 2).unwrap(), 1.0));
    // M6: тип — к обоим, d = 2
    let m6 = all_edges(&files, RankWeights::M6);
    assert!(close(edge(&m6, 0, 1).unwrap(), 8f64.sqrt() / 2.0), "{m6:?}");
    assert!(close(edge(&m6, 0, 2).unwrap(), 8f64.sqrt() / 2.0), "{m6:?}");
    // ранг: класс получает поток ссылок типа, функция — вызова
    let r = rank_with(&files, None, W1);
    let only_type = [files[0].clone(), files[1].clone(), files[2].clone()];
    let rt = rank_with(&only_type, None, W1).file_rank;
    assert!(rt[1] > rt[2], "{rt:?}");
    let r6 = rank_with(&only_type, None, RankWeights::M6).file_rank;
    assert!(close(r6[1], r6[2]), "M6 — поровну: {r6:?}");
    assert!((r.file_rank.iter().sum::<f64>() - 1.0).abs() < 1e-9);
    // ссылка типа на имя одного файла с типом и функцией: поток — типу, не функции
    let both = [
        bfile("a.rs", &[], &[("Store", RefKind::Type, 4)]),
        bfile(
            "b.rs",
            &[("Store", S::Struct, None), ("Store", S::Function, None)],
            &[],
        ),
    ];
    // функции — только доля символов без входящих ссылок (10 %)
    let s = &rank_with(&both, None, W1).symbol_share[1];
    assert!(close(s[0], 0.9) && close(s[1], 0.1), "{s:?}");
    let s6 = &rank_with(&both, None, RankWeights::M6).symbol_share[1];
    assert!(close(s6[0], s6[1]), "M6 — поровну одноимённым: {s6:?}");
}

/// В1: имя без определения нужного вида — ко всем одноимённым, как в M6 (`d` — все); прочие
/// виды ссылок (`import`, `other`) — ко всем.
#[test]
fn bind_by_kind_falls_back() {
    use SymbolKind as S;
    let files = [
        bfile(
            "a.rs",
            &[],
            &[
                ("make_store", RefKind::Type, 2),
                ("Store", RefKind::Call, 4),
                ("cfg", RefKind::Other, 1),
            ],
        ),
        bfile(
            "b.rs",
            &[
                ("make_store", S::Function, None),
                ("Store", S::Struct, None),
                ("cfg", S::Const, None),
            ],
            &[],
        ),
        bfile(
            "c.rs",
            &[("make_store", S::Function, None), ("cfg", S::Module, None)],
            &[],
        ),
    ];
    for w in [W1, W12] {
        let e = all_edges(&files, w);
        let m6 = all_edges(&files, RankWeights::M6);
        assert_eq!(e.len(), m6.len(), "{w:?}: {e:?}");
        for (x, y) in e.iter().zip(&m6) {
            assert!(
                x.0 == y.0 && x.1 == y.1 && close(x.2, y.2),
                "{w:?}: {e:?} / {m6:?}"
            );
        }
        assert_eq!(rank_with(&files, None, w), rank(&files, None), "{w:?}");
    }
    // тип `make_store` (функции в b и c): √(2 × 2) × 10 / 2 к каждому
    let e = all_edges(&files, W1);
    assert!(close(edge(&e, 0, 2).unwrap(), 10.0 + 0.5), "{e:?}");
}

/// В2: ссылка из другого файла не идёт к определению с `exported = Some(false)`; `None`
/// (язык без признака) — идёт; своё закрытое определение файла — среди подходящих (`d`), но
/// ребра к себе нет.
#[test]
fn bind_exported_skips_private_cross_file() {
    use SymbolKind as S;
    let files = [
        bfile("a.rs", &[], &[("helper", RefKind::Call, 9)]),
        bfile(
            "b.rs",
            &[
                ("helper", S::Function, Some(false)),
                ("Api", S::Struct, Some(true)),
            ],
            &[],
        ),
        bfile("c.rs", &[("helper", S::Function, Some(true))], &[]),
        bfile("d.py", &[("helper", S::Function, None)], &[]),
        bfile(
            "e.rs",
            &[("helper", S::Function, Some(false))],
            &[("helper", RefKind::Call, 9), ("Api", RefKind::Type, 2)],
        ),
    ];
    for w in [W2, W12] {
        let e = all_edges(&files, w);
        // a: подходящие — c и d (d = 2): √9 / 2
        assert_eq!(edge(&e, 0, 1), None, "{w:?}: {e:?}");
        assert!(close(edge(&e, 0, 2).unwrap(), 1.5) && close(edge(&e, 0, 3).unwrap(), 1.5));
        // e: подходящие — c, d и своё закрытое (d = 3), ребра к себе и к b нет
        assert!(close(edge(&e, 4, 2).unwrap(), 1.0), "{w:?}: {e:?}");
        assert!(close(edge(&e, 4, 3).unwrap(), 1.0), "{w:?}: {e:?}");
        assert_eq!(edge(&e, 4, 4), None);
        // тип `Api` открыт — ребро есть
        assert!(close(edge(&e, 4, 1).unwrap(), 2.0), "{w:?}: {e:?}");
        // доли b: закрытый `helper` потока не получает — у него доля символов без ссылок
        let s = &rank_with(&files, None, w).symbol_share[1];
        assert!(close(s[1], 0.9) && close(s[0], 0.1), "{w:?}: {s:?}");
    }
    let s6 = &rank_with(&files, None, RankWeights::M6).symbol_share[1];
    assert!(s6[0] > 0.1 && s6[1] < 0.9, "M6: {s6:?}");
    // M6: к закрытому тоже (d = 4)
    let m6 = all_edges(&files, RankWeights::M6);
    assert!(close(edge(&m6, 0, 1).unwrap(), 0.75), "{m6:?}");
    // все определения имени закрыты: ссылка из другого файла рёбер не даёт
    let closed = [
        bfile("a.rs", &[], &[("helper", RefKind::Call, 4)]),
        bfile("b.rs", &[("helper", S::Function, Some(false))], &[]),
    ];
    assert!(all_edges(&closed, W2).is_empty());
    assert_eq!(all_edges(&closed, RankWeights::M6).len(), 1);
}

/// Входы эталона M6 (`tests/fixtures/rank_m57/inputs.json`) с `recv_count = count / 2` у
/// вызовов — так эталон проверяет и вызов по получателю.
fn m6_inputs(v: &serde_json::Value) -> Vec<RankFile> {
    m57_inputs(v)
        .into_iter()
        .map(|mut f| {
            for r in &mut f.refs {
                if r.1 == RefKind::Call {
                    r.3 = r.2 / 2;
                }
            }
            f
        })
        .collect()
}

/// Числа эталона — строки `{:?}` (кратчайшая точная запись `f64`): разбор без потерь.
fn exact(v: &serde_json::Value) -> Vec<f64> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().parse::<f64>().unwrap())
        .collect()
}

/// Правила M6 (`RankWeights::M6`, связывание «ко всем») после M7.5 дают побайтово прежние ранги
/// файлов, доли символов, ранги с фокусом и рёбра. Эталон `tests/fixtures/rank_m57/ranks_m6.json`
/// посчитан библиотекой до M7.5 (база ветки `19c521b7`) на входах эталона M5.7 (`rank_m57`).
#[test]
fn rank_m6_unchanged() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rank_m57");
    let read = |f: &str| -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(dir.join(f)).unwrap()).unwrap()
    };
    let files = m6_inputs(&read("inputs.json"));
    let want = read("ranks_m6.json");
    assert_eq!(
        RankWeights::M6,
        RankWeights {
            kind_table: true,
            recv: true,
            bind_kind: false,
            bind_export: false,
        }
    );
    for r in [rank(&files, None), rank_with(&files, None, RankWeights::M6)] {
        assert_eq!(r.file_rank, exact(&want["file_rank"]), "ранги файлов");
        let shares: Vec<Vec<f64>> = want["symbol_share"]
            .as_array()
            .unwrap()
            .iter()
            .map(exact)
            .collect();
        assert_eq!(r.symbol_share, shares, "доли символов");
    }
    let f = &want["focus"];
    let focus = Focus {
        paths: vec![f["paths"][0].as_str().unwrap().to_string()],
        idents: vec![f["idents"][0].as_str().unwrap().to_string()],
        text: f["text"].as_str().map(str::to_string),
    };
    let restart = focus_restart(&files, &focus).unwrap();
    assert_eq!(
        Graph::new(&files),
        Graph::with_weights(&files, RankWeights::M6)
    );
    let fr = rank_on_graph(&Graph::new(&files), Some(&restart));
    assert_eq!(fr, exact(&want["focus_rank"]), "ранги с фокусом");
    let all: Vec<usize> = (0..files.len()).collect();
    let want_edges: Vec<(usize, usize, f64)> = want["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e[0].as_u64().unwrap() as usize,
                e[1].as_u64().unwrap() as usize,
                e[2].as_str().unwrap().parse().unwrap(),
            )
        })
        .collect();
    assert_eq!(edges_among(&files, &all), want_edges, "рёбра");
    assert_eq!(edges_among_with(&files, &all, RankWeights::M6), want_edges);
    // связывание меняет ранги этих входов (иначе эталон ничего не проверял бы)
    for w in [W1, W2, W12] {
        assert_ne!(rank_with(&files, None, w).file_rank, r_m6(&files), "{w:?}");
    }
}

fn r_m6(files: &[RankFile]) -> Vec<f64> {
    rank(files, None).file_rank
}

/// Класс ссылки при связывании В1 (эталон, независимо от `rank.rs`): 1 — типы, 2 — функции и
/// методы, 0 — все виды.
fn ref_class(k: RefKind, w: RankWeights) -> u8 {
    match k {
        _ if !w.bind_kind => 0,
        RefKind::Type | RefKind::Impl => 1,
        RefKind::Call => 2,
        RefKind::Import | RefKind::Other => 0,
    }
}

fn def_in_class(k: SymbolKind, class: u8) -> bool {
    match class {
        1 => matches!(
            k,
            SymbolKind::Class
                | SymbolKind::Struct
                | SymbolKind::Enum
                | SymbolKind::Interface
                | SymbolKind::Trait
                | SymbolKind::Type
        ),
        2 => matches!(k, SymbolKind::Function | SymbolKind::Method),
        _ => true,
    }
}

/// Плотный эталон связывания (M7.5 §8.6): для каждой ссылки A — класс (нет определений класса —
/// все виды), подходящие файлы (у В2 — с открытым определением; своё закрытое учитывается в `d`),
/// ребро к каждому подходящему B ≠ A весом `√c × m(n, файлов группы) / d`.
type Group = (String, u8);

fn dense_bound(files: &[RankFile], w: RankWeights) -> Vec<(usize, usize, Group, f64)> {
    let files_of = |name: &str, class: u8, open: bool| -> Vec<usize> {
        (0..files.len())
            .filter(|&i| {
                files[i].defs.iter().any(|(n, k, e)| {
                    n == name && def_in_class(*k, class) && (!open || *e != Some(false))
                })
            })
            .collect()
    };
    let mut out = Vec::new();
    for (a, f) in files.iter().enumerate() {
        let mut agg: HashMap<Group, f64> = HashMap::new();
        for (n, k, c, r) in &f.refs {
            let mut class = ref_class(*k, w);
            if class != 0 && files_of(n, class, false).is_empty() {
                class = 0;
            }
            *agg.entry((n.clone(), class)).or_default() += table_contribution(*k, *c, *r);
        }
        for ((n, class), c) in agg {
            let all = files_of(&n, class, false);
            if all.is_empty() || c == 0.0 {
                continue;
            }
            let rcv = if w.bind_export {
                files_of(&n, class, true)
            } else {
                all.clone()
            };
            let own_closed = all.contains(&a) && !rcv.contains(&a);
            let d = rcv.len() + usize::from(own_closed);
            let wt = c.sqrt() * ident_mult(&n, all.len()) / d as f64;
            for &b in rcv.iter().filter(|&&b| b != a) {
                out.push((a, b, (n.clone(), class), wt));
            }
        }
    }
    out
}

fn dense_bound_edges(files: &[RankFile], w: RankWeights) -> Vec<(usize, usize, f64)> {
    let mut sum: HashMap<(usize, usize), f64> = HashMap::new();
    for (a, b, _, wt) in dense_bound(files, w) {
        *sum.entry((a, b)).or_default() += wt;
    }
    let mut e: Vec<_> = sum.into_iter().map(|((a, b), x)| (a, b, x)).collect();
    e.sort_by_key(|e| (e.0, e.1));
    e
}

/// Плотный эталон долей при связывании: поток группы, пришедший в файл, делится поровну между
/// определениями файла, которые его принимают (вид класса, у В2 — открытые).
fn dense_bound_shares(files: &[RankFile], r: &[f64], w: RankWeights) -> Vec<Vec<f64>> {
    let named = dense_bound(files, w);
    let mut out = vec![0.0; files.len()];
    for (a, _, _, x) in &named {
        out[*a] += x;
    }
    let mut inflow: Vec<HashMap<Group, f64>> = vec![HashMap::new(); files.len()];
    for (a, b, g, x) in named {
        *inflow[b].entry(g).or_default() += r[a] * x / out[a];
    }
    files
        .iter()
        .zip(&inflow)
        .map(|(f, inf)| {
            let groups = |(n, k, e): &(String, SymbolKind, Option<bool>)| -> Vec<Group> {
                if w.bind_export && *e == Some(false) {
                    return Vec::new();
                }
                let mut g = vec![(n.clone(), 0)];
                for class in [1, 2] {
                    if w.bind_kind && def_in_class(*k, class) {
                        g.push((n.clone(), class));
                    }
                }
                g
            };
            let mut takers: HashMap<Group, f64> = HashMap::new();
            for d in &f.defs {
                for g in groups(d) {
                    *takers.entry(g).or_default() += 1.0;
                }
            }
            let fl: Vec<f64> = f
                .defs
                .iter()
                .map(|d| {
                    groups(d)
                        .iter()
                        .map(|g| inf.get(g).copied().unwrap_or(0.0) / takers[g])
                        .sum()
                })
                .collect();
            let hot: f64 = fl.iter().sum();
            let cold: f64 = f
                .defs
                .iter()
                .zip(&fl)
                .filter(|(_, x)| **x <= 0.0)
                .map(|((_, k, e), _)| kind_w(*k, *e))
                .sum();
            let (hp, cp) = match (hot > 0.0, cold > 0.0) {
                (true, true) => (0.9, 0.1),
                (true, false) => (1.0, 0.0),
                _ => (0.0, 1.0),
            };
            f.defs
                .iter()
                .zip(&fl)
                .map(|((_, k, e), x)| {
                    if *x > 0.0 {
                        hp * x / hot
                    } else {
                        cp * kind_w(*k, *e) / cold
                    }
                })
                .collect()
        })
        .collect()
}

proptest! {
    /// Факторизованный граф со связыванием (В1, В2, В1 + В2) = плотный эталон: ранги с
    /// перезапуском и без, рёбра, доли символов.
    #[test]
    fn bound_factorized_equals_dense(
        files in arb_files(),
        restart in proptest::collection::vec(0.0f64..1.0, 25),
        focused in any::<bool>(),
        variant in 0usize..3,
    ) {
        let w = [W1, W2, W12][variant];
        let n = files.len();
        let restart = focused.then(|| restart[..n].to_vec());
        let fact = rank_on_graph(&Graph::with_weights(&files, w), restart.as_deref());
        let dense = pagerank(n, &dense_bound_edges(&files, w), restart.as_deref());
        let l1: f64 = fact.iter().zip(&dense).map(|(a, b)| (a - b).abs()).sum();
        prop_assert!(l1 < 1e-9, "L1 {}", l1);
        let all: Vec<usize> = (0..n).rev().collect();
        let mut among = edges_among_with(&files, &all, w);
        among.sort_by_key(|e| (e.0, e.1));
        let de = dense_bound_edges(&files, w);
        prop_assert_eq!(among.len(), de.len(), "{:?} / {:?}", among, de);
        for (x, y) in among.iter().zip(&de) {
            prop_assert!(x.0 == y.0 && x.1 == y.1 && (x.2 - y.2).abs() < 1e-12);
        }
        let r = rank_with(&files, None, w);
        let shares = dense_bound_shares(&files, &r.file_rank, w);
        for (i, (s, d)) in r.symbol_share.iter().zip(&shares).enumerate() {
            prop_assert_eq!(s.len(), d.len());
            for (a, b) in s.iter().zip(d) {
                prop_assert!((a - b).abs() < 1e-9, "файл {}: {:?} != {:?}", i, s, d);
            }
        }
    }
}
