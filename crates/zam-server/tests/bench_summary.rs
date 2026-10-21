//! Замер (`#[ignore]`): `/search`, когда находится только сводка заметки с сотнями листов
//! (§9.2 п. 6 — замена сводки лучшим листом). Запуск:
//! `cargo test -p zam-server --test bench_summary -- --ignored --nocapture`.

mod common;

use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::api::{Api, api};
use common::ml::{MINILM, external, have, ml, until, vector_ready};
use common::qdrant::TestQdrant;
use serde_json::{Value, json};

/// Секций (и листов) в большой заметке.
const SECTIONS: usize = 300;
/// Слова запроса — только в первых предложениях секций 1 и 2, то есть вместе — только в L1.
const QUERY: &str = "zebraword quokkaword";

fn big_note() -> String {
    let mut s = String::from("# Handbook\n\nReference material.\n\n");
    for i in 0..SECTIONS {
        let first = match i {
            1 => "The zebraword topic begins here.".to_string(),
            2 => "The quokkaword topic begins here.".to_string(),
            _ => format!("Topic number {i} begins here."),
        };
        let filler: String = (0..150)
            .map(|k| format!("filler{} ", (i * 7 + k) % 997))
            .collect();
        s.push_str(&format!("## Section {i}\n\n{first} {filler}\n\n"));
    }
    s
}

async fn measure(a: &Api, label: &str) {
    measure_n(a, label, 1).await;
}

/// `notes` больших заметок `big/handbook*`; первым — одна из них.
async fn measure_n(a: &Api, label: &str, notes: usize) {
    let mut lat = Vec::new();
    // этапы из `timings` ответа: fts, embed, vector, fuse
    let mut stages: [Vec<f64>; 4] = Default::default();
    for _ in 0..30 {
        let t = Instant::now();
        let r = a
            .t(
                Method::POST,
                "/api/v1/search",
                Some(json!({"query": QUERY, "top_k": 10})),
            )
            .await;
        lat.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let j: Value = r.json();
        for (k, name) in ["fts_ms", "embed_ms", "vector_ms", "fuse_ms"]
            .iter()
            .enumerate()
        {
            stages[k].push(j["timings"][name].as_f64().unwrap_or(0.0));
        }
        let first = j["items"][0]["path"].as_str().unwrap_or_default();
        assert!(first.starts_with("big/handbook"), "{j}");
        assert!(j["items"].as_array().unwrap().len() >= notes.min(10), "{j}");
    }
    lat.sort_by(f64::total_cmp);
    let med = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let [fts, embed, vector, fuse] = &mut stages;
    eprintln!(
        "T8-BENCH {label}: {notes} × {SECTIONS} листов, p50 {:.1} мс, p95 {:.1} мс (p50 этапов: fts {:.2}, embed {:.2}, vector {:.2}, fuse {:.2})",
        lat[lat.len() / 2],
        lat[(lat.len() * 95 / 100).min(lat.len() - 1)],
        med(fts),
        med(embed),
        med(vector),
        med(fuse)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn bench_big_note_summary_fts_only() {
    let a = api("bench-summary-fts", |_| {}).await;
    a.note("knowledge", "big/handbook", &big_note()).await;
    measure(&a, "FTS-only").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn bench_ten_note_summaries_fts_only() {
    let a = api("bench-summary-fts10", |_| {}).await;
    let body = big_note();
    for i in 0..10 {
        a.note("knowledge", &format!("big/handbook{i}"), &body)
            .await;
    }
    measure_n(&a, "FTS-only, 10 сводок", 10).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn bench_big_note_summary_hybrid() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("bsum");
    let a = api("bench-summary-hyb", |c| {
        ml(c, MINILM);
        external(c, &q);
    })
    .await;
    vector_ready(&a).await;
    let n = a.note("knowledge", "big/handbook", &big_note()).await;
    let id = n["id"].as_str().unwrap().to_string();
    until(
        "заметка проиндексирована",
        Duration::from_secs(600),
        async || {
            let db = a.st.users.get(a.st.admin_id).await.unwrap();
            zam_store::get_node(&db, id.parse().unwrap())
                .await
                .unwrap()
                .index_state
                == "indexed"
        },
    )
    .await;
    measure(&a, "гибрид").await;
}
