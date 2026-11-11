//! Приёмка M5.7 «Идеи» (спецификация `docs/specs/2026-11-04-m5.7-ideas.md` §13, задача 6),
//! серверная часть: дайджест `### Идеи` только на первом старте сессии (п. 1), подсказка идей
//! отдельным поиском (п. 2, ML), предложение по MCP и «начать» на странице «Вопросы» одной
//! транзакцией (п. 3), атомарность и одно решение (п. 4), ответ из чата с `defer` (п. 5), отказ и
//! `reopen` (п. 6), идеи «от агента» (п. 7), выключатель (п. 8), права и общее решение (п. 9),
//! связь с задачей и тупики (п. 10), блок v6 и MCP (п. 11), миграция базы M5.6 (0023 → 0024,
//! п. 12).
//!
//! Клиентская часть (`zam idea`, хук `session-start`, замер p95 с 50 идеями) —
//! `crates/zam-cli/tests/m57_acceptance.rs`; WebUI — `web/e2e/ideas.spec.ts`. Наступивший
//! `horizon` ставится прямым SQL. Серверы без моделей (FTS), кроме п. 2.
mod common;

use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use common::api::{Api, Resp, api, req};
use common::ml::{MINILM, external, have, ml, until, vector_ready};
use common::qdrant::TestQdrant;
use serde_json::{Value, json};
use zam_core::Id;
use zam_server::config::ServerConfig;
use zam_store::{
    Access, Attribution, AuthorKind, Certainty, Db, EntryKind, NewEntry, NewReportFile,
    USER_MIGRATIONS, attachment_link, create_entry, create_report,
};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e58";
const KN: &str = "/api/v1/spaces/knowledge/entries";
const RECALL: &str = "/api/v1/recall";
const SECTION: &str = "### Идеи";
const HINT: &str = "Предложи человеку одну: memory_update_entry action=propose — вопрос «начать / \
                    отложить / отказаться»; без ответа «начать» не начинай.";
const INDEXED: Duration = Duration::from_secs(240);
const PASS: &str = "first-pass-123";
const NEW_PASS: &str = "second-pass-456";
/// Решения `decision` базы.
const DECISIONS: &str = "SELECT count(*) FROM entries
    WHERE kind = 'insight' AND json_extract(data, '$.kind') = 'decision'";

// ---------- помощники ----------

fn sol() -> String {
    format!("/api/v1/spaces/{G}/entries")
}

fn ent(id: &str) -> String {
    format!("/api/v1/entries/{id}")
}

fn id_of(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("нет id: {v}"))
        .to_string()
}

fn ok(r: &Resp, s: StatusCode, ctx: &str) {
    assert_eq!(r.status, s, "{ctx}: {}", r.text());
}

fn problem(r: &Resp, s: StatusCode, kind: &str, ctx: &str) {
    ok(r, s, ctx);
    assert_eq!(
        r.json()["type"],
        format!("/problems/{kind}"),
        "{ctx}: {}",
        r.text()
    );
}

/// Сервер с решением `G` администратора; `recall.timeout_ms` поднят (отладочная сборка на HDD).
async fn world(name: &str, tweak: impl FnOnce(&mut ServerConfig)) -> Api {
    let a = api(&format!("m57-{name}"), |c| {
        c.recall.timeout_ms = 30_000;
        tweak(c);
    })
    .await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Идеи M5.7"})),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    a
}

/// Идея из WebUI (сессия администратора) в `path` с `data`.
async fn idea_web(a: &Api, path: &str, title: &str, data: Value) -> String {
    let r = a
        .adm(
            Method::POST,
            path,
            Some(
                json!({"kind": "idea", "title": title, "body": "Замысел человека.",
                        "data": data}),
            ),
            None,
        )
        .await;
    ok(&r, StatusCode::CREATED, title);
    id_of(&r.json())
}

/// Идея токеном администратора с `origin`.
async fn idea_tok(a: &Api, path: &str, title: &str, origin: &str) -> String {
    let r = a
        .t(
            Method::POST,
            path,
            Some(json!({"kind": "idea", "title": title, "body": "Замысел.",
                        "data": {"origin": origin}})),
        )
        .await;
    ok(&r, StatusCode::CREATED, title);
    id_of(&r.json())
}

async fn get(a: &Api, id: &str) -> Value {
    let r = a.t(Method::GET, &ent(id), None).await;
    ok(&r, StatusCode::OK, id);
    r.json()
}

async fn post_tok(a: &Api, id: &str, op: &str, body: Value) -> Resp {
    a.t(Method::POST, &format!("{}/{op}", ent(id)), Some(body))
        .await
}

async fn post_web(a: &Api, id: &str, op: &str, body: Value) -> Resp {
    a.adm(Method::POST, &format!("{}/{op}", ent(id)), Some(body), None)
        .await
}

async fn recall_as(a: &Api, tok: &str, session: &str, body: Value) -> Value {
    let r = a
        .tok_h(
            tok,
            Method::POST,
            RECALL,
            Some(body),
            &[("x-zam-session", session)],
        )
        .await;
    ok(&r, StatusCode::OK, "recall");
    r.json()
}

async fn start(a: &Api, session: &str) -> Value {
    recall_as(
        a,
        &a.token,
        session,
        json!({"event": "session_start", "solution": G}),
    )
    .await
}

async fn prompt(a: &Api, session: &str, text: &str) -> Value {
    recall_as(
        a,
        &a.token,
        session,
        json!({"event": "prompt", "solution": G, "prompt": text}),
    )
    .await
}

fn text(j: &Value) -> &str {
    j["text"].as_str().unwrap()
}

