//! Учёт выданных токенов сессии (M4.5 §9.5, задача 12): `/recall` прибавляет размер `text`
//! по событию, хуки с локальным выводом — пачкой `POST /sessions {tokens}`; `GET /sessions/{id}`
//! отдаёт `tokens: {event: {calls, tokens}}`. Сервер без моделей.
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, api};
use serde_json::{Value, json};
use zam_server::config::ServerConfig;

fn roomy(c: &mut ServerConfig) {
    c.recall.timeout_ms = 30_000;
}

async fn recall(a: &Api, body: Value) -> Value {
    let r = a.t(Method::POST, "/api/v1/recall", Some(body)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

async fn tokens(a: &Api, session: &str) -> Value {
    let r = a
        .t(Method::GET, &format!("/api/v1/sessions/{session}"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["tokens"].clone()
}

/// Сервер с правилом: дайджест, пакеты восстановления и под задачу не пусты.
async fn seeded(name: &str) -> Api {
    let a = api(name, roomy).await;
    let r = a
        .adm(
            Method::PATCH,
            "/api/v1/admin/settings",
            Some(json!({"rules": {"agent_write": true}})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    a.note("rules", "правило.md", "# Правило\n\nВсегда пиши тесты.")
        .await;
    a
}

/// `/recall` прибавляет `tokens` ответа к событию сессии: `session_start`, `recovery` (и
/// `session_start` после `compact`), `brief`; пустой ответ — не выдача.
#[tokio::test]
async fn recall_counts_tokens_per_event() {
    let a = seeded("tokens-recall").await;
    let s = "s-tok";
    let mut want = std::collections::BTreeMap::<&str, (u64, u64)>::new();
    let mut add = |event: &'static str, j: &Value| {
        let t = j["tokens"].as_u64().unwrap();
        assert!(t > 0, "{j}");
        let e = want.entry(event).or_default();
        e.0 += 1;
        e.1 += t;
    };
    let j = recall(&a, json!({"event": "session_start", "session_id": s})).await;
    add("session_start", &j);
    let j = recall(&a, json!({"event": "session_start", "session_id": s})).await;
    add("session_start", &j);
    let j = recall(
        &a,
        json!({"event": "session_start", "session_id": s, "source": "compact"}),
    )
    .await;
    add("recovery", &j);
    let j = recall(&a, json!({"event": "recovery", "session_id": s})).await;
    add("recovery", &j);
    let j = recall(
        &a,
        json!({"event": "brief", "session_id": s, "task": "поправь сборку"}),
    )
    .await;
    add("brief", &j);
    // пустой ответ (подсказок к запросу нет) — не выдача
    let j = recall(
        &a,
        json!({"event": "prompt", "session_id": s, "prompt": "zzqx"}),
    )
    .await;
    assert_eq!(j["text"], "", "{j}");
    let got = tokens(&a, s).await;
    let want: serde_json::Map<String, Value> = want
        .into_iter()
        .map(|(e, (c, t))| (e.to_string(), json!({"calls": c, "tokens": t})))
        .collect();
    assert_eq!(got, Value::Object(want));
    // без сессии — учёта нет (и ничего не падает)
    recall(&a, json!({"event": "session_start"})).await;
}

/// `POST /sessions {tokens}`: число — одна выдача, `{calls, tokens}` — пачка; прибавляется к
/// прежнему; неизвестное событие и некорректные значения — 400.
#[tokio::test]
async fn post_sessions_local_tokens() {
    let a = api("tokens-local", |_| {}).await;
    let post = async |body: Value| a.t(Method::POST, "/api/v1/sessions", Some(body)).await;
    let r = post(json!({"id": "s-loc", "tokens": {"pre_tool": 12,
                        "post_tool_local": {"calls": 3, "tokens": 40}}}))
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = post(json!({"id": "s-loc", "tokens": {"pre_tool": {"calls": 2, "tokens": 8}}})).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        tokens(&a, "s-loc").await,
        json!({"pre_tool": {"calls": 3, "tokens": 20},
               "post_tool_local": {"calls": 3, "tokens": 40}})
    );
    for bad in [
        json!({"prompt": 5}),
        json!({"pre_tool": -1}),
        json!({"pre_tool": "5"}),
        json!({"pre_tool": {"calls": 1, "tokens": 2, "x": 3}}),
        json!({"subagent_local": 2_000_000_000u64}),
    ] {
        let r = post(json!({"id": "s-loc", "tokens": bad})).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text());
    }
    // новая сессия без учёта — пустой объект
    let r = post(json!({"id": "s-empty"})).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(tokens(&a, "s-empty").await, json!({}));
}
