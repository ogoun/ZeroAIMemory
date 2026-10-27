//! MCP `/mcp` (§13.1, задача 9 M4): streamable HTTP без сессий, bearer, 10 инструментов поверх
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

const TOOLS: [&str; 10] = [
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

/// Результат инструмента: (текст, isError, structuredContent).
struct Out {
    text: String,
    is_error: bool,
    structured: Value,
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
    Out {
        text,
        is_error: r["isError"].as_bool() == Some(true),
        structured: r["structuredContent"].clone(),
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
    assert!(o.text.contains("rev1"), "{}", o.text);
    // format=json — JSON ответа REST и structuredContent
    let o = ok(
        &a,
        &a.token,
        &[],
        "memory_search",
        json!({"query": "ripgrep hidden", "format": "json"}),
    )
    .await;
    assert_eq!(o.structured["items"][0]["node_id"], id, "{}", o.structured);
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