/// Ссылки `w:<id>` подсказок-идей ответа по порядку.
fn idea_items(j: &Value) -> Vec<String> {
    j["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["reason"] == "idea")
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

/// Подсказки, кроме идей: `(id, reason)` по порядку.
fn other_items(j: &Value) -> Vec<(String, String)> {
    j["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["reason"] != "idea")
        .map(|i| {
            (
                i["id"].as_str().unwrap_or_default().to_string(),
                i["reason"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Строки раздела `### Идеи` (без заголовка); нет раздела — `None`.
fn section_lines(j: &Value) -> Option<Vec<String>> {
    let rest = text(j).split(&format!("{SECTION}\n")).nth(1)?;
    let body = rest.split("\n\n").next().unwrap();
    Some(body.lines().map(str::to_string).collect())
}

fn w(id: &str) -> String {
    format!("w:{id}")
}

/// База пользователя `user`.
async fn db_of(a: &Api, user: Id) -> Db {
    a.st.users.get(user).await.unwrap()
}

/// `SELECT count(*)` по базе администратора.
async fn count(a: &Api, q: &'static str) -> i64 {
    db_of(a, a.st.admin_id)
        .await
        .read(move |c| Ok(c.query_row(q, [], |r| r.get(0))?))
        .await
        .unwrap()
}

/// Прямой SQL с параметром `id` по базе администратора; число изменённых строк.
async fn sql(a: &Api, q: &'static str, id: &str) -> usize {
    let id = id.to_string();
    db_of(a, a.st.admin_id)
        .await
        .write(move |tx| Ok(tx.execute(q, [id])?))
        .await
        .unwrap()
}

/// Сегодня (UTC) + `days`, `YYYY-MM-DD`.
fn today_plus(days: i64) -> String {
    jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .checked_add(jiff::Span::new().days(days))
        .unwrap()
        .to_string()
}

/// Правка токеном с `If-Match` текущей ревизии.
async fn patch_tok(a: &Api, id: &str, body: Value) -> Resp {
    a.tok_rev(&a.token, Method::PATCH, &ent(id), Some(body))
        .await
}

/// JSON-RPC к `/mcp` по токену `tok` с решением `G` и сессией `session`: `result`.
async fn rpc(a: &Api, tok: &str, session: &str, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let r = a
        .send(
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(header::HOST, "memory.lan:7700")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header(header::AUTHORIZATION, format!("Bearer {tok}"))
                .header("x-zam-solution", G)
                .header("x-zam-session", session)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
    ok(&r, StatusCode::OK, method);
    let j = r.json();
    assert!(j.get("error").is_none(), "ошибка JSON-RPC: {j}");
    j["result"].clone()
}

/// Вызов инструмента MCP: (текст, isError).
async fn mcp(a: &Api, session: &str, name: &str, args: Value) -> (String, bool) {
    let r = rpc(
        a,
        &a.token,
        session,
        "tools/call",
        json!({"name": name, "arguments": args}),
    )
    .await;
    (
        r["content"][0]["text"].as_str().unwrap_or("").to_string(),
        r["isError"].as_bool() == Some(true),
    )
}

/// Первое слово после `prefix` в `text` (ссылка из ответа MCP).
fn after(text: &str, prefix: &str) -> String {
    let i = text
        .find(prefix)
        .unwrap_or_else(|| panic!("нет «{prefix}»: {text}"));
    text[i + prefix.len()..]
        .split(|c: char| c.is_whitespace())
        .next()
        .unwrap()
        .to_string()
}

// ---------- п. 1 ----------

/// §13 п. 1: 4 идеи решения из WebUI (`high`, 2 × `normal`, `low`) и одна в Knowledge →
/// `session_start` S1 — ровно 3 строки в порядке §6.1 без Knowledge; повторный старт S1
/// (`compact`) — без раздела, ни прежних трёх, ни четвёртой; S2 — снова первые три.
#[tokio::test]
async fn acceptance_digest_top3_first_start_only() {
    let a = world("digest", |_| {}).await;
    let high = idea_web(&a, &sol(), "Важная идея", json!({"priority": "high"})).await;
    let n_old = idea_web(&a, &sol(), "Обычная старая", json!({})).await;
    let n_new = idea_web(&a, &sol(), "Обычная новая", json!({"priority": "normal"})).await;
    let low = idea_web(&a, &sol(), "Низкая идея", json!({"priority": "low"})).await;
    let kn = idea_web(&a, KN, "Общая идея", json!({"priority": "high"})).await;
    let want = vec![
        HINT.to_string(),
        format!("ZeroAIMemory: идея «Важная идея» [high] — w:{high}"),
        format!("ZeroAIMemory: идея «Обычная новая» [normal] — w:{n_new}"),
        format!("ZeroAIMemory: идея «Обычная старая» [normal] — w:{n_old}"),
    ];
    let j = start(&a, "s1").await;
    assert_eq!(section_lines(&j), Some(want.clone()), "{j}");
    assert_eq!(idea_items(&j), [w(&high), w(&n_new), w(&n_old)]);
    assert!(!text(&j).contains(&low) && !text(&j).contains(&kn), "{j}");
    let j = recall_as(
        &a,
        &a.token,
        "s1",
        json!({"event": "session_start", "solution": G, "source": "compact"}),
    )
    .await;
    assert!(section_lines(&j).is_none(), "{j}");
    for id in [&high, &n_new, &n_old, &low, &kn] {
        assert!(!text(&j).contains(id.as_str()), "{id}: {j}");
    }
    let j = start(&a, "s2").await;
    assert_eq!(section_lines(&j), Some(want), "{j}");
}

// ---------- п. 2 ----------

/// §13 п. 2 (ML, MiniLM, `recall.prompt_min_score = 0,6`): `prompt`, близкий к идее решения, —
/// одна строка идеи, повтор в той же сессии — без неё; идея, показанная дайджестом S1, в
/// `prompt` S1 не приходит; общая идея Knowledge без связи не приходит даже на почти дословный
/// запрос (косинус не учитывается — ответ владельца на вопрос 8, А), с тегом задачи
/// `in_progress` сессии — приходит; insight-подсказки и их баллы с идеями те же.
#[tokio::test]
async fn acceptance_prompt_hint_rules() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("m57-prompt");
    let a = world("prompt", |c| {
        ml(c, MINILM);
        external(c, &q);
        c.recall.prompt_min_score = 0.6;
    })
    .await;
    vector_ready(&a).await;
    let db = db_of(&a, a.st.admin_id).await;
    let indexed = async |id: &str| {
        let eid: Id = id.parse().unwrap();
        a.st.index.worker.notify();
        until(
            "запись проиндексирована",
            INDEXED,
            async || {
                zam_store::get_entry(&db, eid).await.unwrap().index_state
                    == zam_store::IndexState::Indexed
            },
        )
        .await;
    };
    let r = a
        .t(
            Method::POST,
            &sol(),
            Some(
                json!({"kind": "insight", "title": "PDF export loses embedded fonts",
                        "body": "The PDF renderer drops custom fonts unless they are embedded.",
                        "data": {"kind": "gotcha"}}),
            ),
        )
        .await;
    ok(&r, StatusCode::CREATED, "insight");
    let ins = id_of(&r.json());
    indexed(&ins).await;
    let p = "export reports to PDF with embedded fonts";
    let base = prompt(&a, "p0", p).await;
    let base_items = other_items(&base);
    let base_scores = main_search(&a, p).await;
    assert!(
        base_scores.iter().any(|(id, _)| *id == ins),
        "{base_scores:?}"
    );
    let idea_id = idea_tok(
        &a,
        &sol(),
        "Export reports to PDF with embedded fonts",
        "human",
    )
    .await;
    let dark = idea_tok(&a, &sol(), "Dark theme for the web interface", "human").await;
    let r = a
        .t(
            Method::POST,
            KN,
            Some(
                json!({"kind": "idea", "title": "Telemetry dashboard for nightly benchmarks",
                        "body": "Charts of nightly runs.", "tags": ["bench"],
                        "data": {"origin": "human"}}),
            ),
        )
        .await;
    ok(&r, StatusCode::CREATED, "идея Knowledge");
    let kn = id_of(&r.json());
    for id in [&idea_id, &dark, &kn] {
        indexed(id).await;
    }
    // идея решения — одна строка; повтор в той же сессии — без неё
    let j = prompt(&a, "p1", p).await;
    assert_eq!(idea_items(&j), [w(&idea_id)], "{j}");
    let item = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["reason"] == "idea")
        .unwrap();
    if let Some(s) = item["score"].as_f64() {
        assert!(s >= 0.6, "балл {s}: {item}");
    }
    assert_eq!(other_items(&j), base_items, "insight-подсказки те же");
    let j = prompt(&a, "p1", p).await;
    assert!(idea_items(&j).is_empty(), "{j}");
    // показанная дайджестом идея в prompt той же сессии не приходит
    // положительный контроль: в чистой сессии тот же запрос подсказывает эту идею
    let j = prompt(&a, "p2c", "dark theme for the web interface").await;
    assert_eq!(idea_items(&j), [w(&dark)], "{j}");
    let d = start(&a, "p2").await;
    assert!(idea_items(&d).contains(&w(&dark)), "{d}");
    let j = prompt(&a, "p2", "dark theme for the web interface").await;
    assert!(!idea_items(&j).contains(&w(&dark)), "{j}");
    // посторонний запрос — идей решения нет: ближайшая по вектору идея без лексического
    // подтверждения не подсказывается (финальное ревью M5.7, I-1)
    let j = prompt(&a, "p-off", "configure server logging rotation").await;
    assert!(idea_items(&j).is_empty(), "посторонний запрос: {j}");
    // Knowledge без связи — нет даже при совпадении текста
    let pk = "telemetry dashboard for nightly benchmarks";
    let j = prompt(&a, "p3", pk).await;
    assert!(!idea_items(&j).contains(&w(&kn)), "без связи: {j}");
    // тег задачи `in_progress` сессии p4 — явная связь
    let r = a
        .t(
            Method::POST,
            &sol(),
            Some(json!({"kind": "task", "title": "Бенчмарки", "tags": ["bench"]})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "задача");
    let task = id_of(&r.json());
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &format!("{}/claim", ent(&task)),
            Some(json!({})),
            &[("x-zam-session", "p4")],
        )
        .await;
    ok(&r, StatusCode::OK, "claim");
    ok(
        &patch_tok(&a, &task, json!({"status": "in_progress"})).await,
        StatusCode::OK,
        "in_progress",
    );
    let j = prompt(&a, "p4", pk).await;
    assert_eq!(idea_items(&j), [w(&kn)], "{j}");
    // основной поиск не меняется: набор и баллы до и после идей
    assert_eq!(main_search(&a, p).await, base_scores);
}

/// Основной поиск подсказок `prompt` (тело как у `prompt_hints`): `(id, балл)` по порядку.
async fn main_search(a: &Api, q: &str) -> Vec<(String, f64)> {
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({
                "query": q,
                "scope": {"solution": G, "include_knowledge": true, "include_rules": true},
                "filters": {"kinds": ["insight", "note"]},
                "mode": "brief",
                "top_k": 20,
                "budget_tokens": 4000,
                "rerank": "off",
                "skip_seen": false,
                "no_cache": true,
                "format": "json",
            })),
        )
        .await;
    ok(&r, StatusCode::OK, "search");
    r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            let id = i["entry_id"].as_str().or(i["node_id"].as_str()).unwrap();
            (id.to_string(), i["score"].as_f64().unwrap())
        })
        .collect()
}

// ---------- п. 3 ----------

/// §13 п. 3: `propose` по MCP → вопрос `open` с вариантами `accept|defer|decline` и
/// `data.task_space`, идея `proposed`, ребро `relates_to`; повторный `propose` — тот же вопрос.
/// Ответ `accept` со страницы «Вопросы» (сессия WebUI) — в одной транзакции идея `accepted` с
/// `task_id`, задача `open` с `derived_from` в `task_space`, вопрос `answered`, одно решение
/// `axiom`; следующий `session_start` — «Ответы человека». Идея Knowledge, предложенная в сессии
/// решения R, после «начать» даёт задачу в R.
#[tokio::test]
async fn acceptance_propose_accept_in_webui_one_tx() {
    let a = world("propose", |_| {}).await;
    let id = idea_web(&a, &sol(), "Экспорт отчётов в PDF", json!({})).await;
    let (t, err) = mcp(
        &a,
        "m1",
        "memory_update_entry",
        json!({"id": w(&id), "action": "propose", "text": "подходит к текущей задаче"}),
    )
    .await;
    assert!(!err, "{t}");
    let qid = after(&t, &format!("Предложено w:{id} → вопрос q:"));
    let q = get(&a, &qid).await;
    assert_eq!(q["status"], "open", "{q}");
    let keys: Vec<&str> = q["data"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["accept", "defer", "decline"], "{q}");
    let space = q["data"]["task_space"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(!space.is_empty(), "{q}");
    let e = get(&a, &id).await;
    assert_eq!(e["status"], "proposed", "{e}");
    assert_eq!(e["data"]["question_id"], qid.as_str(), "{e}");
    let r = a
        .t(Method::GET, &format!("{}/links", ent(&qid)), None)
        .await;
    assert!(
        r.text().contains(&id) && r.text().contains("relates_to"),
        "{}",
        r.text()
    );
    let (t, err) = mcp(
        &a,
        "m1",
        "memory_update_entry",
        json!({"id": w(&id), "action": "propose"}),
    )
    .await;
    assert!(!err, "{t}");
    assert_eq!(
        after(&t, "вопрос q:"),
        qid,
        "повторный propose — тот же вопрос"
    );
    let before = count(&a, DECISIONS).await;
    let r = post_web(
        &a,
        &qid,
        "answer",
        json!({"option": "accept", "channel": "webui"}),
    )
    .await;
    ok(&r, StatusCode::OK, "answer accept");
    let did = r.json()["decision_id"].as_str().unwrap().to_string();
    let e = get(&a, &id).await;
    assert_eq!(e["status"], "accepted", "{e}");
    let task = e["data"]["task_id"].as_str().unwrap().to_string();
    let t = get(&a, &task).await;
    assert_eq!(t["kind"], "task", "{t}");
    assert_eq!(t["status"], "open", "{t}");
    assert_eq!(t["space_id"], space.as_str(), "{t}");
    let r = a
        .t(Method::GET, &format!("{}/links", ent(&task)), None)
        .await;
    assert!(
        r.text().contains(&id) && r.text().contains("derived_from"),
        "{}",
        r.text()
    );
    assert_eq!(get(&a, &qid).await["status"], "answered");
    assert_eq!(count(&a, DECISIONS).await, before + 1, "одно решение");
    assert_eq!(get(&a, &did).await["certainty"], "axiom");
    let j = start(&a, "m2").await;
    assert!(text(&j).contains("Ответы человека"), "{j}");
    assert!(text(&j).contains(&format!("q:{qid}")), "{j}");
    // идея Knowledge, предложенная в сессии решения R → задача в R
    let kn = idea_web(&a, KN, "Общая идея: тёмная тема", json!({})).await;
    let (t, err) = mcp(
        &a,
        "m3",
        "memory_update_entry",
        json!({"id": w(&kn), "action": "propose"}),
    )
    .await;
    assert!(!err, "{t}");
    let kq = after(&t, "вопрос q:");
    assert_eq!(get(&a, &kq).await["data"]["task_space"], space.as_str());
    let r = post_web(
        &a,
        &kq,
        "answer",
        json!({"option": "accept", "channel": "webui"}),
    )
    .await;
    ok(&r, StatusCode::OK, "answer accept Knowledge");
    let kt = get(&a, &kn).await["data"]["task_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        get(&a, &kt).await["space_id"],
        space.as_str(),
        "задача — в R"
    );
}

// ---------- п. 4 ----------

/// Тестовый сбой вставки задачи.
const FAIL_TASK: &str = "CREATE TRIGGER test_fail_task BEFORE INSERT ON entries
    WHEN NEW.kind = 'task' BEGIN SELECT RAISE(ABORT, 'test-fail-task'); END";

/// Состояние для проверки «ничего не изменилось»: ревизии идеи и вопроса, число задач, рёбер,
/// решений.
async fn snapshot(a: &Api, id: &str, qid: &str) -> (Value, Value, i64, i64, i64) {
    (
        get(a, id).await["rev"].clone(),
        get(a, qid).await["rev"].clone(),
        count(a, "SELECT count(*) FROM entries WHERE kind = 'task'").await,
        count(a, "SELECT count(*) FROM edges").await,
        count(a, DECISIONS).await,
    )
}

/// §13 п. 4: сбой вставки задачи (триггер SQLite теста) при `accept` ответом на вопрос и
/// `decide` → 5xx, идея, вопрос, задачи, рёбра и решения не изменились. Решений `decision` —
/// одно на ответ вопроса, одно на `decide` при открытом вопросе, ноль на `decide` без вопроса.
#[tokio::test]
async fn acceptance_accept_atomic_and_one_decision() {
    let a = world("atomic", |_| {}).await;
    let id = idea_tok(&a, &sol(), "Атомарная идея", "human").await;
    let r = post_tok(&a, &id, "propose", json!({})).await;
    ok(&r, StatusCode::OK, "propose");
    let qid = id_of(&r.json()["question"]);
    let before = snapshot(&a, &id, &qid).await;
    let admin_db = db_of(&a, a.st.admin_id).await;
    admin_db
        .write(|tx| Ok(tx.execute_batch(FAIL_TASK)?))
        .await
        .unwrap();
    let r = post_tok(
        &a,
        &qid,
        "answer",
        json!({"option": "accept", "by": "human", "channel": "chat"}),
    )
    .await;
    assert!(r.status.is_server_error(), "{}: {}", r.status, r.text());
    let r = post_tok(
        &a,
        &id,
        "decide",
        json!({"decision": "accept", "by": "human"}),
    )
    .await;
    assert!(r.status.is_server_error(), "{}: {}", r.status, r.text());
    assert_eq!(
        snapshot(&a, &id, &qid).await,
        before,
        "ничего не изменилось"
    );
    assert_eq!(get(&a, &id).await["status"], "proposed");
    assert_eq!(get(&a, &qid).await["status"], "open");
    admin_db
        .write(|tx| Ok(tx.execute_batch("DROP TRIGGER test_fail_task")?))
        .await
        .unwrap();
    // одно решение на ответ вопроса
    let d0 = count(&a, DECISIONS).await;
    let r = post_tok(
        &a,
        &qid,
        "answer",
        json!({"option": "accept", "by": "human", "channel": "chat"}),
    )
    .await;
    ok(&r, StatusCode::OK, "answer");
    assert_eq!(count(&a, DECISIONS).await, d0 + 1);
    // одно на decide при открытом вопросе
    let id2 = idea_tok(&a, &sol(), "Решение при открытом вопросе", "human").await;
    let r = post_tok(&a, &id2, "propose", json!({})).await;
    let q2 = id_of(&r.json()["question"]);
    let d1 = count(&a, DECISIONS).await;
    let r = post_tok(
        &a,
        &id2,
        "decide",
        json!({"decision": "defer", "by": "human", "horizon": today_plus(10)}),
    )
    .await;
    ok(&r, StatusCode::OK, "decide при вопросе");
    assert_eq!(count(&a, DECISIONS).await, d1 + 1);
    assert_eq!(get(&a, &q2).await["status"], "answered");
    // ноль на decide без вопроса
    let id3 = idea_tok(&a, &sol(), "Решение без вопроса", "human").await;
    let d2 = count(&a, DECISIONS).await;
    let r = post_tok(
        &a,
        &id3,
        "decide",
        json!({"decision": "accept", "by": "human"}),
    )
    .await;
    ok(&r, StatusCode::OK, "decide без вопроса");
    assert_eq!(count(&a, DECISIONS).await, d2);
}

// ---------- п. 5 ----------

/// §13 п. 5: ответ из чата `defer` с `horizon` +10 дней → идея `deferred` с этим `horizon`; до
/// срока её нет ни в дайджесте, ни в подсказке; после срока (прямой SQL) — снова кандидат;
/// повторный `defer` с другой датой → 200, `horizon` новый.
#[tokio::test]
async fn acceptance_answer_defer_from_chat() {
    let a = world("defer", |_| {}).await;
    let id = idea_tok(&a, &sol(), "Кэш миниатюр превью", "human").await;
    let r = post_tok(&a, &id, "propose", json!({})).await;
    let qid = id_of(&r.json()["question"]);
    let h = today_plus(10);
    let r = post_tok(
        &a,
        &qid,
        "answer",
        json!({"option": "defer", "horizon": h, "by": "human", "channel": "chat"}),
    )
    .await;
    ok(&r, StatusCode::OK, "answer defer");
    let e = get(&a, &id).await;
    assert_eq!(e["status"], "deferred", "{e}");
    assert_eq!(e["data"]["horizon"], h.as_str(), "{e}");
    let j = start(&a, "d1").await;
    assert!(!text(&j).contains(&id), "{j}");
    let j = prompt(&a, "d2", "кэш миниатюр превью").await;
    assert!(idea_items(&j).is_empty(), "{j}");
    let n = sql(
        &a,
        "UPDATE entries SET data = json_set(data, '$.horizon', '2000-01-01') WHERE id = ?1",
        &id,
    )
    .await;
    assert_eq!(n, 1);
    let j = start(&a, "d3").await;
    assert_eq!(idea_items(&j), [w(&id)], "{j}");
    let j = prompt(&a, "d4", "кэш миниатюр превью").await;
    assert_eq!(idea_items(&j), [w(&id)], "{j}");
    let h2 = today_plus(20);
    let r = post_tok(
        &a,
        &id,
        "decide",
        json!({"decision": "defer", "horizon": h2, "by": "human"}),
    )
    .await;
    ok(&r, StatusCode::OK, "повторный defer");
    assert_eq!(get(&a, &id).await["data"]["horizon"], h2.as_str());
}

// ---------- п. 6 ----------

/// §13 п. 6: `decline` → идеи нет в дайджесте и подсказке новых сессий, `propose` → 409;
/// `reopen` с причиной → снова кандидат.
#[tokio::test]
async fn acceptance_decline_never_again_reopen() {
    let a = world("decline", |_| {}).await;
    let id = idea_tok(&a, &sol(), "Голосовой ввод заметок", "human").await;
    let r = post_tok(
        &a,
        &id,
        "decide",
        json!({"decision": "decline", "by": "human", "reason": "не нужно"}),
    )
    .await;
    ok(&r, StatusCode::OK, "decline");
    assert_eq!(get(&a, &id).await["status"], "declined");
    for s in ["x1", "x2", "x3"] {
        let j = start(&a, s).await;
        assert!(!text(&j).contains(&id), "{s}: {j}");
        let j = prompt(&a, &format!("{s}p"), "голосовой ввод заметок").await;
        assert!(idea_items(&j).is_empty(), "{s}: {j}");
    }
    let r = post_tok(&a, &id, "propose", json!({})).await;
    ok(&r, StatusCode::CONFLICT, "propose отказанной");
    let r = post_web(&a, &id, "decide", json!({"decision": "reopen"})).await;
    problem(
        &r,
        StatusCode::UNPROCESSABLE_ENTITY,
        "reason-required",
        "reopen без причины",
    );
    let r = post_web(
        &a,
        &id,
        "decide",
        json!({"decision": "reopen", "reason": "передумали"}),
    )
    .await;
    ok(&r, StatusCode::OK, "reopen");
    let j = start(&a, "x4").await;
    assert_eq!(idea_items(&j), [w(&id)], "{j}");
}

// ---------- п. 7 ----------

/// §13 п. 7: `memory_log kind=idea data.origin=agent` → нет ни в дайджесте, ни в подсказке (на
/// тексте заголовка), `propose` → 409; лента WebUI с фильтром `origin=agent` её показывает;
/// «принять как свою» → `origin: human`, в новой сессии — в дайджесте. Без `data.origin` токен
/// получает 422; фильтр `origin` без `kind=idea` — 422.
#[tokio::test]
async fn acceptance_agent_ideas_hidden_until_adopt() {
    let a = world("agent", |_| {}).await;
    let (t, err) = mcp(
        &a,
        "g1",
        "memory_log",
        json!({"kind": "idea", "title": "Сжатие журнала хуков",
               "body": "Отложенное замечание ревью.", "data": {"origin": "agent"}}),
    )
    .await;
    assert!(!err, "{t}");
    let id = after(&t, "Записано w:");
    assert_eq!(get(&a, &id).await["data"]["origin"], "agent");
    let j = start(&a, "g2").await;
    assert!(!text(&j).contains(&id), "{j}");
    let j = prompt(&a, "g3", "Сжатие журнала хуков").await;
    assert!(idea_items(&j).is_empty(), "{j}");
    let r = post_tok(&a, &id, "propose", json!({})).await;
    problem(
        &r,
        StatusCode::CONFLICT,
        "idea-agent-origin",
        "propose агентской",
    );
    let r = a
        .adm(
            Method::GET,
            &format!("{}?kind=idea&origin=agent", sol()),
            None,
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "лента «От агента»");
    let items = r.json()["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["id"], id.as_str());
    let r = a
        .adm(Method::GET, &format!("{}?origin=agent", sol()), None, None)
        .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "origin без kind=idea");
    let r = post_web(&a, &id, "adopt", json!({})).await;
    ok(&r, StatusCode::OK, "adopt");
    assert_eq!(get(&a, &id).await["data"]["origin"], "human");
    let j = start(&a, "g4").await;
    assert_eq!(idea_items(&j), [w(&id)], "{j}");
    let r = a
        .t(
            Method::POST,
            &sol(),
            Some(json!({"kind": "idea", "title": "Без происхождения"})),
        )
        .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "без origin");
    assert!(r.text().contains("data.origin"), "{}", r.text());
}

// ---------- п. 8 ----------

/// §13 п. 8: `ideas.proactive = false` у пользователя → ни раздела, ни подсказок, остальной
/// дайджест побайтно равен дайджесту той же базы без идей; то же при `[ideas] proactive = false`
/// сервера.
#[tokio::test]
async fn acceptance_proactive_switch() {
    for server_off in [false, true] {
        let a = world(&format!("switch-{server_off}"), |c| {
            c.ideas.proactive = !server_off;
        })
        .await;
        let r = a
            .t(
                Method::POST,
                &sol(),
                Some(json!({"kind": "insight", "title": "Пул течёт при панике",
                            "body": "Соединение не возвращается.", "data": {"kind": "gotcha"}})),
            )
            .await;
        ok(&r, StatusCode::CREATED, "gotcha");
        let base = start(&a, "o0").await;
        for i in 0..3 {
            idea_web(&a, &sol(), &format!("Идея выключателя {i}"), json!({})).await;
        }
        if !server_off {
            let uri = format!("/api/v1/admin/users/{}/settings", a.admin_id());
            let r = a
                .adm(
                    Method::PATCH,
                    &uri,
                    Some(json!({"ideas": {"proactive": false}})),
                    None,
                )
                .await;
            ok(&r, StatusCode::OK, "ideas.proactive = false");
        }
        let off = start(&a, "o1").await;
        assert_eq!(text(&off), text(&base), "server_off={server_off}");
        assert!(section_lines(&off).is_none());
        let j = prompt(&a, "o2", "идея выключателя").await;
        assert!(idea_items(&j).is_empty(), "{j}");
    }
}

// ---------- п. 9 ----------

/// Сессия пользователя WebUI: cookie, CSRF, адрес клиента.
struct Web {
    cookie: String,
    csrf: String,
    ip: String,
}

fn with_ip(mut r: Request<Body>, ip: &str) -> Request<Body> {
    r.extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip.parse().unwrap(), 40_000)));
    r
}

