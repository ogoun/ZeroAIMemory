//! Пакет под задачу (M4.5 §9.4, задача 10): `POST /recall {event: "brief", task}` — правила
//! L0, действующие решения `axiom`/`theorem` по поиску `task` (до 3), термины словаря из
//! `task` (до 5), ловушки `gotcha`/`failure` (до 3), записи о файлах из `task` (до 3) в бюджете
//! `recall.brief_tokens`; без `task` — мини-дайджест; `task` длиннее 2000 символов
//! обрезается; участник общего решения не получает Knowledge владельца; MCP
//! `memory_recall {event: "brief"}`. Сервер без моделей (поиск — FTS).
//!
//! Пользователи общего решения: A — владелец, B — участник.
mod common;

use axum::body::Body;
use axum::http::{Method, StatusCode, header};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};
use zam_server::config::ServerConfig;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e10";
const RECALL: &str = "/api/v1/recall";

/// Срок сбора с запасом (отладочная сборка на HDD); порог балла снят: поиск — только FTS.
fn roomy(c: &mut ServerConfig) {
    c.recall.timeout_ms = 30_000;
    c.recall.prompt_min_score = 0.0;
}

fn sol() -> String {
    format!("/api/v1/spaces/{G}/entries")
}

async fn created(r: Resp) -> String {
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"]
        .as_str()
        .unwrap_or_else(|| panic!("нет id: {}", r.text()))
        .to_string()
}

/// Запись токеном `tok` в пространство `space` (заголовок `X-Zam-Solution` — решение `G`).
async fn entry(a: &Api, tok: &str, space_uri: &str, body: Value) -> String {
    created(
        a.tok_h(
            tok,
            Method::POST,
            space_uri,
            Some(body),
            &[("x-zam-solution", G)],
        )
        .await,
    )
    .await
}

async fn register(a: &Api, tok: &str) -> Resp {
    a.tok(
        tok,
        Method::POST,
        "/api/v1/solutions",
        Some(json!({"agent_solution_id": G, "name": "Пакет"})),
    )
    .await
}

