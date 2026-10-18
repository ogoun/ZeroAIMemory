//! `POST /search` (FTS, M1): область, Human, режимы, бюджет, форматы, синтаксис FTS5.
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};

const GUID: &str = "0b3c2a8e-6f1d-4c5e-9a7b-1d2e3f405162";

async fn search(a: &Api, body: Value) -> Resp {
    a.t(Method::POST, "/api/v1/search", Some(body)).await
}

fn paths(j: &Value) -> Vec<String> {
    j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["path"].as_str().unwrap().to_string())
        .collect()
}

/// Knowledge, Rules, решение и Human (через cookie администратора).
async fn corpus(a: &Api) {
    a.note(
        "knowledge",
        "Инструменты/Bash скрипты",
        "# Bash скрипты\n\n## Windows\n\nПиши скрипты с `set -euo pipefail`, пути через cygpath.\n",
    )
    .await;
    // Rules по токену закрыты для записи (rules.agent_write = false) — через cookie
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "rules", "path": "Правила", "kind": "note",
                        "content": "# Правила\n\nНикогда не коммить секреты в скрипты.\n"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"id": GUID, "name": "Проект"})),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    a.note(
        GUID,
        "Сборка",
        "# Сборка\n\nСкрипты сборки лежат в xtask.\n",
    )
    .await;
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "human", "path": "Дневник",
                        "kind": "note", "content": "# Дневник\n\nЛичные скрипты для дома.\n"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

#[tokio::test]
async fn finds_note_and_reports_degraded() {
    let a = api("search-basic", |_| {}).await;
    corpus(&a).await;
    let r = search(&a, json!({"query": "bash скрипты"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["degraded"], true);
    assert_eq!(j["degraded_reason"], "vector index not available (M1)");
    // Knowledge и Rules (решение не задано): по одной заметке, обе ждут векторов
    assert_eq!(j["pending_count"], 2, "{j}");
    assert!(j["timings"]["fts_ms"].is_number(), "{j}");
    let items = j["items"].as_array().unwrap();
    let it = &items[0];
    assert_eq!(it["path"], "Инструменты/Bash скрипты");
    assert_eq!(it["kind"], "note");
    assert_eq!(it["space"]["kind"], "knowledge");
    assert_eq!(it["rev"], 1);
    assert!(it["node_id"].is_string() && it["updated_at"].is_string());
    assert!(it["score"].is_number());
    assert!(!it["chunk_ids"].as_array().unwrap().is_empty());
    let snip = it["snippet"].as_str().unwrap();
    assert!(
        snip.contains('\u{E000}') && snip.contains('\u{E001}'),
        "{snip}"
    );
    // brief — без текста
    assert!(it.get("text").is_none(), "{it}");
}

#[tokio::test]
async fn human_only_for_admin_with_include_human() {
    let a = api("search-human", |_| {}).await;
    corpus(&a).await;
    // по токену Human не виден даже с include_human и явным spaces
    for body in [
        json!({"query": "личные скрипты"}),
        json!({"query": "личные скрипты", "scope": {"include_human": true}}),
        json!({"query": "личные скрипты", "scope": {"spaces": ["human", "knowledge"], "include_human": true}}),
    ] {
        let j = search(&a, body).await.json();
        assert!(!paths(&j).contains(&"Дневник".to_string()), "{j}");
    }
    // администратор без include_human — тоже нет
    let r = a
        .adm(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "личные скрипты"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!paths(&r.json()).contains(&"Дневник".to_string()));
    // с include_human — находит
    let r = a
        .adm(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "личные скрипты", "scope": {"include_human": true}})),
            None,
        )
        .await;
    let j = r.json();
    assert_eq!(paths(&j)[0], "Дневник", "{j}");
    assert_eq!(j["items"][0]["space"]["kind"], "human");
}

