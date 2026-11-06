//! Дата и время в тексте агенту (M6 §4.4, задача 6): Markdown MCP и текст хуков — `yyyy-MM-dd
//! HH:mm:ss` UTC без `T`/`Z`; JSON API — RFC 3339; хранение — UTC (`…Z`); замороженные часы
//! `zam_store::time::freeze_now`.
//!
//! Все тесты файла замораживают часы (`freeze_now` держит блокировку тестовых точек): часы общие
//! для процесса, поэтому тесты, чувствительные к сроку (сессии WebUI, аренды задач), рядом с
//! замороженными не идут — отдельный двоичный файл.
mod common;

use std::path::Path;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};
use zam_server::config::ServerConfig;
use zam_store::time::freeze_now;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e66";
const KN: &str = "/api/v1/spaces/knowledge/entries";
const SESSION: &str = "dates-s1";
/// Замороженное «сейчас» сценария.
const T0: &str = "2026-10-10T14:03:22.123456Z";
/// `verified_at` заметки (frontmatter принимает только дату `YYYY-MM-DD`).
const VERIFIED: &str = "2026-10-28";

fn roomy(c: &mut ServerConfig) {
    c.recall.timeout_ms = 30_000;
}

fn t0() -> jiff::Timestamp {
    T0.parse().unwrap()
}

/// Первая позиция «dddd-dd-ddT» (дата ISO с `T`) в строке.
fn iso_t(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let d = |i: usize| b[i].is_ascii_digit();
    (0..b.len().saturating_sub(10)).find(|&i| {
        d(i) && d(i + 1)
            && d(i + 2)
            && d(i + 3)
            && b[i + 4] == b'-'
            && d(i + 5)
            && d(i + 6)
            && b[i + 7] == b'-'
            && d(i + 8)
            && d(i + 9)
            && b[i + 10] == b'T'
    })
}

fn assert_no_iso_t(what: &str, text: &str) {
    if let Some(i) = iso_t(text) {
        let line = text[..i].rsplit('\n').next().unwrap_or("").len();
        let at = i - line;
        let end = text[at..].find('\n').map_or(text.len(), |e| at + e);
        panic!(
            "{what}: дата ISO с T в тексте агенту: «{}»\n{text}",
            &text[at..end]
        );
    }
}

