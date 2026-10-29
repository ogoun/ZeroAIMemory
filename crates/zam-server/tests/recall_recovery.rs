//! Пакет восстановления после сжатия контекста (M4.5 §9.2, задача 9): `event: recovery` и
//! `session_start` с `source: compact | resume` — задачи (захваченные сессией первыми), ответы
//! человека без фильтра показанного, решения за 7 дней (до 5), затем обычный дайджест в
//! бюджете `recall.recovery_tokens`; выключенный `hooks.recovery` — обычный дайджест; MCP
//! `memory_recall {event: "recovery"}`. Сервер без моделей (поиск — FTS).
mod common;

use axum::body::Body;
use axum::http::{Method, StatusCode, header};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};
use zam_server::config::ServerConfig;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e93";
const KN: &str = "/api/v1/spaces/knowledge/entries";
const RECALL: &str = "/api/v1/recall";

fn roomy(c: &mut ServerConfig) {
    c.recall.timeout_ms = 30_000;
}

fn id(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("нет id: {v}"))
        .to_string()
}

async fn created(r: Resp) -> String {
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    id(&r.json())
}

/// Сервер с правилом, решением `G`, задачами (одна захвачена сессией `s1`), отвеченным вопросом
/// и решениями `decision` в пространстве решения; `(api, задачи, вопрос, решения)`; захвачена
/// вторая задача («Моя задача»).
async fn seeded(name: &str) -> (Api, Vec<String>, String, Vec<String>) {
    let a = api(&format!("recovery-{name}"), roomy).await;
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
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Восстановление"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let mut tasks = Vec::new();
    for title in ["Чужая задача", "Моя задача"] {
        let t = created(
            a.t(
                Method::POST,
                KN,
                Some(json!({"kind": "task", "title": title,
                            "data": {"next_step": format!("шаг: {title}")}})),
            )
            .await,
        )
        .await;
        let r = a
            .tok_rev(
                &a.token,
                Method::PATCH,
                &format!("/api/v1/entries/{t}"),
                Some(json!({"status": "in_progress"})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        tasks.push(t);
    }
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &format!("/api/v1/entries/{}/claim", tasks[1]),
            Some(json!({})),
            &[("x-zam-session", "s1")],
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let q = created(
        a.t(
            Method::POST,
            KN,
            Some(
                json!({"kind": "question", "title": "Какой формат логов?", "body": "к",
                        "data": {"options": [{"key": "a", "text": "JSON"},
                                             {"key": "b", "text": "Текст"}]}}),
            ),
        )
        .await,
    )
    .await;
    let r = a
        .adm(
            Method::POST,
            &format!("/api/v1/entries/{q}/answer"),
            Some(json!({"option": "a"})),
            None,
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let mut decisions = Vec::new();
    for i in 0..7 {
        decisions.push(
            created(
                a.t(
                    Method::POST,
                    &format!("/api/v1/spaces/{G}/entries"),
                    Some(
                        json!({"kind": "insight", "title": format!("Решение номер {i}"),
                                "body": "обоснование", "data": {"kind": "decision"}}),
                    ),
                )
                .await,
            )
            .await,
        );
    }
    (a, tasks, q, decisions)
}

async fn recall(a: &Api, session: &str, body: Value) -> Value {
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            RECALL,
            Some(body),
            &[("x-zam-session", session)],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

fn text(j: &Value) -> &str {
    j["text"].as_str().unwrap()
}

fn pos(t: &str, needle: &str) -> usize {
    t.find(needle)
        .unwrap_or_else(|| panic!("нет «{needle}» в\n{t}"))
}

/// Порядок разделов пакета: задачи (захваченная первой) → ответы → решения (до 5, новые первыми)
/// → обычный дайджест (уведомления, правила); бюджет по умолчанию — `recovery_tokens`, меньший
/// бюджет запроса соблюдается, решения старше 7 дней не входят.
#[tokio::test]
async fn recovery_package_order_and_budget() {
    let (a, tasks, q, decisions) = seeded("order").await;
    let j = recall(
        &a,
        "s1",
        json!({"event": "recovery", "solution": G, "agent_block_version": 1}),
    )
    .await;
    let t = text(&j);
    assert!(pos(t, "### Задачи") < pos(t, "### Ответы человека"), "{t}");
    assert!(pos(t, "### Ответы человека") < pos(t, "### Решения"), "{t}");
    assert!(pos(t, "### Решения") < pos(t, "устарел"), "{t}");
    assert!(pos(t, "устарел") < pos(t, "### Правила"), "{t}");
    assert!(pos(t, "Моя задача") < pos(t, "Чужая задача"), "{t}");
    assert!(t.contains(&format!("q:{q}")), "{t}");
    // до 5 решений, новые первыми
    assert_eq!(t.matches("Решение номер").count(), 5, "{t}");
    assert!(pos(t, "Решение номер 6") < pos(t, "Решение номер 2"), "{t}");
    assert!(!t.contains("Решение номер 1") && !t.contains("Решение номер 0"));
    let items = j["items"].as_array().unwrap();
    let first = &items[0];
    assert_eq!(first["id"], format!("k:{}", tasks[1]), "{j}");
    let dec: Vec<&Value> = items.iter().filter(|i| i["reason"] == "decision").collect();
    assert_eq!(dec.len(), 5, "{j}");
    assert_eq!(dec[0]["id"], format!("i:{}", decisions[6]));
    assert!(j.get("hooks").is_none(), "{j}");
    let tokens = j["tokens"].as_u64().unwrap();
    assert!(tokens > 0 && tokens <= 2000, "{tokens}");

    // меньший бюджет запроса: первыми остаются задачи
    let j = recall(
        &a,
        "s1",
        json!({"event": "recovery", "solution": G, "budget_tokens": 80}),
    )
    .await;
    let t = text(&j);
    assert!(j["tokens"].as_u64().unwrap() <= 80, "{t}");
    assert!(
        t.starts_with("### Задачи\nZeroAIMemory: [in_progress] Моя задача"),
        "{t}"
    );

    // решения старше 7 дней — нет
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let old = zam_store::days_ago(jiff::Timestamp::now(), 8);
    db.write(move |tx| {
        Ok(tx.execute(
            "UPDATE entries SET created_at = ?1 WHERE kind = 'insight'
             AND json_extract(data, '$.kind') = 'decision'",
            [zam_store::time::fmt_ts(old)],
        )?)
    })
    .await
    .unwrap();
    let j = recall(&a, "s1", json!({"event": "recovery", "solution": G})).await;
    let t = text(&j);
    assert!(!t.contains("### Решения"), "{t}");
    // ответ на вопрос — тоже decision, но в Knowledge: раздел решений его не берёт
    assert!(t.contains("### Ответы человека"), "{t}");
}

/// `session_start` с `source: compact | resume` — тот же пакет; `startup` и `clear` — обычный
/// дайджест (без раздела решений, уведомления — первыми).
#[tokio::test]
async fn session_start_compact_is_recovery() {
    let (a, _, _, _) = seeded("compact").await;
    for src in ["compact", "resume"] {
        let j = recall(
            &a,
            "s1",
            json!({"event": "session_start", "solution": G, "source": src,
                   "agent_block_version": 1}),
        )
        .await;
        let t = text(&j);
        assert!(t.contains("### Решения"), "{src}: {t}");
        assert!(pos(t, "### Решения") < pos(t, "устарел"), "{src}: {t}");
        assert!(j["hooks"].is_object(), "{j}");
    }
    for src in ["startup", "clear"] {
        let j = recall(
            &a,
            "s2",
            json!({"event": "session_start", "solution": G, "source": src,
                   "agent_block_version": 1}),
        )
        .await;
        let t = text(&j);
        assert!(!t.contains("### Решения"), "{src}: {t}");
        assert!(pos(t, "устарел") < pos(t, "### Задачи"), "{src}: {t}");
    }
}

/// Ответ, уже показанный сессии, в обычном дайджесте не повторяется, а в пакете
/// восстановления — повторяется.
#[tokio::test]
async fn recovery_answers_ignore_seen() {
    let (a, _, q, _) = seeded("answers").await;
    let body = json!({"event": "session_start", "solution": G});
    let j = recall(&a, "s3", body.clone()).await;
    assert!(text(&j).contains(&format!("q:{q}")), "{j}");
    let j = recall(&a, "s3", body).await;
    assert!(!text(&j).contains(&format!("q:{q}")), "{j}");
    let j = recall(
        &a,
        "s3",
        json!({"event": "session_start", "solution": G, "source": "compact"}),
    )
    .await;
    assert!(text(&j).contains(&format!("q:{q}")), "{j}");
    let j = recall(&a, "s3", json!({"event": "recovery", "solution": G})).await;
    assert!(text(&j).contains(&format!("q:{q}")), "{j}");
}

/// `hooks.recovery: false` у пользователя — `compact` и `event: recovery` дают обычный
/// дайджест; `hooks.session_start: false` — тишина и для `recovery`.
#[tokio::test]
async fn recovery_disabled_falls_back() {
    let (a, _, _, _) = seeded("off").await;
    let uri = format!("/api/v1/admin/users/{}/settings", a.admin_id());
    let set = |hooks: Value| {
        let (a, uri) = (&a, &uri);
        async move {
            let r = a
                .adm(Method::PATCH, uri, Some(json!({ "hooks": hooks })), None)
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        }
    };
    set(json!({"recovery": false})).await;
    for body in [
        json!({"event": "session_start", "solution": G, "source": "compact",
               "agent_block_version": 1}),
        json!({"event": "recovery", "solution": G, "agent_block_version": 1}),
    ] {
        let j = recall(&a, "s4", body.clone()).await;
        let t = text(&j);
        assert!(!t.contains("### Решения"), "{body}: {t}");
        assert!(pos(t, "устарел") < pos(t, "### Задачи"), "{body}: {t}");
        assert!(t.contains("### Правила"), "{t}");
    }
    set(json!({"recovery": true, "session_start": false})).await;
    let j = recall(&a, "s4", json!({"event": "recovery", "solution": G})).await;
    assert_eq!(j["text"], "", "{j}");
}

/// MCP `memory_recall {event: "recovery"}` — тот же пакет (сессия — `X-Zam-Session`).
#[tokio::test]
async fn mcp_recall_recovery() {
    let (a, _, _, _) = seeded("mcp").await;
    let b = axum::http::Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::HOST, "memory.lan:7700")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {}", a.token))
        .header("x-zam-session", "s1")
        .header("x-zam-solution", G);
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": "memory_recall", "arguments": {"event": "recovery"}}});
    let r = a.send(b.body(Body::from(body.to_string())).unwrap()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_ne!(j["result"]["isError"], true, "{j}");
    let t = j["result"]["content"][0]["text"].as_str().unwrap();
    assert!(pos(t, "### Задачи") < pos(t, "### Решения"), "{t}");
    assert!(pos(t, "Моя задача") < pos(t, "Чужая задача"), "{t}");
    assert!(t.contains("### Ответы человека"), "{t}");
    // неизвестное событие — ошибка инструмента
    let b = axum::http::Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::HOST, "memory.lan:7700")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {}", a.token));
    let body = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                      "params": {"name": "memory_recall", "arguments": {"event": "restore"}}});
    let r = a.send(b.body(Body::from(body.to_string())).unwrap()).await;
    assert_eq!(r.json()["result"]["isError"], true, "{}", r.text());
}