#[tokio::test]
async fn scope_defaults_solution_and_flags() {
    let a = api("search-scope", |_| {}).await;
    corpus(&a).await;
    let q = "скрипты";
    // по умолчанию: Knowledge и Rules, без решений
    let p = paths(&search(&a, json!({"query": q})).await.json());
    assert!(p.contains(&"Инструменты/Bash скрипты".to_string()), "{p:?}");
    assert!(p.contains(&"Правила".to_string()), "{p:?}");
    assert!(!p.contains(&"Сборка".to_string()), "{p:?}");
    // all_solutions
    let p = paths(
        &search(&a, json!({"query": q, "scope": {"all_solutions": true}}))
            .await
            .json(),
    );
    assert!(p.contains(&"Сборка".to_string()), "{p:?}");
    // решение из заголовка X-Zam-Solution, без Rules
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": q, "scope": {"include_rules": false}})),
            &[("x-zam-solution", &GUID.to_uppercase())],
        )
        .await;
    let p = paths(&r.json());
    assert!(p.contains(&"Сборка".to_string()), "{p:?}");
    assert!(!p.contains(&"Правила".to_string()), "{p:?}");
    assert!(p.contains(&"Инструменты/Bash скрипты".to_string()), "{p:?}");
    // только решение
    let p = paths(
        &search(
            &a,
            json!({"query": q, "scope": {"solution": GUID, "include_knowledge": false, "include_rules": false}}),
        )
        .await
        .json(),
    );
    assert_eq!(p, vec!["Сборка".to_string()]);
    // неизвестное решение — пустая выдача, не 404
    let r = search(
        &a,
        json!({"query": q, "scope": {"solution": "11111111-2222-3333-4444-555555555555",
                                     "include_knowledge": false, "include_rules": false}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["items"].as_array().unwrap().is_empty());
    // явный список пространств и префикс пути
    let p = paths(
        &search(
            &a,
            json!({"query": q, "scope": {"spaces": ["knowledge"], "path_prefix": "Инструменты"}}),
        )
        .await
        .json(),
    );
    assert_eq!(p, vec!["Инструменты/Bash скрипты".to_string()]);
    // некорректные параметры → 400
    for bad in [
        json!({"query": q, "mode": "x"}),
        json!({"query": q, "format": "x"}),
        json!({"query": q, "top_k": 0}),
        json!({"query": q, "scope": {"solution": "nope"}}),
        json!({"nope": 1}),
    ] {
        assert_eq!(
            search(&a, bad.clone()).await.status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
}

#[tokio::test]
async fn modes_budget_and_md_format() {
    let a = api("search-modes", |_| {}).await;
    corpus(&a).await;
    let long = format!("# Длинная\n\nскрипты {}\n", "слово ".repeat(2000));
    a.note("knowledge", "Длинная", &long).await;

    // chunks: текст есть и урезан бюджетом
    let j = search(
        &a,
        json!({"query": "скрипты", "mode": "chunks", "budget_tokens": 50}),
    )
    .await
    .json();
    let items = j["items"].as_array().unwrap();
    assert!(!items.is_empty());
    let total: usize = items
        .iter()
        .map(|i| i["text"].as_str().unwrap().chars().count())
        .sum();
    assert!(total <= 50 * 4, "{total}");
    // бюджет 0 — хотя бы один элемент
    let j = search(
        &a,
        json!({"query": "скрипты", "mode": "chunks", "budget_tokens": 0}),
    )
    .await
    .json();
    assert_eq!(j["items"].as_array().unwrap().len(), 1, "{j}");

    // notes: по одному на узел, l0 вместо snippet
    let j = search(&a, json!({"query": "скрипты", "mode": "notes"}))
        .await
        .json();
    let items = j["items"].as_array().unwrap();
    let mut ids: Vec<&str> = items
        .iter()
        .map(|i| i["node_id"].as_str().unwrap())
        .collect();
    let n = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), n);
    assert!(
        items[0].get("l0").is_some() && items[0].get("snippet").is_none(),
        "{j}"
    );

    // md
    let r = search(&a, json!({"query": "cygpath", "format": "md"})).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.header("content-type").starts_with("text/markdown"));
    let t = r.text();
    assert!(
        t.starts_with("- [Knowledge/Инструменты/Bash скрипты › "),
        "{t}"
    );
    assert!(t.contains("**"), "{t}");
    assert!(!t.contains('\u{E000}'), "{t}");
}

#[tokio::test]
async fn fts_syntax_is_plain_words() {
    let a = api("search-syntax", |_| {}).await;
    corpus(&a).await;
    for q in [
        r#""bash"*"#,
        "bash AND -windows",
        "title:скрипты",
        "(bash OR",
        "NEAR(bash скрипты)",
        "\"",
        "*",
        "-",
        "",
    ] {
        let r = search(&a, json!({ "query": q })).await;
        assert_eq!(r.status, StatusCode::OK, "{q}: {}", r.text());
    }
    let j = search(&a, json!({"query": "NEAR(bash скрипты)"}))
        .await
        .json();
    assert_eq!(paths(&j)[0], "Инструменты/Bash скрипты");
    // слишком длинный запрос → 400
    let r = search(&a, json!({ "query": "а".repeat(513) })).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn pending_count_counts_searched_spaces_only() {
    let a = api("search-pending", |_| {}).await;
    corpus(&a).await;
    a.note("knowledge", "Без векторов", "---\nindex: false\n---\n# Б\n")
        .await;
    // явные пространства: решение — одна заметка
    let r = search(&a, json!({"query": "скрипты", "scope": {"spaces": [GUID]}})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["pending_count"], 1);
    // Knowledge + Rules + решение; index: false и Human не считаются
    let r = search(&a, json!({"query": "скрипты", "scope": {"solution": GUID}})).await;
    assert_eq!(r.json()["pending_count"], 3, "{}", r.text());
    // префикс пути
    let r = search(
        &a,
        json!({"query": "скрипты", "scope": {"path_prefix": "Инструменты"}}),
    )
    .await;
    assert_eq!(r.json()["pending_count"], 1, "{}", r.text());
}