async fn created(r: Resp) -> String {
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

/// JSON-RPC `tools/call` к `/mcp` в сессии сценария и решении `G`; текст ответа.
async fn mcp(a: &Api, name: &str, args: Value) -> String {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": name, "arguments": args}});
    let r = a
        .send(
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(header::HOST, "memory.lan:7700")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header(header::AUTHORIZATION, format!("Bearer {}", a.token))
                .header("x-zam-session", SESSION)
                .header("x-zam-solution", G)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert!(j.get("error").is_none(), "{name}: {j}");
    assert_ne!(j["result"]["isError"], true, "{name}: {j}");
    j["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

/// `POST /recall` (текст хуков) в сессии `session`.
async fn recall(a: &Api, session: &str, body: Value) -> Value {
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/recall",
            Some(body),
            &[("x-zam-session", session)],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Сценарий: решение `G`, правило, заметки (одна — с `verified_at`), задача в работе (захвачена
/// сессией), отвеченный вопрос, insight-решение, выжимка кэша, карта кода. Всё — при
/// замороженных часах `T0`. `(api, заметка Knowledge, задача)`.
async fn world(name: &str) -> (Api, String, String) {
    let a = api(&format!("dates-{name}"), roomy).await;
    let r = a
        .adm(
            Method::PATCH,
            "/api/v1/admin/settings",
            Some(json!({"rules": {"agent_write": true}})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    a.note(
        "rules",
        "правило.md",
        "# Правило\n\nСкрипты bash пишутся с LF.",
    )
    .await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Даты"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let n = a
        .note(
            "knowledge",
            "Инструменты/Bash",
            "# Bash\n\nСкрипты bash на windows пишите с LF.\n\n## Причина\n\nCRLF ломает bash.\n",
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    a.note(
        G,
        "Команды/Сборка",
        &format!("---\nverified_at: {VERIFIED}\n---\n# Сборка\n\ncargo build\n"),
    )
    .await;
    let task = created(
        a.t(
            Method::POST,
            KN,
            Some(json!({"kind": "task", "title": "Починить bash скрипты",
                        "data": {"next_step": "заменить CRLF"}})),
        )
        .await,
    )
    .await;
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{task}"),
            Some(json!({"status": "in_progress"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &format!("/api/v1/entries/{task}/claim"),
            Some(json!({})),
            &[("x-zam-session", SESSION)],
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let q = created(
        a.t(
            Method::POST,
            KN,
            Some(
                json!({"kind": "question", "title": "Какой перевод строк в bash?", "body": "к",
                        "data": {"options": [{"key": "a", "text": "LF"},
                                             {"key": "b", "text": "CRLF"}]}}),
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
    created(
        a.t(
            Method::POST,
            &format!("/api/v1/spaces/{G}/entries"),
            Some(
                json!({"kind": "insight", "title": "Решение: bash только с LF",
                        "body": "обоснование", "data": {"kind": "decision"}}),
            ),
        )
        .await,
    )
    .await;
    // выжимка — через MCP: область кэша та же, что у поиска MCP (решение `G`)
    mcp(
        &a,
        "memory_cache_put",
        json!({"query": "bash скрипты на windows", "summary": "Пишите bash-скрипты с LF.",
               "sources": [{"id": format!("n:{n}"), "rev": 1}]}),
    )
    .await;
    load_map(&a).await;
    (a, n, task)
}

/// Карта кода решения: два файла, три символа.
async fn load_map(a: &Api) {
    let h = |n: usize| format!("{n:064x}");
    let files = [("src/store.rs", "Store::open"), ("src/api.rs", "route")];
    let manifest: Vec<Value> = files
        .iter()
        .enumerate()
        .map(|(i, (p, _))| json!({"path": p, "hash": h(i + 1)}))
        .collect();
    let r = a
        .t(
            Method::POST,
            "/api/v1/code/sync",
            Some(
                json!({"solution": G, "commit": "abcdef1234", "branch": "main",
                        "machine": "box", "manifest": manifest}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let up: Vec<Value> = files
        .iter()
        .enumerate()
        .map(|(i, (p, q))| {
            let name = q.rsplit("::").next().unwrap();
            json!({"path": p, "lang": "rust", "hash": h(i + 1), "size": 10,
                   "parse_state": "ok", "refs": [],
                   "symbols": [{"name": name, "qual": q, "kind": "function", "line": 1,
                                "end_line": 3, "sig": format!("fn {name}()"),
                                "body_hash": format!("{:016x}", i + 1), "exported": true}]})
        })
        .collect();
    let r = a
        .t(
            Method::PUT,
            "/api/v1/code/files",
            Some(
                json!({"solution": G, "commit": "abcdef1234", "branch": "main",
                        "machine": "box", "files": up}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// Тексты агенту сценария: MCP (как ТС-1) и хуки `/recall` (как ТС-2), `(имя, текст)`.
async fn agent_texts(a: &Api, n: &str, task: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, args) in [
        ("memory_search", json!({"query": "bash скрипты на windows"})),
        (
            "memory_search",
            json!({"query": "bash LF", "mode": "notes"}),
        ),
        (
            "memory_search",
            json!({"query": "bash", "filters": {"kinds": ["task", "question", "insight"]}}),
        ),
        (
            "memory_read",
            json!({"items": [{"id": format!("n:{n}")}, {"id": format!("t:{task}")}]}),
        ),
        ("memory_tree", json!({"space": "knowledge"})),
        ("memory_code", json!({"action": "map"})),
        (
            "memory_code",
            json!({"action": "symbol", "query": "Store::open"}),
        ),
        ("memory_recall", json!({"event": "recovery"})),
        (
            "memory_recall",
            json!({"event": "brief", "task": "починить bash скрипты"}),
        ),
        ("memory_vault", json!({"action": "list"})),
    ] {
        out.push((format!("{name} {args}"), mcp(a, name, args).await));
    }
    for (i, body) in [
        json!({"event": "session_start", "solution": G}),
        json!({"event": "prompt", "prompt": "почему bash скрипты падают на windows", "solution": G}),
        json!({"event": "post_tool", "tool": "Bash", "error": "bash: $'\\r': command not found",
               "solution": G}),
        json!({"event": "recovery", "solution": G}),
        json!({"event": "brief", "task": "починить bash скрипты", "solution": G}),
    ]
    .into_iter()
    .enumerate()
    {
        let j = recall(a, &format!("hook-{i}"), body.clone()).await;
        out.push((
            format!("recall {body}"),
            j["text"].as_str().unwrap_or("").to_string(),
        ));
    }
    out
}

/// Markdown MCP и текст хуков без `\d{4}-\d{2}-\d{2}T` (§4.4); даты — `yyyy-MM-dd HH:mm:ss` UTC
/// замороженных часов, `verified_at` — тоже в этом виде.
#[tokio::test]
async fn markdown_has_no_iso_t() {
    let _clock = freeze_now(t0());
    let (a, n, task) = world("md").await;
    let texts = agent_texts(&a, &n, &task).await;
    for (what, text) in &texts {
        assert_no_iso_t(what, text);
    }
    let all: String = texts.iter().map(|(_, t)| t.as_str()).collect();
    // карточка — дата обновления по замороженным часам
    assert!(all.contains(" · 2026-10-10 14:03:22 · "), "{all}");
    // строка `[cache]` — дата выжимки
    assert!(all.contains("— выжимка 2026-10-10,"), "{all}");
    // проверенные команды — дата frontmatter как есть
    assert!(
        all.contains("Команды/Сборка (проверено 2026-10-28)"),
        "{all}"
    );
}

/// JSON API — RFC 3339 UTC с `T` и `Z` (решение владельца 24), формат хранения `fmt_ts`.
#[tokio::test]
async fn json_dates_unchanged() {
    let _clock = freeze_now(t0());
    let (a, n, task) = world("json").await;
    let r = a.t(Method::GET, &format!("/api/v1/nodes/{n}"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = &r.json()["node"];
    assert_eq!(j["updated_at"], T0, "{j}");
    assert_eq!(j["created_at"], T0, "{j}");
    let r = a
        .t(Method::GET, &format!("/api/v1/entries/{task}"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["updated_at"], T0, "{j}");
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "bash скрипты на windows", "format": "json"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let items = j["items"].as_array().unwrap();
    assert!(!items.is_empty(), "{j}");
    for it in items {
        if let Some(u) = it.get("updated_at") {
            assert_eq!(u, T0, "{it}");
        }
    }
    // MCP с `format: json` — тот же машинный контракт, что REST
    let t = mcp(
        &a,
        "memory_search",
        json!({"query": "bash скрипты на windows", "format": "json"}),
    )
    .await;
    assert!(t.contains(&format!("\"updated_at\":\"{T0}\"")), "{t}");
    assert!(!t.contains("2026-10-10 14:03:22"), "{t}");
}

/// Все `*_at` (и `ts`) в SQLite — UTC с `Z` (§4.4: хранение — UTC). `verified_at` — свободное поле
/// frontmatter (дата или метка автора), не время системы: не проверяется.
#[tokio::test]
async fn utc_storage_audit() {
    let _clock = freeze_now(t0());
    let (a, n, task) = world("audit").await;
    agent_texts(&a, &n, &task).await;
    let mut checked = 0usize;
    let mut bad = Vec::new();
    for db in sqlite_files(a.st.data_dir.root()) {
        let c = rusqlite::Connection::open_with_flags(
            &db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        let tables: Vec<String> = c
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for t in tables {
            let cols: Vec<String> = c
                .prepare(&format!("SELECT name FROM pragma_table_info('{t}')"))
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            for col in cols
                .iter()
                .filter(|c| (c.ends_with("_at") || *c == "ts") && *c != "verified_at")
            {
                let q = format!("SELECT \"{col}\" FROM \"{t}\" WHERE typeof(\"{col}\") = 'text'");
                let Ok(mut st) = c.prepare(&q) else { continue };
                let vals: Vec<String> = st
                    .query_map([], |r| r.get(0))
                    .unwrap()
                    .map(Result::unwrap)
                    .collect();
                for v in vals {
                    checked += 1;
                    if !(v.ends_with('Z') && v.parse::<jiff::Timestamp>().is_ok()) {
                        bad.push(format!("{}: {t}.{col} = «{v}»", db.display()));
                    }
                }
            }
        }
    }
    assert!(checked > 20, "проверено значений: {checked}");
    assert!(bad.is_empty(), "не UTC: {bad:#?}");
}

/// Замороженные часы доходят до сервера: запись получает `T0`, «сегодня» — дата `T0`.
#[tokio::test]
async fn freeze_now_applies() {
    let _clock = freeze_now(t0());
    let (a, n, _) = world("freeze").await;
    let r = a.t(Method::GET, &format!("/api/v1/nodes/{n}"), None).await;
    assert_eq!(r.json()["node"]["updated_at"], T0, "{}", r.text());
    assert_eq!(zam_store::time::utc_today().to_string(), "2026-10-10");
    // карта кода: возраст снимка по тем же часам — «0 мин» (без замороженных часов запись и
    // чтение разошлись бы на доли секунды, но не на минуты)
    let j = recall(
        &a,
        "freeze-1",
        json!({"event": "session_start", "solution": G}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    assert!(text.contains("снимок 0 мин"), "{text}");
}

fn sqlite_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(sqlite_files(&p));
        } else if p.extension().is_some_and(|x| x == "db") {
            out.push(p);
        }
    }
    out
}
