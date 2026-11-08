//! MCP `/mcp` (§13.1, задача 9 M4): streamable HTTP без сессий, bearer, 11 инструментов поверх
//! REST (решение R12: внутренний `Router` с исходными заголовками), ошибки сервиса —
//! `isError: true`, область решения и участника, раскрытие Vault с аудитом.
//!
//! Запросы — сырые JSON-RPC через `oneshot` к роутеру сервера (как у REST-тестов), без модели
//! (поиск FTS-only).
mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e77";
const SESSION: &str = "mcp-session-1";
const MACHINE: &str = "mcp-box";

const TOOLS: [&str; 11] = [
    "memory_search",
    "memory_read",
    "memory_cache_put",
    "memory_tree",
    "memory_write",
    "memory_log",
    "memory_update_entry",
    "memory_link",
    "memory_recall",
    "memory_vault",
    "memory_code",
];

/// JSON-RPC к `/mcp`: `auth` — значение `Authorization` (или без него), `extra` — заголовки.
async fn rpc_raw(
    a: &Api,
    auth: Option<&str>,
    extra: &[(&str, &str)],
    method: &str,
    params: Value,
) -> Resp {
    let mut b = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        // rmcp требует Host (или :authority HTTP/2) даже без проверки списка хостов
        .header(header::HOST, "memory.lan:7700")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(v) = auth {
        b = b.header(header::AUTHORIZATION, v);
    }
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    a.send(b.body(Body::from(body.to_string())).unwrap()).await
}

/// JSON-RPC по токену `tok`; ответ 200 с `result` (не ошибка JSON-RPC).
async fn rpc(a: &Api, tok: &str, extra: &[(&str, &str)], method: &str, params: Value) -> Value {
    let r = rpc_raw(a, Some(&format!("Bearer {tok}")), extra, method, params).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert!(j.get("error").is_none(), "ошибка JSON-RPC: {j}");
    j["result"].clone()
}

/// Результат инструмента: текст и isError.
struct Out {
    text: String,
    is_error: bool,
}

impl Out {
    /// JSON ответа `format: "json"` — текст без последней строки `~N tok`.
    fn json(&self) -> Value {
        let body = self
            .text
            .trim_end()
            .rsplit_once('\n')
            .map_or("", |(b, _)| b);
        serde_json::from_str(body).unwrap_or_else(|e| panic!("не JSON ({e}): {}", self.text))
    }
}

async fn tool(a: &Api, tok: &str, extra: &[(&str, &str)], name: &str, args: Value) -> Out {
    let r = rpc(
        a,
        tok,
        extra,
        "tools/call",
        json!({"name": name, "arguments": args}),
    )
    .await;
    let text = r["content"][0]["text"].as_str().unwrap_or("").to_string();
    let last = text.trim_end().rsplit('\n').next().unwrap_or("");
    assert!(
        last.starts_with('~') && last.ends_with(" tok"),
        "{name}: последняя строка не ~N tok: {text}"
    );
    // JSON не дублируется в structuredContent (только текст)
    assert!(r.get("structuredContent").is_none(), "{r}");
    Out {
        text,
        is_error: r["isError"].as_bool() == Some(true),
    }
}

/// Успешный вызов инструмента — текст.
async fn ok(a: &Api, tok: &str, extra: &[(&str, &str)], name: &str, args: Value) -> Out {
    let o = tool(a, tok, extra, name, args).await;
    assert!(!o.is_error, "{name}: {}", o.text);
    o
}

fn sess() -> [(&'static str, &'static str); 2] {
    [("x-zam-session", SESSION), ("x-zam-machine", MACHINE)]
}

#[tokio::test]
async fn mcp_initialize_and_list_tools() {
    let a = api("mcp-init", |_| {}).await;
    let init = rpc(
        &a,
        &a.token,
        &[],
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {},
               "clientInfo": {"name": "test", "version": "1"}}),
    )
    .await;
    assert_eq!(init["serverInfo"]["name"], "zeroaimemory", "{init}");
    assert!(init["capabilities"]["tools"].is_object(), "{init}");
    assert!(init["protocolVersion"].is_string(), "{init}");
    // без сессий: уведомление принимается, следующий запрос — без Mcp-Session-Id
    let r = rpc_raw(
        &a,
        Some(&format!("Bearer {}", a.token)),
        &[],
        "notifications/initialized",
        json!({}),
    )
    .await;
    assert!(r.status.is_success(), "{} {}", r.status, r.text());
    let list = rpc(&a, &a.token, &[], "tools/list", json!({})).await;
    let tools = list["tools"].as_array().unwrap();
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    let mut want = TOOLS.to_vec();
    want.sort_unstable();
    assert_eq!(names, want);
    for t in tools {
        let d = t["description"].as_str().unwrap();
        assert!(
            !d.is_empty() && d.chars().count() <= 300,
            "{}: {} символов",
            t["name"],
            d.chars().count()
        );
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
    }
    // неизвестный инструмент — ошибка JSON-RPC, а не результат
    let r = rpc_raw(
        &a,
        Some(&format!("Bearer {}", a.token)),
        &[],
        "tools/call",
        json!({"name": "memory_nope", "arguments": {}}),
    )
    .await;
    assert!(r.json().get("error").is_some(), "{}", r.text());
}

#[tokio::test]
async fn mcp_requires_bearer_401() {
    let a = api("mcp-401", |_| {}).await;
    let list = json!({});
    // без заголовка, с неверным токеном, с cookie администратора вместо токена
    for (auth, cookie) in [
        (None, None),
        (Some("Bearer zam_wrong"), None),
        (Some("Basic YWRtaW46cHc="), None),
        (None, Some(a.cookie.as_str())),
    ] {
        let extra: Vec<(&str, &str)> = cookie
            .map(|c| vec![("cookie", c), ("x-csrf-token", a.csrf.as_str())])
            .unwrap_or_default();
        let r = rpc_raw(&a, auth, &extra, "tools/list", list.clone()).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{auth:?}: {}", r.text());
        assert_eq!(r.header("www-authenticate"), "Bearer");
        assert_eq!(r.header("content-type"), "application/problem+json");
    }
    // действующий токен — 200
    rpc(&a, &a.token, &[], "tools/list", list).await;
}

#[tokio::test]
async fn mcp_search_returns_md_cards() {
    let a = api("mcp-search", |_| {}).await;
    let n = a
        .note(
            "knowledge",
            "Инструменты/Ripgrep",
            "# Ripgrep\n\nИщи по коду через rg с флагом --hidden.\n",
        )
        .await;
    let id = n["id"].as_str().unwrap();
    let o = ok(
        &a,
        &a.token,
        &[],
        "memory_search",
        json!({"query": "ripgrep hidden"}),
    )
    .await;
    assert!(o.text.contains(&format!("n:{id}")), "{}", o.text);
    assert!(o.text.contains("Инструменты/Ripgrep"), "{}", o.text);
    // карточка v2 (M6 §4.1): `r1` и дата UTC
    assert!(o.text.contains("] r1 · "), "{}", o.text);
    // format=json — JSON ответа REST в тексте
    let o = ok(
        &a,
        &a.token,
        &[],
        "memory_search",
        json!({"query": "ripgrep hidden", "format": "json"}),
    )
    .await;
    assert_eq!(o.json()["items"][0]["node_id"], id, "{}", o.text);
    // дерево с L0
    let o = ok(&a, &a.token, &[], "memory_tree", json!({"depth": 2})).await;
    assert!(o.text.contains("Инструменты/Ripgrep"), "{}", o.text);
    // ничего не найдено — тоже ответ с ~N tok
    let o = ok(
        &a,
        &a.token,
        &[],
        "memory_search",
        json!({"query": "несуществующееслово"}),
    )
    .await;
    assert!(!o.text.contains("n:"), "{}", o.text);
}

#[tokio::test]
async fn mcp_read_and_cache_put_roundtrip() {
    let a = api("mcp-read-cache", |_| {}).await;
    let n = a
        .note(
            "knowledge",
            "Сборка/Кэш cargo",
            "# Кэш cargo\n\nCARGO_TARGET_DIR общий для всех веток ускоряет сборку.\n",
        )
        .await;
    let id = n["id"].as_str().unwrap();
    let h = sess();
    let o = ok(
        &a,
        &a.token,
        &h,
        "memory_read",
        json!({"items": [{"id": format!("n:{id}")}, {"id": "n:01JZZZZZZZZZZZZZZZZZZZZZZZ"}]}),
    )
    .await;
    assert!(o.text.contains("CARGO_TARGET_DIR"), "{}", o.text);
    // ошибка по элементу — в самом элементе, результат не isError
    assert!(o.text.contains("not_found"), "{}", o.text);
    let o = ok(
        &a,
        &a.token,
        &h,
        "memory_cache_put",
        json!({"query": "как ускорить сборку cargo",
               "summary": "Общий CARGO_TARGET_DIR для веток ускоряет сборку.",
               "sources": [{"id": format!("n:{id}"), "rev": 1}]}),
    )
    .await;
    assert!(o.text.contains("c_"), "{}", o.text);
    let o = ok(
        &a,
        &a.token,
        &h,
        "memory_search",
        json!({"query": "как ускорить сборку cargo"}),
    )
    .await;
    assert!(o.text.contains("[cache]"), "{}", o.text);
    assert!(o.text.contains("Общий CARGO_TARGET_DIR"), "{}", o.text);
    // R12: запись через MCP идёт слоем memory_routes — счётчик сессии растёт только от неё
    let s = a
        .t(Method::GET, &format!("/api/v1/sessions/{SESSION}"), None)
        .await;
    assert_eq!(s.status, StatusCode::OK, "{}", s.text());
    assert_eq!(s.json()["memory_writes"], 1, "{}", s.text());
    assert_eq!(s.json()["machine"], MACHINE);
}

