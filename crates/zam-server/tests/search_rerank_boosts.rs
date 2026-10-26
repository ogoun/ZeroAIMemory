//! Бусты после серверного реранка (§9.2 п. 5, решение владельца 2026-10-26): у переставленных
//! реранкером `score = rerank_score × boost_kind × boost_area × boost_certainty`, затем деление
//! при переполнении; `rerank_score` в ответе — сырой балл реранкера. Реранкер — детерминированная
//! заглушка (`RerankSlot::install`): балл по метке в тексте фрагмента. Без моделей (FTS).

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use common::api::{Api, api};
use serde_json::{Value, json};
use zam_ml::MlError;
use zam_server::rerank::PassageScorer;

/// Заглушка реранкера: балл фрагмента — по первой подходящей метке, иначе 0,1.
struct Stub(Vec<(&'static str, f32)>);

impl PassageScorer for Stub {
    fn model_id(&self) -> &str {
        "stub-reranker"
    }

    fn score_capped(&self, _q: &str, passages: &[&str], _t: usize) -> Result<Vec<f32>, MlError> {
        Ok(passages
            .iter()
            .map(|p| {
                self.0
                    .iter()
                    .find(|(m, _)| p.contains(m))
                    .map_or(0.1, |(_, s)| *s)
            })
            .collect())
    }
}

/// Сервер с двумя выводами: `hypothesis` (метка «маркергипотезы») и `theorem` с доказательством
/// (метка «маркертеоремы»); оба находятся запросом «сборка кэша».
async fn setup(name: &str, marks: Vec<(&'static str, f32)>) -> Api {
    let a = api(name, |_| {}).await;
    a.st.rerank.install(Arc::new(Stub(marks))).await;
    for (title, body, certainty) in [
        (
            "Сборка кэша: гипотеза",
            "Сборка кэша ускоряется прогревом, маркергипотезы.",
            "hypothesis",
        ),
        (
            "Сборка кэша: теорема",
            "Сборка кэша ускоряется прогревом, маркертеоремы.",
            "theorem",
        ),
    ] {
        let mut data = json!({"kind": "discovery"});
        if certainty == "theorem" {
            data["evidence"] = json!({"checked": "сборка на CI", "method": "прогон 3 раза",
                                      "result": "воспроизводится"});
        }
        let r = a
            .t(
                Method::POST,
                "/api/v1/spaces/knowledge/entries",
                Some(
                    json!({"kind": "insight", "title": title, "body": body, "data": data,
                            "certainty": certainty, "force": true}),
                ),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    a
}

async fn search(a: &Api, body: Value) -> Value {
    let r = a.t(Method::POST, "/api/v1/search", Some(body)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

fn item<'a>(j: &'a Value, certainty: &str) -> &'a Value {
    j["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["certainty"] == certainty)
        .unwrap_or_else(|| panic!("нет {certainty}: {j}"))
}

/// Гипотеза с бо́льшим баллом реранкера (0,80 × 1,00) уступает теореме с меньшим
/// (0,75 × 1,10 = 0,825): порядок — по произведению.
#[tokio::test]
async fn rerank_respects_certainty_boost() {
    let a = setup(
        "rr-certainty",
        vec![("маркергипотезы", 0.80), ("маркертеоремы", 0.75)],
    )
    .await;
    let j = search(&a, json!({"query": "сборка кэша", "rerank": "on"})).await;
    assert_eq!(j["warnings"], json!([]), "{j}");
    let order: Vec<&str> = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["certainty"].as_str().unwrap())
        .collect();
    assert_eq!(order, ["theorem", "hypothesis"], "{j}");
    let (thm, hyp) = (item(&j, "theorem"), item(&j, "hypothesis"));
    assert_eq!(thm["rerank_score"], json!(0.75), "{j}");
    assert_eq!(thm["score"], json!(0.825), "{j}");
    assert_eq!(hyp["rerank_score"], json!(0.8), "{j}");
    assert_eq!(hyp["score"], json!(0.8), "{j}");
    // без реранка — тот же буст к слитому баллу: теорема тоже первая
    let j = search(&a, json!({"query": "сборка кэша"})).await;
    assert_eq!(j["items"][0]["certainty"], "theorem", "{j}");
    assert!(j["items"][0].get("rerank_score").is_none(), "{j}");
}

/// `rerank_score` — сырой балл реранкера, `score` — с бустами и делением при переполнении:
/// 0,95 × 1,10 = 1,045 > 1 → у лучшего `score` 1, у второго 0,5 / 1,045. Карточка `md` — с
/// баллом `score`.
#[tokio::test]
async fn rerank_score_raw_in_output() {
    let a = setup(
        "rr-raw",
        vec![("маркергипотезы", 0.50), ("маркертеоремы", 0.95)],
    )
    .await;
    let j = search(&a, json!({"query": "сборка кэша", "rerank": "on"})).await;
    let (thm, hyp) = (item(&j, "theorem"), item(&j, "hypothesis"));
    assert_eq!(j["items"][0]["certainty"], "theorem", "{j}");
    assert_eq!(thm["rerank_score"], json!(0.95), "{j}");
    assert_eq!(thm["score"], json!(1.0), "{j}");
    assert_eq!(hyp["rerank_score"], json!(0.5), "{j}");
    assert_eq!(hyp["score"], json!(0.4785), "{j}");
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "сборка кэша", "rerank": "on", "format": "md"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let md = r.text();
    assert!(md.contains("[1.00]") && md.contains("[0.48]"), "{md}");
}