async fn web(a: &Api, wb: &Web, m: Method, uri: &str, body: Option<Value>) -> Resp {
    let b = req(m, uri, body.as_ref())
        .header(header::COOKIE, &wb.cookie)
        .header("x-csrf-token", &wb.csrf);
    let body = body.map_or_else(Body::empty, |v| Body::from(v.to_string()));
    a.send(with_ip(b.body(body).unwrap(), &wb.ip)).await
}

/// Пользователь с паролем после обязательной смены: (id, токен `rw`, сессия).
async fn ready_user(a: &Api, name: &str, ip: &str) -> (String, String, Web) {
    let (uid, tok) = a.new_user(name).await;
    let r = a
        .adm(
            Method::POST,
            &format!("/api/v1/admin/users/{uid}/password"),
            Some(json!({ "password": PASS })),
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "пароль");
    let body = json!({ "login": name, "password": PASS });
    let r = req(Method::POST, "/api/v1/auth/login", Some(&body))
        .body(Body::from(body.to_string()))
        .unwrap();
    let r = a.send(with_ip(r, ip)).await;
    ok(&r, StatusCode::OK, "вход");
    let wb = Web {
        cookie: r
            .header("set-cookie")
            .split(';')
            .next()
            .unwrap()
            .to_string(),
        csrf: r.json()["csrf"].as_str().unwrap().to_string(),
        ip: ip.into(),
    };
    let r = web(
        a,
        &wb,
        Method::POST,
        "/api/v1/auth/password",
        Some(json!({ "old": PASS, "new": NEW_PASS })),
    )
    .await;
    ok(&r, StatusCode::NO_CONTENT, "смена пароля");
    (uid, tok, wb)
}