/// R13 (задача 10 M4): `.mcp.json` не знает `session_id` агента — запись через MCP без
/// `X-Zam-Session` учитывается в последней активной сессии пользователя (хук `session-start`), с
/// `X-Zam-Machine` — только той же машины; подходящей нет — без сессии.
#[tokio::test]
async fn mcp_without_session_header_uses_latest_hook_session() {
    let a = api("mcp-implicit-session", |_| {}).await;
    let hook = |id: &'static str, machine: &'static str| {
        let a = &a;
        async move {
            let r = a
                .tok_h(
                    &a.token,
                    Method::POST,
                    "/api/v1/sessions",
                    Some(json!({"id": id, "agent": "claude-code"})),
                    &[("x-zam-machine", machine)],
                )
                .await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        }
    };
    let writes = |id: &'static str| {
        let a = &a;
        async move {
            let s = a
                .t(Method::GET, &format!("/api/v1/sessions/{id}"), None)
                .await;
            assert_eq!(s.status, StatusCode::OK, "{}", s.text());
            s.json()["memory_writes"].as_i64().unwrap()
        }
    };
    hook("hook-a", MACHINE).await;
    hook("hook-b", "other-box").await;
    // с машиной — сессия этой машины, хотя «hook-b» свежее
    ok(
        &a,
        &a.token,
        &[("x-zam-machine", MACHINE)],
        "memory_write",
        json!({"path": "Неявная сессия/A", "content": "# A

текст
"}),
    )
    .await;
    assert_eq!(writes("hook-a").await, 1);
    assert_eq!(writes("hook-b").await, 0);
    // без машины — последняя сессия пользователя (после записи свежее «hook-a»)
    hook("hook-b", "other-box").await;
    ok(
        &a,
        &a.token,
        &[],
        "memory_write",
        json!({"path": "Неявная сессия/B", "content": "# B

текст
"}),
    )
    .await;
    assert_eq!(writes("hook-b").await, 1);
    assert_eq!(writes("hook-a").await, 1);
    // машина без сессии — запись без сессии, счётчики не растут
    ok(
        &a,
        &a.token,
        &[("x-zam-machine", "nobody-box")],
        "memory_write",
        json!({"path": "Неявная сессия/C", "content": "# C

текст
"}),
    )
    .await;
    assert_eq!((writes("hook-a").await, writes("hook-b").await), (1, 1));
}

/// R13 + решение: с `X-Zam-Solution` запись засчитывается сессии этого решения, а не просто
/// последней (fix round 1 задачи 10).
#[tokio::test]
async fn mcp_implicit_session_filtered_by_solution() {
    let a = api("mcp-implicit-solution", |_| {}).await;
    const G1: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e81";
    const G2: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e82";
    for (id, g) in [("sol-1", G1), ("sol-2", G2)] {
        let r = a
            .t(
                Method::POST,
                "/api/v1/sessions",
                Some(json!({"id": id, "agent": "codex", "solution": g})),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    let writes = |id: &'static str| {
        let a = &a;
        async move {
            let s = a
                .t(Method::GET, &format!("/api/v1/sessions/{id}"), None)
                .await;
            s.json()["memory_writes"].as_i64().unwrap()
        }
    };
    // «sol-2» свежее, но решение запроса — G1
    ok(
        &a,
        &a.token,
        &[("x-zam-solution", G1)],
        "memory_write",
        json!({"path": "knowledge:/Решение сессии/A", "content": "# A

текст
"}),
    )
    .await;
    assert_eq!((writes("sol-1").await, writes("sol-2").await), (1, 0));
    // без решения — последняя сессия пользователя (после записи свежее «sol-1»)
    ok(
        &a,
        &a.token,
        &[],
        "memory_write",
        json!({"path": "knowledge:/Решение сессии/B", "content": "# B

текст
"}),
    )
    .await;
    assert_eq!((writes("sol-1").await, writes("sol-2").await), (2, 0));
}

#[tokio::test]
async fn mcp_write_conflict_is_tool_error() {
    let a = api("mcp-conflict", |_| {}).await;
    let path = "knowledge:/Заметки/Конфликт";
    let o = ok(
        &a,
        &a.token,
        &[],
        "memory_write",
        json!({"path": path, "content": "# Конфликт\n\nпервая\n"}),
    )
    .await;
    assert!(o.text.contains("rev1"), "{}", o.text);
    // повторное создание по тому же пути — ошибка с подсказкой
    let o = tool(
        &a,
        &a.token,
        &[],
        "memory_write",
        json!({"path": path, "content": "# Конфликт\n\nдубль\n"}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    assert!(o.text.contains("base_rev"), "{}", o.text);
    let o = ok(
        &a,
        &a.token,
        &[],
        "memory_write",
        json!({"path": path, "content": "# Конфликт\n\nвторая\n", "base_rev": 1}),
    )
    .await;
    assert!(o.text.contains("rev2"), "{}", o.text);
    // устаревшая ревизия — isError с текущей ревизией и диффом, а не ошибка JSON-RPC
    let o = tool(
        &a,
        &a.token,
        &[],
        "memory_write",
        json!({"path": path, "content": "# Конфликт\n\nтретья\n", "base_rev": 1}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    assert!(o.text.contains("409"), "{}", o.text);
    assert!(o.text.contains("Текущая ревизия — 2"), "{}", o.text);
    assert!(o.text.contains("```diff"), "{}", o.text);
    assert!(o.text.contains("memory_read"), "{}", o.text);
    // правка секций ops с base_rev
    let o = ok(
        &a,
        &a.token,
        &[],
        "memory_write",
        json!({"path": path, "base_rev": 2,
               "ops": [{"op": "append", "content": "дописано\n"}]}),
    )
    .await;
    assert!(o.text.contains("rev3"), "{}", o.text);
    // запись журнала: правка без rev — 428 с объяснением; с rev — проходит; старая rev — 409
    let o = ok(
        &a,
        &a.token,
        &[],
        "memory_log",
        json!({"kind": "insight", "title": "Конфликт ревизий в MCP",
               "body": "409 приходит результатом инструмента", "data": {"kind": "gotcha"}}),
    )
    .await;
    let eid = o
        .text
        .split_whitespace()
        .find_map(|w| w.strip_prefix("i:"))
        .expect("ссылка i:<id>")
        .to_string();
    let o = tool(
        &a,
        &a.token,
        &[],
        "memory_update_entry",
        json!({"id": format!("i:{eid}"), "action": "revisit"}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    assert!(o.text.contains("428"), "{}", o.text);
    assert!(o.text.contains("rev"), "{}", o.text);
    let o = ok(
        &a,
        &a.token,
        &[],
        "memory_update_entry",
        json!({"id": format!("i:{eid}"), "action": "revisit", "rev": 1}),
    )
    .await;
    assert!(o.text.contains("revisit"), "{}", o.text);
    let o = tool(
        &a,
        &a.token,
        &[],
        "memory_update_entry",
        json!({"id": format!("i:{eid}"), "action": "append", "rev": 1, "body": "ещё"}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    assert!(o.text.contains("409"), "{}", o.text);
    // агенту certainty — только с reason (§7.5)
    let o = tool(
        &a,
        &a.token,
        &[],
        "memory_update_entry",
        json!({"id": eid, "action": "certify", "rev": 2, "certainty": "axiom"}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    assert!(o.text.contains("422"), "{}", o.text);
    let o = ok(
        &a,
        &a.token,
        &[],
        "memory_update_entry",
        json!({"id": eid, "action": "feedback", "value": "useful"}),
    )
    .await;
    assert!(o.text.contains("useful"), "{}", o.text);
}

/// Регистрирует решение `G` токеном `tok` (201 — новое, 202 — запрос доступа).
async fn register(a: &Api, tok: &str) -> StatusCode {
    a.tok(
        tok,
        Method::POST,
        "/api/v1/solutions",
        Some(json!({"agent_solution_id": G, "name": "Общее"})),
    )
    .await
    .status
}

#[tokio::test]
async fn mcp_solution_header_scopes_search() {
    let a = api("mcp-solution", |_| {}).await;
    assert_eq!(register(&a, &a.token).await, StatusCode::CREATED);
    let sol = a
        .note(G, "Сборка", "# Сборка\n\nКвазитермин сборки — xtask.\n")
        .await;
    a.note(
        "knowledge",
        "Общее",
        "# Общее\n\nКвазитермин знаний без решения.\n",
    )
    .await;
    let sid = sol["id"].as_str().unwrap();
    let hdr = [("x-zam-solution", G)];
    let o = ok(
        &a,
        &a.token,
        &hdr,
        "memory_search",
        json!({"query": "квазитермин"}),
    )
    .await;
    assert!(o.text.contains(&format!("n:{sid}")), "{}", o.text);
    // без заголовка решение в область не входит
    let o = ok(
        &a,
        &a.token,
        &[],
        "memory_search",
        json!({"query": "квазитермин"}),
    )
    .await;
    assert!(!o.text.contains(&format!("n:{sid}")), "{}", o.text);
    // запись по умолчанию — в решение из заголовка; changelog — только в решении
    let o = ok(
        &a,
        &a.token,
        &hdr,
        "memory_write",
        json!({"path": "Заметка решения", "content": "# Заметка решения\n"}),
    )
    .await;
    assert!(
        o.text.contains(&format!("{G}:/Заметка решения")),
        "{}",
        o.text
    );
    let nid = o
        .text
        .split_whitespace()
        .find_map(|w| w.strip_prefix("n:"))
        .unwrap()
        .to_string();
    // связь двух узлов решения и её удаление
    let o = ok(
        &a,
        &a.token,
        &hdr,
        "memory_link",
        json!({"src": format!("n:{nid}"), "type": "relates_to", "dst": format!("n:{sid}")}),
    )
    .await;
    let edge = o
        .text
        .split_whitespace()
        .nth(1)
        .unwrap()
        .trim_end_matches(':');
    let o = ok(&a, &a.token, &hdr, "memory_link", json!({"remove": edge})).await;
    assert!(o.text.contains("удалена"), "{}", o.text);
    let o = ok(
        &a,
        &a.token,
        &hdr,
        "memory_log",
        json!({"kind": "changelog", "title": "Добавлен MCP", "data": {"kind": "feat"}}),
    )
    .await;
    assert!(o.text.contains("c:"), "{}", o.text);
    let o = tool(
        &a,
        &a.token,
        &[],
        "memory_log",
        json!({"kind": "changelog", "title": "Без решения", "data": {"kind": "feat"}}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    // подсказки старта сессии видят решение из заголовка
    let o = ok(
        &a,
        &a.token,
        &hdr,
        "memory_recall",
        json!({"event": "session_start"}),
    )
    .await;
    assert!(o.text.contains("Добавлен MCP"), "{}", o.text);
}

#[tokio::test]
async fn mcp_participant_cannot_escape_solution() {
    let a = api("mcp-member", |_| {}).await;
    let (_, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    assert_eq!(register(&a, &a_tok).await, StatusCode::CREATED);
    assert_eq!(register(&a, &b_tok).await, StatusCode::ACCEPTED);
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let note = |space: &'static str, path: &'static str, content: &'static str| {
        let a = &a;
        let tok = a_tok.clone();
        async move {
            let r = a
                .tok(
                    &tok,
                    Method::POST,
                    "/api/v1/nodes",
                    Some(json!({"space": space, "path": path, "kind": "note", "content": content})),
                )
                .await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
            r.json()
        }
    };
    let secret = note(
        "knowledge",
        "Личное",
        "# Личное\n\nЗакрытотермин владельца.\n",
    )
    .await;
    let shared = note(G, "Общее", "# Общее\n\nОткрытотермин решения.\n").await;
    let secret_id = secret["id"].as_str().unwrap();
    let shared_id = shared["id"].as_str().unwrap();
    let hdr = [("x-zam-solution", G)];
    // решение видно
    let o = ok(
        &a,
        &b_tok,
        &hdr,
        "memory_search",
        json!({"query": "открытотермин"}),
    )
    .await;
    assert!(o.text.contains(&format!("n:{shared_id}")), "{}", o.text);
    // Knowledge владельца — нет: ни поиском (в том числе явным пространством), ни чтением
    let o = ok(
        &a,
        &b_tok,
        &hdr,
        "memory_search",
        json!({"query": "закрытотермин"}),
    )
    .await;
    assert!(!o.text.contains(secret_id), "{}", o.text);
    let o = tool(
        &a,
        &b_tok,
        &hdr,
        "memory_search",
        json!({"query": "закрытотермин", "scope": {"spaces": [secret["space_id"]]}}),
    )
    .await;
    assert!(!o.text.contains(secret_id), "{}", o.text);
    let o = ok(
        &a,
        &b_tok,
        &hdr,
        "memory_read",
        json!({"items": [{"id": format!("n:{secret_id}")}, {"id": format!("n:{shared_id}")}]}),
    )
    .await;
    assert!(!o.text.contains("Закрытотермин"), "{}", o.text);
    assert!(o.text.contains("not_found"), "{}", o.text);
    assert!(o.text.contains("Открытотермин"), "{}", o.text);
    // дерево Knowledge владельца по id пространства — 404
    let o = tool(
        &a,
        &b_tok,
        &hdr,
        "memory_tree",
        json!({"space": secret["space_id"]}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    assert!(o.text.contains("404"), "{}", o.text);
    // связь из решения в Knowledge владельца — нельзя (цель вне области)
    let o = tool(
        &a,
        &b_tok,
        &hdr,
        "memory_link",
        json!({"src": format!("n:{shared_id}"), "type": "relates_to", "dst": format!("n:{secret_id}")}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    // выжимка с источником вне решения — отказ
    let o = tool(
        &a,
        &b_tok,
        &hdr,
        "memory_cache_put",
        json!({"query": "закрытотермин", "summary": "утечка",
               "sources": [{"id": format!("n:{secret_id}")}]}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    // подсказки — без Knowledge владельца
    let o = ok(
        &a,
        &b_tok,
        &hdr,
        "memory_recall",
        json!({"event": "prompt", "prompt": "закрытотермин владельца"}),
    )
    .await;
    assert!(!o.text.contains("Закрытотермин"), "{}", o.text);
}

#[tokio::test]
async fn mcp_vault_get_audited() {
    let a = api("mcp-vault", |_| {}).await;
    let value = "mcp-secret-0b9f3c";
    let h = sess();
    let o = ok(
        &a,
        &a.token,
        &h,
        "memory_vault",
        json!({"action": "set", "name": "github", "value": value, "type": "token"}),
    )
    .await;
    assert!(!o.text.contains(value), "{}", o.text);
    let o = ok(&a, &a.token, &h, "memory_vault", json!({"action": "list"})).await;
    assert!(o.text.contains("github"), "{}", o.text);
    assert!(!o.text.contains(value), "список без значений: {}", o.text);
    // аудита без раскрытия нет
    let audit = a.st.system.list_audit(100, None).await.unwrap();
    assert!(!audit.iter().any(|x| x.action == "vault.reveal"));
    let o = ok(
        &a,
        &a.token,
        &h,
        "memory_vault",
        json!({"action": "get", "name": "github"}),
    )
    .await;
    assert!(o.text.contains(value), "{}", o.text);
    // раскрытие — в аудите с сессией и машиной из заголовков MCP, actor — id токена
    let tok = a.st.system.list_tokens(a.st.admin_id).await.unwrap();
    let audit = a.st.system.list_audit(100, None).await.unwrap();
    let row = audit
        .iter()
        .find(|x| x.action == "vault.reveal")
        .expect("аудит vault.reveal");
    assert_eq!(row.actor, tok[0].id.to_string());
    assert_eq!(row.user_id, Some(a.st.admin_id));
    assert_eq!(row.details["session"], SESSION);
    assert_eq!(row.details["machine"], MACHINE);
    for x in &audit {
        assert!(!x.details.to_string().contains(value), "значение в аудите");
    }
    // обновление того же имени — новая ревизия, не второй секрет
    let o = ok(
        &a,
        &a.token,
        &h,
        "memory_vault",
        json!({"action": "set", "name": "github", "value": "mcp-secret-2"}),
    )
    .await;
    assert!(o.text.contains("rev2"), "{}", o.text);
    // нет такого — isError
    let o = tool(
        &a,
        &a.token,
        &h,
        "memory_vault",
        json!({"action": "get", "name": "нет-такого"}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
}

/// Запись окружения без значения: `memory_vault get id=v:<id>` (ссылка дайджеста) отдаёт
/// открытые поля без строки `value`, поиск `list name=` идёт по открытым полям.
#[tokio::test]
async fn mcp_vault_env_get_by_digest_ref() {
    let a = api("mcp-vault-env", |_| {}).await;
    let h = sess();
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/vault",
            Some(json!({"title": "stage", "data": {"type": "host",
                "purpose": "приёмочный стенд", "address": "10.0.0.5:22"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    let o = ok(
        &a,
        &a.token,
        &h,
        "memory_vault",
        json!({"action": "get", "id": format!("v:{id}")}),
    )
    .await;
    assert!(
        o.text.contains("address: 10.0.0.5:22") && o.text.contains("purpose: приёмочный стенд"),
        "{}",
        o.text
    );
    assert!(!o.text.contains("value:"), "{}", o.text);
    let o = ok(
        &a,
        &a.token,
        &h,
        "memory_vault",
        json!({"action": "list", "name": "приёмочный"}),
    )
    .await;
    assert!(o.text.contains("stage"), "{}", o.text);
}

#[tokio::test]
async fn mcp_foreign_origin_403() {
    let a = api("mcp-origin", |_| {}).await;
    let auth = format!("Bearer {}", a.token);
    // чужая страница — 403 problem до проверки токена и разбора JSON-RPC
    for origin in ["http://evil.example", "http://memory.lan:7701", "null"] {
        let r = rpc_raw(
            &a,
            Some(&auth),
            &[("origin", origin)],
            "tools/list",
            json!({}),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{origin}: {}", r.text());
        assert_eq!(r.header("content-type"), "application/problem+json");
    }
    // своя страница (Origin = Host) и запрос без Origin — проходят
    let r = rpc_raw(
        &a,
        Some(&auth),
        &[("origin", "http://memory.lan:7700")],
        "tools/list",
        json!({}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    rpc(&a, &a.token, &[], "tools/list", json!({})).await;
}

async fn vault_call(a: &Api, tok: &str, extra: &[(&str, &str)], args: Value) -> Out {
    tool(a, tok, extra, "memory_vault", args).await
}

/// Регистр имени важен (уникальный индекс его учитывает): `set` — только точное имя, `get` —
/// точное, иначе единственное без учёта регистра, несколько — ошибка со списком; нет — 404.
#[tokio::test]
async fn mcp_vault_name_case() {
    let a = api("mcp-vault-case", |_| {}).await;
    let h = sess();
    let set = |name: &str, value: &str| json!({"action": "set", "name": name, "value": value});
    let get = |name: &str| json!({"action": "get", "name": name});
    // сценарий А: DB_PASS и db_pass — два секрета, set не трогает «почти тот же»
    let o = vault_call(&a, &a.token, &h, set("DB_PASS", "value-upper-1")).await;
    assert!(!o.is_error && o.text.contains("rev1"), "{}", o.text);
    let o = vault_call(&a, &a.token, &h, set("db_pass", "value-lower-1")).await;
    assert!(
        !o.is_error && o.text.contains("rev1"),
        "новый секрет, а не ревизия DB_PASS: {}",
        o.text
    );
    let o = vault_call(&a, &a.token, &h, get("DB_PASS")).await;
    assert!(
        !o.is_error && o.text.contains("value-upper-1"),
        "{}",
        o.text
    );
    let o = vault_call(&a, &a.token, &h, get("db_pass")).await;
    assert!(
        !o.is_error && o.text.contains("value-lower-1"),
        "{}",
        o.text
    );
    // сценарий Б: точного нет, без учёта регистра — два кандидата → ошибка со списком
    let o = vault_call(&a, &a.token, &h, get("Db_Pass")).await;
    assert!(o.is_error, "{}", o.text);
    assert!(o.text.contains("409"), "{}", o.text);
    assert!(
        o.text.contains("DB_PASS") && o.text.contains("db_pass"),
        "{}",
        o.text
    );
    assert!(!o.text.contains("value-"), "значения в списке: {}", o.text);
    // единственный без учёта регистра — находится
    let o = vault_call(&a, &a.token, &h, set("GitHub", "value-gh")).await;
    assert!(!o.is_error, "{}", o.text);
    let o = vault_call(&a, &a.token, &h, get("github")).await;
    assert!(!o.is_error && o.text.contains("value-gh"), "{}", o.text);
    // нет — 404 с пространствами поиска
    let o = vault_call(&a, &a.token, &h, get("нет-такого")).await;
    assert!(o.is_error, "{}", o.text);
    assert!(
        o.text.contains("404") && o.text.contains("knowledge"),
        "{}",
        o.text
    );
}

/// Участник решения через `memory_vault` видит только свои секреты (решения и личные), не
/// Vault владельца — ни личный, ни решения.
#[tokio::test]
async fn mcp_vault_member_isolated() {
    let a = api("mcp-vault-member", |_| {}).await;
    let (_, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    assert_eq!(register(&a, &a_tok).await, StatusCode::CREATED);
    assert_eq!(register(&a, &b_tok).await, StatusCode::ACCEPTED);
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let hdr = [("x-zam-solution", G)];
    let set = |scope: &str, name: &str, value: &str| json!({"action": "set", "scope": scope, "name": name, "value": value});
    for (tok, args) in [
        (
            &a_tok,
            set("global", "owner-personal", "val-owner-personal"),
        ),
        (
            &a_tok,
            set("solution", "owner-solution", "val-owner-solution"),
        ),
        (
            &b_tok,
            set("solution", "member-solution", "val-member-solution"),
        ),
    ] {
        let o = vault_call(&a, tok, &hdr, args).await;
        assert!(!o.is_error, "{}", o.text);
    }
    // список решения у участника — только его секрет; личный Vault — без секретов владельца
    let o = vault_call(&a, &b_tok, &hdr, json!({"action": "list"})).await;
    assert!(
        !o.is_error && o.text.contains("member-solution"),
        "{}",
        o.text
    );
    assert!(!o.text.contains("owner-"), "{}", o.text);
    let o = vault_call(
        &a,
        &b_tok,
        &hdr,
        json!({"action": "list", "scope": "global"}),
    )
    .await;
    assert!(!o.is_error && !o.text.contains("owner-"), "{}", o.text);
    // секреты владельца по имени не раскрываются
    for name in ["owner-personal", "owner-solution"] {
        let o = vault_call(&a, &b_tok, &hdr, json!({"action": "get", "name": name})).await;
        assert!(o.is_error, "{name}: {}", o.text);
        assert!(!o.text.contains("val-owner"), "{}", o.text);
    }
    let o = vault_call(
        &a,
        &b_tok,
        &hdr,
        json!({"action": "get", "name": "member-solution"}),
    )
    .await;
    assert!(
        !o.is_error && o.text.contains("val-member-solution"),
        "{}",
        o.text
    );
}

/// Настоящий процесс с `log.level = debug`: значение секрета, прошедшее через `memory_vault`
/// `set` и `get`, не попадает ни в журнал, ни в stderr (`rmcp` пишет JSON-RPC на debug —
/// он ограничен `warn`).
#[test]
fn mcp_vault_value_not_in_debug_log() {
    use common::process::ServerProc;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::{Duration, Instant};

    let root = common::tmp("mcp-log");
    let data = root.join("data");
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, common::qdrant::free_port()));
    let mut c = common::cfg_in(data.clone());
    c.server.bind = addr;
    c.log.level = "debug".into();
    c.log.format = zam_server::config::LogFormat::Text;
    let conf = root.join("zam.toml");
    std::fs::write(&conf, toml::to_string(&c).unwrap()).unwrap();
    let mut srv = ServerProc::start(&conf, addr);
    let token = srv.initial_token(Duration::from_secs(120));
    let t = Instant::now();
    while srv.try_call("GET", "/api/v1/spaces", &token, None).is_err() {
        srv.assert_running("приём соединений");
        assert!(
            t.elapsed() < Duration::from_secs(120),
            "сервер не слушает порт"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let value = "mcp-log-secret-7f3a91";
    let accept = [("Accept", "application/json, text/event-stream")];
    let call = |args: Value| {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": {"name": "memory_vault", "arguments": args}});
        let (code, j) = srv.call_h("POST", "/mcp", &token, Some(&body), &accept);
        assert_eq!(code, 200, "{j}");
        assert_ne!(j["result"]["isError"].as_bool(), Some(true), "{j}");
        j["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    };
    call(json!({"action": "set", "name": "github", "value": value}));
    assert!(call(json!({"action": "get", "name": "github"})).contains(value));
    // журнал пишется фоновым потоком: kill без штатной остановки — сначала дождаться записи
    common::process::wait_server_log(&data, "DEBUG", Duration::from_secs(5));
    srv.kill();
    let mut log = String::new();
    for e in std::fs::read_dir(data.join("logs")).unwrap().flatten() {
        if e.file_name()
            .to_string_lossy()
            .starts_with("zam-server.log")
        {
            log.push_str(&String::from_utf8_lossy(&std::fs::read(e.path()).unwrap()));
        }
    }
    // журнал содержателен: debug-строки сервера в нём есть
    assert!(log.contains("DEBUG"), "журнал без debug-строк");
    assert!(!log.contains(value), "значение Vault в журнале");
    assert!(!srv.tail().contains(value), "значение Vault в stderr");
}

/// M4.5 (принцип 0.2): новые виды и действия идут через прежние инструменты; M5.6 (§9, решение
/// §15 п. 3) — 11-й инструмент `memory_code`. Инструкция `initialize` называет рабочую память.
#[tokio::test]
async fn mcp_tool_count_11() {
    let a = api("mcp-count-m56", |_| {}).await;
    let list = rpc(&a, &a.token, &[], "tools/list", json!({})).await;
    assert_eq!(list["tools"].as_array().unwrap().len(), 11, "{list}");
    let code = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "memory_code")
        .unwrap_or_else(|| panic!("нет memory_code: {list}"));
    assert_eq!(code["annotations"]["readOnlyHint"], true, "{code}");
    assert_eq!(
        code["inputSchema"]["properties"]["action"]["enum"],
        json!(["map", "symbol", "refs", "impact"]),
        "{code}"
    );
    assert_eq!(code["inputSchema"]["required"], json!(["action"]), "{code}");
    let init = rpc(
        &a,
        &a.token,
        &[],
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {},
               "clientInfo": {"name": "test", "version": "1"}}),
    )
    .await;
    let s = init["instructions"].as_str().unwrap_or_default();
    for want in [
        "kind=task|question|term",
        "event=recovery",
        "event=brief",
        "kind=report",
    ] {
        assert!(s.contains(want), "{want}: {s}");
    }
}

/// Описания инструментов — до 300 символов и перечисляют новые виды и действия M4.5.
#[tokio::test]
async fn mcp_descriptions_under_300() {
    let a = api("mcp-desc-m45", |_| {}).await;
    let list = rpc(&a, &a.token, &[], "tools/list", json!({})).await;
    let desc = |name: &str| {
        list["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .and_then(|t| t["description"].as_str())
            .unwrap_or_else(|| panic!("{name}"))
            .to_string()
    };
    for t in TOOLS {
        let d = desc(t);
        assert!(
            d.chars().count() <= 300,
            "{t}: {} символов",
            d.chars().count()
        );
    }
    // поля идей `memory_update_entry` — тоже до 300 символов (ревью задачи 7 M5.7, M4)
    let upd = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "memory_update_entry")
        .unwrap();
    for k in ["option", "text", "decision", "horizon", "space", "by"] {
        let n = upd["inputSchema"]["properties"][k]["description"]
            .as_str()
            .unwrap_or("")
            .chars()
            .count();
        assert!(n <= 300, "memory_update_entry.{k}: {n} символов");
    }
    let need: [(&str, &[&str]); 7] = [
        (
            "memory_log",
            &[
                "term",
                "task",
                "question",
                "refs.files",
                "report",
                "images",
                "zam report",
            ],
        ),
        (
            "memory_update_entry",
            &["status", "claim", "release", "answer"],
        ),
        ("memory_recall", &["recovery", "brief"]),
        (
            "memory_search",
            &[
                "term",
                "task",
                "question",
                "file",
                "decided_by",
                "attachment",
                "a:",
            ],
        ),
        ("memory_link", &["contradicts"]),
        ("memory_vault", &["host", "environment"]),
        (
            "memory_code",
            &["map", "focus", "symbol", "refs", "impact", "путь#qual"],
        ),
    ];
    for (tool, words) in need {
        let d = desc(tool);
        for w in words {
            assert!(d.contains(w), "{tool}: нет «{w}» в «{d}»");
        }
    }
    let read = desc("memory_read");
    assert!(read.contains("since_rev"), "{read}");
}

/// Окружение через MCP (задача 13, раунд 1): `set` записи `host` без значения с открытыми
/// полями; `get` и `list` их видят; обновление `limits` сохраняет прежние поля и значение;
/// без значения у прочих типов — ошибка.
#[tokio::test]
async fn mcp_vault_set_environment_fields() {
    let a = api("mcp-vault-env-set", |_| {}).await;
    let h = sess();
    let o = vault_call(
        &a,
        &a.token,
        &h,
        json!({"action": "set", "name": "stage-box", "type": "host",
               "purpose": "приёмочный стенд", "address": "10.0.0.7:22"}),
    )
    .await;
    assert!(!o.is_error && o.text.contains("host"), "{}", o.text);
    let get = json!({"action": "get", "name": "stage-box"});
    let o = vault_call(&a, &a.token, &h, get.clone()).await;
    assert!(
        o.text.contains("purpose: приёмочный стенд") && o.text.contains("address: 10.0.0.7:22"),
        "{}",
        o.text
    );
    assert!(!o.text.contains("value:"), "{}", o.text);
    let o = vault_call(
        &a,
        &a.token,
        &h,
        json!({"action": "list", "name": "приёмочный"}),
    )
    .await;
    assert!(o.text.contains("stage-box"), "{}", o.text);
    // обновление одного поля: прежние сохраняются
    let o = vault_call(
        &a,
        &a.token,
        &h,
        json!({"action": "set", "name": "stage-box", "limits": "2 ядра, 2 ГБ"}),
    )
    .await;
    assert!(!o.is_error && o.text.contains("rev2"), "{}", o.text);
    let o = vault_call(&a, &a.token, &h, get.clone()).await;
    for want in [
        "purpose: приёмочный стенд",
        "address: 10.0.0.7:22",
        "limits: 2 ядра, 2 ГБ",
    ] {
        assert!(o.text.contains(want), "{want}: {}", o.text);
    }
    // значение позже — поля остаются, значение раскрывается
    let o = vault_call(
        &a,
        &a.token,
        &h,
        json!({"action": "set", "name": "stage-box", "value": "ssh-pass-1"}),
    )
    .await;
    assert!(!o.is_error, "{}", o.text);
    let o = vault_call(&a, &a.token, &h, get).await;
    assert!(
        o.text.contains("value: ssh-pass-1") && o.text.contains("limits: 2 ядра, 2 ГБ"),
        "{}",
        o.text
    );
    // пароль без значения — ошибка инструмента; пустая правка — тоже
    let o = vault_call(
        &a,
        &a.token,
        &h,
        json!({"action": "set", "name": "db-pass", "type": "password"}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    let o = vault_call(
        &a,
        &a.token,
        &h,
        json!({"action": "set", "name": "stage-box"}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
}

/// Параллельные `set` разных полей одной записи не теряют правки молча (задача 14, ревью
/// задачи 13): PATCH уходит с `If-Match` прочитанной ревизии. Каждый успешный `set` оставляет
/// своё поле, проигравший получает понятную ошибку «Запись изменилась».
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_vault_set_concurrent_no_lost_update() {
    let a = api("mcp-vault-set-race", |_| {}).await;
    let h = sess();
    let o = vault_call(
        &a,
        &a.token,
        &h,
        json!({"action": "set", "name": "race-box", "type": "host", "purpose": "начало"}),
    )
    .await;
    assert!(!o.is_error, "{}", o.text);
    let fields = ["address", "limits", "notes", "purpose"];
    let call = |k: &'static str| {
        let (a, h) = (&a, &h);
        async move {
            let o = vault_call(
                a,
                &a.token,
                h,
                json!({"action": "set", "name": "race-box", k: format!("значение-{k}")}),
            )
            .await;
            (k, o)
        }
    };
    let (r0, r1, r2, r3) = tokio::join!(
        call(fields[0]),
        call(fields[1]),
        call(fields[2]),
        call(fields[3])
    );
    let results = [r0, r1, r2, r3];
    let get = vault_call(
        &a,
        &a.token,
        &h,
        json!({"action": "get", "name": "race-box"}),
    )
    .await;
    for (k, o) in &results {
        if o.is_error {
            assert!(o.text.contains("Запись изменилась"), "{k}: {}", o.text);
        } else {
            assert!(
                get.text.contains(&format!("{k}: значение-{k}")),
                "успешный set {k} потерян: {}",
                get.text
            );
        }
    }
    assert!(results.iter().any(|(_, o)| !o.is_error));
}

/// `memory_update_entry action=answer` без `by` — ошибка инструмента с подсказкой, вопрос не
/// тронут; с `by: agent` — решение `hypothesis` (финальное ревью M4.5, I-2).
#[tokio::test]
async fn mcp_answer_requires_by() {
    let a = api("mcp-answer-by", |_| {}).await;
    let q = ok(
        &a,
        &a.token,
        &sess(),
        "memory_log",
        json!({"kind": "question", "title": "Чей выбор?", "space": "knowledge", "format": "json"}),
    )
    .await
    .json();
    let qid = q["id"].as_str().unwrap().to_string();
    let o = tool(
        &a,
        &a.token,
        &sess(),
        "memory_update_entry",
        json!({"id": format!("q:{qid}"), "action": "answer", "text": "так"}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    assert!(
        o.text.contains("by: human") && o.text.contains("by: agent"),
        "{}",
        o.text
    );
    let r = a
        .t(Method::GET, &format!("/api/v1/entries/{qid}"), None)
        .await;
    assert_eq!(r.json()["status"], "open");
    let o = ok(
        &a,
        &a.token,
        &sess(),
        "memory_update_entry",
        json!({"id": format!("q:{qid}"), "action": "answer", "text": "так", "by": "agent",
               "format": "json"}),
    )
    .await
    .json();
    assert_eq!(o["decision"]["certainty"], "hypothesis", "{o}");
}

/// M5, задача 7: `memory_read` читает вложение по `locator` (`locator` — в схеме элемента,
/// описание упоминает `a:`, `b:` и `locator`, до 300 символов); `locator` у заметки — ошибка
/// инструмента с `locator-invalid`; инструментов 10 (с M5.6 — 11: `memory_code`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_read_locator() {
    let a = api("mcp-read-locator", |_| {}).await;
    let list = rpc(&a, &a.token, &[], "tools/list", json!({})).await;
    let tools = list["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 11, "{list}");
    let t = tools.iter().find(|t| t["name"] == "memory_read").unwrap();
    let props = &t["inputSchema"]["properties"]["items"]["items"]["properties"];
    assert_eq!(props["locator"]["type"], "string", "{t}");
    let d = t["description"].as_str().unwrap();
    assert!(d.chars().count() <= 300, "{} символов", d.chars().count());
    for w in ["a:", "b:", "locator"] {
        assert!(d.contains(w), "нет «{w}»: {d}");
    }
    let n = a
        .note("knowledge", "Доклады/Квартал", "# Квартал\n\nИтоги.\n")
        .await;
    let id = n["id"].as_str().unwrap();
    let deck = common::attach::pptx(&[
        ("Intro", "nothing special"),
        ("Second", "cobalt walrus migration"),
    ]);
    common::attach::upload(&a, &a.token, id, "квартал.pptx", &deck).await;
    let d = common::attach::wait_done(&a, a.st.admin_id, id, "квартал.pptx").await;
    let b = format!("b:{}", d.blob_sha256);
    let o = ok(
        &a,
        &a.token,
        &sess(),
        "memory_read",
        json!({"items": [{"id": &b, "locator": "slide:2"}]}),
    )
    .await;
    assert!(o.text.contains("cobalt walrus"), "{}", o.text);
    assert!(!o.text.contains("nothing special"), "{}", o.text);
    assert!(o.text.contains("квартал.pptx · слайд 2"), "{}", o.text);
    // без locator — оглавление
    let o = ok(
        &a,
        &a.token,
        &[],
        "memory_read",
        json!({"items": [{"id": &b}]}),
    )
    .await;
    assert!(
        o.text.contains("slide:2") && o.text.contains("Second"),
        "{}",
        o.text
    );
    // locator у заметки — ошибка инструмента
    let o = tool(
        &a,
        &a.token,
        &[],
        "memory_read",
        json!({"items": [{"id": format!("n:{id}"), "locator": "page:1"}]}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    assert!(o.text.contains("locator"), "{}", o.text);
}

/// `memory_read` с `focus` (M6 §6, задача 7b; zero-agent: раньше — 400 `unknown field focus`):
/// поле объявлено в схеме, ответ — оглавление, лист и пропуски; без текста — ошибка инструмента.
#[tokio::test]
async fn mcp_read_focus() {
    let a = api("mcp-read-focus", |_| {}).await;
    let list = rpc(&a, &a.token, &[], "tools/list", json!({})).await;
    let t = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "memory_read")
        .unwrap();
    let props = &t["inputSchema"]["properties"]["items"]["items"]["properties"];
    assert_eq!(props["focus"]["type"], "string", "{t}");
    assert_eq!(props["range"]["type"], "string", "{t}");
    assert!(
        props["tier"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("focus")),
        "{t}"
    );
    let mut text = String::from("# Справочник\n\n");
    for i in 1..=8 {
        text.push_str(&format!("## Глава {i}\n\n"));
        for k in 0..12 {
            text.push_str(&format!(
                "Абзац {k} главы {i} рассказывает о настройке и обслуживании сервера. "
            ));
        }
        if i == 6 {
            text.push_str("Здесь описан протокол квазиметрии. ");
        }
        text.push_str("\n\n");
    }
    let n = a.note("knowledge", "Справочник", &text).await;
    let id = n["id"].as_str().unwrap();
    let o = ok(
        &a,
        &a.token,
        &sess(),
        "memory_read",
        json!({"items": [{"id": format!("n:{id}"), "focus": "протокол квазиметрии"}],
               "budget_tokens": 20000}),
    )
    .await;
    assert!(o.text.contains("протокол квазиметрии"), "{}", o.text);
    assert!(o.text.contains("Оглавление: "), "{}", o.text);
    assert!(o.text.contains("*Глава 6"), "{}", o.text);
    assert!(
        o.text.contains("[пропущено: range:r1:0-")
            && o.text.contains(" (Справочник — Справочник > Глава "),
        "{}",
        o.text
    );
    assert!(!o.text.contains("главы 2 "), "{}", o.text);
    let o = tool(
        &a,
        &a.token,
        &[],
        "memory_read",
        json!({"items": [{"id": format!("n:{id}"), "tier": "focus"}]}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    assert!(o.text.contains("нужен текст focus"), "{}", o.text);
}

// ---- отчёты (M5.5, задача 5) ----

/// PNG-подобные байты: сигнатура и `n` байт по `seed`.
fn png(seed: u8, n: usize) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend((0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)));
    v
}

fn b64(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// Аргументы `memory_log` отчёта с одной картинкой `figs/loss.png`.
fn report_args(title: &str, img: &[u8]) -> Value {
    json!({"kind": "report", "title": title,
           "body": "# Итог\n\nВывод kvazirep: точность выросла.\n\n![loss](figs/loss.png)\n",
           "images": [{"name": "figs/loss.png", "data": b64(img)}], "tags": ["ml"]})
}

/// Id отчёта из адреса `…/j/reports/<id>` в тексте.
fn report_id(text: &str) -> String {
    let at = text
        .find("/j/reports/")
        .unwrap_or_else(|| panic!("нет адреса: {text}"));
    text[at + "/j/reports/".len()..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect()
}

async fn report_count(a: &Api) -> i64 {
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    db.read(|c| {
        Ok(c.query_row(
            "SELECT count(*) FROM entries WHERE kind = 'report'",
            [],
            |r| r.get::<_, i64>(0),
        )?)
    })
    .await
    .unwrap()
}

/// `memory_log kind=report`: картинки base64, ответ — только ссылка WebUI (без тела и `p:`),
/// `format: json` — `{id, url}`.
#[tokio::test]
async fn mcp_log_report_base64_images() {
    let a = api("mcp-report", |_| {}).await;
    assert_eq!(register(&a, &a.token).await, StatusCode::CREATED);
    let hdr = [("x-zam-solution", G)];
    let img = png(1, 300);
    let o = ok(
        &a,
        &a.token,
        &hdr,
        "memory_log",
        report_args("Итог обучения", &img),
    )
    .await;
    assert!(
        o.text
            .starts_with("Отчёт записан: http://memory.lan:7700/#/u/"),
        "{}",
        o.text
    );
    assert!(o.text.contains(&format!("/s/{G}/j/reports/")), "{}", o.text);
    let id = report_id(&o.text);
    let pid = format!("p:{id}");
    for leak in ["kvazirep", "Итог обучения", pid.as_str(), "figs/loss.png"] {
        assert!(!o.text.contains(leak), "{leak}: {}", o.text);
    }
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let files = zam_store::entry_attachments(&db, id.parse().unwrap())
        .await
        .unwrap();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(
        (files[0].filename.as_str(), files[0].mime.as_str()),
        ("loss.png", "image/png")
    );
    assert_eq!(files[0].size, img.len() as u64);
    // JSON — ровно {id, url}
    let mut args = report_args("Второй", &img);
    args["format"] = json!("json");
    let o = ok(&a, &a.token, &hdr, "memory_log", args).await;
    let j = o.json();
    assert_eq!(j.as_object().unwrap().len(), 2, "{j}");
    assert!(
        j["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/j/reports/{}", j["id"].as_str().unwrap())),
        "{j}"
    );
    assert_eq!(report_count(&a).await, 2);
}

/// Больше `reports.mcp_max_bytes` — ошибка инструмента 413 с подсказкой `zam report`; тело
/// больше предела `/mcp` — 413 транспорта с той же подсказкой; отчёт не создаётся.
#[tokio::test]
async fn mcp_log_report_over_limit_hint_cli() {
    let a = api("mcp-report-limit", |c| {
        c.limits.note_max_bytes = 1000;
        c.reports.mcp_max_bytes = 2048;
    })
    .await;
    assert_eq!(register(&a, &a.token).await, StatusCode::CREATED);
    let hdr = [("x-zam-solution", G)];
    let o = tool(
        &a,
        &a.token,
        &hdr,
        "memory_log",
        report_args("Большой", &png(2, 4096)),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    assert!(o.text.contains("413"), "{}", o.text);
    assert!(o.text.contains("zam report"), "{}", o.text);
    // транспорт: тело больше предела /mcp
    let r = rpc_raw(
        &a,
        Some(&format!("Bearer {}", a.token)),
        &hdr,
        "tools/call",
        json!({"name": "memory_log",
               "arguments": report_args("Огромный", &png(3, 100 * 1024))}),
    )
    .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
    assert!(r.text().contains("zam report"), "{}", r.text());
    assert_eq!(report_count(&a).await, 0);
}

/// Запрос `tools/call` к `/mcp` с телом потоком: `at` байт сразу, остальное — после сигнала.
fn gated_mcp(
    tok: &str,
    body: String,
    at: usize,
) -> (Request<Body>, tokio::sync::oneshot::Sender<()>) {
    use futures_util::StreamExt as _;
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let head = body.as_bytes()[..at].to_vec();
    let rest = body.as_bytes()[at..].to_vec();
    let s = futures_util::stream::once(async move {
        Ok::<_, std::io::Error>(axum::body::Bytes::from(head))
    })
    .chain(futures_util::stream::once(async move {
        let _ = rx.await;
        Ok::<_, std::io::Error>(axum::body::Bytes::from(rest))
    }));
    let r = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::HOST, "memory.lan:7700")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {tok}"))
        .header("x-zam-solution", G)
        .body(Body::from_stream(s))
        .unwrap();
    (r, tx)
}

/// Сквозной тест семафора больших тел (ревью задачи 2, Minor 3): два больших MCP-отчёта
/// одновременно держат оба разрешения — их внутренние `POST /entries` второго разрешения не
/// берут и проходят; третий большой запрос в это время — 503 `busy`.
#[tokio::test]
async fn mcp_two_big_reports_parallel_third_busy() {
    use tower::ServiceExt as _;
    let a = api("mcp-report-parallel", |c| c.limits.note_max_bytes = 1000).await;
    assert_eq!(register(&a, &a.token).await, StatusCode::CREATED);
    let threshold = a.st.big_bodies.threshold();
    let call = |title: &str, seed: u8| {
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": "memory_log",
                          "arguments": report_args(title, &png(seed, 80 * 1024))}})
        .to_string()
    };
    let mut gates = Vec::new();
    let mut tasks = Vec::new();
    for (i, title) in ["Первый", "Второй"].iter().enumerate() {
        let body = call(title, i as u8);
        assert!(body.len() > threshold + 10);
        let (req, tx) = gated_mcp(&a.token, body, threshold + 10);
        gates.push(tx);
        let app = a.app.clone();
        tasks.push(tokio::spawn(async move {
            let r = app.oneshot(req).await.unwrap();
            let status = r.status();
            let b = http_body_util::BodyExt::collect(r.into_body())
                .await
                .unwrap()
                .to_bytes();
            (status, String::from_utf8_lossy(&b).into_owned())
        }));
    }
    // оба тела прочитаны за порог — оба разрешения заняты
    let t0 = std::time::Instant::now();
    while a.st.big_bodies.available() > 0 {
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(30),
            "разрешения не заняты"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let r = rpc_raw(
        &a,
        Some(&format!("Bearer {}", a.token)),
        &[("x-zam-solution", G)],
        "tools/call",
        json!({"name": "memory_log",
               "arguments": report_args("Третий", &png(9, 80 * 1024))}),
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert_eq!(r.json()["type"], "/problems/busy");
    for g in gates {
        g.send(()).unwrap();
    }
    for t in tasks {
        let (status, text) = t.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{text}");
        let j: Value = serde_json::from_str(&text).unwrap();
        let out = j["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert_ne!(j["result"]["isError"], json!(true), "{out}");
        assert!(out.starts_with("Отчёт записан: "), "{out}");
    }
    assert_eq!(report_count(&a).await, 2);
    assert_eq!(a.st.big_bodies.available(), 2, "разрешения освобождены");
}

/// Внутренний вызов без внешнего разрешения (малое тело `/mcp` → большое внутреннее) берёт
/// разрешение сам: при занятых — 503; с `outer_permit` — проходит.
#[tokio::test]
async fn inner_call_without_outer_permit_takes_permit() {
    use zam_server::api::big_body::InnerCall;
    let a = api("mcp-inner-permit", |c| c.limits.note_max_bytes = 1000).await;
    assert_eq!(register(&a, &a.token).await, StatusCode::CREATED);
    let body = json!({"kind": "report", "title": "Внутренний", "body": "![a](a.png)",
                      "images": [{"name": "a.png", "data": b64(&png(4, 80 * 1024))}]})
    .to_string();
    let req = |outer: bool| {
        let mut r = Request::post(format!("/api/v1/spaces/{G}/entries"))
            .header(header::AUTHORIZATION, format!("Bearer {}", a.token))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.clone()))
            .unwrap();
        r.extensions_mut().insert(InnerCall {
            outer_permit: outer,
        });
        r
    };
    let p1 = a.st.big_bodies.admit().await.unwrap();
    let p2 = a.st.big_bodies.admit().await.unwrap();
    let r = a.send(req(false)).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    let r = a.send(req(true)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    drop((p1, p2));
    let r = a.send(req(false)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

/// Ревью задачи 5, I1: rmcp выполняет инструмент в своей задаче — обрыв клиента после чтения
/// большого тела уничтожает слой `limit_mcp`, но разрешение держит вызов инструмента: оно
/// освобождается не раньше, чем инструмент закончил (отчёт после освобождения не появляется).
#[tokio::test]
async fn mcp_aborted_big_request_keeps_permit_until_tool_done() {
    use tower::ServiceExt as _;
    let a = api("mcp-report-abort", |c| c.limits.note_max_bytes = 1000).await;
    assert_eq!(register(&a, &a.token).await, StatusCode::CREATED);
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": "memory_log",
                                 "arguments": report_args("Оборванный", &png(5, 3 * 1024 * 1024))}})
    .to_string();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::HOST, "memory.lan:7700")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {}", a.token))
        .header("x-zam-solution", G)
        .body(Body::from(body))
        .unwrap();
    let app = a.app.clone();
    let h = tokio::spawn(async move { app.oneshot(req).await });
    // тело прочитано за порог — разрешение занято
    let t0 = std::time::Instant::now();
    while a.st.big_bodies.available() == 2 {
        if report_count(&a).await > 0 {
            eprintln!("обработка закончилась раньше обрыва — проверка не состоялась");
            return;
        }
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(60),
            "разрешение не занято"
        );
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    h.abort();
    // разрешение вернулось — инструмент закончил: позже отчёт появиться не может
    let t0 = std::time::Instant::now();
    while a.st.big_bodies.available() < 2 {
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(60),
            "разрешение не вернулось"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let at_release = report_count(&a).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert_eq!(
        report_count(&a).await,
        at_release,
        "инструмент записал отчёт после освобождения разрешения"
    );
}

// ---- memory_code (M5.6 §9, §17 п. 11) ----

/// Символ карты: `qual` вида `kind` в строке `line`, тело — по строке.
fn code_sym(qual: &str, kind: &str, line: u32) -> Value {
    let name = qual.rsplit("::").next().unwrap();
    json!({"name": name, "qual": qual, "kind": kind, "line": line, "end_line": line + 2,
           "sig": format!("fn {qual}()"), "body_hash": format!("{line:016x}"), "exported": true})
}

/// Файл загрузки карты: путь, номер хэша, символы, ссылки `(name, count)`.
fn code_file(path: &str, n: u64, symbols: Vec<Value>, refs: &[(&str, u32)]) -> Value {
    let refs: Vec<Value> = refs
        .iter()
        .map(|(name, count)| json!({"name": name, "kind": "call", "count": count, "first_line": 1}))
        .collect();
    json!({"path": path, "lang": "rust", "hash": format!("{n:064x}"), "size": 100,
           "parse_state": "ok", "symbols": symbols, "refs": refs})
}

async fn code_put(a: &Api, files: Vec<Value>) {
    let r = a
        .t(
            Method::PUT,
            "/api/v1/code/files",
            Some(
                json!({"solution": G, "commit": "abcdef1", "branch": "main", "machine": "box",
                        "files": files}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "загрузка карты: {}", r.text());
}

/// Решение `G` с картой: `a.rs` — `foo`, `bar`, `baz` (ссылается на `helper`), `b.rs`
/// ссылается на `foo` ×3, `c.rs` — на `baz`; заметки о `a.rs#foo` и `a.rs#bar`, insight о файле
/// `a.rs`.
struct CodeWorld {
    a: Api,
    foo_note: String,
    bar_note: String,
    insight: String,
}

async fn code_world(name: &str) -> CodeWorld {
    code_world_with(name, |_| {}).await
}

async fn code_world_with(
    name: &str,
    tweak: impl FnOnce(&mut zam_server::config::ServerConfig),
) -> CodeWorld {
    let a = api(&format!("mcp-code-{name}"), tweak).await;
    assert_eq!(register(&a, &a.token).await, StatusCode::CREATED);
    code_put(
        &a,
        vec![
            code_file(
                "a.rs",
                1,
                vec![
                    code_sym("foo", "function", 1),
                    code_sym("bar", "function", 10),
                    code_sym("baz", "function", 20),
                ],
                &[("helper", 1)],
            ),
            code_file(
                "b.rs",
                2,
                vec![code_sym("user_b", "function", 1)],
                &[("foo", 3)],
            ),
            code_file(
                "c.rs",
                3,
                vec![code_sym("user_c", "function", 1)],
                &[("baz", 1)],
            ),
        ],
    )
    .await;
    let note = |path: &'static str, r: &'static str| {
        let a = &a;
        async move {
            let n = a
                .note(
                    G,
                    path,
                    &format!("---\nrefs: [\"{r}\"]\n---\nтекст о коде\n"),
                )
                .await;
            n["id"].as_str().unwrap().to_string()
        }
    };
    let foo_note = note("О foo", "a.rs#foo").await;
    let bar_note = note("О bar", "a.rs#bar").await;
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/spaces/{G}/entries"),
            Some(
                json!({"kind": "insight", "title": "Файл a.rs", "body": "о файле a.rs",
                        "data": {"kind": "gotcha"}, "refs": {"files": [{"path": "a.rs"}]},
                        "force": true}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let insight = r.json()["id"].as_str().unwrap().to_string();
    CodeWorld {
        a,
        foo_note,
        bar_note,
        insight,
    }
}

const SOL: [(&str, &str); 1] = [("x-zam-solution", G)];

/// Текст ответа без строк `> warning:` и `~N tok` в конце.
fn body_of(text: &str) -> String {
    let mut lines: Vec<&str> = text.trim_end().lines().collect();
    while lines
        .last()
        .is_some_and(|l| l.starts_with("> warning:") || (l.starts_with('~') && l.ends_with(" tok")))
    {
        lines.pop();
    }
    lines.join("\n")
}

/// §17 п. 11: `memory_code action=map` — та же карта, что `GET /code/map` (текст до строки
/// `~N tok`), в том числе с бюджетом, фокусом и областью; `format: json` — JSON ответа REST.
#[tokio::test]
async fn mcp_code_map_same_as_rest() {
    let w = code_world("map").await;
    let a = &w.a;
    let rest_text = |q: String| async move {
        let r = a
            .t(
                Method::GET,
                &format!("/api/v1/code/map?solution={G}{q}"),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        r.json()["text"].as_str().unwrap().to_string()
    };
    let focus = json!({"paths": ["b.rs"], "idents": ["foo"]});
    for (args, q) in [
        (json!({"action": "map"}), String::new()),
        (
            json!({"action": "map", "budget_tokens": 200}),
            "&budget_tokens=200".to_string(),
        ),
        (
            json!({"action": "map", "focus": focus, "budget_tokens": 500}),
            format!(
                "&budget_tokens=500&focus={}",
                common::api::enc(&focus.to_string())
            ),
        ),
        (
            json!({"action": "map", "path": "a.rs"}),
            "&area=a.rs".to_string(),
        ),
    ] {
        let o = ok(a, &a.token, &SOL, "memory_code", args.clone()).await;
        let want = rest_text(q).await;
        assert!(want.starts_with("# Карта кода: 3 файла"), "{want}");
        assert_eq!(body_of(&o.text), want.trim_end(), "{args}");
    }
    // фокус называет заголовок; JSON — как у REST
    let o = ok(
        a,
        &a.token,
        &SOL,
        "memory_code",
        json!({"action": "map", "focus": {"paths": ["b.rs"]}, "format": "json"}),
    )
    .await;
    let j = o.json();
    assert_eq!(j["format"], "json", "{j}");
    assert!(j["header"].as_str().unwrap().contains("с фокусом"), "{j}");
    assert_eq!(j["files"][0]["path"], "b.rs", "{j}");
    // неверный action и фокус — ошибка инструмента, а не JSON-RPC
    for bad in [
        json!({}),
        json!({"action": "graph"}),
        json!({"action": "map", "focus": "b.rs"}),
    ] {
        let o = tool(a, &a.token, &SOL, "memory_code", bad.clone()).await;
        assert!(o.is_error, "{bad}: {}", o.text);
    }
}

/// `symbol`: одно точное совпадение — карточка (определение, кто ссылается, знания о символе и
/// файле, без чужого символа); `путь#qual` — та же карточка; несколько — список адресов;
/// `refs` — только «кто ссылается».
#[tokio::test]
async fn mcp_code_symbol_card_and_refs() {
    let w = code_world("symbol").await;
    let a = &w.a;
    let call = |args: Value| async move { ok(a, &a.token, &SOL, "memory_code", args).await.text };
    let card = call(json!({"action": "symbol", "query": "foo"})).await;
    for want in [
        "`a.rs#foo`",
        "function",
        "fn foo()",
        "b.rs ×3",
        "helper",
        w.foo_note.as_str(),
        w.insight.as_str(),
    ] {
        assert!(card.contains(want), "{want}: {card}");
    }
    assert!(!card.contains(&w.bar_note), "чужой символ: {card}");
    // знание о символе — раньше знания о файле
    assert!(
        card.find(&w.foo_note).unwrap() < card.find(&w.insight).unwrap(),
        "{card}"
    );
    let by_ref = call(json!({"action": "symbol", "query": "a.rs#foo"})).await;
    assert_eq!(body_of(&by_ref), body_of(&card));
    // JSON карточки — ответ REST
    let j = ok(
        a,
        &a.token,
        &SOL,
        "memory_code",
        json!({"action": "symbol", "query": "a.rs#foo", "format": "json"}),
    )
    .await
    .json();
    assert_eq!(j["symbol"]["ref"], "a.rs#foo", "{j}");
    assert_eq!(j["callers"][0]["path"], "b.rs", "{j}");
    // кто ссылается — без знаний и того, на что ссылается файл
    let refs = call(json!({"action": "refs", "query": "foo"})).await;
    assert!(
        refs.contains("`a.rs#foo`") && refs.contains("b.rs ×3"),
        "{refs}"
    );
    assert!(
        !refs.contains(&w.foo_note) && !refs.contains("helper"),
        "{refs}"
    );
    let none = call(json!({"action": "refs", "query": "a.rs#bar"})).await;
    assert!(none.contains("не найдено"), "{none}");
    // второй `foo` (метод) — два точных совпадения: список адресов, не карточка
    code_put(
        a,
        vec![code_file(
            "d.rs",
            4,
            vec![code_sym("Other::foo", "method", 5)],
            &[],
        )],
    )
    .await;
    let list = call(json!({"action": "symbol", "query": "foo"})).await;
    assert!(
        list.contains("`a.rs#foo`") && list.contains("`d.rs#Other::foo`"),
        "{list}"
    );
    assert!(!list.contains(&w.foo_note), "список без знаний: {list}");
    // path сужает до одного — карточка
    let one = call(json!({"action": "symbol", "query": "foo", "path": "d.rs"})).await;
    assert!(
        one.contains("`d.rs#Other::foo`") && one.contains("method"),
        "{one}"
    );
    assert!(!one.contains("`a.rs#foo`"), "{one}");
    // нет символа — не ошибка; без query — ошибка аргументов
    let t = call(json!({"action": "symbol", "query": "nope_xyz"})).await;
    assert!(t.contains("не найден"), "{t}");
    let t = call(json!({"action": "symbol", "query": "a.rs#nope_xyz"})).await;
    assert!(t.contains("не найден"), "{t}");
    let o = tool(
        a,
        &a.token,
        &SOL,
        "memory_code",
        json!({"action": "symbol"}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
}

/// `impact` по MCP: путь без хэшей символов — все знания пути; `files` с символами — изменённые,
/// удалённые и потребители на 1 шаг.
#[tokio::test]
async fn mcp_code_impact_paths_without_hashes() {
    let w = code_world("impact").await;
    let a = &w.a;
    let o = ok(
        a,
        &a.token,
        &SOL,
        "memory_code",
        json!({"action": "impact", "path": "a.rs"}),
    )
    .await
    .text;
    for want in [&w.foo_note, &w.bar_note, &w.insight] {
        assert!(o.contains(want.as_str()), "{want}: {o}");
    }
    // foo — другое тело, bar — то же, baz — удалён
    let o = ok(
        a,
        &a.token,
        &SOL,
        "memory_code",
        json!({"action": "impact", "files": [{"path": "a.rs", "symbols": [
            {"qual": "foo", "body_hash": "ffffffffffffffff"},
            {"qual": "bar", "body_hash": format!("{:016x}", 10)}]}]}),
    )
    .await;
    let t = &o.text;
    assert!(t.contains("`a.rs#foo`") && t.contains("`a.rs#baz`"), "{t}");
    assert!(t.contains("b.rs") && t.contains("c.rs"), "потребители: {t}");
    assert!(t.contains(&w.foo_note) && t.contains(&w.insight), "{t}");
    assert!(!t.contains(&w.bar_note), "bar не изменён: {t}");
    let j = ok(
        a,
        &a.token,
        &SOL,
        "memory_code",
        json!({"action": "impact", "path": "a.rs", "format": "json"}),
    )
    .await
    .json();
    assert_eq!(j["paths_only"], json!(["a.rs"]), "{j}");
    // ни path, ни files — ошибка аргументов; без решения — ошибка
    let o = tool(
        a,
        &a.token,
        &SOL,
        "memory_code",
        json!({"action": "impact"}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    let o = tool(
        a,
        &a.token,
        &[],
        "memory_code",
        json!({"action": "impact", "path": "a.rs"}),
    )
    .await;
    assert!(
        o.is_error && o.text.contains("X-Zam-Solution"),
        "{}",
        o.text
    );
}

/// Права — как у REST: токен `ro` владельца читает карту, символ и влияние; без решения в
/// заголовке — 400 маршрута как ошибка инструмента; Knowledge — 422.
#[tokio::test]
async fn mcp_code_ro_token_reads() {
    let w = code_world("ro").await;
    let a = &w.a;
    let ro = a.issue(&a.admin_id(), "ro").await;
    for args in [
        json!({"action": "map"}),
        json!({"action": "symbol", "query": "foo"}),
        json!({"action": "refs", "query": "a.rs#foo"}),
        json!({"action": "impact", "path": "a.rs"}),
    ] {
        let o = ok(a, &ro, &SOL, "memory_code", args.clone()).await;
        assert!(o.text.contains("a.rs"), "{args}: {}", o.text);
    }
    let o = tool(a, &ro, &[], "memory_code", json!({"action": "map"})).await;
    assert!(o.is_error && o.text.contains("400"), "{}", o.text);
    let o = tool(
        a,
        &ro,
        &[("x-zam-solution", "knowledge")],
        "memory_code",
        json!({"action": "map"}),
    )
    .await;
    assert!(o.is_error && o.text.contains("422"), "{}", o.text);
}

/// Инструкция `initialize` называет карту кода одной фразой.
#[tokio::test]
async fn mcp_initialize_mentions_memory_code() {
    let a = api("mcp-init-code", |_| {}).await;
    let init = rpc(
        &a,
        &a.token,
        &[],
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {},
               "clientInfo": {"name": "test", "version": "1"}}),
    )
    .await;
    let s = init["instructions"].as_str().unwrap_or_default();
    assert!(s.contains("карта кода — memory_code"), "{s}");
}

/// Ревью задачи 7 (M1, M2, M4): «символа нет в карте» (свой тип problem REST) — не ошибка, в
/// JSON — `symbol: null`; выключенная карта — честная ошибка; `path` при адресе `путь#qual` —
/// предупреждение; предупреждения поиска (`rank pending`) — и у единственной карточки.
#[tokio::test]
async fn mcp_code_not_found_kinds_and_warnings() {
    let w = code_world_with("nf", |c| c.code.rank_timeout_ms = 0).await;
    let a = &w.a;
    // первый запрос после загрузки: ранги ещё не посчитаны (срок 0 — не ждать)
    let card = ok(
        a,
        &a.token,
        &SOL,
        "memory_code",
        json!({"action": "symbol", "query": "foo"}),
    )
    .await
    .text;
    assert!(card.contains("`a.rs#foo`"), "{card}");
    assert!(card.contains("> warning: rank pending"), "{card}");
    let t = ok(
        a,
        &a.token,
        &SOL,
        "memory_code",
        json!({"action": "symbol", "query": "a.rs#foo", "path": "b.rs"}),
    )
    .await
    .text;
    assert!(
        t.contains("`a.rs#foo`") && t.contains("path «b.rs» не учтён"),
        "{t}"
    );
    let j = ok(
        a,
        &a.token,
        &SOL,
        "memory_code",
        json!({"action": "symbol", "query": "a.rs#nope_xyz", "format": "json"}),
    )
    .await
    .json();
    assert_eq!(j["symbol"], Value::Null, "{j}");
    assert_eq!(j["ref"], "a.rs#nope_xyz", "{j}");
    let j = ok(
        a,
        &a.token,
        &SOL,
        "memory_code",
        json!({"action": "refs", "query": "a.rs#foo", "format": "json"}),
    )
    .await
    .json();
    assert_eq!(j["callers"][0]["path"], "b.rs", "{j}");
    // выключенная карта: 404 маршрута — ошибка инструмента, а не «символ не найден»
    let off = api("mcp-code-off", |c| c.code.enabled = false).await;
    assert_eq!(register(&off, &off.token).await, StatusCode::CREATED);
    for q in ["a.rs#foo", "foo"] {
        let o = tool(
            &off,
            &off.token,
            &SOL,
            "memory_code",
            json!({"action": "symbol", "query": q}),
        )
        .await;
        assert!(o.is_error && o.text.contains("404"), "{q}: {}", o.text);
        assert!(!o.text.contains("в карте не найден"), "{q}: {}", o.text);
    }
}

/// Ревью задачи 7 (M3): списки Markdown карточки и влияния — до 20 строк и «… ещё N»;
/// полный список — `format: json`.
#[tokio::test]
async fn mcp_code_markdown_lists_capped() {
    let w = code_world("cap").await;
    let a = &w.a;
    for i in 0..25 {
        a.note(
            G,
            &format!("О foo {i:02}"),
            "---\nrefs: [\"a.rs#foo\"]\n---\nтекст о коде\n",
        )
        .await;
    }
    let card = ok(
        a,
        &a.token,
        &SOL,
        "memory_code",
        json!({"action": "symbol", "query": "a.rs#foo"}),
    )
    .await
    .text;
    let lines = card
        .lines()
        .filter(|l| l.starts_with("- n:") || l.starts_with("- i:"))
        .count();
    assert_eq!(lines, 20, "{card}");
    assert!(card.contains("Знания (27):"), "{card}");
    assert!(
        card.contains("… ещё 7 (полный список — format: json)"),
        "{card}"
    );
    let o = ok(
        a,
        &a.token,
        &SOL,
        "memory_code",
        json!({"action": "impact", "path": "a.rs"}),
    )
    .await
    .text;
    let lines = o
        .lines()
        .filter(|l| l.starts_with("- n:") || l.starts_with("- i:"))
        .count();
    assert_eq!(lines, 20, "{o}");
    assert!(o.contains("… ещё 8"), "{o}");
    let j = ok(
        a,
        &a.token,
        &SOL,
        "memory_code",
        json!({"action": "impact", "path": "a.rs", "format": "json"}),
    )
    .await
    .json();
    assert_eq!(j["knowledge"].as_array().unwrap().len(), 28, "{j}");
}

/// Ревью задачи 7: участник решения читает карту через `memory_code` (права маршрутов REST);
/// после отзыва доступа — 403 ошибкой инструмента, отозванный токен — 401 у `/mcp`.
#[tokio::test]
async fn mcp_code_member_revoked_access_and_token() {
    let w = code_world("member").await;
    let a = &w.a;
    let (b_id, b_tok) = a.new_user("Борис").await;
    assert_eq!(register(a, &b_tok).await, StatusCode::ACCEPTED);
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    for args in [
        json!({"action": "map"}),
        json!({"action": "symbol", "query": "a.rs#foo"}),
        json!({"action": "refs", "query": "foo"}),
        json!({"action": "impact", "path": "a.rs"}),
    ] {
        let o = ok(a, &b_tok, &SOL, "memory_code", args.clone()).await;
        assert!(o.text.contains("a.rs"), "{args}: {}", o.text);
    }
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/revoke"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let o = tool(a, &b_tok, &SOL, "memory_code", json!({"action": "map"})).await;
    assert!(o.is_error && o.text.contains("403"), "{}", o.text);
    // отзыв токена: `/mcp` отвечает 401 до вызова инструмента
    let uid: zam_core::Id = b_id.parse().unwrap();
    for t in a.st.system.list_tokens(uid).await.unwrap() {
        a.st.system.revoke_token(t.id).await.unwrap();
    }
    let r = rpc_raw(
        a,
        Some(&format!("Bearer {b_tok}")),
        &SOL,
        "tools/call",
        json!({"name": "memory_code", "arguments": {"action": "map"}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text());
}

/// Ревью задачи 7: фокус дольше срока — карта без фокуса с `focus rank timeout`, не ошибка;
/// тело `impact` больше `code.max_request_bytes` — 413 ошибкой инструмента.
#[tokio::test]
async fn mcp_code_focus_timeout_and_413() {
    let w = code_world_with("focus-413", |c| {
        c.code.rank_timeout_ms = 0;
        c.code.max_request_bytes = 4096;
    })
    .await;
    let a = &w.a;
    // ранги посчитаны (фокусу нужны входы, срок фокуса — не меньше 200 мс)
    for _ in 0..200 {
        let r = a
            .t(
                Method::GET,
                &format!("/api/v1/code/status?solution={G}"),
                None,
            )
            .await;
        if r.json()["rank_state"] == "ok" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    a.st.code
        .hooks
        .focus_delay_ms
        .store(400, std::sync::atomic::Ordering::SeqCst);
    let o = ok(
        a,
        &a.token,
        &SOL,
        "memory_code",
        json!({"action": "map", "focus": {"paths": ["b.rs"]}}),
    )
    .await;
    assert!(
        o.text.contains("> warning: focus rank timeout"),
        "{}",
        o.text
    );
    let files: Vec<Value> = (0..120)
        .map(|i| json!({"path": format!("src/very/long/path/number/{i:03}/file.rs")}))
        .collect();
    let o = tool(
        a,
        &a.token,
        &SOL,
        "memory_code",
        json!({"action": "impact", "files": files}),
    )
    .await;
    assert!(o.is_error && o.text.contains("413"), "{}", o.text);
}

/// Ревью задачи 7: `refs` популярного имени в Markdown — до 20 файлов и «… ещё N».
#[tokio::test]
async fn mcp_code_refs_markdown_capped() {
    let w = code_world("refs-cap").await;
    let a = &w.a;
    let users: Vec<Value> = (0..30)
        .map(|i| {
            code_file(
                &format!("u{i:02}.rs"),
                100 + i,
                vec![code_sym(&format!("user_{i:02}"), "function", 1)],
                &[("foo", 1)],
            )
        })
        .collect();
    code_put(a, users).await;
    let o = ok(
        a,
        &a.token,
        &SOL,
        "memory_code",
        json!({"action": "refs", "query": "a.rs#foo"}),
    )
    .await
    .text;
    let lines = o.lines().filter(|l| l.starts_with("- ")).count();
    assert_eq!(lines, 20, "{o}");
    assert!(o.contains("файлов 31:"), "{o}");
    assert!(o.contains("… ещё 11 (полный список — format: json)"), "{o}");
}

// ---- идеи (M5.7, задача 3) ----

/// Идея человека токеном через MCP: `id`.
async fn mcp_idea(a: &Api, title: &str) -> String {
    let o = ok(
        a,
        &a.token,
        &sess(),
        "memory_log",
        json!({"kind": "idea", "title": title, "space": "knowledge",
               "data": {"origin": "human"}}),
    )
    .await;
    assert!(o.text.starts_with("Записано w:"), "{}", o.text);
    o.text
        .trim_start_matches("Записано w:")
        .split(' ')
        .next()
        .unwrap()
        .to_string()
}

/// `memory_log kind=idea` без `data.origin` — ошибка с подсказкой human|agent; с ним — `w:`.
#[tokio::test]
async fn mcp_log_idea_requires_origin() {
    let a = api("mcp-idea-origin", |_| {}).await;
    let o = tool(
        &a,
        &a.token,
        &sess(),
        "memory_log",
        json!({"kind": "idea", "title": "Тёмная тема", "space": "knowledge"}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    assert!(
        o.text.contains("origin") && o.text.contains("human") && o.text.contains("agent"),
        "{}",
        o.text
    );
    mcp_idea(&a, "Тёмная тема").await;
}

/// `propose` → вопрос с тремя вариантами; ответ `accept` человека из чата → задача.
#[tokio::test]
async fn mcp_propose_then_answer_accept_creates_task() {
    let a = api("mcp-idea-accept", |_| {}).await;
    let id = mcp_idea(&a, "Экспорт отчётов в PDF").await;
    let o = ok(
        &a,
        &a.token,
        &sess(),
        "memory_update_entry",
        json!({"id": format!("w:{id}"), "action": "propose", "text": "подходит к задаче"}),
    )
    .await;
    let head = format!("Предложено w:{id} → вопрос q:");
    assert!(o.text.starts_with(&head), "{}", o.text);
    let qid = o.text[head.len()..]
        .split(|c: char| c.is_whitespace())
        .next()
        .unwrap()
        .to_string();
    let q = a
        .t(Method::GET, &format!("/api/v1/entries/{qid}"), None)
        .await
        .json();
    let keys: Vec<&str> = q["data"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["accept", "defer", "decline"], "{q}");
    let o = ok(
        &a,
        &a.token,
        &sess(),
        "memory_update_entry",
        json!({"id": format!("q:{qid}"), "action": "answer", "option": "accept",
               "by": "human"}),
    )
    .await;
    assert!(
        o.text
            .contains(&format!("Идея w:{id}: accepted → задача k:")),
        "{}",
        o.text
    );
    let idea = a
        .t(Method::GET, &format!("/api/v1/entries/{id}"), None)
        .await
        .json();
    assert_eq!(idea["status"], "accepted", "{idea}");
    let task = idea["data"]["task_id"].as_str().unwrap();
    let t = a
        .t(Method::GET, &format!("/api/v1/entries/{task}"), None)
        .await
        .json();
    assert_eq!(t["kind"], "task", "{t}");
    assert_eq!(t["status"], "open", "{t}");
}

/// `decide` агентом за человека — ошибка 403 с текстом правила; без `by` — подсказка; `by:
/// human` — переход и строка `Идея w:…: accepted → задача k:…`.
#[tokio::test]
async fn mcp_decide_by_agent_403_text() {
    let a = api("mcp-idea-decide", |_| {}).await;
    let id = mcp_idea(&a, "Голосовой ввод").await;
    let w = format!("w:{id}");
    let o = tool(
        &a,
        &a.token,
        &sess(),
        "memory_update_entry",
        json!({"id": w, "action": "decide", "decision": "accept", "by": "agent"}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    assert!(o.text.contains("Ошибка 403"), "{}", o.text);
    assert!(
        o.text.contains("решение по идее принимает человек"),
        "{}",
        o.text
    );
    let o = tool(
        &a,
        &a.token,
        &sess(),
        "memory_update_entry",
        json!({"id": w, "action": "decide", "decision": "accept"}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    assert!(o.text.contains("by: human"), "{}", o.text);
    let st = a
        .t(Method::GET, &format!("/api/v1/entries/{id}"), None)
        .await
        .json();
    assert_eq!(st["status"], "new", "{st}");
    let o = ok(
        &a,
        &a.token,
        &sess(),
        "memory_update_entry",
        json!({"id": w, "action": "decide", "decision": "accept", "by": "human"}),
    )
    .await;
    assert!(
        o.text
            .starts_with(&format!("Идея w:{id}: accepted → задача k:")),
        "{}",
        o.text
    );
}

/// Ответ «отложить до …» из чата: `answer option=defer horizon=<+10 дней>` → идея `deferred`
/// с этим `horizon`.
#[tokio::test]
async fn mcp_answer_defer_with_horizon() {
    let a = api("mcp-idea-defer", |_| {}).await;
    let id = mcp_idea(&a, "Резервное копирование").await;
    let o = ok(
        &a,
        &a.token,
        &sess(),
        "memory_update_entry",
        json!({"id": format!("w:{id}"), "action": "propose", "format": "json"}),
    )
    .await
    .json();
    let qid = o["question"]["id"].as_str().unwrap().to_string();
    let h = jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .checked_add(jiff::Span::new().days(10))
        .unwrap()
        .to_string();
    let o = ok(
        &a,
        &a.token,
        &sess(),
        "memory_update_entry",
        json!({"id": format!("q:{qid}"), "action": "answer", "option": "defer",
               "horizon": h, "by": "human", "channel": "chat"}),
    )
    .await;
    assert!(
        o.text.contains(&format!("Идея w:{id}: deferred")),
        "{}",
        o.text
    );
    let idea = a
        .t(Method::GET, &format!("/api/v1/entries/{id}"), None)
        .await
        .json();
    assert_eq!(idea["status"], "deferred", "{idea}");
    assert_eq!(idea["data"]["horizon"], h, "{idea}");
}

/// Пояснения идей — в `description` полей схемы (`data` у `memory_log`; `decision`, `horizon`,
/// `space` у `memory_update_entry`), в описаниях инструментов — только имена.
#[tokio::test]
async fn mcp_schema_field_descriptions_mention_idea() {
    let a = api("mcp-idea-schema", |_| {}).await;
    let list = rpc(&a, &a.token, &[], "tools/list", json!({})).await;
    let tool_of = |name: &str| {
        list["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name}"))
            .clone()
    };
    let log = tool_of("memory_log");
    assert!(
        log["description"].as_str().unwrap().contains("idea"),
        "{log}"
    );
    let props = &log["inputSchema"]["properties"];
    assert!(
        props["kind"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("idea")),
        "{log}"
    );
    let data = props["data"]["description"].as_str().unwrap();
    for want in ["idea", "origin", "human", "agent", "priority", "horizon"] {
        assert!(data.contains(want), "{want}: {data}");
    }
    let upd = tool_of("memory_update_entry");
    let d = upd["description"].as_str().unwrap();
    assert!(d.contains("propose") && d.contains("decide"), "{d}");
    let props = &upd["inputSchema"]["properties"];
    let actions = props["action"]["enum"].as_array().unwrap();
    assert!(actions.contains(&json!("propose")) && actions.contains(&json!("decide")));
    assert_eq!(
        props["decision"]["enum"],
        json!(["accept", "defer", "decline", "reopen"]),
        "{upd}"
    );
    let dd = props["decision"]["description"].as_str().unwrap();
    assert!(dd.contains("by"), "{dd}");
    assert!(
        props["horizon"]["description"]
            .as_str()
            .unwrap()
            .contains("YYYY-MM-DD"),
        "{upd}"
    );
    assert!(props["space"]["description"].as_str().is_some(), "{upd}");
    let opt = props["option"]["description"].as_str().unwrap();
    assert!(opt.contains("text") && opt.contains("accept"), "{opt}");
}

/// Задача 7 (§16): `propose` по идее со снятой задачей — без `text` ошибка с объяснением,
/// с `text` — «Предложено вернуть …» и вопрос `reopen|decline`; ответ человека `reopen` из чата
/// возвращает идею в `new`.
#[tokio::test]
async fn mcp_propose_reopen_dropped_task() {
    let a = api("mcp-idea-reopen", |_| {}).await;
    let id = mcp_idea(&a, "Кэш выжимок").await;
    let w = format!("w:{id}");
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{id}/decide"),
            Some(json!({"decision": "accept", "by": "human"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let task = r.json()["task"]["id"].as_str().unwrap().to_string();
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{task}"),
            Some(json!({"status": "dropped", "reason": "не до того"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let o = tool(
        &a,
        &a.token,
        &sess(),
        "memory_update_entry",
        json!({"id": w, "action": "propose"}),
    )
    .await;
    assert!(o.is_error, "{}", o.text);
    assert!(o.text.contains("почему идею пора вернуть"), "{}", o.text);
    let o = ok(
        &a,
        &a.token,
        &sess(),
        "memory_update_entry",
        json!({"id": w, "action": "propose", "text": "кэш снова тормозит"}),
    )
    .await;
    let head = format!("Предложено вернуть w:{id} → вопрос q:");
    assert!(o.text.starts_with(&head), "{}", o.text);
    assert!(o.text.contains("вернуть или отказаться"), "{}", o.text);
    let qid = o.text[head.len()..]
        .split(|c: char| c.is_whitespace())
        .next()
        .unwrap()
        .to_string();
    let q = a
        .t(Method::GET, &format!("/api/v1/entries/{qid}"), None)
        .await
        .json();
    let keys: Vec<&str> = q["data"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["reopen", "decline"], "{q}");
    assert_eq!(q["data"]["proposal"], "reopen", "{q}");
    let o = ok(
        &a,
        &a.token,
        &sess(),
        "memory_update_entry",
        json!({"id": format!("q:{qid}"), "action": "answer", "option": "reopen",
               "by": "human"}),
    )
    .await;
    assert!(o.text.contains(&format!("Идея w:{id}: new")), "{}", o.text);
}

// ---- M6 §7.1: сессия MCP — явная или никакая ----

/// `~N tok` последней строки ответа инструмента.
fn tok_of(o: &Out) -> i64 {
    let last = o.text.trim_end().rsplit('\n').next().unwrap();
    last.trim_start_matches('~')
        .trim_end_matches(" tok")
        .parse()
        .unwrap_or_else(|_| panic!("нет ~N tok: {}", o.text))
}

/// Учёт выданного сессии: `GET /sessions/{id}` → `tokens`.
async fn session_tokens(a: &Api, id: &str) -> Value {
    let r = a
        .t(Method::GET, &format!("/api/v1/sessions/{id}"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["tokens"].clone()
}

/// `GET /sessions/unattributed?days=1` → `items`.
async fn unattributed(a: &Api) -> Vec<Value> {
    let r = a
        .t(Method::GET, "/api/v1/sessions/unattributed?days=1", None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["items"].as_array().unwrap().clone()
}

/// Учёт MCP пишется в фоне после ответа (ревью задачи 3, M2): опрос `tokens` сессии, пока у
/// события `mcp` не станет `calls` вызовов (до 10 с).
async fn session_tokens_when(a: &Api, id: &str, calls: i64) -> Value {
    let t = std::time::Instant::now();
    loop {
        let r = a
            .t(Method::GET, &format!("/api/v1/sessions/{id}"), None)
            .await;
        let v = r.json()["tokens"].clone();
        if v["mcp"]["calls"] == calls || t.elapsed() > std::time::Duration::from_secs(10) {
            return v;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Как [`session_tokens_when`], для `GET /sessions/unattributed` (сумма `calls` события `mcp`).
async fn unattributed_when(a: &Api, calls: i64) -> Vec<Value> {
    let t = std::time::Instant::now();
    loop {
        let items = unattributed(a).await;
        let n: i64 = items
            .iter()
            .filter(|i| i["event"] == "mcp")
            .map(|i| i["calls"].as_i64().unwrap())
            .sum();
        if n == calls || t.elapsed() > std::time::Duration::from_secs(10) {
            return items;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// REST-поиск «квазар» с заголовками `extra`.
async fn rest_search(a: &Api, extra: &[(&str, &str)], mode: &str, format: &str) -> Resp {
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "квазар", "mode": mode, "format": format})),
            extra,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r
}

/// Заметка о квазаре; id узла.
async fn quasar(a: &Api) -> String {
    let n = a
        .note(
            "knowledge",
            "Астро/Квазар",
            "# Квазар\n\nКвазар светит ярко.\n",
        )
        .await;
    n["id"].as_str().unwrap().to_string()
}

/// Явная сессия (`X-Zam-Session` запроса MCP): `/read` отмечает выданное, следующий
/// `memory_search` — карточка с фрагментом и пометкой «(выдавалось)»; REST той же сессии —
/// прежнее `(seen)` без фрагмента. Учёт — событие `mcp` сессии.
#[tokio::test]
async fn mcp_explicit_session_marks_seen() {
    let a = api("mcp-explicit-seen", |_| {}).await;
    let id = quasar(&a).await;
    let h = sess();
    let r = ok(
        &a,
        &a.token,
        &h,
        "memory_read",
        json!({"items": [{"id": format!("n:{id}")}]}),
    )
    .await;
    assert!(r.text.contains("светит ярко"), "{}", r.text);
    let s = ok(
        &a,
        &a.token,
        &h,
        "memory_search",
        json!({"query": "квазар"}),
    )
    .await;
    assert!(s.text.contains(&format!("n:{id}")), "{}", s.text);
    assert!(s.text.contains("(выдавалось)"), "{}", s.text);
    assert!(s.text.contains("светит"), "фрагмент остаётся: {}", s.text);
    assert!(!s.text.contains("(seen)"), "{}", s.text);
    // JSON: `seen: true` и фрагмент
    let j = ok(
        &a,
        &a.token,
        &h,
        "memory_search",
        json!({"query": "квазар", "format": "json"}),
    )
    .await;
    let it = j.json()["items"][0].clone();
    assert_eq!(it["seen"], true, "{it}");
    assert!(
        it["snippet"].as_str().is_some_and(|s| s.contains("светит")),
        "{it}"
    );
    // REST той же сессии — прежнее поведение
    let md = rest_search(&a, &h, "brief", "md").await.text();
    assert!(
        md.contains("(seen)") && !md.contains("(выдавалось)"),
        "{md}"
    );
    assert!(!md.contains("светит"), "{md}");
    // учёт: три вызова MCP — событие `mcp`
    let t = session_tokens_when(&a, SESSION, 3).await;
    assert_eq!(t["mcp"]["calls"], 3, "{t}");
    let want = tok_of(&r) + tok_of(&s) + tok_of(&j);
    assert_eq!(t["mcp"]["tokens"], want, "{t}");
    assert!(unattributed(&a).await.is_empty());
}

/// Без `X-Zam-Session` (неявная сессия R13 — последняя сессия хуков): запись по-прежнему
/// засчитывается ей, но отметок `session_seen` и пометок нет, а токены — в счётчик
/// `unattributed` пользователя по дням.
#[tokio::test]
async fn implicit_session_no_marks_goes_unattributed() {
    let a = api("mcp-implicit-unattributed", |_| {}).await;
    let id = quasar(&a).await;
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/sessions",
            Some(json!({"id": "hook-s", "agent": "claude-code"})),
            &[("x-zam-machine", MACHINE)],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let m = [("x-zam-machine", MACHINE)];
    let r = ok(
        &a,
        &a.token,
        &m,
        "memory_read",
        json!({"items": [{"id": format!("n:{id}")}]}),
    )
    .await;
    assert!(r.text.contains("светит ярко"), "{}", r.text);
    let s = ok(
        &a,
        &a.token,
        &m,
        "memory_search",
        json!({"query": "квазар", "mode": "chunks"}),
    )
    .await;
    assert!(
        !s.text.contains("(выдавалось)") && !s.text.contains("(seen)"),
        "{}",
        s.text
    );
    let w = ok(
        &a,
        &a.token,
        &m,
        "memory_write",
        json!({"path": "Неявная/A", "content": "# A\n\nтекст\n"}),
    )
    .await;
    // отметок нет: та же сессия явно по REST видит цель невыданной
    let hs = [("x-zam-session", "hook-s"), ("x-zam-machine", MACHINE)];
    let j = rest_search(&a, &hs, "brief", "json").await.json();
    assert_eq!(j["items"][0]["seen"], false, "{j}");
    // учёт (в фоне) — в unattributed; запись засчитана неявной сессии (R13), учёт выданного — нет
    let items = unattributed_when(&a, 3).await;
    let g = a
        .t(Method::GET, "/api/v1/sessions/hook-s", None)
        .await
        .json();
    assert_eq!(g["memory_writes"], 1, "{g}");
    assert!(g["tokens"].get("mcp").is_none(), "{g}");
    assert_eq!(items.len(), 1, "{items:?}");
    let today = jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .to_string();
    assert_eq!(items[0]["day"], today.as_str(), "{items:?}");
    assert_eq!(items[0]["event"], "mcp");
    assert_eq!(items[0]["calls"], 3);
    assert_eq!(items[0]["tokens"], tok_of(&r) + tok_of(&s) + tok_of(&w));
    // параметр days: 1..=366
    for bad in ["0", "367", "x"] {
        let r = a
            .t(
                Method::GET,
                &format!("/api/v1/sessions/unattributed?days={bad}"),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text());
    }
}

/// Признак MCP (`McpOrigin`) ставит только внутренний клиент: внешний запрос REST с любыми
/// заголовками — обычный REST (`(seen)` прячет фрагмент, отметки пишутся, учёта `mcp` нет).
#[tokio::test]
async fn mcp_origin_not_forgeable_by_header() {
    let a = api("mcp-origin-forge", |_| {}).await;
    quasar(&a).await;
    let forged = [
        ("x-zam-session", "forge-s"),
        ("x-zam-machine", MACHINE),
        ("mcp-origin", "implicit"),
        ("x-zam-mcp", "1"),
        ("x-zam-mcp-origin", "{\"implicit\":true}"),
        ("mcp-session-id", "abc"),
        ("mcp-protocol-version", "2025-06-18"),
    ];
    // chunks отмечает выданное — «неявность» заголовками не подделать
    rest_search(&a, &forged, "chunks", "json").await;
    let md = rest_search(&a, &forged, "brief", "md").await.text();
    assert!(
        md.contains("(seen)") && !md.contains("(выдавалось)"),
        "{md}"
    );
    assert!(!md.contains("светит"), "{md}");
    let t = session_tokens(&a, "forge-s").await;
    assert!(t.get("mcp").is_none(), "{t}");
    assert!(unattributed(&a).await.is_empty());
}

/// Вызов MCP учитывается один раз — событием `mcp`: вложенные `/search`, `/read` и `/recall`
/// событий `search`, `read` и `session_start` не пишут; чтение из MCP — ещё строкой `mcp_read`
/// (вызов, токенов 0 — токены в `mcp`; правки ревью 8b M6).
#[tokio::test]
async fn mcp_search_counts_once() {
    let a = api("mcp-counts-once", |_| {}).await;
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
    let id = quasar(&a).await;
    let h = sess();
    let s = ok(
        &a,
        &a.token,
        &h,
        "memory_search",
        json!({"query": "квазар"}),
    )
    .await;
    let r = ok(
        &a,
        &a.token,
        &h,
        "memory_read",
        json!({"items": [{"id": format!("n:{id}")}]}),
    )
    .await;
    let c = ok(
        &a,
        &a.token,
        &h,
        "memory_recall",
        json!({"event": "session_start"}),
    )
    .await;
    assert!(c.text.contains("Всегда пиши тесты"), "{}", c.text);
    let t = session_tokens_when(&a, SESSION, 3).await;
    let keys: Vec<&String> = t.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["mcp", "mcp_read"], "{t}");
    assert_eq!(t["mcp_read"], json!({"calls": 1, "tokens": 0}), "{t}");
    assert_eq!(t["mcp"]["calls"], 3, "{t}");
    assert_eq!(
        t["mcp"]["tokens"],
        tok_of(&s) + tok_of(&r) + tok_of(&c),
        "{t}"
    );
}

/// Субагент на общем подключении MCP приходит с сессией родителя: выданное родителем остаётся
/// в карточке с фрагментом и пометкой «(выдавалось)»; прячет только явный `skip_seen: true`.
#[tokio::test]
async fn subagent_on_shared_mcp_keeps_snippet() {
    let a = api("mcp-subagent-shared", |_| {}).await;
    let id = quasar(&a).await;
    let h = sess();
    // родитель получил текст целиком
    ok(
        &a,
        &a.token,
        &h,
        "memory_search",
        json!({"query": "квазар", "mode": "chunks"}),
    )
    .await;
    // «субагент» на том же подключении: фрагмент и пометка
    let o = ok(
        &a,
        &a.token,
        &h,
        "memory_search",
        json!({"query": "квазар"}),
    )
    .await;
    assert!(o.text.contains(&format!("n:{id}")), "{}", o.text);
    assert!(
        o.text.contains("(выдавалось)") && o.text.contains("светит"),
        "{}",
        o.text
    );
    // chunks повторно — текст отдаётся
    let o = ok(
        &a,
        &a.token,
        &h,
        "memory_search",
        json!({"query": "квазар", "mode": "chunks"}),
    )
    .await;
    assert!(o.text.contains("светит ярко"), "{}", o.text);
    // явный skip_seen: true — как в REST: без фрагмента
    let o = ok(
        &a,
        &a.token,
        &h,
        "memory_search",
        json!({"query": "квазар", "skip_seen": true}),
    )
    .await;
    assert!(
        o.text.contains("(seen)") && !o.text.contains("светит"),
        "{}",
        o.text
    );
    // схема инструмента знает skip_seen
    let list = rpc(&a, &a.token, &[], "tools/list", json!({})).await;
    let search = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "memory_search")
        .unwrap()
        .clone();
    assert_eq!(
        search["inputSchema"]["properties"]["skip_seen"]["type"], "boolean",
        "{search}"
    );
}

// ---- ответы записи (M6 §5, задача 5) ----

/// Строки ответа без последней `~N tok`.
fn body_lines(o: &Out) -> Vec<String> {
    let t = o.text.trim_end();
    let (body, _) = t.rsplit_once('\n').unwrap_or(("", t));
    body.lines().map(str::to_string).collect()
}

/// `memory_cache_put` — одна строка (≤ 80 токенов вместе с `~N tok`), без эха запроса.
#[tokio::test]
async fn mcp_cache_put_one_line_le_80_tokens() {
    let a = api("mcp-cache-one-line", |_| {}).await;
    let n = a
        .note(
            "knowledge",
            "Сборка/Кэш cargo",
            "# Кэш cargo\n\nCARGO_TARGET_DIR общий для всех веток ускоряет сборку.\n",
        )
        .await;
    let id = n["id"].as_str().unwrap();
    let long_q = "как ускорить сборку cargo в рабочем пространстве с десятком крейтов, \
                  общим каталогом target и несколькими ветками одновременно на HDD";
    let o = ok(
        &a,
        &a.token,
        &sess(),
        "memory_cache_put",
        json!({"query": long_q,
               "summary": "Общий CARGO_TARGET_DIR для веток ускоряет сборку.",
               "sources": [{"id": format!("n:{id}"), "rev": 1}],
               "promote": {"as": "note"}}),
    )
    .await;
    let lines = body_lines(&o);
    assert_eq!(lines.len(), 1, "{}", o.text);
    assert!(lines[0].contains("c_"), "{}", o.text);
    assert!(
        !o.text.contains("десятком крейтов"),
        "эхо запроса: {}",
        o.text
    );
    let n = zam_store::count_tokens(&o.text);
    assert!(n <= 80, "{n} токенов: {}", o.text);
}

/// `memory_update_entry` — одна строка (≤ 80 токенов) при длинном заголовке; замещение — тоже.
#[tokio::test]
async fn mcp_update_entry_one_line_le_80_tokens() {
    let a = api("mcp-update-one-line", |_| {}).await;
    let title = "Очень длинный заголовок ловушки про сборку на Windows с путями, пробелами \
                 и кавычками в именах каталогов проекта";
    let o = ok(
        &a,
        &a.token,
        &sess(),
        "memory_log",
        json!({"kind": "insight", "title": title, "body": "Разобрано.",
               "data": {"kind": "gotcha"}}),
    )
    .await;
    let id = o
        .text
        .split_whitespace()
        .find_map(|w| w.strip_prefix("i:"))
        .unwrap()
        .to_string();
    let mut rev = 1;
    for mut args in [
        json!({"id": format!("i:{id}"), "action": "revisit"}),
        json!({"id": format!("i:{id}"), "action": "certify", "certainty": "theorem",
               "reason": "проверено на двух машинах",
               "data": {"kind": "gotcha", "evidence": {"checked": "сборка на двух машинах",
                        "method": "повтор", "result": "воспроизводится"}}}),
        json!({"id": format!("i:{id}"), "action": "supersede", "title": title,
               "body": "Разобрано заново.", "data": {"kind": "gotcha"}}),
    ] {
        args["rev"] = json!(rev);
        let o = ok(&a, &a.token, &sess(), "memory_update_entry", args.clone()).await;
        // ревизия — из ответа (`i:<id> rev<N>`)
        if let Some(n) = o
            .text
            .split_whitespace()
            .find_map(|w| w.strip_prefix("rev").and_then(|n| n.parse::<i64>().ok()))
        {
            rev = n;
        }
        let lines = body_lines(&o);
        assert_eq!(lines.len(), 1, "{args}: {}", o.text);
        let n = zam_store::count_tokens(&o.text);
        assert!(n <= 80, "{args}: {n} токенов: {}", o.text);
        if args["action"] == "supersede" {
            assert!(
                o.text.contains(&format!("i:{id}")),
                "замещённая: {}",
                o.text
            );
        }
    }
}

/// `memory_update_entry` с предупреждениями (секрет в дописанном теле): одна строка, они — в ней
/// через «; », ≤ 80 токенов (M6 §5, ревью задачи 5, M3).
#[tokio::test]
async fn mcp_update_entry_warnings_one_line() {
    let a = api("mcp-update-warn", |_| {}).await;
    let o = ok(
        &a,
        &a.token,
        &sess(),
        "memory_log",
        json!({"kind": "insight", "title": "Ключи в конфиге", "body": "Разобрано.",
               "data": {"kind": "gotcha"}}),
    )
    .await;
    let id = o
        .text
        .split_whitespace()
        .find_map(|w| w.strip_prefix("i:"))
        .unwrap()
        .to_string();
    let key = ["AK", "IA", "IOSFODNN7", "EXAMPLE"].concat();
    let o = ok(
        &a,
        &a.token,
        &sess(),
        "memory_update_entry",
        json!({"id": format!("i:{id}"), "action": "append", "rev": 1,
               "body": format!("Ключ {key} в конфиге.")}),
    )
    .await;
    let lines = body_lines(&o);
    assert_eq!(lines.len(), 1, "{}", o.text);
    assert!(lines[0].contains(" · warning: "), "{}", o.text);
    assert!(!o.text.contains(&key), "{}", o.text);
    let n = zam_store::count_tokens(&o.text);
    assert!(n <= 80, "{n} токенов: {}", o.text);
}

/// `memory_log` с предупреждениями: строка записи и по строке на предупреждение.
#[tokio::test]
async fn memory_log_warnings_line_per_warning() {
    let a = api("mcp-log-warnings", |_| {}).await;
    ok(
        &a,
        &a.token,
        &[],
        "memory_log",
        json!({"kind": "term", "title": "Выжимка", "body": "Подробности.",
               "data": {"definition": "Короткая сводка", "aliases": ["summary", "digest"]}}),
    )
    .await;
    let key = ["AK", "IA", "IOSFODNN7", "EXAMPLE"].concat();
    let o = ok(
        &a,
        &a.token,
        &[],
        "memory_log",
        json!({"kind": "insight", "title": "Про summary и digest",
               "body": format!("summary и digest; ключ {key}"),
               "data": {"kind": "gotcha"}}),
    )
    .await;
    let lines = body_lines(&o);
    let warns: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("> warning: "))
        .collect();
    assert!(warns.len() >= 3, "{}", o.text);
    assert_eq!(lines.len(), 1 + warns.len(), "{}", o.text);
    assert!(lines[0].starts_with("Записано i:"), "{}", o.text);
    assert!(!o.text.contains(&key), "{}", o.text);
}

// ---- источник выжимки в том же ответе (M6 §7.1, решение контролёра 2026-11-06) ----

/// Пометка карточки MCP: выдавалось, текст источника — в выжимке этого же ответа.
const IN_CACHE: &str = "(выдавалось, в выжимке)";

/// Текст из двух секций о квазаре, каждая длиннее чанка: карточки секций — отдельные.
fn section_note() -> String {
    let mut t = String::from("# Квазар\n\n## Яркость\nКвазар светит ярко.\n");
    for i in 0..60 {
        t.push_str(&format!("Строка {i}: свет квазара виден издалека.\n"));
    }
    t.push_str("\n## Расстояние\nКвазар далеко и квазар стар.\n");
    for i in 0..60 {
        t.push_str(&format!("Строка {i}: до квазара миллиарды лет пути.\n"));
    }
    t
}

/// Заметка с двумя секциями о квазаре; id узла.
async fn quasar_sections(a: &Api) -> String {
    let n = a.note("knowledge", "Астро/Квазар2", &section_note()).await;
    n["id"].as_str().unwrap().to_string()
}

/// Под карточкой цели `n:<id>` есть строка фрагмента (с отступом).
fn fragment_under(text: &str, id: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    lines
        .iter()
        .position(|l| l.contains(&format!("n:{id}")))
        .is_some_and(|k| lines.get(k + 1).is_some_and(|l| l.starts_with("   ")))
}

/// Сессия получила цель целиком (`memory_read`), затем выжимка по ней (`memory_cache_put`).
async fn read_and_cache(a: &Api, h: &[(&str, &str)], query: &str, sources: Value) {
    for s in sources.as_array().unwrap() {
        ok(
            a,
            &a.token,
            h,
            "memory_read",
            json!({"items": [{"id": s["id"]}], "budget_tokens": 8000}),
        )
        .await;
    }
    let o = ok(
        a,
        &a.token,
        h,
        "memory_cache_put",
        json!({"query": query, "summary": "Сводка: объект яркий.", "sources": sources}),
    )
    .await;
    assert!(o.text.contains("c_"), "{}", o.text);
}

/// Карточка `(выдавалось)`, чья цель — источник действующей выжимки этого же ответа: без
/// фрагмента и с пометкой `(выдавалось, в выжимке)`; в JSON — `seen: true`, `in_cache: true`,
/// без `snippet`. Выжимка в ответе есть (`[cache] … все актуальны`).
#[tokio::test]
async fn mcp_seen_cache_source_without_fragment() {
    let a = api("mcp-cache-source", |_| {}).await;
    let id = quasar(&a).await;
    let h = sess();
    let q = "чем светит квазар";
    read_and_cache(&a, &h, q, json!([{"id": format!("n:{id}"), "rev": 1}])).await;
    let o = ok(&a, &a.token, &h, "memory_search", json!({"query": q})).await;
    assert!(
        o.text.contains("[cache]") && o.text.contains("все актуальны"),
        "{}",
        o.text
    );
    assert!(
        o.text.contains("Сводка: объект яркий."),
        "выжимка в ответе: {}",
        o.text
    );
    let card = o
        .text
        .lines()
        .find(|l| l.contains(&format!("n:{id}")))
        .unwrap_or_else(|| panic!("нет карточки: {}", o.text));
    assert!(card.ends_with(IN_CACHE), "{card}");
    // под карточкой нет строки фрагмента (в тексте выдачи «светит» — только в запросе выжимки)
    let lines: Vec<&str> = o.text.lines().collect();
    let k = lines.iter().position(|l| *l == card).unwrap();
    assert!(
        !lines.get(k + 1).is_some_and(|l| l.starts_with("   ")),
        "фрагмента нет: {}",
        o.text
    );
    assert!(!o.text.contains("**светит**"), "{}", o.text);
    let j = ok(
        &a,
        &a.token,
        &h,
        "memory_search",
        json!({"query": q, "format": "json"}),
    )
    .await;
    let v = j.json();
    let it = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["node_id"] == id.as_str())
        .cloned()
        .unwrap_or_else(|| panic!("{v}"));
    assert_eq!(it["seen"], true, "{it}");
    assert_eq!(it["in_cache"], true, "{it}");
    assert!(it.get("snippet").is_none(), "{it}");
    assert_eq!(v["items"][0]["kind"], "cache", "{v}");
}

/// Отрицательные: выжимки в ответе нет (другой запрос — кэш не находит; у MCP нет `no_cache`),
/// выжимка `stale`, явный `skip_seen: false` —
/// карточка с фрагментом, как по §7.1.
#[tokio::test]
async fn mcp_seen_cache_source_keeps_fragment_otherwise() {
    let a = api("mcp-cache-source-neg", |_| {}).await;
    let id = quasar(&a).await;
    let other = a
        .note(
            "knowledge",
            "Астро/Пульсар",
            "# Пульсар\n\nПульсар мигает.\n",
        )
        .await;
    let other = other["id"].as_str().unwrap().to_string();
    let h = sess();
    let q = "чем светит квазар";
    read_and_cache(
        &a,
        &h,
        q,
        json!([{"id": format!("n:{id}"), "rev": 1}, {"id": format!("n:{other}"), "rev": 1}]),
    )
    .await;
    let card = |text: &str| -> String {
        text.lines()
            .find(|l| l.contains(&format!("n:{id}")))
            .unwrap_or_else(|| panic!("нет карточки: {text}"))
            .to_string()
    };
    // выжимки в ответе нет — фрагмент остаётся
    let o = ok(
        &a,
        &a.token,
        &h,
        "memory_search",
        json!({"query": "квазар ярко"}),
    )
    .await;
    assert!(!o.text.contains("[cache"), "{}", o.text);
    assert!(card(&o.text).ends_with("(выдавалось)"), "{}", o.text);
    assert!(fragment_under(&o.text, &id), "{}", o.text);
    // явный `skip_seen: false` — фрагмент без пометки
    let o = ok(
        &a,
        &a.token,
        &h,
        "memory_search",
        json!({"query": q, "skip_seen": false}),
    )
    .await;
    assert!(o.text.contains("[cache]"), "{}", o.text);
    assert!(!card(&o.text).contains("(выдавалось"), "{}", o.text);
    assert!(fragment_under(&o.text, &id), "{}", o.text);
    // второй источник изменился — `[cache, stale: …]`: фрагмент у первого остаётся
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{other}"),
            Some(json!({"content": "# Пульсар\n\nПульсар мигает часто.\n"})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let o = ok(&a, &a.token, &h, "memory_search", json!({"query": q})).await;
    assert!(o.text.contains("[cache, stale:"), "{}", o.text);
    assert!(card(&o.text).ends_with("(выдавалось)"), "{}", o.text);
    assert!(fragment_under(&o.text, &id), "{}", o.text);
}

/// Источник — секция: без фрагмента только карточка этой секции, карточка другой секции той же
/// заметки — `(выдавалось)` с фрагментом.
#[tokio::test]
async fn mcp_seen_cache_section_source_only_that_section() {
    let a = api("mcp-cache-section", |_| {}).await;
    let id = quasar_sections(&a).await;
    let h = sess();
    let q = "квазар";
    read_and_cache(
        &a,
        &h,
        q,
        json!([{"id": format!("n:{id}"), "rev": 1, "section": "Квазар > Яркость"}]),
    )
    .await;
    let o = ok(&a, &a.token, &h, "memory_search", json!({"query": q})).await;
    assert!(o.text.contains("все актуальны"), "{}", o.text);
    let lines: Vec<&str> = o.text.lines().collect();
    // карточки секции и есть ли под ними фрагмент (строка с отступом)
    let cards = |sec: &str| -> Vec<(&str, bool)> {
        (0..lines.len())
            .filter(|&k| lines[k].contains(&format!("n:{id}")) && lines[k].contains(sec))
            .map(|k| {
                (
                    lines[k],
                    lines.get(k + 1).is_some_and(|l| l.starts_with("   ")),
                )
            })
            .collect()
    };
    let bright = cards("› Квазар > Яркость");
    let far = cards("› Квазар > Расстояние");
    assert!(!bright.is_empty(), "нет карточки «Яркость»: {}", o.text);
    assert!(!far.is_empty(), "нет карточки «Расстояние»: {}", o.text);
    for (l, frag) in bright {
        assert!(l.ends_with(IN_CACHE) && !frag, "{l}: {}", o.text);
    }
    for (l, frag) in far {
        assert!(l.ends_with("(выдавалось)") && frag, "{l}: {}", o.text);
    }
}

/// Повторное ревью задачи 7a (I-A): источник-секция покрывает карточку, только если её чанк
/// целиком лежит в секции. Карточки `n:<id>` с секцией `sec` в пути: `(пометка в конце, есть
/// ли фрагмент)`.
fn section_cards(text: &str, id: &str, sec: &str) -> Vec<(String, bool)> {
    let lines: Vec<&str> = text.lines().collect();
    (0..lines.len())
        .filter(|&k| lines[k].contains(&format!("n:{id}")) && lines[k].contains(sec))
        .map(|k| {
            let frag = lines.get(k + 1).is_some_and(|l| l.starts_with("   "));
            (lines[k].to_string(), frag)
        })
        .collect()
}

/// Заметка `path` с текстом `text`: прочитана целиком, выжимка по секции `sec`, поиск
/// «квазар»; текст ответа MCP и id узла.
async fn cache_section_search(name: &str, text: &str, sec: &str) -> (String, String) {
    let a = api(name, |_| {}).await;
    let n = a.note("knowledge", "Астро/Секции", text).await;
    let id = n["id"].as_str().unwrap().to_string();
    let h = sess();
    read_and_cache(
        &a,
        &h,
        "квазар",
        json!([{"id": format!("n:{id}"), "rev": 1, "section": sec}]),
    )
    .await;
    let o = ok(
        &a,
        &a.token,
        &h,
        "memory_search",
        json!({"query": "квазар"}),
    )
    .await;
    assert!(o.text.contains("все актуальны"), "{}", o.text);
    (o.text, id)
}

/// Короткая секция «A > B» склеена чанкером со следующей «A > C» (путь склейки — первой):
/// источник «A > B» карточку склеенного чанка не покрывает — `(выдавалось)` с фрагментом.
#[tokio::test]
async fn mcp_cache_section_merged_chunk_keeps_fragment() {
    let (text, id) = cache_section_search(
        "mcp-cache-merged",
        "# A\n\n## B\nКвазар светит.\n\n## C\nКвазар гаснет.\n",
        "A > B",
    )
    .await;
    let cards = section_cards(&text, &id, "› A > B");
    assert!(!cards.is_empty(), "нет карточки «A > B»: {text}");
    for (l, frag) in cards {
        assert!(l.ends_with("(выдавалось)") && frag, "{l}: {text}");
    }
    assert!(
        text.contains("гаснет"),
        "текст соседней секции — во фрагменте: {text}"
    );
}

/// Вложенность: источник «Квазар» (H1 со всеми подсекциями) покрывает карточки «Квазар >
/// Яркость» и «Квазар > Расстояние».
#[tokio::test]
async fn mcp_cache_parent_section_covers_nested() {
    let (text, id) = cache_section_search("mcp-cache-nested", &section_note(), "Квазар").await;
    let cards = section_cards(&text, &id, "› Квазар > ");
    assert!(cards.len() >= 2, "{text}");
    for (l, frag) in cards {
        assert!(l.ends_with(IN_CACHE) && !frag, "{l}: {text}");
    }
}

/// Лист с путём H4 (`heading_path_h4`): короткая H3 «C» с подзаголовком H4 «D» склеена со
/// следующей H3 «F», путь листа — «A > B > C > D». Источник-секция — только H1–H3 (H4 как
/// источник — 422), поэтому источник — «A > B > C»: проверка по префиксу пути покрывала лист,
/// хотя в нём текст «F»; карточка сохраняет фрагмент.
#[tokio::test]
async fn mcp_cache_h4_leaf_spanning_next_keeps_fragment() {
    let (text, id) = cache_section_search(
        "mcp-cache-h4",
        "# A\n\n## B\n\n### C\n\n#### D\nКвазар светит.\n\n### F\nКвазар гаснет.\n",
        "A > B > C",
    )
    .await;
    let cards = section_cards(&text, &id, &format!("n:{id}"));
    assert!(!cards.is_empty(), "{text}");
    for (l, frag) in cards {
        assert!(!l.ends_with(IN_CACHE) && frag, "{l}: {text}");
    }
    assert!(text.contains("гаснет"), "{text}");
}

/// M6 §12а (отложено финальным ревью M5.6): символ с `qual` вида якоря строки (`L10`, `L1-L2`) —
/// `путь#L10` адресует строку файла (REST 400), поэтому `memory_code` даёт карточку по `id` с
/// подсказкой «уточните path»; в списке кандидатов — `id`, а `query=<id>` открывает карточку.
#[tokio::test]
async fn line_anchor_symbol_link_by_id() {
    let w = code_world("line-anchor").await;
    let a = &w.a;
    code_put(
        a,
        vec![code_file(
            "d.rs",
            4,
            vec![
                code_sym("L10", "const", 1),
                code_sym("other", "function", 5),
            ],
            &[],
        )],
    )
    .await;
    let call = |args: Value| async move { ok(a, &a.token, &SOL, "memory_code", args).await.text };
    // REST по адресу `путь#L10` — не символ (400), поэтому адрес карточки — id
    let r = a
        .t(
            Method::GET,
            &format!(
                "/api/v1/code/symbols/{}?solution={G}",
                common::api::enc("d.rs#L10")
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let card = call(json!({"action": "symbol", "query": "L10"})).await;
    assert!(card.contains("### `d.rs · L10 · id "), "{card}");
    assert!(card.contains("const"), "{card}");
    assert!(card.contains("уточните path"), "подсказка: {card}");
    let id: String = card
        .split("· id ")
        .nth(1)
        .unwrap()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let by_id = call(json!({"action": "symbol", "query": id})).await;
    assert_eq!(
        body_of(&by_id).lines().next(),
        body_of(&card).lines().next(),
        "{by_id}"
    );
    // два символа `L10` — список кандидатов с id и подсказкой
    code_put(
        a,
        vec![code_file("e.rs", 5, vec![code_sym("L10", "const", 2)], &[])],
    )
    .await;
    let list = call(json!({"action": "symbol", "query": "L10"})).await;
    assert!(
        list.contains("`d.rs · L10 · id ") && list.contains("`e.rs · L10 · id "),
        "{list}"
    );
    assert!(!list.contains("d.rs#L10"), "{list}");
    assert!(list.contains("уточните path"), "{list}");
    // обычный символ — по-прежнему адрес путь#qual
    let other = call(json!({"action": "symbol", "query": "other"})).await;
    assert!(other.contains("`d.rs#other`"), "{other}");
}
