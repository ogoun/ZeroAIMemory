//! Учёт выданных токенов сессии (M4.5 §9.5, задача 12): `/recall` прибавляет размер `text`
//! по событию (резерв Rules — отдельной строкой `rules`, M6 §8), `/search` и `/read` REST —
//! события `search` и `read`, инструменты MCP — `mcp` (задача 8b M6), хуки с локальным выводом —
//! пачкой `POST /sessions {tokens}`; `GET /sessions/{id}` отдаёт `tokens: {event: {calls,
//! tokens}}`. Сервер без моделей.
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
    // резерв Rules — строкой `rules`, событие — без него (M6 §8, §17 п. 3)
    let mut add = |event: &'static str, j: &Value| {
        let t = j["tokens"].as_u64().unwrap();
        let r = j["rules_tokens"].as_u64().unwrap_or(0).min(t);
        assert!(t > 0, "{j}");
        let e = want.entry(event).or_default();
        e.0 += 1;
        e.1 += t - r;
        if r > 0 {
            let e = want.entry("rules").or_default();
            e.0 += 1;
            e.1 += r;
        }
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

/// `GET /sessions/unattributed?days=N` (M6 §7.1): строки `tokens_unattributed` базы пользователя
/// за последние N суток UTC (сегодня — первые сутки), свежие первыми, внутри дня — по событию.
#[tokio::test]
async fn unattributed_by_days_window() {
    let a = api("tokens-unattributed", |_| {}).await;
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let today = jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date();
    let ago = |d: i64| today.checked_sub(jiff::Span::new().days(d)).unwrap();
    for (day, event, calls, tokens) in [
        (today, "mcp", 2, 30),
        (today, "brief", 1, 5),
        (ago(3), "mcp", 1, 7),
        (ago(40), "mcp", 4, 100),
    ] {
        zam_store::add_unattributed(&db, day, event, calls, tokens)
            .await
            .unwrap();
    }
    let get = async |q: &str| {
        let r = a
            .t(
                Method::GET,
                &format!("/api/v1/sessions/unattributed{q}"),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        r.json()
    };
    let t = today.to_string();
    assert_eq!(
        get("?days=1").await,
        json!({"items": [
            {"day": t, "event": "brief", "calls": 1, "tokens": 5},
            {"day": t, "event": "mcp", "calls": 2, "tokens": 30},
        ]})
    );
    let j = get("?days=7").await;
    let days: Vec<&str> = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["day"].as_str().unwrap())
        .collect();
    assert_eq!(days, [t.as_str(), t.as_str(), ago(3).to_string().as_str()]);
    // по умолчанию — 30 суток
    assert_eq!(get("").await["items"].as_array().unwrap().len(), 3);
    assert_eq!(get("?days=366").await["items"].as_array().unwrap().len(), 4);
}

/// Вызов инструмента MCP `name` с явной сессией `session` (заголовок `X-Zam-Session`).
async fn mcp_call(a: &Api, session: &str, name: &str, args: Value) {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": name, "arguments": args}});
    let r = a
        .send(
            axum::http::Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(axum::http::header::HOST, "memory.lan:7700")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(
                    axum::http::header::ACCEPT,
                    "application/json, text/event-stream",
                )
                .header(
                    axum::http::header::AUTHORIZATION,
                    format!("Bearer {}", a.token),
                )
                .header("x-zam-session", session)
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json().get("error").is_none(), "{}", r.text());
}

