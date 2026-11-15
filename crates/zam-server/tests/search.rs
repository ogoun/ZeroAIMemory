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
    // expand=graph — с M3 (граф знаний, задача 10): принимается
    assert_eq!(
        search(&a, json!({"query": q, "mode": "chunks", "expand": "graph"}))
            .await
            .status,
        StatusCode::OK
    );
    // некорректные параметры → 400
    for bad in [
        json!({"query": q, "mode": "x"}),
        json!({"query": q, "format": "x"}),
        json!({"query": q, "expand": "x"}),
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
    assert!(t.contains("t\n   "), "{t}");
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
async fn entry_summary_replaced_by_leaf_with_most_query_words() {
    // то же для записи журнала с несколькими листами (финальная волна M3, задача 6): все три
    // слова вместе — только в L1 записи (`note`)
    let a = api("search-entry-summary", |_| {}).await;
    let filler = "текст ".repeat(80);
    let body = format!(
        "## Первая\n\nАльфаслово открывает раздел. {filler}\n\n## Вторая\n\n\
         Бетаслово и гаммаслово открывают раздел. {filler}\n"
    );
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(json!({"kind": "insight", "title": "Две части",
                        "body": body, "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = search(&a, json!({"query": "альфаслово бетаслово гаммаслово"}))
        .await
        .json();
    let items = j["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{j}");
    assert!(items[0]["entry_id"].is_string(), "{j}");
    assert_eq!(items[0]["level"], "chunk", "{j}");
    assert!(
        items[0]["heading_path"]
            .as_str()
            .unwrap()
            .ends_with("Вторая"),
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

/// Листы под H4–H6 (путь `H2 > H4`, задача 17 M3): `expand=section` отдаёт секцию H1–H3,
/// в которой лежит лист, а листы одной секции под разными H4 сливаются в один элемент.
#[tokio::test]
async fn expand_section_for_leaves_under_h4() {
    let a = api("search-expand-h4", |_| {}).await;
    let filler = |m: &str| {
        (0..150)
            .map(|i| format!("{m}{i}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let content = format!(
        "# Док\n\nВступление.\n\n## Детали\n\nНачало секции.\n\n#### Первый\n\n\
         АЛЬФАМЕТКА общеслово. {}\n\n#### Второй\n\nБЕТАМЕТКА общеслово. {}\n\n\
         ## Другое\n\nПро другое.\n",
        filler("первый"),
        filler("второй")
    );
    let node = a.note("knowledge", "глубокая", &content).await;
    let node = node["id"].as_str().unwrap();
    let body = |q: &str, extra: Value| {
        let mut b = json!({"query": q, "mode": "chunks", "top_k": 5, "budget_tokens": 20000});
        for (k, v) in extra.as_object().unwrap() {
            b[k] = v.clone();
        }
        b
    };
    // контроль: без expand — два отдельных листа; второй начинается под H4 (путь до H4)
    let j = search(&a, body("общеслово", json!({}))).await.json();
    let mut hp: Vec<&str> = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["node_id"] == node)
        .map(|i| i["heading_path"].as_str().unwrap())
        .collect();
    hp.sort();
    assert_eq!(hp, ["Док > Детали", "Док > Детали > Второй"], "{j}");

    // лист под H4 разворачивается в свою секцию H2
    let j = search(
        &a,
        body("БЕТАМЕТКА", json!({"expand": "section", "top_k": 1})),
    )
    .await
    .json();
    let sec = j["items"][0]["text"].as_str().unwrap();
    assert!(sec.starts_with("## Детали"), "{sec}");
    for m in ["Начало секции", "АЛЬФАМЕТКА", "БЕТАМЕТКА"] {
        assert!(sec.contains(m), "{m}: {sec}");
    }
    assert!(!sec.contains("Про другое"), "{sec}");

    // два листа одной секции под разными H4 — один элемент
    let j = search(&a, body("общеслово", json!({"expand": "section"})))
        .await
        .json();
    let mine: Vec<&Value> = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["node_id"] == node)
        .collect();
    assert_eq!(mine.len(), 1, "{j}");
    assert_eq!(mine[0]["chunk_ids"].as_array().unwrap().len(), 2, "{j}");
    assert!(
        mine[0]["text"].as_str().unwrap().starts_with("## Детали"),
        "{j}"
    );
}

/// `filters.insight_kinds` (§9.1, задача 13 раунд 1): insight чужих видов отсеиваются, заметки
/// и записи других видов остаются, если их включает `kinds`; неизвестный вид — 400.
#[tokio::test]
async fn insight_kinds_filter() {
    let a = api("search-insight-kinds", |_| {}).await;
    let post = |body: Value| {
        let a = &a;
        async move {
            let r = a
                .t(Method::POST, "/api/v1/spaces/knowledge/entries", Some(body))
                .await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
            r.json()["id"].as_str().unwrap().to_string()
        }
    };
    let ins = |title: &str, kind: &str| {
        json!({"kind": "insight", "title": title, "body": "квазар профилирование сборки",
               "data": {"kind": kind}, "force": true})
    };
    let gotcha = post(ins("Квазар: ловушка", "gotcha")).await;
    let failure = post(ins("Квазар: провал", "failure")).await;
    let experiment = post(ins("Квазар: эксперимент", "experiment")).await;
    let term = post(json!({"kind": "term", "title": "Квазар",
                           "data": {"definition": "квазар профилирование сборки"}}))
    .await;
    a.note(
        "knowledge",
        "Заметки/Квазар",
        "# Квазар\n\nквазар профилирование сборки\n",
    )
    .await;
    let found = |filters: Value| {
        let a = &a;
        async move {
            let r = search(
                a,
                json!({"query": "квазар профилирование", "top_k": 20, "filters": filters}),
            )
            .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            let j = r.json();
            let ids: Vec<String> = j["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| {
                    i["entry_id"]
                        .as_str()
                        .or(i["node_id"].as_str())
                        .or(i["id"].as_str())
                        .unwrap_or("")
                        .to_string()
                })
                .collect();
            let notes = j["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|i| i["kind"] == "note")
                .count();
            (ids, notes)
        }
    };
    let ik = json!(["experiment", "failure"]);
    // виды по умолчанию: gotcha отсеян, заметка и термин остались
    let (ids, notes) = found(json!({"insight_kinds": ik})).await;
    assert!(!ids.contains(&gotcha), "{ids:?}");
    assert!(
        ids.contains(&failure) && ids.contains(&experiment),
        "{ids:?}"
    );
    assert!(
        ids.contains(&term),
        "термин не insight — фильтр его не трогает: {ids:?}"
    );
    assert_eq!(notes, 1, "{ids:?}");
    // только insight — только нужные виды
    let (ids, notes) = found(json!({"kinds": ["insight"], "insight_kinds": ik})).await;
    let mut want = vec![failure.clone(), experiment.clone()];
    want.sort();
    let mut got = ids.clone();
    got.sort();
    assert_eq!(got, want);
    assert_eq!(notes, 0);
    // без фильтра — все три insight
    let (ids, _) = found(json!({"kinds": ["insight"]})).await;
    assert_eq!(ids.len(), 3, "{ids:?}");
    // неизвестный вид — 400
    let r = search(
        &a,
        json!({"query": "квазар", "filters": {"insight_kinds": ["nope"]}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

/// Идея Knowledge токеном администратора (M5.7).
async fn idea(a: &Api, title: &str) -> String {
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(
                json!({"kind": "idea", "title": title, "body": "Замысел: гиперкуб в отчётах.",
                        "data": {"origin": "human"}}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

/// Виды элементов выдачи.
fn kinds(j: &Value) -> Vec<String> {
    j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap_or("").to_string())
        .collect()
}

/// M5.7 §4: у токена идеи — только явным `filters.kinds` (как task); статусы по умолчанию —
/// `new, proposed, deferred, accepted`.
#[tokio::test]
async fn search_token_idea_only_explicit_kinds() {
    let a = api("search-idea-token", |_| {}).await;
    let id = idea(&a, "Гиперкуб отчётов").await;
    let r = search(&a, json!({"query": "гиперкуб"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!r.text().contains(&id), "без kinds идей нет: {}", r.text());
    let r = search(
        &a,
        json!({"query": "гиперкуб", "filters": {"kinds": ["idea"]}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert!(r.text().contains(&id), "{j}");
    assert_eq!(kinds(&j), ["idea"], "{j}");
    // отказ по умолчанию не находится, явным статусом — находится
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{id}/decide"),
            Some(json!({"decision": "decline", "by": "human"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = search(
        &a,
        json!({"query": "гиперкуб", "filters": {"kinds": ["idea"]}}),
    )
    .await;
    assert!(!r.text().contains(&id), "{}", r.text());
    let r = search(
        &a,
        json!({"query": "гиперкуб", "filters": {"kinds": ["idea"], "status": ["declined"]}}),
    )
    .await;
    assert!(r.text().contains(&id), "{}", r.text());
}

/// Ctrl+K WebUI находит идею умолчаниями видов.
#[tokio::test]
async fn search_webui_default_finds_idea() {
    let a = api("search-idea-webui", |_| {}).await;
    let id = idea(&a, "Гиперкуб для WebUI").await;
    let r = a
        .adm(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "гиперкуб"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains(&id), "{}", r.text());
    assert!(kinds(&r.json()).contains(&"idea".to_string()));
}

/// Ключ кэша выжимок при умолчаниях M5.6 — как до этапа (I9, Review Focus 5): вид `idea` и
/// статусы идей в ключ не входят ни у токена, ни у WebUI (у WebUI `idea` — в умолчаниях).
#[tokio::test]
async fn scope_key_ignores_idea_kind_and_statuses() {
    let a = api("search-idea-scope", |_| {}).await;
    corpus(&a).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(
                json!({"kind": "insight", "title": "Источник выжимки", "body": "Вывод.",
                        "data": {"kind": "discovery"}}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let src = r.json()["id"].as_str().unwrap().to_string();
    let r = a
        .t(
            Method::POST,
            "/api/v1/cache",
            Some(
                json!({"query": "итог", "summary": "Кратко.", "solution": GUID,
                        "sources": [{"id": format!("i:{src}")}]}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let cid = r.json()["id"].as_str().unwrap().to_string();
    // фикстура — ключ M5.6 (до вида idea)
    let want = zam_store::scope_hash(&json!({
        "solution": GUID,
        "include_knowledge": true,
        "include_rules": true,
        "path_prefix": null,
        "area": null,
        "filters": {
            "kinds": ["attachment", "changelog", "insight", "note", "question", "release", "term"],
            "status": ["active", "answered", "revisit"],
            "certainty": [],
        },
    }));
    let raw = cid.strip_prefix("c_").unwrap_or(&cid).to_string();
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let got: String = db
        .read(move |c| {
            Ok(c.query_row(
                "SELECT scope_hash FROM answer_cache WHERE id = ?1",
                [&raw],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(got, want);
    // выжимку находит и агент, и поиск WebUI с умолчаниями (там есть idea и report)
    let body = json!({"query": "итог", "scope": {"solution": GUID}});
    let r = search(&a, body.clone()).await;
    assert!(r.text().contains(&cid), "агент: {}", r.text());
    let r = a
        .adm(Method::POST, "/api/v1/search", Some(body), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains(&cid), "WebUI: {}", r.text());
}

// ---- карточки v2 и честный бюджет (M6 §4.1–§4.2, задача 5) ----

/// 30 заметок про сборку с многобайтовыми заголовками и длинным текстом.
async fn budget_corpus(a: &Api) {
    for i in 0..30 {
        let body = format!(
            "# Сборка проекта, этап {i} — «кириллица» и ёлочки\n\n## Причина\n\n{}\n",
            "Скрипт сборки падает, если переменная окружения не задана; проверяй сборку заранее. "
                .repeat(8)
        );
        a.note("knowledge", &format!("Сборка/Этап {i:02} — ёлки"), &body)
            .await;
    }
}

/// Markdown поиска вместе со строкой MCP `~N tok` — в токенах счётчика сервера.
fn md_cost(md: &str) -> usize {
    let n = zam_store::count_tokens(md);
    zam_store::count_tokens(&format!("{md}~{n} tok"))
}

/// Номера карточек Markdown (`<n>. `) по порядку.
fn card_numbers(md: &str) -> Vec<usize> {
    md.lines()
        .filter_map(|l| l.split_once(". ").and_then(|(n, _)| n.parse().ok()))
        .collect()
}

/// Ответ `brief` в Markdown с `~N tok` MCP укладывается в бюджет (100, 400, 800, 2000).
#[tokio::test]
async fn brief_markdown_within_budget() {
    let a = api("search-md-budget", |_| {}).await;
    budget_corpus(&a).await;
    let mut cards = Vec::new();
    for budget in [100usize, 400, 800, 2000] {
        for mode in ["brief", "notes"] {
            let md = search(
                &a,
                json!({"query": "сборка скрипт", "format": "md", "mode": mode, "top_k": 30,
                       "budget_tokens": budget}),
            )
            .await
            .text();
            let cost = md_cost(&md);
            // первая карточка отдаётся всегда: дороже бюджета — только она одна, без фрагмента
            let only_head =
                card_numbers(&md) == [1] && md.lines().nth(1).is_some_and(|l| l.starts_with("> "));
            assert!(
                cost <= budget || only_head,
                "{mode} {budget}: {cost} токенов\n{md}"
            );
            if budget >= 400 {
                assert!(cost <= budget, "{mode} {budget}: {cost} токенов\n{md}");
            }
            if mode == "brief" {
                cards.push(card_numbers(&md).len());
            }
        }
    }
    // бюджет действует: больше бюджет — больше карточек, а при 2000 их несколько
    assert!(cards.windows(2).all(|w| w[0] <= w[1]), "{cards:?}");
    assert!(cards[3] > cards[1] && cards[1] > 1, "{cards:?}");
    // JSON — тот же отбор
    let j = search(
        &a,
        json!({"query": "сборка скрипт", "mode": "brief", "top_k": 30, "budget_tokens": 400}),
    )
    .await
    .json();
    assert_eq!(j["items"].as_array().unwrap().len(), cards[1], "{j}");
}

/// Края бюджета: 1 и строка карточки длиннее бюджета — только первая карточка, фрагмент обрезан.
#[tokio::test]
async fn brief_budget_edges_first_card_only() {
    let a = api("search-md-edges", |_| {}).await;
    budget_corpus(&a).await;
    for budget in [1usize, 20] {
        let md = search(
            &a,
            json!({"query": "сборка скрипт", "format": "md", "budget_tokens": budget}),
        )
        .await
        .text();
        assert_eq!(card_numbers(&md), [1], "{budget}: {md}");
        assert!(md.starts_with("1. n:"), "{md}");
        let j = search(
            &a,
            json!({"query": "сборка скрипт", "budget_tokens": budget}),
        )
        .await
        .json();
        assert_eq!(j["items"].as_array().unwrap().len(), 1, "{j}");
    }
}

/// Фрагмент первой карточки режется по границе символа: он — начало полного фрагмента.
#[tokio::test]
async fn snippet_cut_on_char_boundary() {
    let a = api("search-md-cut", |_| {}).await;
    budget_corpus(&a).await;
    let snippet = |md: &str| -> String {
        md.lines()
            .nth(1)
            .filter(|l| l.starts_with("   "))
            .unwrap_or("")
            .trim()
            .replace("**", "")
            .trim_end_matches('…')
            .to_string()
    };
    let full = search(
        &a,
        json!({"query": "сборка скрипт", "format": "md", "budget_tokens": 2000}),
    )
    .await
    .text();
    let cut = search(
        &a,
        json!({"query": "сборка скрипт", "format": "md", "budget_tokens": 170}),
    )
    .await
    .text();
    let (f, c) = (snippet(&full), snippet(&cut));
    assert!(!f.is_empty() && !c.is_empty(), "{full}\n{cut}");
    assert_eq!(card_numbers(&cut), [1], "{cut}");
    assert!(c.chars().count() < f.chars().count(), "не обрезан:\n{cut}");
    assert!(f.starts_with(&c), "«{c}» не начало «{f}»");
}

/// Строка карточки §4.1: полный id, хвост `[.NN] rN · yyyy-MM-dd HH:mm:ss · Nt`, без «дн. назад».
#[tokio::test]
async fn card_v2_tail_format() {
    let a = api("search-card-v2", |_| {}).await;
    let n = a
        .note(
            "knowledge",
            "Инструменты/Bash",
            "# Bash\n\n## Причина\n\nСкрипт с CRLF падает в bash.\n",
        )
        .await;
    let id = n["id"].as_str().unwrap();
    let md = search(&a, json!({"query": "CRLF bash", "format": "md"}))
        .await
        .text();
    let first = md.lines().next().unwrap();
    let head = format!("1. n:{id} Knowledge/Инструменты/Bash › Bash > Причина  [");
    assert!(first.starts_with(&head), "{first}");
    let tail = first.strip_prefix(&head).unwrap();
    // `.NN] r1 · 2026-11-05 12:34:56 · 12t` (балл 1.00 у единственного — `1.0`)
    let (score, rest) = tail.split_once("] ").unwrap();
    assert!(
        score == "1.0" || (score.len() == 3 && score.starts_with('.')),
        "{first}"
    );
    let parts: Vec<&str> = rest.split(" · ").collect();
    assert_eq!(parts.len(), 3, "{first}");
    assert_eq!(parts[0], "r1");
    let date = parts[1];
    assert_eq!(date.len(), 19, "{first}");
    assert!(
        jiff::civil::DateTime::strptime("%Y-%m-%d %H:%M:%S", date).is_ok(),
        "{first}"
    );
    assert!(parts[2].ends_with('t') && parts[2][..parts[2].len() - 1].parse::<u32>().is_ok());
    assert!(!md.contains("дн. назад") && !md.contains("сегодня"), "{md}");
    assert!(!md.contains(" tok"), "{md}");
    // JSON — `age_days` остаётся
    let j = search(&a, json!({"query": "CRLF bash"})).await.json();
    assert_eq!(j["items"][0]["age_days"], 0, "{j}");
}

/// `search.brief_min_rel`: карточки с баллом ниже `r × лучший` не выдаются (и при явном top_k);
/// `chunks` отсечка не касается. Без модели баллы — ранги RRF одного канала (лучший 1,0, дальше
/// 0,98, 0,97…), поэтому порог 0,99 оставляет только лучших.
#[tokio::test]
async fn brief_min_rel_drops_weak() {
    let body =
        |mode: &str| json!({"query": "квазар", "mode": mode, "top_k": 30, "budget_tokens": 20000});
    let fill = async |a: &Api| {
        for i in 0..10 {
            a.note(
                "knowledge",
                &format!("Квазар {i}"),
                &format!(
                    "# Квазар {i}\n\nквазар {}\n",
                    "длинное пояснение без нужных слов ".repeat(1 + i * 10)
                ),
            )
            .await;
        }
    };
    let off = api("search-min-rel-off", |c| c.search.brief_min_rel = 0.0).await;
    fill(&off).await;
    let all = search(&off, body("brief")).await.json();
    let all = all["items"].as_array().unwrap().clone();
    assert!(all.len() >= 5, "{all:?}");
    let on = api("search-min-rel-on", |c| c.search.brief_min_rel = 0.99).await;
    fill(&on).await;
    let kept = search(&on, body("brief")).await.json();
    let kept = kept["items"].as_array().unwrap().clone();
    let best = kept[0]["score"].as_f64().unwrap();
    assert!(
        kept.iter()
            .all(|i| i["score"].as_f64().unwrap() >= 0.99 * best - 1e-4),
        "{kept:?}"
    );
    let weak = all
        .iter()
        .filter(|i| i["score"].as_f64().unwrap() < 0.99 * all[0]["score"].as_f64().unwrap())
        .count();
    assert!(weak > 0, "набор без слабых карточек: {all:?}");
    assert_eq!(kept.len() + weak, all.len());
    // notes — так же, chunks — без отсечки
    let notes = search(&on, body("notes")).await.json();
    assert_eq!(
        notes["items"].as_array().unwrap().len(),
        kept.len(),
        "{notes}"
    );
    let ch = search(&on, body("chunks")).await.json();
    assert_eq!(ch["items"].as_array().unwrap().len(), all.len(), "{ch}");
}