async fn allow_rules(a: &Api) {
    let r = a
        .adm(
            Method::PATCH,
            "/api/v1/admin/settings",
            Some(json!({"rules": {"agent_write": true}})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// Ид записей мира пакета.
struct World {
    a: Api,
    axiom: String,
    hypothesis: String,
    gotcha: String,
    term: String,
    file_note: String,
    task: String,
}

/// Правило, решение `G` с аксиомой, гипотезой, ловушкой (ссылка на `crates/cache/src/lib.rs`),
/// записью со ссылкой на `docs/plan.md`, термином «Квазикэш» и задачей, захваченной `p1`.
async fn world(name: &str) -> World {
    let a = api(&format!("brief-{name}"), roomy).await;
    allow_rules(&a).await;
    a.note(
        "rules",
        "правило.md",
        "# Правило\n\nВсегда пиши тесты к кэшу.",
    )
    .await;
    assert_eq!(register(&a, &a.token).await.status, StatusCode::CREATED);
    let tok = a.token.clone();
    let axiom = entry(
        &a,
        &tok,
        &sol(),
        json!({"kind": "insight", "title": "Кэш только в SQLite", "body": "кэш кэш",
               "certainty": "axiom", "data": {"kind": "decision"}, "force": true}),
    )
    .await;
    let hypothesis = entry(
        &a,
        &tok,
        &sol(),
        json!({"kind": "insight", "title": "Кэш в Redis возможно", "body": "кэш",
               "certainty": "hypothesis", "data": {"kind": "decision"}, "force": true}),
    )
    .await;
    let gotcha = entry(
        &a,
        &tok,
        &sol(),
        json!({"kind": "insight", "title": "Кэш сбрасывается при миграции", "body": "кэш",
               "data": {"kind": "gotcha"}, "refs": {"files": [{"path": "crates/cache/src/lib.rs"}]},
               "force": true}),
    )
    .await;
    let file_note = entry(
        &a,
        &tok,
        &sol(),
        json!({"kind": "insight", "title": "Схема плана", "body": "раскладка",
               "data": {"kind": "pattern"}, "refs": {"files": [{"path": "docs/plan.md"}]},
               "force": true}),
    )
    .await;
    let term = entry(
        &a,
        &tok,
        &sol(),
        json!({"kind": "term", "title": "Квазикэш", "body": "",
               "data": {"definition": "Кэш выжимок поиска"}}),
    )
    .await;
    let task = entry(
        &a,
        &tok,
        &sol(),
        json!({"kind": "task", "title": "Починить кэш", "data": {"next_step": "тест"}}),
    )
    .await;
    let r = a
        .tok_rev(
            &tok,
            Method::PATCH,
            &format!("/api/v1/entries/{task}"),
            Some(json!({"status": "in_progress"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = a
        .tok_h(
            &tok,
            Method::POST,
            &format!("/api/v1/entries/{task}/claim"),
            Some(json!({})),
            &[("x-zam-session", "p1")],
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    World {
        a,
        axiom,
        hypothesis,
        gotcha,
        term,
        file_note,
        task,
    }
}

async fn brief(a: &Api, tok: &str, session: &str, body: Value) -> Value {
    let r = a
        .tok_h(
            tok,
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

const TASK: &str = "Разбери кэш: квазикэш в `crates/cache/src/lib.rs:12` и docs/plan.md";

/// Разделы по порядку (правила → решения → термины → ловушки → файлы), только `axiom`/`theorem`,
/// бюджет `recall.brief_tokens` и меньший бюджет запроса; показ — `usage.source = brief`.
#[tokio::test]
async fn brief_sections_and_budget() {
    let w = world("sections").await;
    let a = &w.a;
    let j = brief(
        a,
        &a.token,
        "p1:ag1",
        json!({"event": "brief", "solution": G, "task": TASK}),
    )
    .await;
    let t = text(&j);
    let order = [
        "### Правила",
        "### Решения",
        "### Термины",
        "### Ловушки",
        "### Файлы",
    ];
    for w2 in order.windows(2) {
        assert!(pos(t, w2[0]) < pos(t, w2[1]), "{w2:?}: {t}");
    }
    assert!(t.contains("Всегда пиши тесты"), "{t}");
    assert!(t.contains(&format!("i:{}", w.axiom)), "{t}");
    assert!(!t.contains(&format!("i:{}", w.hypothesis)), "{t}");
    assert!(t.contains("«Квазикэш» — Кэш выжимок поиска"), "{t}");
    assert!(t.contains(&format!("t:{}", w.term)), "{t}");
    assert!(
        pos(t, "### Ловушки") < pos(t, &format!("i:{}", w.gotcha)),
        "{t}"
    );
    // файл ловушки уже показан выше — в «Файлах» только запись о docs/plan.md
    let files = &t[pos(t, "### Файлы")..];
    assert!(
        files.contains(&format!(
            "файл docs/plan.md: Схема плана [insight] — i:{}",
            w.file_note
        )),
        "{t}"
    );
    assert!(!files.contains(&w.gotcha), "{t}");
    // запись о файле (ревизия неизвестна) в session_seen не отмечается, решение — отмечается
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let seen = |id: String| {
        let db = db.clone();
        async move {
            db.read(move |c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM session_seen WHERE session_id = 'p1:ag1'
                     AND target_id = ?1",
                    [id],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap()
        }
    };
    assert_eq!(seen(w.file_note.clone()).await, 0);
    assert_eq!(seen(w.axiom.clone()).await, 1);
    assert!(!t.contains("### Задачи"), "{t}");
    assert!(!t.contains(&w.task), "{t}");
    assert!(j.get("hooks").is_none(), "{j}");
    let tokens = j["tokens"].as_u64().unwrap();
    assert!(tokens > 0 && tokens <= 800, "{tokens}");
    let reasons: Vec<&str> = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["reason"].as_str().unwrap())
        .collect();
    for r in ["rules", "decision", "glossary", "gotcha", "file"] {
        assert!(reasons.contains(&r), "{r}: {j}");
    }

    // повтор той же сессии — пакет тот же (повторы не отсекаются)
    let j2 = brief(
        a,
        &a.token,
        "p1:ag1",
        json!({"event": "brief", "solution": G, "task": TASK}),
    )
    .await;
    assert_eq!(text(&j2), t);

    // меньший бюджет: первыми остаются правила
    let j = brief(
        a,
        &a.token,
        "p1:ag2",
        json!({"event": "brief", "solution": G, "task": TASK, "budget_tokens": 40}),
    )
    .await;
    let t = text(&j);
    assert!(j["tokens"].as_u64().unwrap() <= 40, "{t}");
    assert!(t.starts_with("### Правила\n"), "{t}");
    assert!(!t.contains("### Файлы"), "{t}");

    // показы — в usage с источником brief
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let n: i64 = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM usage WHERE event = 'recall_shown' AND source = 'brief'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert!(n > 0, "{n}");

    // бюджет по умолчанию — `recall.brief_tokens`
    let small = api("brief-small-budget", |c| {
        roomy(c);
        c.recall.brief_tokens = 100;
    })
    .await;
    allow_rules(&small).await;
    for i in 0..10 {
        small
            .note(
                "rules",
                &format!("правило-{i}.md"),
                &format!("# Правило {i}\n\nДлинное правило номер {i} про кэш и тесты."),
            )
            .await;
    }
    let j = brief(
        &small,
        &small.token,
        "s",
        json!({"event": "brief", "task": "кэш"}),
    )
    .await;
    let tokens = j["tokens"].as_u64().unwrap();
    assert!(tokens > 0 && tokens <= 100, "{tokens}: {j}");
    assert!(text(&j).matches("правило правило-").count() < 10, "{j}");
}

/// Без `task` — мини-дайджест: правила L0, ловушки, задачи (захваченная родителем — первой);
/// без разделов решений, терминов и файлов. `hooks.subagent_start: false` пакет не гасит:
/// гейт хука — на клиенте (ревью задачи 10, I-1).
#[tokio::test]
async fn brief_without_task_minidigest() {
    let w = world("mini").await;
    let a = &w.a;
    for body in [
        json!({"event": "brief", "solution": G, "parent_id": "p1"}),
        json!({"event": "brief", "solution": G, "parent_id": "p1", "task": "   "}),
    ] {
        let j = brief(a, &a.token, "p1:ag1", body).await;
        let t = text(&j);
        assert!(pos(t, "### Правила") < pos(t, "### Ловушки"), "{t}");
        assert!(pos(t, "### Ловушки") < pos(t, "### Задачи"), "{t}");
        assert!(t.contains(&format!("i:{}", w.gotcha)), "{t}");
        assert!(t.contains(&format!("k:{}", w.task)), "{t}");
        for s in ["### Решения", "### Термины", "### Файлы"] {
            assert!(!t.contains(s), "{s}: {t}");
        }
    }
    let uri = format!("/api/v1/admin/users/{}/settings", a.admin_id());
    let r = a
        .adm(
            Method::PATCH,
            &uri,
            Some(json!({"hooks": {"subagent_start": false}})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = brief(
        a,
        &a.token,
        "p1:ag1",
        json!({"event": "brief", "solution": G, "task": TASK}),
    )
    .await;
    assert!(text(&j).contains("### Решения"), "{j}");
}

/// `task` длиннее 2000 символов обрезается (не 400): термин после предела не находится,
/// `warnings: ["task truncated"]`; ровно 2000 — без предупреждения.
#[tokio::test]
async fn brief_long_task_truncated() {
    let w = world("long").await;
    let a = &w.a;
    entry(
        a,
        &a.token,
        &sol(),
        json!({"kind": "term", "title": "Мегатерм", "body": "",
               "data": {"definition": "Термин в хвосте"}}),
    )
    .await;
    let head = "квазикэш ";
    let pad = "я".repeat(2000 - head.chars().count());
    let long = format!("{head}{pad} мегатерм");
    let j = brief(
        a,
        &a.token,
        "s",
        json!({"event": "brief", "solution": G, "task": long}),
    )
    .await;
    let t = text(&j);
    assert!(t.contains("«Квазикэш»"), "{t}");
    assert!(!t.contains("Мегатерм"), "{t}");
    let warnings: Vec<&str> = j["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(warnings.contains(&"task truncated"), "{j}");
    let exact = format!("{head}{pad}");
    assert_eq!(exact.chars().count(), 2000);
    let j = brief(
        a,
        &a.token,
        "s",
        json!({"event": "brief", "solution": G, "task": exact}),
    )
    .await;
    assert!(!j["warnings"].to_string().contains("task truncated"), "{j}");
}

/// Участник общего решения получает решение, термины и файлы пространства решения, но не
/// Knowledge владельца (ловушку, термин и ссылку на файл с `X-Zam-Solution` этого решения).
#[tokio::test]
async fn brief_participant_scope() {
    let a = api("brief-member", roomy).await;
    let (_, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    assert_eq!(register(&a, &a_tok).await.status, StatusCode::CREATED);
    assert_eq!(register(&a, &b_tok).await.status, StatusCode::ACCEPTED);
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
            Some(json!({"access": "ro"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let kn = "/api/v1/spaces/knowledge/entries";
    let shared = entry(
        &a,
        &a_tok,
        &sol(),
        json!({"kind": "insight", "title": "Кэш решения ломается", "body": "кэш",
               "data": {"kind": "gotcha"}, "refs": {"files": [{"path": "src/shared.rs"}]},
               "force": true}),
    )
    .await;
    let private = entry(
        &a,
        &a_tok,
        kn,
        json!({"kind": "insight", "title": "Кэш Анны ломается", "body": "кэш",
               "data": {"kind": "gotcha"}, "refs": {"files": [{"path": "src/secret.rs"}]},
               "force": true}),
    )
    .await;
    let private_term = entry(
        &a,
        &a_tok,
        kn,
        json!({"kind": "term", "title": "Анникэш", "body": "",
               "data": {"definition": "Личный термин"}}),
    )
    .await;
    let task = "кэш анникэш src/secret.rs src/shared.rs";
    let j = brief(
        &a,
        &b_tok,
        "b1",
        json!({"event": "brief", "solution": G, "task": task}),
    )
    .await;
    let t = text(&j);
    assert!(t.contains(&format!("i:{shared}")), "{t}");
    assert!(!t.contains(&private), "{t}");
    assert!(!t.contains(&private_term), "{t}");
    assert!(!t.contains("Анникэш"), "{t}");
    // у владельца те же записи видны
    let j = brief(
        &a,
        &a_tok,
        "a1",
        json!({"event": "brief", "solution": G, "task": task}),
    )
    .await;
    let t = text(&j);
    assert!(t.contains(&format!("i:{private}")), "{t}");
    assert!(t.contains(&format!("t:{private_term}")), "{t}");
}

/// MCP `memory_recall` (сессия `m1`, решение `G`); текст результата.
async fn mcp_recall(a: &Api, args: Value) -> String {
    let b = axum::http::Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::HOST, "memory.lan:7700")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {}", a.token))
        .header("x-zam-session", "m1")
        .header("x-zam-solution", G);
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": "memory_recall", "arguments": args}});
    let r = a.send(b.body(Body::from(body.to_string())).unwrap()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_ne!(j["result"]["isError"], true, "{j}");
    j["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

/// MCP `memory_recall {event: "brief", task}` — тот же пакет. Выключенные у пользователя
/// `hooks.subagent_start` и `hooks.session_start` не гасят ни `brief`, ни `recovery`:
/// MCP-вызов от настроек подсказок хуков не зависит (§0, §10; ревью задачи 10, I-1).
#[tokio::test]
async fn mcp_recall_brief() {
    let w = world("mcp").await;
    let a = &w.a;
    let t = mcp_recall(a, json!({"event": "brief", "task": TASK})).await;
    assert!(pos(&t, "### Решения") < pos(&t, "### Термины"), "{t}");
    assert!(t.contains(&format!("i:{}", w.axiom)), "{t}");
    assert!(t.contains(&format!("i:{}", w.file_note)), "{t}");
    let uri = format!("/api/v1/admin/users/{}/settings", a.admin_id());
    let r = a
        .adm(
            Method::PATCH,
            &uri,
            Some(json!({"hooks": {"subagent_start": false, "session_start": false}})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let t = mcp_recall(a, json!({"event": "brief", "task": TASK})).await;
    assert!(t.contains(&format!("i:{}", w.axiom)), "{t}");
    let t = mcp_recall(a, json!({"event": "recovery"})).await;
    assert!(t.contains("### Задачи"), "{t}");
    assert!(t.contains(&format!("k:{}", w.task)), "{t}");
}
