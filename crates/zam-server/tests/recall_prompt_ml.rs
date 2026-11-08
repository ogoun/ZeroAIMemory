//! Порог подсказки `prompt` на гибриде (M6 §9, ревью задачи 9, M-1): векторный канал жив —
//! действует `recall.prompt_min_score`, а не порог FTS-only. Отдельный файл: модель ставит
//! счётчик токенов процесса. Без ORT/модели/Qdrant — пропуск, в CI (`ZAM_REQUIRE_ML=1`) —
//! падение.
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};
use zam_server::config::ServerConfig;

fn roomy(c: &mut ServerConfig) {
    c.recall.timeout_ms = 30_000;
}

async fn ok(a: &Api, tok: &str, _session: Option<&str>, body: Value) -> Value {
    let r: Resp = a
        .tok_h(tok, Method::POST, "/api/v1/recall", Some(body), &[])
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

async fn insight(a: &Api, tok: &str, space: &str, title: &str, body: &str, kind: &str) -> String {
    let r = a
        .tok(
            tok,
            Method::POST,
            &format!("/api/v1/spaces/{space}/entries"),
            Some(json!({"kind": "insight", "title": title, "body": body, "data": {"kind": kind}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

fn ids(j: &Value) -> Vec<String> {
    j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

/// Ревью задачи 9, M-1 (ML, MiniLM): векторный канал жив (`degraded: false`) — порог
/// подсказки `prompt` — `recall.prompt_min_score`, а `prompt_min_score_fts` не действует.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prompt_hybrid_uses_main_threshold() {
    use common::ml::{MINILM, external, have, ml, until, vector_ready};
    if !have(MINILM) {
        return;
    }
    let mut counts = Vec::new();
    for (name, min, fts) in [("hyb-low", 0.0, 1.0), ("hyb-high", 1.0, 0.0)] {
        let q = common::qdrant::TestQdrant::start(&format!("recall-{name}"));
        let a = api(&format!("recall-prompt-{name}"), |c| {
            roomy(c);
            ml(c, MINILM);
            external(c, &q);
            c.recall.prompt_min_score = min;
            c.recall.prompt_min_score_fts = Some(fts);
        })
        .await;
        vector_ready(&a).await;
        let mut ids_all = Vec::new();
        for (t, b) in [
            ("Quasar brightness", "A quasar outshines its host galaxy."),
            (
                "Pulsar timing",
                "Pulsar timing arrays detect waves; a quasar is far.",
            ),
            (
                "Telescope notes",
                "Telescope calibration before observing a quasar field.",
            ),
            (
                "Observatory log",
                "Long notes about instruments and the dome; quasar seen.",
            ),
        ] {
            ids_all.push(insight(&a, &a.token, "knowledge", t, b, "discovery").await);
        }
        let db = a.st.users.get(a.st.admin_id).await.unwrap();
        until(
            "записи проиндексированы",
            std::time::Duration::from_secs(240),
            async || {
                a.st.index.worker.notify();
                let mut all = true;
                for id in &ids_all {
                    let e = zam_store::get_entry(&db, id.parse().unwrap())
                        .await
                        .unwrap();
                    all &= e.index_state == zam_store::IndexState::Indexed;
                }
                all
            },
        )
        .await;
        let j = ok(
            &a,
            &a.token,
            None,
            json!({"event": "prompt", "prompt": "how bright is a quasar"}),
        )
        .await;
        counts.push((ids(&j).len(), j));
    }
    // порог 0 — все 3 места; порог 1 — только лучшие (балл 1,0)
    assert_eq!(counts[0].0, 3, "{}", counts[0].1);
    assert!(counts[1].0 < 3, "{}", counts[1].1);
}