/// Учёт MCP идёт в фоне после ответа: ждём, пока у события `mcp` не станет `calls` вызовов.
async fn wait_mcp(a: &Api, session: &str, calls: u64) -> Value {
    for _ in 0..200 {
        let t = tokens(a, session).await;
        if t["mcp"]["calls"].as_u64() == Some(calls) {
            return t;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("учёт mcp не дошёл: {}", tokens(a, session).await);
}

/// M6 §8 (задача 8b): `/search` и `/read` REST с явной сессией — события `search` и `read`
/// (вызов и токены ответа), повтор `/read` той же ревизии — `reread_calls` строки `read`;
/// вызовы из MCP — одна строка `mcp` (вложенные `/search`, `/read` своих строк не пишут), повтор
/// чтения из MCP — `reread_calls` строки `mcp`; без сессии — учёта нет.
#[tokio::test]
async fn session_tokens_search_read_mcp() {
    let a = seeded("tokens-search-read-mcp").await;
    let n = a
        .note(
            "knowledge",
            "Сборка/Кэш",
            "# Кэш сборки\n\nОбщий каталог сборки ускоряет ветки.\n",
        )
        .await;
    let id = format!("n:{}", n["id"].as_str().unwrap());
    let post = async |uri: &str, session: Option<&str>, body: Value| {
        let h: Vec<(&str, &str)> = session.map(|s| ("x-zam-session", s)).into_iter().collect();
        let r = a.tok_h(&a.token, Method::POST, uri, Some(body), &h).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        r
    };
    // REST: поиск (Markdown) и два чтения
    let r = post(
        "/api/v1/search",
        Some("s-rest"),
        json!({"query": "каталог сборки", "format": "md"}),
    )
    .await;
    let search_tokens = zam_store::count_tokens(&r.text()) as u64;
    let mut read_tokens = 0;
    for _ in 0..2 {
        let r = post(
            "/api/v1/read",
            Some("s-rest"),
            json!({"items": [{"id": id}]}),
        )
        .await;
        read_tokens += r.json()["total_tokens"].as_u64().unwrap();
    }
    let t = tokens(&a, "s-rest").await;
    assert_eq!(
        t["search"],
        json!({"calls": 1, "tokens": search_tokens}),
        "{t}"
    );
    assert_eq!(
        t["read"],
        json!({"calls": 2, "tokens": read_tokens, "reread_calls": 1}),
        "{t}"
    );
    assert!(t.get("mcp").is_none(), "{t}");
    // JSON-поиск — тоже учёт (размер — Markdown того же отбора)
    post(
        "/api/v1/search",
        Some("s-rest"),
        json!({"query": "каталог сборки"}),
    )
    .await;
    assert_eq!(tokens(&a, "s-rest").await["search"]["calls"], 2);
    // без сессии — ничего не падает, учёта нет
    post("/api/v1/read", None, json!({"items": [{"id": id}]})).await;

    // MCP: поиск и два чтения — строка `mcp`, без `search`/`read`
    mcp_call(
        &a,
        "s-mcp",
        "memory_search",
        json!({"query": "каталог сборки"}),
    )
    .await;
    wait_mcp(&a, "s-mcp", 1).await;
    mcp_call(&a, "s-mcp", "memory_read", json!({"items": [{"id": id}]})).await;
    wait_mcp(&a, "s-mcp", 2).await;
    mcp_call(&a, "s-mcp", "memory_read", json!({"items": [{"id": id}]})).await;
    let t = wait_mcp(&a, "s-mcp", 3).await;
    assert_eq!(t["mcp"]["reread_calls"], 1, "{t}");
    assert!(t["mcp"]["tokens"].as_u64().unwrap() > 0, "{t}");
    assert!(t.get("search").is_none() && t.get("read").is_none(), "{t}");
}

/// `GET /sessions?tokens=true` — у каждой сессии `tokens` (как в `GET /sessions/{id}`) и
/// `holdout`; без параметра — прежняя форма без `tokens`.
#[tokio::test]
async fn sessions_list_with_tokens() {
    let a = api("tokens-list", |_| {}).await;
    let post = async |body: Value| a.t(Method::POST, "/api/v1/sessions", Some(body)).await;
    let r = post(json!({"id": "s-a", "tokens": {"pre_tool": 7}})).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = post(json!({"id": "s-b"})).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    zam_store::set_holdout(&db, "s-a", true).await.unwrap();
    let r = a.t(Method::GET, "/api/v1/sessions?tokens=true", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let items = r.json()["items"].as_array().unwrap().clone();
    let by = |id: &str| items.iter().find(|s| s["id"] == id).unwrap().clone();
    assert_eq!(
        by("s-a")["tokens"],
        json!({"pre_tool": {"calls": 1, "tokens": 7}})
    );
    assert_eq!(by("s-a")["holdout"], true);
    assert_eq!(by("s-b")["tokens"], json!({}));
    assert_eq!(by("s-b")["holdout"], false);
    let r = a.t(Method::GET, "/api/v1/sessions", None).await;
    assert!(r.json()["items"][0].get("tokens").is_none(), "{}", r.text());
}