/// Шаг доступа участника `member` к общему решению `SG` владельцем `o_tok`.
async fn access(a: &Api, o_tok: &str, member: &str, step: &str, body: Value) {
    let r = a
        .tok(
            o_tok,
            Method::POST,
            &format!("/api/v1/solutions/{SG}/access/{member}/{step}"),
            Some(body),
        )
        .await;
    ok(&r, StatusCode::OK, step);
}

/// Общее решение п. 9.
const SG: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e59";

/// §13 п. 9: права токена (решение и ответ `by: agent` за человека, правка и удаление идеи
/// человека, `adopt`, клиентское `derived_from`), сессия WebUI с `by: agent` — 422, токен `ro`,
/// участники `rw` и `ro` общего решения, отзыв доступа.
#[tokio::test]
async fn acceptance_rights_and_shared() {
    let a = world("rights", |_| {}).await;
    let id = idea_web(&a, &sol(), "Идея человека", json!({})).await;
    let r = post_tok(
        &a,
        &id,
        "decide",
        json!({"decision": "accept", "by": "agent"}),
    )
    .await;
    problem(
        &r,
        StatusCode::FORBIDDEN,
        "idea-human-decision",
        "accept by agent",
    );
    let r = post_tok(&a, &id, "decide", json!({"decision": "accept"})).await;
    ok(&r, StatusCode::BAD_REQUEST, "без by");
    let r = patch_tok(&a, &id, json!({"title": "Переписано агентом"})).await;
    problem(&r, StatusCode::FORBIDDEN, "idea-human-owned", "правка тела");
    let r = patch_tok(&a, &id, json!({"append_body": "Дополнение агента."})).await;
    ok(&r, StatusCode::OK, "append");
    let r = a.tok_rev(&a.token, Method::DELETE, &ent(&id), None).await;
    problem(&r, StatusCode::FORBIDDEN, "idea-human-owned", "удаление");
    let r = post_tok(&a, &id, "adopt", json!({})).await;
    ok(&r, StatusCode::FORBIDDEN, "adopt токеном");
    let r = post_tok(&a, &id, "propose", json!({})).await;
    let qid = id_of(&r.json()["question"]);
    let r = post_tok(
        &a,
        &qid,
        "answer",
        json!({"option": "accept", "by": "agent"}),
    )
    .await;
    ok(&r, StatusCode::FORBIDDEN, "ответ by agent");
    // сессия WebUI с явным by: agent — 422 (ревью задачи 2), в decide и в answer
    let r = post_web(
        &a,
        &id,
        "decide",
        json!({"decision": "decline", "by": "agent"}),
    )
    .await;
    ok(
        &r,
        StatusCode::UNPROCESSABLE_ENTITY,
        "WebUI decide by agent",
    );
    let r = post_web(
        &a,
        &qid,
        "answer",
        json!({"option": "decline", "by": "agent"}),
    )
    .await;
    ok(
        &r,
        StatusCode::UNPROCESSABLE_ENTITY,
        "WebUI answer by agent",
    );
    let r = a
        .t(
            Method::POST,
            &sol(),
            Some(json!({"kind": "task", "title": "Задача агента"})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "задача");
    let task = id_of(&r.json());
    let r = a
        .t(
            Method::POST,
            "/api/v1/edges",
            Some(json!({"src": format!("e:{task}"), "type": "derived_from",
                        "dst": format!("e:{id}")})),
        )
        .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "derived_from на идею");
    // токен ro
    let ro = a.issue(&a.admin_id(), "ro").await;
    let r = a
        .tok(
            &ro,
            Method::POST,
            &sol(),
            Some(json!({"kind": "idea", "title": "ro", "data": {"origin": "human"}})),
        )
        .await;
    ok(&r, StatusCode::FORBIDDEN, "ro пишет");
    let r = a
        .tok(
            &ro,
            Method::POST,
            &format!("{}/decide", ent(&id)),
            Some(json!({"decision": "decline", "by": "human"})),
        )
        .await;
    ok(&r, StatusCode::FORBIDDEN, "ro решает");
    // общее решение: владелец Ольга, участники Марк (rw) и Нина (ro)
    let (_, o_tok, _) = ready_user(&a, "Ольга", "10.9.0.1").await;
    let (m_id, m_tok, m_web) = ready_user(&a, "Марк", "10.9.0.2").await;
    let (n_id, n_tok, n_web) = ready_user(&a, "Нина", "10.9.0.3").await;
    let reg = json!({"agent_solution_id": SG, "name": "Общее"});
    for t in [&o_tok, &m_tok, &n_tok] {
        let r = a
            .tok(t, Method::POST, "/api/v1/solutions", Some(reg.clone()))
            .await;
        assert!(r.status.is_success(), "{}", r.text());
    }
    access(&a, &o_tok, &m_id, "approve", json!({"access": "rw"})).await;
    access(&a, &o_tok, &n_id, "approve", json!({"access": "ro"})).await;
    let feed = format!("/api/v1/spaces/{SG}/entries");
    let r = web(
        &a,
        &m_web,
        Method::POST,
        &feed,
        Some(json!({"kind": "idea", "title": "Идея участника"})),
    )
    .await;
    ok(&r, StatusCode::CREATED, "участник rw создаёт");
    let mi = id_of(&r.json());
    let r = a
        .tok(
            &m_tok,
            Method::POST,
            &feed,
            Some(json!({"kind": "idea", "title": "Агент участника",
                        "data": {"origin": "agent"}})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "токен участника rw создаёт");
    let ma = id_of(&r.json());
    let r = web(
        &a,
        &m_web,
        Method::POST,
        &format!("{}/adopt", ent(&ma)),
        Some(json!({})),
    )
    .await;
    ok(&r, StatusCode::OK, "участник rw принимает как свою");
    let r = web(
        &a,
        &m_web,
        Method::POST,
        &format!("{}/decide", ent(&mi)),
        Some(json!({"decision": "accept"})),
    )
    .await;
    ok(&r, StatusCode::OK, "участник rw решает");
    let r = web(&a, &n_web, Method::GET, &format!("{feed}?kind=idea"), None).await;
    ok(&r, StatusCode::OK, "ro читает");
    assert_eq!(
        r.json()["items"].as_array().unwrap().len(),
        2,
        "{}",
        r.text()
    );
    let r = web(
        &a,
        &n_web,
        Method::POST,
        &feed,
        Some(json!({"kind": "idea", "title": "ro"})),
    )
    .await;
    ok(&r, StatusCode::FORBIDDEN, "участник ro создаёт");
    let r = web(
        &a,
        &n_web,
        Method::POST,
        &format!("{}/decide", ent(&ma)),
        Some(json!({"decision": "decline"})),
    )
    .await;
    ok(&r, StatusCode::FORBIDDEN, "участник ro решает");
    // дайджест участника — идеи общего решения, после отзыва — нет
    let body = json!({"event": "session_start", "solution": SG});
    let j = recall_as(&a, &m_tok, "sh1", body.clone()).await;
    assert_eq!(idea_items(&j), [w(&ma)], "{j}");
    access(&a, &o_tok, &m_id, "revoke", json!({})).await;
    let j = recall_as(&a, &m_tok, "sh2", body).await;
    assert!(idea_items(&j).is_empty(), "{j}");
    assert!(!text(&j).contains(&ma), "{j}");
}

// ---------- п. 10 ----------

async fn accept_web(a: &Api, id: &str) -> String {
    let r = post_web(a, id, "decide", json!({"decision": "accept"})).await;
    ok(&r, StatusCode::OK, "accept");
    id_of(&r.json()["task"])
}

/// §13 п. 10: задача из `accept` → `done` — идея `done`, задача переоткрыта — идея `accepted`;
/// задача `dropped` или удалена — идея `accepted`, `reopen` и `decline` разрешены (ждёт ответа
/// владельца), второй `accept` после `reopen` — новая задача; идея с открытым вопросом удалена
/// → вопрос `obsolete`, вопрос-предложение удалён → идея `new`.
#[tokio::test]
async fn acceptance_task_lifecycle_and_dead_ends() {
    let a = world("lifecycle", |_| {}).await;
    let id = idea_web(&a, &sol(), "Жизненный цикл", json!({})).await;
    let task = accept_web(&a, &id).await;
    let r = patch_tok(&a, &task, json!({"status": "done"})).await;
    ok(&r, StatusCode::OK, "done");
    assert_eq!(get(&a, &id).await["status"], "done");
    let r = patch_tok(
        &a,
        &task,
        json!({"status": "open", "reason": "не доделано"}),
    )
    .await;
    ok(&r, StatusCode::OK, "переоткрытие");
    assert_eq!(get(&a, &id).await["status"], "accepted");
    // dropped → accepted; reopen → второй accept — новая задача
    let r = patch_tok(
        &a,
        &task,
        json!({"status": "dropped", "reason": "не нужно"}),
    )
    .await;
    ok(&r, StatusCode::OK, "dropped");
    assert_eq!(get(&a, &id).await["status"], "accepted");
    let r = post_web(
        &a,
        &id,
        "decide",
        json!({"decision": "reopen", "reason": "задачу сняли"}),
    )
    .await;
    ok(&r, StatusCode::OK, "reopen после dropped");
    assert_eq!(get(&a, &id).await["status"], "new");
    let task2 = accept_web(&a, &id).await;
    assert_ne!(task2, task, "новая задача");
    // задача удалена → accepted; decline разрешён
    let r = a
        .tok_rev(&a.token, Method::DELETE, &ent(&task2), None)
        .await;
    ok(&r, StatusCode::NO_CONTENT, "удаление задачи");
    assert_eq!(get(&a, &id).await["status"], "accepted");
    let r = post_web(&a, &id, "decide", json!({"decision": "decline"})).await;
    ok(&r, StatusCode::OK, "decline после удаления задачи");
    assert_eq!(get(&a, &id).await["status"], "declined");
    // корзина: идея с открытым вопросом удалена → вопрос obsolete
    let i2 = idea_web(&a, &sol(), "Удаляемая идея", json!({})).await;
    let r = post_tok(&a, &i2, "propose", json!({})).await;
    let q2 = id_of(&r.json()["question"]);
    let rev = a.adm(Method::GET, &ent(&i2), None, None).await.json()["rev"].to_string();
    let r = a
        .adm_h(
            Method::DELETE,
            &ent(&i2),
            None,
            &[("if-match", rev.as_str())],
        )
        .await;
    ok(&r, StatusCode::NO_CONTENT, "удаление идеи");
    assert_eq!(get(&a, &q2).await["status"], "obsolete");
    // вопрос-предложение удалён → идея new
    let i3 = idea_tok(&a, &sol(), "Идея без вопроса", "human").await;
    let r = post_tok(&a, &i3, "propose", json!({})).await;
    let q3 = id_of(&r.json()["question"]);
    let r = a.tok_rev(&a.token, Method::DELETE, &ent(&q3), None).await;
    ok(&r, StatusCode::NO_CONTENT, "удаление вопроса");
    let e = get(&a, &i3).await;
    assert_eq!(e["status"], "new", "{e}");
    assert!(e["data"]["question_id"].is_null(), "{e}");
}

// ---------- п. 11 ----------

/// §13 п. 11: блок v6 (ru, en) со строкой идей ≤ 500 токенов bge-m3 и без четырёх перенесённых
/// фраз (они — в протоколе вместе с разделом «Идеи» и `--channel chat`); напоминание v5 → v6 —
/// одно; MCP — 11 инструментов, описания ≤ 300 символов, `propose`/`decide`/`horizon` в схеме.
/// Худший случай (длинные адрес, имя, GUID) — `agent_setup_block_v6_under_500_tokens_ru_en`.
#[tokio::test]
async fn acceptance_block_v6_and_mcp() {
    use zam_markdown::TokenCounter as _;
    let a = world("block", |_| {}).await;
    let counter = common::ml::bge_counter();
    let cases = [
        (
            "ru",
            "- Идея → `idea`; начинать её — только после ответа человека «начать».\n",
            [
                "(409 — перечитай)",
                "заметке — `l0`",
                "перед работой",
                "в чате",
            ],
            "",
            "\n## Идеи\n",
        ),
        (
            "en",
            "- Idea → `idea`; start it only after the human answers “start”.\n",
            [
                "(409 — reread)",
                "give notes an `l0`",
                "before work",
                "in chat",
            ],
            "?lang=en",
            "\n## Ideas\n",
        ),
    ];
    for (lang, idea, moved, guide_q, section) in cases {
        let r = a
            .tok_h(
                &a.token,
                Method::GET,
                &format!("/api/v1/agent-setup?agent=claude&lang={lang}"),
                None,
                &[("host", "memory.lan:7700")],
            )
            .await;
        ok(&r, StatusCode::OK, "agent-setup");
        let j = r.json();
        // с M6 — блок v7 (строка идей и перенесённые фразы M5.7 в нём сохранены)
        let v = zam_core::AGENT_BLOCK_VERSION;
        assert!(v >= 6, "{v}");
        assert_eq!(j["version"], v);
        let block = j["block"].as_str().unwrap();
        assert!(
            block.starts_with(&format!("<!-- zeroaimemory:begin v{v} -->\n")),
            "{block}"
        );
        assert!(block.contains(idea), "{lang}: {block}");
        for m in moved {
            assert!(!block.contains(m), "{lang}: «{m}» в блоке");
        }
        if let Some(c) = &counter {
            let n = c.count(block);
            eprintln!("ZAM_TIMING блок v6 {lang}: {n} токенов bge-m3");
            assert!(n <= 500, "{lang}: {n} токенов");
        }
        let r = a
            .t(Method::GET, &format!("/api/v1/agent-guide{guide_q}"), None)
            .await;
        ok(&r, StatusCode::OK, "agent-guide");
        let g = r.text();
        assert!(g.contains(section), "{lang}: нет раздела идей");
        for m in moved {
            assert!(g.contains(m), "{lang}: «{m}» нет в протоколе");
        }
        assert!(
            g.contains("zam idea accept|defer|decline w:<id> --by human --channel chat"),
            "{lang}: нет --channel chat у zam idea"
        );
    }
    let r = a
        .t(
            Method::POST,
            RECALL,
            Some(json!({"event": "session_start", "agent_block_version": 5, "record": false})),
        )
        .await;
    ok(&r, StatusCode::OK, "напоминание");
    let t = r.json()["text"].as_str().unwrap_or_default().to_string();
    let want = format!("устарел (v5 → v{})", zam_core::AGENT_BLOCK_VERSION);
    assert_eq!(t.matches(want.as_str()).count(), 1, "{t}");
    assert_eq!(t.matches("устарел").count(), 1, "{t}");
    let list = rpc(&a, &a.token, "b1", "tools/list", json!({})).await;
    let tools = list["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 11, "{list}");
    for t in tools {
        let n = t["description"].as_str().unwrap().chars().count();
        assert!(n <= 300, "{}: {n} символов", t["name"]);
    }
    let upd = tools
        .iter()
        .find(|t| t["name"] == "memory_update_entry")
        .unwrap();
    let schema = upd["inputSchema"].to_string();
    for want in ["propose", "decide", "horizon", "decision"] {
        assert!(schema.contains(want), "{want}: {schema}");
    }
}

// ---------- п. 12 ----------

fn agent_attr() -> Attribution {
    Attribution {
        author_kind: AuthorKind::Agent,
        token_id: Some(Id::new()),
        session_id: Some("s-m56".into()),
        machine: Some("box".into()),
        message: None,
        user_id: None,
        area: None,
        solution: None,
    }
}

fn new_entry(space: Id, kind: EntryKind, title: &str, data: Value) -> NewEntry {
    NewEntry {
        refs: None,
        space_id: space,
        kind,
        title: title.into(),
        body: "Тело записи M5.6.".into(),
        data,
        area: Some("store".into()),
        tags: vec!["m56".into()],
        status: None,
        certainty: (kind == EntryKind::Insight).then_some(Certainty::Hypothesis),
        idempotency_key: None,
        triggers: Vec::new(),
        links: Vec::new(),
    }
}

/// Объект `sqlite_master`: тип, имя, нормализованный SQL.
type Obj = (String, String, String);

/// Снимок базы: объекты `sqlite_master`, строки `entries` (id, вид, статус, `data`, ревизия) и
/// строки связанных таблиц (`entry_revisions`, `entry_attachments`, `spaces`) целиком.
type Snapshot = (Vec<Obj>, Vec<String>, Vec<String>);

/// Строки `entry_revisions`, `entry_attachments` и `spaces` с префиксом таблицы.
const RELATED_ROWS: &str = "
    SELECT 'rev ' || entry_id || ' ' || rev || ' ' || title || ' ' || coalesce(body, '') || ' ' ||
           coalesce(data, '') || ' ' || coalesce(tags, '') || ' ' || coalesce(status, '-') || ' ' ||
           coalesce(certainty, '-') || ' ' || ts || ' ' || author_kind
    FROM entry_revisions
    UNION ALL
    SELECT 'att ' || entry_id || ' ' || blob_sha256 || ' ' || filename || ' ' || mime || ' ' ||
           size || ' ' || created_at
    FROM entry_attachments
    UNION ALL
    SELECT 'space ' || id || ' ' || kind || ' ' || coalesce(solution_guid, '-') || ' ' || name
    FROM spaces
    ORDER BY 1";

fn schema_and_rows(c: &rusqlite::Connection) -> rusqlite::Result<Snapshot> {
    let mut st =
        c.prepare("SELECT type, name, coalesce(sql, '') FROM sqlite_master ORDER BY type, name")?;
    let objs = st
        .query_map([], |r| {
            let sql: String = r.get(2)?;
            Ok((
                r.get(0)?,
                r.get(1)?,
                sql.split_whitespace().collect::<Vec<_>>().join(" "),
            ))
        })?
        .collect::<Result<_, _>>()?;
    let mut st = c.prepare(
        "SELECT id || ' ' || kind || ' ' || coalesce(status, '-') || ' ' || data || ' ' ||
                current_rev FROM entries ORDER BY id",
    )?;
    let rows = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    let mut st = c.prepare(RELATED_ROWS)?;
    let related = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    Ok((objs, rows, related))
}

/// Ошибка прямого SQL — срабатывание триггера отчёта.
fn immutable(r: &Result<usize, zam_store::StoreError>, what: &str) {
    let e = format!("{r:?}");
    assert!(e.contains("report-immutable"), "{what}: {e}");
}

/// §13 п. 12: база пользователя M5.6 (0023: записи всех видов, отчёт с файлом, задача, вопрос)
/// открывается сервером M5.7 и переходит на 0024: строки на месте, `foreign_key_check` пуст,
/// `bak-v23` есть; объекты схемы совпадают (кроме SQL `entries`); три пересозданных триггера
/// 0022 и два оставшихся срабатывают на прямой SQL; идея пишется, «начать» создаёт задачу. Эту
/// же базу (после миграции) M6 берёт фикстурой 0024 → 0025 (журнал этапа, «Передать в M6»).
#[tokio::test]
async fn acceptance_migration_from_m56_db() {
    let a = api("m57-migration", |_| {}).await;
    assert_eq!(USER_MIGRATIONS[22].version, 23);
    let sol_id = Id::new();
    let kn = Id::new();
    let sha = format!("{:08x}{}", 7, "ab".repeat(28));
    // База 0023 собирается вне каталога `users/` и встаёт на место жёсткой ссылкой уже
    // закрытой: фоновые задачи сервера (конвейер документов) открывают базы всех пользователей
    // каждые 2 с и мигрировали бы её посреди заполнения.
    let fixture = a.st.cfg.data_dir.join("fixture-m56.db");
    let (before, report) = {
        let db = Db::open(&fixture, &USER_MIGRATIONS[..23]).unwrap();
        let now = zam_store::time::fmt_ts(jiff::Timestamp::now());
        db.write(move |tx| {
            for (id, kind, guid) in [
                (sol_id, "solution", Some(G)),
                (kn, "knowledge", None),
                (Id::new(), "rules", None),
                (Id::new(), "human", None),
            ] {
                tx.execute(
                    "INSERT INTO spaces (id, kind, solution_guid, name, created_at)
                     VALUES (?1, ?2, ?3, ?2, ?4)",
                    rusqlite::params![id, kind, guid, now],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
        let at = agent_attr();
        let options = json!({"options": [{"key": "a", "text": "SQLite"},
                                         {"key": "b", "text": "PG"}]});
        for e in [
            new_entry(
                sol_id,
                EntryKind::Changelog,
                "Изменение",
                json!({"kind": "fix"}),
            ),
            new_entry(
                sol_id,
                EntryKind::Release,
                "Релиз",
                json!({"version": "0.0.9"}),
            ),
            new_entry(
                sol_id,
                EntryKind::Task,
                "Задача M5.6",
                json!({"next_step": "сделать"}),
            ),
            new_entry(sol_id, EntryKind::Question, "Какую БД?", options),
            new_entry(
                kn,
                EntryKind::Term,
                "Выжимка",
                json!({"definition": "Готовый ответ кэша"}),
            ),
            new_entry(kn, EntryKind::Insight, "Инсайт", json!({"kind": "gotcha"})),
        ] {
            create_entry(&db, e, &at).await.unwrap();
        }
        let mut rep = new_entry(sol_id, EntryKind::Report, "Отчёт", json!({}));
        rep.body = format!(
            "## Итоги\n\n![loss.png]({})\n",
            attachment_link(&sha, "loss.png")
        );
        rep.certainty = None;
        rep.tags = Vec::new();
        let file = NewReportFile {
            sha256: sha.clone(),
            filename: "loss.png".into(),
            mime: "image/png".into(),
            size: 100,
        };
        let (rep, _) = create_report(&db, rep, vec![file], &at).await.unwrap();
        let snap = db
            .read(|c| {
                let v: u32 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
                assert_eq!(v, 23);
                Ok(schema_and_rows(c)?)
            })
            .await
            .unwrap();
        (snap, rep.id)
    };
    // закрытая база — одним файлом (WAL сброшен)
    {
        let c = rusqlite::Connection::open(&fixture).unwrap();
        c.busy_timeout(Duration::from_secs(10)).unwrap();
        c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode = DELETE;")
            .unwrap();
    }
    assert_eq!(before.1.len(), 7, "{:?}", before.1);
    // ревизии (по одной на запись), файл отчёта, четыре пространства
    let count = |p: &str| before.2.iter().filter(|r| r.starts_with(p)).count();
    assert_eq!(
        (count("rev "), count("att "), count("space ")),
        (7, 1, 4),
        "{:?}",
        before.2
    );
    let u = a.st.system.create_user("Мира", false).await.unwrap();
    let (_, tok) =
        a.st.system
            .issue_token(u.id, "t", Access::Rw)
            .await
            .unwrap();
    let path = a.st.cfg.data_dir.join("users").join(format!("{}.db", u.id));
    // жёсткая ссылка не перезаписывает: база, уже открытая сервером, — явный отказ
    std::fs::hard_link(&fixture, &path)
        .unwrap_or_else(|e| panic!("база 0023 не встала на место {}: {e}", path.display()));
    // первый запрос пользователя открывает базу сервером — миграция 0024
    let r = a
        .tok(
            &tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Решение M5.6"})),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    assert_eq!(r.json()["id"], sol_id.to_string(), "прежнее пространство");
    let db = a.st.users.get(u.id).await.unwrap();
    a.st.users.wait_rechunk(u.id).await;
    assert!(path.with_file_name(format!("{}.db.bak-v23", u.id)).exists());
    let (after, version, violations) = db
        .read(|c| {
            let v: u32 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            let mut fk = c.prepare("PRAGMA foreign_key_check")?;
            let n = fk.query_map([], |_| Ok(()))?.count();
            Ok((schema_and_rows(c)?, v, n))
        })
        .await
        .unwrap();
    assert_eq!(version, USER_MIGRATIONS.len() as u32);
    assert!(version >= 24);
    assert_eq!(violations, 0, "foreign_key_check не пуст");
    assert_eq!(after.1, before.1, "строки entries на месте");
    assert_eq!(
        after.2, before.2,
        "строки entry_revisions, entry_attachments, spaces на месте"
    );
    let is_entries = |o: &&Obj| o.0 == "table" && o.1 == "entries";
    // база открывается последней схемой: объекты таблиц 0025 (M6) сверяет
    // `zam-store/tests/migrate_rebuild.rs::migrate_0025_from_m57_db`
    let m6 = |o: &&Obj| {
        [
            "sessions",
            "session_seen",
            "session_tokens",
            "code_refs",
            "tokens_unattributed",
            "session_snapshots",
            "sqlite_autoindex_session_snapshots_1",
            // `nodes.imported_from` (0025, задача 18b2)
            "nodes",
            // ключи идемпотентности заметок (0025, задача 19)
            "node_idempotency",
        ]
        .contains(&o.1.as_str())
    };
    assert_eq!(
        after
            .0
            .iter()
            .filter(|o| !is_entries(o) && !m6(o))
            .collect::<Vec<_>>(),
        before
            .0
            .iter()
            .filter(|o| !is_entries(o) && !m6(o))
            .collect::<Vec<_>>(),
        "объекты схемы, кроме entries и таблиц 0025"
    );
    let entries_sql = &after.0.iter().find(is_entries).unwrap().2;
    assert!(entries_sql.contains("'idea'"), "{entries_sql}");
    // пять триггеров отчёта (три пересозданы в 0024) — прямым SQL
    let exec = |q: String| {
        let db = db.clone();
        async move { db.write(move |tx| Ok(tx.execute(&q, [])?)).await }
    };
    immutable(
        &exec(format!(
            "UPDATE entries SET title = 'x' WHERE id = '{report}'"
        ))
        .await,
        "entries_report_immutable",
    );
    immutable(
        &exec(format!(
            "INSERT INTO entry_revisions (entry_id, rev, title, body, data, tags, area, status,
                                          certainty, reason, ts, author_kind, user_id, token_id,
                                          session_id, machine)
             SELECT entry_id, 2, title, body, data, tags, area, status, certainty, reason, ts,
                    author_kind, user_id, token_id, session_id, machine
             FROM entry_revisions WHERE entry_id = '{report}' AND rev = 1"
        ))
        .await,
        "entry_revisions_report_once",
    );
    immutable(
        &exec(format!(
            "INSERT INTO entry_attachments (entry_id, blob_sha256, filename, mime, size, created_at)
             VALUES ('{report}', '{sha}', 'late.png', 'image/png', 100, '2030-01-01T00:00:00Z')"
        ))
        .await,
        "entry_attachments_report_only",
    );
    immutable(
        &exec("UPDATE entry_attachments SET filename = 'x.png'".into()).await,
        "entry_attachments_no_update",
    );
    immutable(
        &exec("DELETE FROM entry_attachments".into()).await,
        "entry_attachments_no_delete",
    );
    // идея пишется, «начать» создаёт задачу в мигрированной базе
    let r = a
        .tok(
            &tok,
            Method::POST,
            &sol(),
            Some(json!({"kind": "idea", "title": "Идея после миграции",
                        "data": {"origin": "human"}})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "идея");
    let id = id_of(&r.json());
    let r = a
        .tok(
            &tok,
            Method::POST,
            &format!("{}/decide", ent(&id)),
            Some(json!({"decision": "accept", "by": "human"})),
        )
        .await;
    ok(&r, StatusCode::OK, "accept");
    assert_eq!(r.json()["task"]["kind"], "task");
}

// ---------- задача 7 (§16, п. 8) ----------

/// §16, п. 8: предложение вернуть идею со снятой задачей. Сценарий 1: задача снята → идея в
/// дайджесте с пометкой → `propose` без `text` 422, с `text` (MCP) — вопрос «вернуть» → ответ
/// человека `reopen` из чата → идея `new`, `prev_task_ids` со старой задачей, одно решение.
/// Сценарий 2: задача удалена → ответ `decline` в WebUI → `declined`. Сценарий 3: задача
/// переоткрыта при открытом вопросе → вопрос `obsolete`, идея `accepted` без `question_id`.
#[tokio::test]
async fn acceptance_reopen_proposal_flow() {
    let a = world("reopen-flow", |_| {}).await;
    // 1: reopen
    let id = idea_web(&a, &sol(), "Кэш выжимок", json!({"priority": "high"})).await;
    let task = accept_web(&a, &id).await;
    let r = post_tok(&a, &id, "propose", json!({"text": "пора"})).await;
    ok(&r, StatusCode::CONFLICT, "живая задача");
    let r = patch_tok(
        &a,
        &task,
        json!({"status": "dropped", "reason": "не до того"}),
    )
    .await;
    ok(&r, StatusCode::OK, "dropped");
    let j = start(&a, "rf1").await;
    let lines = section_lines(&j).unwrap_or_else(|| panic!("нет раздела: {j}"));
    let line = format!(
        "ZeroAIMemory: идея «Кэш выжимок» [high] — задача снята, можно предложить вернуть — {}",
        w(&id)
    );
    assert!(lines.contains(&line), "{j}");
    assert_eq!(idea_items(&j), [w(&id)], "{j}");
    let r = post_tok(&a, &id, "propose", json!({})).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "propose без text");
    let (t, err) = mcp(
        &a,
        "rf1",
        "memory_update_entry",
        json!({"id": w(&id), "action": "propose", "text": "Кэш снова тормозит на сборке"}),
    )
    .await;
    assert!(!err, "{t}");
    let qid = after(&t, &format!("Предложено вернуть w:{id} → вопрос q:"));
    let q = get(&a, &qid).await;
    assert_eq!(q["title"], "Вернуть идею «Кэш выжимок»?", "{q}");
    assert_eq!(q["data"]["proposal"], "reopen", "{q}");
    let e = get(&a, &id).await;
    assert_eq!(e["status"], "accepted", "{e}");
    assert_eq!(e["data"]["question_id"], qid.as_str(), "{e}");
    let decisions = count(&a, DECISIONS).await;
    let r = post_tok(
        &a,
        &qid,
        "answer",
        json!({"option": "accept", "by": "human"}),
    )
    .await;
    ok(
        &r,
        StatusCode::UNPROCESSABLE_ENTITY,
        "accept на вопросе «вернуть»",
    );
    let r = post_tok(
        &a,
        &qid,
        "answer",
        json!({"option": "reopen", "by": "agent"}),
    )
    .await;
    problem(
        &r,
        StatusCode::FORBIDDEN,
        "idea-human-decision",
        "агент за человека",
    );
    let (t, err) = mcp(
        &a,
        "rf1",
        "memory_update_entry",
        json!({"id": format!("q:{qid}"), "action": "answer", "option": "reopen",
               "by": "human", "channel": "chat"}),
    )
    .await;
    assert!(!err, "{t}");
    let e = get(&a, &id).await;
    assert_eq!(e["status"], "new", "{e}");
    assert_eq!(e["data"]["prev_task_ids"], json!([task]), "{e}");
    assert!(e["data"]["task_id"].is_null(), "{e}");
    assert_eq!(get(&a, &qid).await["status"], "answered");
    assert_eq!(count(&a, DECISIONS).await, decisions + 1);
    // следующий «начать» — новая задача
    let task2 = accept_web(&a, &id).await;
    assert_ne!(task2, task);

    // 2: задача удалена → decline в WebUI («Вопросы»)
    let id2 = idea_web(&a, &sol(), "Голосовой ввод", json!({})).await;
    let t2 = accept_web(&a, &id2).await;
    let r = a.tok_rev(&a.token, Method::DELETE, &ent(&t2), None).await;
    ok(&r, StatusCode::NO_CONTENT, "удаление задачи");
    let j = start(&a, "rf2").await;
    let line = format!(
        "ZeroAIMemory: идея «Голосовой ввод» [normal] — задача удалена, можно предложить \
         вернуть — {}",
        w(&id2)
    );
    assert!(section_lines(&j).unwrap_or_default().contains(&line), "{j}");
    let r = post_tok(&a, &id2, "propose", json!({"text": "снова просили"})).await;
    ok(&r, StatusCode::OK, "propose удалённой задачи");
    let q2 = id_of(&r.json()["question"]);
    let r = post_web(&a, &q2, "answer", json!({"option": "decline"})).await;
    ok(&r, StatusCode::OK, "decline в WebUI");
    assert_eq!(get(&a, &id2).await["status"], "declined");

    // 3: задача переоткрыта при открытом вопросе → вопрос obsolete
    let id3 = idea_web(&a, &sol(), "Тёмная тема", json!({})).await;
    let t3 = accept_web(&a, &id3).await;
    let r = patch_tok(&a, &t3, json!({"status": "dropped", "reason": "позже"})).await;
    ok(&r, StatusCode::OK, "dropped 3");
    let r = post_tok(&a, &id3, "propose", json!({"text": "уже пора"})).await;
    ok(&r, StatusCode::OK, "propose 3");
    let q3 = id_of(&r.json()["question"]);
    let r = patch_tok(&a, &t3, json!({"status": "open", "reason": "вернулись"})).await;
    ok(&r, StatusCode::OK, "переоткрытие задачи");
    assert_eq!(get(&a, &q3).await["status"], "obsolete");
    let e = get(&a, &id3).await;
    assert_eq!(e["status"], "accepted", "{e}");
    assert!(e["data"]["question_id"].is_null(), "{e}");
    let r = post_tok(
        &a,
        &q3,
        "answer",
        json!({"option": "reopen", "by": "human"}),
    )
    .await;
    ok(&r, StatusCode::CONFLICT, "ответ на устаревший вопрос");
}
