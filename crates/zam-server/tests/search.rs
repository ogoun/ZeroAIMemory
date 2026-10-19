//! `POST /search` без моделей (FTS-only, деградация): область, Human, режимы, бюджет,
//! форматы, синтаксис FTS5. Гибрид с векторами — `hybrid.rs`.
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
    // без ORT и моделей векторный канал выключен: причина из DegradedReason
    let reason = j["degraded_reason"].as_str().unwrap();
    assert!(["no_runtime", "no_model"].contains(&reason), "{j}");
    // Knowledge и Rules (решение не задано): по одной заметке, обе ждут векторов
    assert_eq!(j["pending_count"], 2, "{j}");
    assert!(j["timings"]["fts_ms"].is_number(), "{j}");
    assert!(j["timings"]["fuse_ms"].is_number(), "{j}");
    // векторный канал не запускался
    assert!(j["timings"]["embed_ms"].is_null() && j["timings"]["vector_ms"].is_null());
    let items = j["items"].as_array().unwrap();
    let it = &items[0];
    assert_eq!(it["path"], "Инструменты/Bash скрипты");
    assert_eq!(it["kind"], "note");
    assert_eq!(it["space"]["kind"], "knowledge");
    assert_eq!(it["rev"], 1);
    assert!(it["node_id"].is_string() && it["updated_at"].is_string());
    assert!(it["score"].is_number());
    assert_eq!(it["sources"], json!(["fts"]), "{it}");
    assert_eq!(it["level"], "chunk", "{it}");
    assert!(it["tokens"].as_u64().unwrap() > 0, "{it}");
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
        json!({"query": q, "expand": "x"}),
        // граф знаний — M3
        json!({"query": q, "mode": "chunks", "expand": "graph"}),
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
    // бюджет в токенах (без модели — оценка ~3,5 символа на токен)
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
    // карточка §9.3 и строка деградации в конце
    assert!(t.starts_with("1. n:"), "{t}");
    assert!(t.contains(" Knowledge/Инструменты/Bash скрипты › "), "{t}");
    assert!(t.contains(" tok\n   "), "{t}");
    assert!(t.contains("**"), "{t}");
    assert!(!t.contains('\u{E000}'), "{t}");
    assert!(t.trim_end().ends_with("(только FTS)"), "{t}");
}

#[tokio::test]
async fn summary_replaced_by_leaf_with_most_query_words() {
    // FTS-only: все три слова вместе — только в L1 заметки; лист «Вторая» содержит два
    // из них, «Первая» — одно
    let a = api("search-summary", |_| {}).await;
    let filler = "текст ".repeat(80);
    a.note(
        "knowledge",
        "две",
        &format!(
            "# Первая\n\nАльфаслово открывает раздел. {filler}\n\n# Вторая\n\n\
             Бетаслово и гаммаслово открывают раздел. {filler}\n"
        ),
    )
    .await;
    let j = search(&a, json!({"query": "альфаслово бетаслово гаммаслово"}))
        .await
        .json();
    let items = j["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{j}");
    assert_eq!(items[0]["level"], "chunk", "{j}");
    assert_eq!(items[0]["heading_path"], "Вторая", "{j}");
    // сниппет листа подсвечивает слово запроса
    assert!(
        items[0]["snippet"].as_str().unwrap().contains('\u{E000}'),
        "{j}"
    );
}

#[tokio::test]
async fn md_empty_result_still_reports_degraded() {
    let a = api("search-md-empty", |_| {}).await;
    corpus(&a).await;
    let r = search(&a, json!({"query": "несуществующееслово", "format": "md"})).await;
    assert_eq!(r.status, StatusCode::OK);
    let t = r.text();
    assert!(
        t.starts_with("> degraded: ") && t.trim_end().ends_with("(только FTS)"),
        "{t}"
    );
    assert_eq!(t.lines().count(), 1, "{t}");
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

#[tokio::test]
async fn webui_brief_budget_fits_top_k_20() {
    // параметры поиска WebUI (web/src/lib/api.ts): brief, top_k 20, 4000 токенов
    let a = api("search-webui", |_| {}).await;
    let long = "Очень длинное имя папки с пояснением".repeat(3);
    for i in 0..25 {
        let body = format!(
            "# Раздел с длинным заголовком номер {i} про сборку проекта\n\n{}\n",
            "общий текст про сборку и скрипты ".repeat(30)
        );
        a.note("knowledge", &format!("{long}/Заметка {i}"), &body)
            .await;
    }
    let r = search(
        &a,
        json!({"query": "сборку", "scope": {"spaces": ["knowledge"]},
               "mode": "brief", "top_k": 20, "budget_tokens": 4000}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["items"].as_array().unwrap().len(), 20);
}

#[tokio::test]
async fn rerank_on_without_model_warns_and_keeps_order() {
    let a = api("search-rerank", |_| {}).await;
    corpus(&a).await;
    let base = search(&a, json!({"query": "скрипты"})).await.json();
    assert_eq!(base["warnings"], json!([]), "{base}");
    assert!(base["timings"]["rerank_ms"].is_null(), "{base}");
    let r = search(&a, json!({"query": "скрипты", "rerank": "on"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["warnings"], json!(["reranker not installed"]), "{j}");
    assert!(j["timings"]["rerank_ms"].is_number(), "{j}");
    // без реранкера — порядок слияния, баллов реранка нет
    assert_eq!(paths(&j), paths(&base));
    assert!(
        j["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i.get("rerank_score").is_none()),
        "{j}"
    );
    let t = search(
        &a,
        json!({"query": "скрипты", "rerank": "on", "format": "md"}),
    )
    .await
    .text();
    assert!(
        t.trim_end().ends_with("> warning: reranker not installed"),
        "{t}"
    );
    // rerank=off — как без поля
    let off = search(&a, json!({"query": "скрипты", "rerank": "off"}))
        .await
        .json();
    assert_eq!(off["warnings"], json!([]));
    let r = search(&a, json!({"query": "скрипты", "rerank": "maybe"})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}
