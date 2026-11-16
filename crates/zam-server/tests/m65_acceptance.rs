//! Приёмка M6.5 «Разбор идей агентом» (спецификация `docs/specs/2026-11-15-m6.5-idea-review.md`
//! §11, задача 9), серверная часть — сквозными сценариями через REST и MCP:
//! - п. 1 и п. 6 (строка плана по `priority`, `plan` с `task.id`) — `acceptance_plan_flow`;
//! - п. 2 — `acceptance_return_edit_rereview`;
//! - п. 3 — `acceptance_decline_reopen_with_reason`;
//! - п. 4 и п. 6 (`done` без ссылки на сделанное, второй разбор, `propose` у `new` в режиме
//!   `review`) — `acceptance_refusals`;
//! - п. 5 — `acceptance_migration_0026`;
//! - п. 6 (`ideas.mode = propose` — поведение M5.7) — `acceptance_propose_mode_is_m57`.
//!
//! П. 7 (WebUI: чипы и группировка, карточка с разбором, «Изменить» у возвращённой →
//! «Отправлено на разбор») — `web/e2e/ideas.spec.ts` (`ideas_review_*`, `ideas_returned_*`,
//! `tasks_from_idea_mark_and_filter`, `agent_setup_ideas_mode_persists`); CLI (`zam idea
//! review|queue`, старый сервер) — `crates/zam-cli/tests/idea.rs`; блок v8 и протокол «Разбор
//! идей» — `agent_setup.rs`. Серверы без моделей (поиск — FTS).
mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};
use zam_core::Id;
use zam_store::{
    Access, AnswerBy, Attribution, AuthorKind, Channel, Db, EntryKind, EntryPatch, EntryStatus,
    IdeaDecision, NewDecision, NewEntry, NewReportFile, USER_MIGRATIONS, attachment_link,
    create_entry, create_report, decide_idea, propose_idea, update_entry,
};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e65";
const KN: &str = "/api/v1/spaces/knowledge/entries";
const RECALL: &str = "/api/v1/recall";
const SETTINGS: &str = "/api/v1/auth/settings";
const REVIEW_SECTION: &str = "### Идеи на разбор";
const SECTION: &str = "### Идеи";
/// Строка-указание раздела M5.7 (режим `propose`).
const HINT: &str = "Предложи человеку одну: memory_update_entry action=propose — вопрос «начать / \
                    отложить / отказаться»; без ответа «начать» не начинай.";
/// Системное ребро `src → dst` типа `?3` (живое).
const EDGE: &str = "SELECT count(*) FROM edges
    WHERE src_kind = 'entry' AND src_id = ?1 AND dst_kind = 'entry' AND dst_id = ?2
      AND type = ?3 AND origin = 'system' AND deleted_at IS NULL";
/// Рёбра `mentions` от записи `?1` (любого происхождения).
const MENTIONS_FROM: &str = "SELECT count(*) FROM edges WHERE src_id = ?1 AND type = 'mentions'";

// ---------- помощники ----------

fn sol() -> String {
    format!("/api/v1/spaces/{G}/entries")
}

fn ent(id: &str) -> String {
    format!("/api/v1/entries/{id}")
}

fn w(id: &str) -> String {
    format!("w:{id}")
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

/// 409 `/problems/conflict` с кодом `code`.
fn conflict(r: &Resp, code: &str, ctx: &str) {
    problem(r, StatusCode::CONFLICT, "conflict", ctx);
    assert_eq!(r.json()["code"], code, "{ctx}: {}", r.text());
}

/// Разбор для человека не короче `ideas.review_min_chars` (200); `head` — первая строка (она
/// же строка истории и похожей записи в пакете).
fn summary(head: &str) -> String {
    let s = format!(
        "{head}\nСверка: в плане (открытые задачи) такого пункта нет, в ТЗ журнал «Идеи» этого не \
         описывает, по changelog и сделанным задачам темы не было. Решение принято по этой сверке: \
         владелец видит, что стало с идеей, и не обсуждает её второй раз."
    );
    assert!(s.chars().count() >= 200, "{}", s.chars().count());
    s
}

/// Строка-указание очереди разбора с длиной `n`.
fn review_hint(n: usize) -> String {
    format!(
        "Непросмотренных идей: {n}. Разбери, когда закончишь текущий шаг: memory_recall \
         event=idea_review → memory_update_entry action=review."
    )
}

/// Строка идеи раздела дайджеста и пакета.
fn idea_line(title: &str, priority: &str, id: &str) -> String {
    format!("ZeroAIMemory: идея «{title}» [{priority}] — w:{id}")
}

/// Сервер с решением `G` администратора; режим идей — умолчание сервера (`review`);
/// `recall.timeout_ms` поднят (отладочная сборка на HDD).
async fn world(name: &str) -> Api {
    let a = api(&format!("m65-{name}"), |c| c.recall.timeout_ms = 30_000).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Разбор идей M6.5"})),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    a
}

/// Идея человека из WebUI (сессия администратора) в `path`.
async fn idea_web(a: &Api, path: &str, title: &str, body: &str, data: Value) -> String {
    let r = a
        .adm(
            Method::POST,
            path,
            Some(json!({"kind": "idea", "title": title, "body": body, "data": data})),
            None,
        )
        .await;
    ok(&r, StatusCode::CREATED, title);
    id_of(&r.json())
}

/// Запись токеном администратора в решение `G`.
async fn create_tok(a: &Api, body: Value) -> String {
    let r = a.t(Method::POST, &sol(), Some(body.clone())).await;
    ok(&r, StatusCode::CREATED, &body.to_string());
    id_of(&r.json())
}

/// Идея токеном администратора в решении `G` с `origin`.
async fn idea_tok(a: &Api, title: &str, origin: &str) -> String {
    create_tok(
        a,
        json!({"kind": "idea", "title": title, "body": "Замысел.", "data": {"origin": origin}}),
    )
    .await
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

/// Разбор `POST /entries/{id}/review` токеном администратора.
async fn review(a: &Api, id: &str, body: Value) -> Resp {
    post_tok(a, id, "review", body).await
}

/// Правка токеном администратора с `If-Match` текущей ревизии.
async fn patch_tok(a: &Api, id: &str, body: Value) -> Resp {
    a.tok_rev(&a.token, Method::PATCH, &ent(id), Some(body))
        .await
}

/// Правка человеком в WebUI (сессия администратора) с `If-Match` текущей ревизии.
async fn patch_web(a: &Api, id: &str, body: Value) -> Resp {
    let rev = a.adm(Method::GET, &ent(id), None, None).await.json()["rev"].to_string();
    a.adm_h(
        Method::PATCH,
        &ent(id),
        Some(body),
        &[("if-match", rev.as_str())],
    )
    .await
}

async fn recall_as(a: &Api, tok: &str, session: Option<&str>, body: Value) -> Value {
    let h: Vec<(&str, &str)> = session.map(|s| ("x-zam-session", s)).into_iter().collect();
    let r = a.tok_h(tok, Method::POST, RECALL, Some(body), &h).await;
    ok(&r, StatusCode::OK, "recall");
    r.json()
}

/// `session_start` решения `G` токеном `tok` в сессии `session`.
async fn start_as(a: &Api, tok: &str, session: &str) -> Value {
    recall_as(
        a,
        tok,
        Some(session),
        json!({"event": "session_start", "solution": G}),
    )
    .await
}

async fn start(a: &Api, session: &str) -> Value {
    start_as(a, &a.token, session).await
}

fn text(j: &Value) -> &str {
    j["text"].as_str().unwrap()
}

/// Ссылки элементов ответа с `reason` по порядку (все — `kind: idea`).
fn items(j: &Value, reason: &str) -> Vec<String> {
    j["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["reason"] == reason)
        .map(|i| {
            assert_eq!(i["kind"], "idea", "{i}");
            i["id"].as_str().unwrap().to_string()
        })
        .collect()
}

/// Строки раздела `title` (без заголовка) до пустой строки; нет раздела — `None`.
fn section_lines(j: &Value, title: &str) -> Option<Vec<String>> {
    let rest = text(j).split(&format!("{title}\n")).nth(1)?;
    let body = rest.split("\n\n").next().unwrap();
    Some(body.lines().map(str::to_string).collect())
}

/// Строка плана дайджеста есть в ответе.
fn has_line(j: &Value, line: &str) -> bool {
    text(j).lines().any(|l| l == line)
}

/// Блок идеи `id` пакета `idea_review`: от её строки до строки следующей идеи.
fn idea_block<'t>(t: &'t str, id: &str) -> &'t str {
    let at = t
        .find(&format!("— w:{id}\n"))
        .unwrap_or_else(|| panic!("нет идеи {id}: {t}"));
    let rest = &t[at..];
    let end = rest.find("\nZeroAIMemory: ").unwrap_or(rest.len());
    &rest[..end]
}

/// JSON-RPC к `/mcp` по первому токену администратора с решением `G` и сессией `session`:
/// `result`.
async fn rpc(a: &Api, session: &str, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let r = a
        .send(
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(header::HOST, "memory.lan:7700")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header(header::AUTHORIZATION, format!("Bearer {}", a.token))
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

/// `detail` ответа problem+json (нет — паника с телом).
fn detail_of(r: &Resp) -> String {
    r.json()["detail"]
        .as_str()
        .unwrap_or_else(|| panic!("нет detail: {}", r.text()))
        .to_string()
}

/// Отказ инструмента MCP: `isError`, «Ошибка <status>» и причина problem — его `detail`
/// (MCP пишет «Ошибка <код> <title>: <detail>»; `title` у 409 общий — «Conflict», поэтому
/// разные problem различаются по `detail`).
fn mcp_refusal(text: &str, is_error: bool, status: u16, detail: &str) {
    assert!(is_error, "ожидалась ошибка инструмента: {text}");
    assert!(
        text.starts_with(&format!("Ошибка {status} ")),
        "ожидалась «Ошибка {status}»: {text}"
    );
    assert!(text.contains(detail), "нет причины «{detail}»: {text}");
}

/// Строки аудита `action` по цели `target`.
async fn audit_of(a: &Api, action: &str, target: &str) -> Vec<zam_store::AuditRow> {
    a.st.system
        .list_audit(1000, None)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.action == action && r.target.as_deref() == Some(target))
        .collect()
}

/// `SELECT count(*)` с параметрами по базе администратора.
async fn scalar(a: &Api, q: &'static str, params: &[&str]) -> i64 {
    let params: Vec<String> = params.iter().map(|p| p.to_string()).collect();
    a.st.users
        .get(a.st.admin_id)
        .await
        .unwrap()
        .read(move |c| Ok(c.query_row(q, rusqlite::params_from_iter(params.iter()), |r| r.get(0))?))
        .await
        .unwrap()
}

/// Задачи решения `G` (лента токеном).
async fn tasks(a: &Api) -> Vec<Value> {
    let r = a
        .t(Method::GET, &format!("{}?kind=task&limit=200", sol()), None)
        .await;
    ok(&r, StatusCode::OK, "задачи");
    r.json()["items"].as_array().unwrap().clone()
}

/// Счётчики статусов ленты идей решения `G` токеном `tok` (WebUI, чипы «Идеи»).
async fn counts_as(a: &Api, tok: &str) -> Value {
    let r = a
        .tok(
            tok,
            Method::GET,
            &format!("{}/counts?kind=idea", sol()),
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "counts");
    r.json()
}

/// Режим идей администратора со страницы «Подключение агента» (сессия WebUI).
async fn set_mode_web(a: &Api, mode: &str) {
    let r = a
        .adm(
            Method::PATCH,
            SETTINGS,
            Some(json!({"ideas": {"mode": mode}})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "ideas.mode");
    assert_eq!(r.json()["ideas"]["mode"], mode, "{}", r.text());
}

// ---------- п. 1, п. 6: в план ----------

/// §11 п. 1: память решения — отклонённая похожая идея, открытая задача, changelog и решение;
/// идея человека из WebUI → раздел `### Идеи на разбор` первой сессии → пакет `idea_review`
/// (MCP) с похожими, у отклонённой — её строка разбора → `review plan` (MCP, ключ повтора):
/// задача `open` с `priority` и планом шагов, системное ребро `derived_from`, рёбер `mentions` из
/// `basis` нет, `data.review`, `data.decided` агента, аудит; повтор — тот же итог без второй
/// задачи и аудита. §11 п. 6: `plan` с `task.id` открытой задачи — ребро без новой задачи;
/// строка плана следующей сессии — по `priority`, затем по возрасту; задача `done` закрывает все
/// свои идеи, следующая — оставшийся пункт.
#[tokio::test]
async fn acceptance_plan_flow() {
    let a = world("plan").await;
    let declined = create_tok(
        &a,
        json!({"kind": "idea", "title": "PDF-экспорт отчётов продаж",
               "body": "Выгрузка отчётов продаж в PDF для бухгалтерии.",
               "data": {"origin": "human"}}),
    )
    .await;
    let why = "Отклонено: печать отчёта продаж уже покрывает выгрузку в PDF.";
    let r = review(
        &a,
        &declined,
        json!({"outcome": "decline", "summary": summary(why)}),
    )
    .await;
    ok(&r, StatusCode::OK, "decline похожей");
    let print = create_tok(
        &a,
        json!({"kind": "task", "title": "Печать отчётов продаж в PDF",
               "body": "Печать отчёта продаж из браузера, в том числе в PDF."}),
    )
    .await;
    let cl = create_tok(
        &a,
        json!({"kind": "changelog", "title": "Отчёты продаж в CSV",
               "body": "Выгрузка отчётов продаж в CSV одной кнопкой; PDF — позже.",
               "data": {"kind": "feat"}}),
    )
    .await;
    let dec = create_tok(
        &a,
        json!({"kind": "insight", "title": "Решение: PDF отчётов продаж собирает сервер",
               "body": "Генерация PDF отчётов продаж — на сервере, не в браузере.",
               "data": {"kind": "decision"}}),
    )
    .await;
    let title = "Экспорт отчётов продаж в PDF";
    let x = idea_web(
        &a,
        &sol(),
        title,
        "Выгружать отчёт продаж в PDF одной кнопкой.",
        json!({"priority": "high"}),
    )
    .await;

    // дайджест первой сессии: очередь разбора из одной идеи, строки плана ещё нет
    let j = start(&a, "s1").await;
    assert_eq!(
        section_lines(&j, REVIEW_SECTION),
        Some(vec![review_hint(1), idea_line(title, "high", &x)]),
        "{j}"
    );
    assert_eq!(items(&j, "idea_review"), [w(&x)], "{j}");
    assert!(items(&j, "idea").is_empty(), "{j}");
    assert!(!text(&j).contains("План из идей"), "{j}");

    // пакет разбора (MCP): похожие по смыслу и словам — с видом, статусом и причиной отказа
    let (t, err) = mcp(&a, "s1", "memory_recall", json!({"event": "idea_review"})).await;
    assert!(!err, "{t}");
    assert!(
        t.starts_with(&format!("{REVIEW_SECTION}\nШаги разбора: ")),
        "{t}"
    );
    assert!(t.contains("action=review"), "{t}");
    let b = idea_block(&t, &x);
    assert!(
        b.starts_with(&format!(
            "— w:{x}\n  Выгружать отчёт продаж в PDF одной кнопкой."
        )),
        "{b}"
    );
    assert!(
        b.contains(&format!(
            "\n  - w:{declined} · declined · «PDF-экспорт отчётов продаж» — разбор: {why}"
        )),
        "{b}"
    );
    assert!(
        b.contains(&format!(
            "\n  - k:{print} · open · «Печать отчётов продаж в PDF»"
        )),
        "{b}"
    );
    assert!(b.contains(&format!("\n  - c:{cl} ")), "{b}");
    assert!(b.contains(&format!("\n  - i:{dec} ")), "{b}");
    assert!(
        !b.contains(&format!("- w:{x} ")),
        "сама идея — не похожая: {b}"
    );
    assert!(t.contains("Показано идей на разбор: 1 из 1."), "{t}");

    // старший пункт плана из другой идеи — приоритет normal (REST)
    let y_title = "Тёмная тема интерфейса";
    let y = idea_web(
        &a,
        &sol(),
        y_title,
        "Тёмная тема для вечерней работы.",
        json!({}),
    )
    .await;
    let r = review(
        &a,
        &y,
        json!({"outcome": "plan", "summary": summary("Решение — в план: тема интерфейса."),
               "task": {"body": "1. Палитра.\n2. Переключатель.", "priority": "normal"}}),
    )
    .await;
    ok(&r, StatusCode::OK, "plan y");
    let ty = id_of(&r.json()["task"]);

    // разбор «в план» (MCP) с ключом повтора
    let head = "Решение — в план: кнопка PDF у отчёта продаж.";
    let basis = json!([
        format!("c:{cl}"),
        format!("i:{dec}"),
        "file:docs/specs/2026-10-17-zeroaimemory-tz.md",
        "commit:7aa91f8"
    ]);
    let args = json!({
        "id": w(&x), "action": "review", "outcome": "plan", "summary": summary(head),
        "basis": basis, "similar": [w(&declined)],
        "task": {"body": "1. Кнопка «PDF» в отчёте продаж.\n2. Генерация на сервере.",
                 "priority": "high"},
        "idempotency_key": "m65-plan-x",
    });
    let (t, err) = mcp(&a, "s1", "memory_update_entry", args.clone()).await;
    assert!(!err, "{t}");
    assert!(
        t.starts_with(&format!("Идея w:{x}: В плане · разбор plan → задача k:")),
        "{t}"
    );
    let task = after(&t, "→ задача k:");
    // повтор после обрыва связи — тот же итог, без второй задачи и второго аудита
    let (t2, err) = mcp(&a, "s1", "memory_update_entry", args).await;
    assert!(!err && t2.contains("повтор"), "{t2}");
    assert_eq!(after(&t2, "→ задача k:"), task, "{t2}");
    assert_eq!(tasks(&a).await.len(), 3, "печать, тёмная тема, экспорт");

    // задача плана: `open`, приоритет, план шагов и строка идеи; «из идеи» — у задачи
    let tk = get(&a, &task).await;
    assert_eq!(tk["kind"], "task", "{tk}");
    assert_eq!(tk["status"], "open", "{tk}");
    assert_eq!(tk["title"], title, "{tk}");
    assert_eq!(tk["data"]["priority"], "high", "{tk}");
    let body = tk["body"].as_str().unwrap();
    assert!(body.starts_with("1. Кнопка «PDF» в отчёте продаж."), "{tk}");
    assert!(body.trim_end().ends_with(&format!("Идея: w:{x}")), "{tk}");
    assert_eq!(tk["idea"]["id"], x.as_str(), "{tk}");
    assert_eq!(scalar(&a, EDGE, &[&task, &x, "derived_from"]).await, 1);
    // ссылки разбора — только в `data.review` (решение составителя плана 1)
    assert_eq!(scalar(&a, MENTIONS_FROM, &[&x]).await, 0);

    // идея: `accepted`, разбор агента и решение агента каналом review
    let e = get(&a, &x).await;
    assert_eq!(e["status"], "accepted", "{e}");
    assert_eq!(e["data"]["task_id"], task.as_str(), "{e}");
    let rv = &e["data"]["review"];
    assert_eq!(rv["outcome"], "plan", "{rv}");
    assert_eq!(rv["summary"], summary(head), "{rv}");
    assert_eq!(rv["basis"], basis, "{rv}");
    assert_eq!(rv["similar"], json!([w(&declined)]), "{rv}");
    assert_eq!(rv["task"], format!("k:{task}"), "{rv}");
    assert_eq!(rv["by_user"], a.admin_id(), "{rv}");
    assert_eq!(rv["session"], "s1", "{rv}");
    assert!(rv["at"].is_string(), "{rv}");
    assert!(rv.get("by").is_none(), "признак агента — в decided: {rv}");
    assert_eq!(e["data"]["reviews"], json!([rv.clone()]), "{e}");
    let dc = &e["data"]["decided"];
    assert_eq!(
        (&dc["decision"], &dc["by"], &dc["channel"]),
        (&json!("accept"), &json!("agent"), &json!("review")),
        "{dc}"
    );
    let rs = audit_of(&a, "entry.idea.review", &x).await;
    assert_eq!(rs.len(), 1, "{rs:?}");
    assert_eq!(rs[0].details["outcome"], "plan");
    assert_eq!(rs[0].details["task"], task.as_str());

    // идею покрывает открытый пункт плана — ребро без новой задачи
    let z = idea_web(
        &a,
        &sol(),
        "PDF-отчёт продаж для бухгалтерии",
        "То же, что экспорт: отчёт продаж в PDF.",
        json!({}),
    )
    .await;
    let r = review(
        &a,
        &z,
        json!({"outcome": "plan", "summary": summary("Совпадает с пунктом плана k:…."),
               "similar": [w(&x)], "task": {"id": format!("k:{task}")}}),
    )
    .await;
    ok(&r, StatusCode::OK, "plan с task.id");
    assert_eq!(r.json()["task"]["id"], task.as_str(), "{}", r.text());
    assert_eq!(r.json()["entry"]["status"], "accepted");
    assert_eq!(r.json()["entry"]["data"]["task_id"], task.as_str());
    assert_eq!(tasks(&a).await.len(), 3, "новой задачи нет");
    assert_eq!(scalar(&a, EDGE, &[&task, &z, "derived_from"]).await, 1);
    let ideas: Vec<String> = get(&a, &task).await["ideas"]
        .as_array()
        .unwrap()
        .iter()
        .map(id_of)
        .collect();
    assert_eq!(ideas, [x.clone(), z.clone()]);

    // следующая сессия: очереди нет, строка плана — `high` (новее) раньше `normal` (старше)
    let j = start(&a, "s2").await;
    assert!(!text(&j).contains(REVIEW_SECTION), "{j}");
    assert!(
        has_line(
            &j,
            &format!("План из идей: 2 открытых, следующая — k:{task} «{title}» [high]")
        ),
        "{j}"
    );
    // пункт плана сделан — обе его идеи «Сделано», следующая — оставшийся пункт
    let r = patch_tok(&a, &task, json!({"status": "done"})).await;
    ok(&r, StatusCode::OK, "задача done");
    for i in [&x, &z] {
        let e = get(&a, i).await;
        assert_eq!(e["status"], "done", "{e}");
        assert!(e["data"]["done_at"].is_string(), "{e}");
    }
    let j = start(&a, "s3").await;
    assert!(
        has_line(
            &j,
            &format!("План из идей: 1 открытых, следующая — k:{ty} «{y_title}» [normal]")
        ),
        "{j}"
    );
}

// ---------- п. 2: на доработку ----------

/// §11 п. 2: `review return` (MCP) → `returned` с `returned_at` и `missing`, в очередь не
/// попадает; дописка агента статус не меняет; правка человеком в WebUI → `new` без
/// `returned_at` (устаревший `If-Match` — 409); новая сессия — снова в очереди, в пакете прежний
/// разбор и «не хватало»; второй разбор — история из двух, ревизии хранят каждый шаг.
#[tokio::test]
async fn acceptance_return_edit_rereview() {
    let a = world("return").await;
    let title = "Уведомления о новых идеях";
    let r_id = idea_web(&a, &sol(), title, "Хочу знать о новых идеях.", json!({})).await;
    let missing = "Куда присылать уведомление: письмо или значок в WebUI? Как часто?";
    let head1 = "Не хватает подробностей: канал и частота уведомлений.";
    let (t, err) = mcp(
        &a,
        "r1",
        "memory_update_entry",
        json!({"id": w(&r_id), "action": "review", "outcome": "return",
               "summary": summary(head1), "missing": missing}),
    )
    .await;
    assert!(!err, "{t}");
    assert!(
        t.starts_with(&format!("Идея w:{r_id}: На доработку · разбор return")),
        "{t}"
    );
    assert!(!t.contains("задача"), "{t}");
    let e = get(&a, &r_id).await;
    assert_eq!(e["status"], "returned", "{e}");
    assert!(e["data"]["returned_at"].is_string(), "{e}");
    assert_eq!(e["data"]["review"]["missing"], missing, "{e}");
    assert_eq!(e["data"]["decided"]["decision"], "return", "{e}");
    assert_eq!(e["data"]["decided"]["by"], "agent", "{e}");
    let c = counts_as(&a, &a.token).await;
    assert_eq!((&c["returned"], &c["new"]), (&json!(1), &json!(0)), "{c}");
    // возвращённая — не очередь разбора
    let j = start(&a, "r2").await;
    assert!(!text(&j).contains(REVIEW_SECTION), "{j}");
    // дописка агента — статус прежний (пинг-понга нет)
    let r = patch_tok(
        &a,
        &r_id,
        json!({"append_body": "Агент: жду ответа человека."}),
    )
    .await;
    ok(&r, StatusCode::OK, "append агента");
    assert_eq!(r.json()["status"], "returned", "{}", r.text());
    // правка человеком в WebUI — снова «Непросмотрено»; правка по прежней ревизии — 409
    let stale = get(&a, &r_id).await["rev"].to_string();
    let edited = "Хочу знать о новых идеях: письмом раз в день, со ссылкой на идею.";
    let r = patch_web(&a, &r_id, json!({"body": edited})).await;
    ok(&r, StatusCode::OK, "правка человеком");
    let e = r.json();
    assert_eq!(e["status"], "new", "{e}");
    assert!(e["data"].get("returned_at").is_none(), "{e}");
    assert_eq!(e["data"]["reviews"].as_array().unwrap().len(), 1, "{e}");
    let r = a
        .adm_h(
            Method::PATCH,
            &ent(&r_id),
            Some(json!({"body": "Другая правка."})),
            &[("if-match", stale.as_str())],
        )
        .await;
    ok(&r, StatusCode::CONFLICT, "устаревший If-Match");

    // новая сессия — снова в очереди; в пакете прежний разбор и что не хватало
    let j = start(&a, "r3").await;
    assert_eq!(
        section_lines(&j, REVIEW_SECTION),
        Some(vec![review_hint(1), idea_line(title, "normal", &r_id)]),
        "{j}"
    );
    let (t, err) = mcp(&a, "r3", "memory_recall", json!({"event": "idea_review"})).await;
    assert!(!err, "{t}");
    let b = idea_block(&t, &r_id);
    assert!(b.contains(&format!("\n  {edited}")), "{b}");
    assert!(b.contains("\n  История:\n"), "{b}");
    assert!(
        b.contains(&format!(" разбор return: {head1}; не хватало: {missing}")),
        "{b}"
    );

    // второй разбор — в план: история из двух, последний — первым
    let head2 = "Решение — в план: канал и частота названы.";
    let r = review(
        &a,
        &r_id,
        json!({"outcome": "plan", "summary": summary(head2),
               "task": {"body": "1. Письмо раз в день.\n2. Ссылка на идею."}}),
    )
    .await;
    ok(&r, StatusCode::OK, "второй разбор");
    let e = &r.json()["entry"];
    assert_eq!(e["status"], "accepted", "{e}");
    let reviews = e["data"]["reviews"].as_array().unwrap();
    assert_eq!(reviews.len(), 2, "{e}");
    assert_eq!(reviews[0]["outcome"], "plan", "{e}");
    assert_eq!(reviews[0]["summary"], summary(head2), "{e}");
    assert_eq!(reviews[1]["outcome"], "return", "{e}");
    assert_eq!(reviews[1]["missing"], missing, "{e}");
    assert_eq!(e["data"]["review"], reviews[0], "{e}");
    assert_eq!(e["data"]["decided"]["decision"], "accept", "{e}");
    // ревизии: создание, разбор, дописка агента, правка человеком, второй разбор
    let r = a
        .t(Method::GET, &format!("{}/revisions", ent(&r_id)), None)
        .await;
    ok(&r, StatusCode::OK, "ревизии");
    let mut revs = r.json()["items"].as_array().unwrap().clone();
    revs.sort_by_key(|v| v["rev"].as_i64().unwrap());
    let statuses: Vec<&str> = revs.iter().map(|v| v["status"].as_str().unwrap()).collect();
    assert_eq!(
        statuses,
        ["new", "returned", "returned", "new", "accepted"],
        "{revs:?}"
    );
    assert_eq!(revs[1]["data"]["review"]["outcome"], "return");
    assert_eq!(revs[4]["data"]["review"]["outcome"], "plan");
    let outcomes: Vec<String> = audit_of(&a, "entry.idea.review", &r_id)
        .await
        .iter()
        .map(|r| r.details["outcome"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(outcomes.len(), 2, "{outcomes:?}");
    assert!(
        outcomes.contains(&"return".to_string()) && outcomes.contains(&"plan".to_string()),
        "{outcomes:?}"
    );
}

// ---------- п. 3: отклонена ----------

/// §11 п. 3: `review decline` (MCP) → `declined`; повтор отклонённой без новых доводов — в
/// пакете похожая с причиной, `decline` со ссылкой в `similar`; «Вернуть на разбор» в WebUI без
/// довода — 422, с доводом — `new` (история на месте); новая сессия — в очереди, в пакете
/// прежний разбор и довод человека по времени; второй разбор учитывает довод.
#[tokio::test]
async fn acceptance_decline_reopen_with_reason() {
    let a = world("decline").await;
    let title = "Голосовой ввод идей";
    let d = idea_web(
        &a,
        &sol(),
        title,
        "Надиктовывать идею голосом в WebUI.",
        json!({}),
    )
    .await;
    let why = "Отклонено: распознавания речи нет в ТЗ, голосовой ввод даёт сама ОС.";
    let (t, err) = mcp(
        &a,
        "d1",
        "memory_update_entry",
        json!({"id": w(&d), "action": "review", "outcome": "decline", "summary": summary(why),
               "basis": ["file:docs/specs/2026-10-17-zeroaimemory-tz.md"]}),
    )
    .await;
    assert!(!err, "{t}");
    assert!(
        t.starts_with(&format!("Идея w:{d}: Отклонена · разбор decline")),
        "{t}"
    );
    let e = get(&a, &d).await;
    assert_eq!(e["status"], "declined", "{e}");
    assert_eq!(e["data"]["decided"]["decision"], "decline", "{e}");
    assert_eq!(e["data"]["decided"]["channel"], "review", "{e}");

    // повтор отклонённой: в пакете — похожая с причиной; `decline` со ссылкой на прежнюю
    let rep = idea_web(
        &a,
        &sol(),
        "Голосовой ввод заметок",
        "Надиктовывать заметки голосом в WebUI.",
        json!({}),
    )
    .await;
    let (t, err) = mcp(&a, "d1", "memory_recall", json!({"event": "idea_review"})).await;
    assert!(!err, "{t}");
    let b = idea_block(&t, &rep);
    assert!(
        b.contains(&format!(
            "\n  - w:{d} · declined · «{title}» — разбор: {why}"
        )),
        "{b}"
    );
    let r = review(
        &a,
        &rep,
        json!({"outcome": "decline", "similar": [w(&d)],
               "summary": summary("Повтор отклонённой идеи без новых доводов.")}),
    )
    .await;
    ok(&r, StatusCode::OK, "decline повтора");
    assert_eq!(
        r.json()["entry"]["data"]["review"]["similar"],
        json!([w(&d)])
    );

    // «Вернуть на разбор» в WebUI: без довода — 422, с доводом — снова «Непросмотрено»
    let r = post_web(&a, &d, "decide", json!({"decision": "reopen"})).await;
    problem(
        &r,
        StatusCode::UNPROCESSABLE_ENTITY,
        "reason-required",
        "reopen без довода",
    );
    let reason = "Диктую идеи с телефона в дороге: системный ввод там неудобен.";
    let r = post_web(
        &a,
        &d,
        "decide",
        json!({"decision": "reopen", "reason": reason}),
    )
    .await;
    ok(&r, StatusCode::OK, "reopen с доводом");
    let e = get(&a, &d).await;
    assert_eq!(e["status"], "new", "{e}");
    assert_eq!(e["data"]["decided"]["decision"], "reopen", "{e}");
    assert_eq!(e["data"]["decided"]["by"], "human", "{e}");
    assert_eq!(e["data"]["reviews"].as_array().unwrap().len(), 1, "{e}");

    // новая сессия — в очереди; в пакете прежний разбор и довод человека по времени
    let j = start(&a, "d2").await;
    let lines = section_lines(&j, REVIEW_SECTION).unwrap_or_else(|| panic!("{j}"));
    assert_eq!(
        lines,
        [review_hint(1), idea_line(title, "normal", &d)],
        "{j}"
    );
    let (t, err) = mcp(&a, "d2", "memory_recall", json!({"event": "idea_review"})).await;
    assert!(!err, "{t}");
    let b = idea_block(&t, &d);
    assert!(b.contains("\n  История:\n"), "{b}");
    let (rv, arg) = (
        b.find(&format!(" разбор decline: {why}"))
            .unwrap_or_else(|| panic!("нет прежнего разбора: {b}")),
        b.find(&format!(" довод человека (reopen): {reason}"))
            .unwrap_or_else(|| panic!("нет довода человека: {b}")),
    );
    assert!(rv < arg, "история — по времени: {b}");

    // второй разбор — с учётом довода
    let r = review(
        &a,
        &d,
        json!({"outcome": "plan",
               "summary": summary("Довод человека меняет решение: диктовка с телефона."),
               "task": {"body": "1. Кнопка микрофона в форме идеи."}}),
    )
    .await;
    ok(&r, StatusCode::OK, "второй разбор");
    let e = &r.json()["entry"];
    assert_eq!(e["status"], "accepted", "{e}");
    let outcomes: Vec<&str> = e["data"]["reviews"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(outcomes, ["plan", "decline"], "{e}");
}

// ---------- п. 4, п. 6: отказы ----------

/// §11 п. 4: отказы разбора — из WebUI (422 «разбор — действие агента»), идея «от агента» (409
/// `idea-agent-origin`, REST и MCP), Knowledge (422), не `new` и решение человека («Отложить»,
/// «Отказаться» в WebUI, «Начать» из чата — 409 `transition-not-allowed`), пустой, короткий или
/// длинный разбор, `return` без `missing`, `data.review` от клиента (запись, правка, MCP). §11
/// п. 6: `done` без ссылки на сделанное — 422, со ссылкой — `done`; второй разбор той же идеи —
/// 409; `propose` идеи `new` в режиме `review` — 409 `idea-mode`. Отказы идею не меняют и аудита
/// не пишут.
#[tokio::test]
async fn acceptance_refusals() {
    let a = world("refusals").await;
    let i = idea_web(&a, &sol(), "Идея человека", "Замысел человека.", json!({})).await;
    let plan = json!({"outcome": "plan", "summary": summary("Решение — в план.")});

    // разбор — действие агента: из WebUI — 422
    let r = post_web(&a, &i, "review", plan.clone()).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "разбор из WebUI");
    assert!(
        r.text().contains("разбор — действие агента"),
        "{}",
        r.text()
    );
    // идея «от агента» — 409 (REST и MCP)
    let ag = idea_tok(&a, "Своя идея агента", "agent").await;
    let r = review(&a, &ag, plan.clone()).await;
    problem(&r, StatusCode::CONFLICT, "idea-agent-origin", "идея агента");
    let agent_detail = detail_of(&r);
    assert!(agent_detail.contains("идея от агента"), "{agent_detail}");
    let (t, err) = mcp(
        &a,
        "f1",
        "memory_update_entry",
        json!({"id": w(&ag), "action": "review", "outcome": "plan",
               "summary": summary("Решение — в план.")}),
    )
    .await;
    // в ответе MCP — причина problem (`detail`), а не только код: idea-agent-origin, не idea-mode
    mcp_refusal(&t, err, 409, &agent_detail);
    assert!(!t.contains("ideas.mode"), "{t}");
    // общая идея Knowledge — 422 (остаётся по M5.7)
    let kn = idea_web(&a, KN, "Общая идея", "Замысел.", json!({})).await;
    let r = review(&a, &kn, plan.clone()).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "Knowledge");
    assert!(r.text().contains("Knowledge"), "{}", r.text());

    // поля разбора: пустой, короткий, длинный разбор; return без missing; done без сделанного
    let open = create_tok(&a, json!({"kind": "task", "title": "Открытая задача"})).await;
    let s = summary("Разбор.");
    for (body, field) in [
        (json!({"outcome": "plan"}), "summary:"),
        (json!({"outcome": "plan", "summary": ""}), "summary:"),
        (json!({"outcome": "plan", "summary": "   "}), "summary:"),
        (
            json!({"outcome": "plan", "summary": "ы".repeat(199)}),
            "summary:",
        ),
        (
            json!({"outcome": "plan", "summary": "ы".repeat(8001)}),
            "summary:",
        ),
        (json!({"outcome": "return", "summary": s}), "missing:"),
        (
            json!({"outcome": "return", "summary": s, "missing": " "}),
            "missing:",
        ),
        (
            json!({"outcome": "return", "summary": s, "missing": "ы".repeat(2001)}),
            "missing:",
        ),
        (json!({"outcome": "done", "summary": s}), "basis:"),
        (
            json!({"outcome": "done", "summary": s, "basis": [format!("k:{open}")]}),
            "basis:",
        ),
        (
            json!({"outcome": "done", "summary": s, "basis": ["file:docs/journal.md"]}),
            "basis:",
        ),
    ] {
        let r = review(&a, &i, body.clone()).await;
        ok(&r, StatusCode::UNPROCESSABLE_ENTITY, &body.to_string());
        let detail = r.json()["detail"].as_str().unwrap_or_default().to_string();
        assert!(detail.starts_with(field), "{body}: {detail}");
    }
    let e = get(&a, &i).await;
    assert_eq!(e["status"], "new", "отказы идею не меняют: {e}");
    assert!(e["data"].get("review").is_none(), "{e}");

    // `data.review` и прочие поля разбора от клиента — 422 «поле ведёт сервер»
    let r = a
        .t(
            Method::POST,
            &sol(),
            Some(json!({"kind": "idea", "title": "С разбором от клиента",
                        "data": {"origin": "human", "review": {"outcome": "plan"}}})),
        )
        .await;
    ok(
        &r,
        StatusCode::UNPROCESSABLE_ENTITY,
        "data.review при записи",
    );
    assert!(r.text().contains("поле ведёт сервер"), "{}", r.text());
    for key in ["review", "reviews", "returned_at"] {
        let r = patch_web(&a, &i, json!({"data": {key: {"outcome": "plan"}}})).await;
        ok(&r, StatusCode::UNPROCESSABLE_ENTITY, key);
        assert!(r.text().contains("поле ведёт сервер"), "{}", r.text());
    }
    let (t, err) = mcp(
        &a,
        "f1",
        "memory_log",
        json!({"kind": "idea", "title": "С разбором из MCP",
               "data": {"origin": "human", "review": {"outcome": "plan"}}}),
    )
    .await;
    assert!(err && t.contains("поле ведёт сервер"), "{t}");

    // режим review: предлагать новую идею вопросом нельзя — её разбирает агент (REST и MCP)
    let r = post_tok(&a, &i, "propose", json!({"text": "подходит к задаче"})).await;
    problem(&r, StatusCode::CONFLICT, "idea-mode", "propose идеи new");
    let mode_detail = detail_of(&r);
    assert!(
        mode_detail.contains("ideas.mode = review") && mode_detail.contains("action=review"),
        "{mode_detail}"
    );
    let (t, err) = mcp(
        &a,
        "f1",
        "memory_update_entry",
        json!({"id": w(&i), "action": "propose", "text": "подходит к задаче"}),
    )
    .await;
    mcp_refusal(&t, err, 409, &mode_detail);
    assert!(!t.contains("идея от агента"), "{t}");
    assert_eq!(get(&a, &i).await["status"], "new");

    // решение человека агент не пересматривает: «Отложить», «Отказаться» в WebUI, «Начать» из
    // чата — разбор 409 transition-not-allowed, статус прежний
    let deferred = idea_web(&a, &sol(), "Отложенная человеком", "Замысел.", json!({})).await;
    let r = post_web(&a, &deferred, "decide", json!({"decision": "defer"})).await;
    ok(&r, StatusCode::OK, "defer в WebUI");
    let declined = idea_web(&a, &sol(), "Отклонённая человеком", "Замысел.", json!({})).await;
    let r = post_web(&a, &declined, "decide", json!({"decision": "decline"})).await;
    ok(&r, StatusCode::OK, "decline в WebUI");
    let started = idea_web(&a, &sol(), "Начатая человеком", "Замысел.", json!({})).await;
    let r = post_tok(
        &a,
        &started,
        "decide",
        json!({"decision": "accept", "by": "human", "channel": "chat"}),
    )
    .await;
    ok(&r, StatusCode::OK, "accept из чата");
    for (id, status) in [
        (&deferred, "deferred"),
        (&declined, "declined"),
        (&started, "accepted"),
    ] {
        let r = review(&a, id, plan.clone()).await;
        conflict(&r, "transition-not-allowed", status);
        let e = get(&a, id).await;
        assert_eq!(e["status"], status, "{e}");
        assert_eq!(e["data"]["decided"]["by"], "human", "{e}");
    }

    // второй разбор той же идеи — 409
    let r = review(&a, &i, plan.clone()).await;
    ok(&r, StatusCode::OK, "первый разбор");
    let r = review(
        &a,
        &i,
        json!({"outcome": "decline", "summary": summary("Передумал.")}),
    )
    .await;
    conflict(&r, "transition-not-allowed", "второй разбор");
    assert_eq!(get(&a, &i).await["status"], "accepted");

    // «уже сделано» со ссылкой на сделанное (changelog) — `done` с `done_at`
    let cl = create_tok(
        &a,
        json!({"kind": "changelog", "title": "Сделано раньше", "data": {"kind": "feat"}}),
    )
    .await;
    let dn = idea_web(&a, &sol(), "Уже сделанная идея", "Замысел.", json!({})).await;
    let r = review(
        &a,
        &dn,
        json!({"outcome": "done", "summary": summary("Уже сделано: см. changelog."),
               "basis": [format!("c:{cl}")]}),
    )
    .await;
    ok(&r, StatusCode::OK, "done со ссылкой");
    let e = &r.json()["entry"];
    assert_eq!(e["status"], "done", "{e}");
    assert!(e["data"]["done_at"].is_string(), "{e}");
    assert_eq!(e["data"]["decided"]["decision"], "done", "{e}");

    // аудит разбора — только у удачных разборов
    for (id, n) in [
        (&i, 1),
        (&dn, 1),
        (&ag, 0),
        (&kn, 0),
        (&deferred, 0),
        (&declined, 0),
        (&started, 0),
    ] {
        assert_eq!(audit_of(&a, "entry.idea.review", id).await.len(), n, "{id}");
    }
}

// ---------- п. 5: миграция 0026 ----------

fn agent_attr() -> Attribution {
    Attribution {
        author_kind: AuthorKind::Agent,
        token_id: Some(Id::new()),
        session_id: Some("s-m6".into()),
        machine: Some("box".into()),
        message: None,
        user_id: None,
        area: None,
        solution: None,
    }
}

fn human_attr() -> Attribution {
    Attribution {
        author_kind: AuthorKind::Human,
        token_id: None,
        session_id: None,
        machine: None,
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
        body: "Тело записи M6.".into(),
        data,
        area: Some("ideas".into()),
        tags: vec!["m6".into()],
        status: None,
        certainty: None,
        idempotency_key: None,
        triggers: Vec::new(),
        links: Vec::new(),
    }
}

/// Решение человека в WebUI.
fn decision(d: IdeaDecision, reason: Option<&str>) -> NewDecision {
    NewDecision {
        decision: d,
        horizon: None,
        reason: reason.map(str::to_string),
        by: AnswerBy::Human,
        channel: Channel::Webui,
        task_space: None,
        request_solution: None,
        defer_days: 30,
        expected_origin: None,
    }
}

/// Объект `sqlite_master`: тип, имя, нормализованный SQL.
type Obj = (String, String, String);

/// Снимок базы: объекты `sqlite_master`, строки `entries` (id, вид, статус, `data`, ревизия) и
/// строки связанных таблиц (`entry_revisions`, `entry_attachments`, `edges`, `spaces`) целиком.
type Snapshot = (Vec<Obj>, Vec<String>, Vec<String>);

/// Строки `entry_revisions`, `entry_attachments`, `edges` и `spaces` с префиксом таблицы.
const RELATED_ROWS: &str = "
    SELECT 'rev ' || entry_id || ' ' || rev || ' ' || title || ' ' || coalesce(body, '') || ' ' ||
           coalesce(data, '') || ' ' || coalesce(tags, '') || ' ' || coalesce(status, '-') || ' ' ||
           coalesce(certainty, '-') || ' ' || coalesce(reason, '-') || ' ' || ts || ' ' ||
           author_kind
    FROM entry_revisions
    UNION ALL
    SELECT 'att ' || entry_id || ' ' || blob_sha256 || ' ' || filename || ' ' || mime || ' ' ||
           size || ' ' || created_at
    FROM entry_attachments
    UNION ALL
    SELECT 'edge ' || src_id || ' ' || type || ' ' || dst_id || ' ' || origin || ' ' ||
           coalesce(deleted_at, '-')
    FROM edges
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

/// Идеи базы M6 всех статусов.
struct M6Ideas {
    fresh: Id,
    agent_new: Id,
    proposed: Id,
    question: Id,
    accepted: Id,
}

/// §11 п. 5: база пользователя M6 (схема 0025) с идеями всех статусов M5.7 (`new` человека и
/// агента, `proposed` с открытым вопросом, `accepted` с задачей, `deferred`, `declined` с
/// причиной, `done` по задаче) и отчётом с файлом открывается сервером M6.5 и переходит на 0026:
/// строки `entries`, ревизий, файлов, рёбер и пространств на месте, `foreign_key_check` пуст,
/// `bak-v25` есть; объекты схемы совпадают, кроме SQL `entries` (CHECK += `returned`); пять
/// триггеров отчёта срабатывают. Существующие идеи: `new` человека — сразу очередь разбора,
/// `proposed` с открытым вопросом остаётся до ответа человека; разбор `return` пишет статус
/// `returned`, правка токеном с `by: human` возвращает идею в `new`.
#[tokio::test]
async fn acceptance_migration_0026() {
    let a = api("m65-migration", |c| c.recall.timeout_ms = 30_000).await;
    assert_eq!(USER_MIGRATIONS[24].version, 25);
    assert_eq!(USER_MIGRATIONS[25].version, 26);
    assert!(USER_MIGRATIONS[25].rebuild);
    let sol_id = Id::new();
    let sha = format!("{:08x}{}", 9, "cd".repeat(28));
    // База 0025 собирается вне каталога `users/` и встаёт на место жёсткой ссылкой уже
    // закрытой: фоновые задачи сервера открывают базы всех пользователей и мигрировали бы её
    // посреди заполнения (как в `m57_acceptance::acceptance_migration_from_m56_db`).
    let fixture = a.st.cfg.data_dir.join("fixture-m6.db");
    let (before, report, ids) = {
        let db = Db::open(&fixture, &USER_MIGRATIONS[..25]).unwrap();
        let now = zam_store::time::fmt_ts(jiff::Timestamp::now());
        db.write(move |tx| {
            for (id, kind, guid) in [
                (sol_id, "solution", Some(G)),
                (Id::new(), "knowledge", None),
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
        let idea = |title: &str, origin: &str| {
            new_entry(
                sol_id,
                EntryKind::Idea,
                title,
                json!({"origin": origin, "priority": "normal"}),
            )
        };
        let mk = |title: &'static str, origin: &'static str, at: Attribution| {
            let db = db.clone();
            let e = idea(title, origin);
            async move { create_entry(&db, e, &at).await.unwrap().0.id }
        };
        let fresh = mk("Идея на разбор", "human", human_attr()).await;
        let agent_new = mk("Своя идея агента", "agent", agent_attr()).await;
        let proposed = mk("Предложенная идея", "human", human_attr()).await;
        let (_, q) = propose_idea(&db, proposed, None, None, None, &agent_attr())
            .await
            .unwrap();
        let accepted = mk("Начатая идея", "human", human_attr()).await;
        let d = decide_idea(
            &db,
            accepted,
            decision(IdeaDecision::Accept, None),
            &human_attr(),
        )
        .await
        .unwrap();
        assert!(d.task.is_some(), "«начать» создаёт задачу");
        let deferred = mk("Отложенная идея", "human", human_attr()).await;
        decide_idea(
            &db,
            deferred,
            decision(IdeaDecision::Defer, None),
            &human_attr(),
        )
        .await
        .unwrap();
        let declined = mk("Отклонённая идея", "human", human_attr()).await;
        decide_idea(
            &db,
            declined,
            decision(IdeaDecision::Decline, Some("не для этого проекта")),
            &human_attr(),
        )
        .await
        .unwrap();
        let done = mk("Сделанная идея", "human", human_attr()).await;
        let task = decide_idea(
            &db,
            done,
            decision(IdeaDecision::Accept, None),
            &human_attr(),
        )
        .await
        .unwrap()
        .task
        .unwrap();
        update_entry(
            &db,
            task.id,
            None,
            EntryPatch {
                status: Some(EntryStatus::Done),
                ..EntryPatch::default()
            },
            &agent_attr(),
        )
        .await
        .unwrap();
        let mut rep = new_entry(sol_id, EntryKind::Report, "Отчёт", json!({}));
        rep.body = format!(
            "## Итоги\n\n![loss.png]({})\n",
            attachment_link(&sha, "loss.png")
        );
        rep.tags = Vec::new();
        let file = NewReportFile {
            sha256: sha.clone(),
            filename: "loss.png".into(),
            mime: "image/png".into(),
            size: 100,
        };
        let (rep, _) = create_report(&db, rep, vec![file], &agent_attr())
            .await
            .unwrap();
        let snap = db
            .read(|c| {
                let v: u32 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
                assert_eq!(v, 25);
                Ok(schema_and_rows(c)?)
            })
            .await
            .unwrap();
        let ids = M6Ideas {
            fresh,
            agent_new,
            proposed,
            question: q.id,
            accepted,
        };
        (snap, rep.id, ids)
    };
    // закрытая база — одним файлом (WAL сброшен)
    {
        let c = rusqlite::Connection::open(&fixture).unwrap();
        c.busy_timeout(Duration::from_secs(10)).unwrap();
        c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode = DELETE;")
            .unwrap();
    }
    // идеи всех статусов M5.7, рёбра задач и вопроса, файл отчёта
    let statuses: std::collections::BTreeSet<String> = before
        .1
        .iter()
        .filter(|r| r.split(' ').nth(1) == Some("idea"))
        .map(|r| r.split(' ').nth(2).unwrap().to_string())
        .collect();
    assert_eq!(
        statuses,
        [
            "accepted", "declined", "deferred", "done", "new", "proposed"
        ]
        .map(String::from)
        .into(),
        "{:?}",
        before.1
    );
    let count = |p: &str| before.2.iter().filter(|r| r.starts_with(p)).count();
    assert_eq!((count("att "), count("space ")), (1, 4), "{:?}", before.2);
    assert!(count("edge ") >= 3, "{:?}", before.2);

    let u = a.st.system.create_user("Мира", false).await.unwrap();
    let (_, tok) =
        a.st.system
            .issue_token(u.id, "t", Access::Rw)
            .await
            .unwrap();
    let path = a.st.cfg.data_dir.join("users").join(format!("{}.db", u.id));
    std::fs::hard_link(&fixture, &path)
        .unwrap_or_else(|e| panic!("база 0025 не встала на место {}: {e}", path.display()));
    // первый запрос пользователя открывает базу сервером — миграция 0026
    let r = a
        .tok(
            &tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Решение M6"})),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    assert_eq!(r.json()["id"], sol_id.to_string(), "прежнее пространство");
    let db = a.st.users.get(u.id).await.unwrap();
    a.st.users.wait_rechunk(u.id).await;
    let bak = path.with_file_name(format!("{}.db.bak-v25", u.id));
    assert!(bak.exists(), "нет {}", bak.display());
    let (after, version, violations, integrity) = db
        .read(|c| {
            let v: u32 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            let mut fk = c.prepare("PRAGMA foreign_key_check")?;
            let n = fk.query_map([], |_| Ok(()))?.count();
            let ok: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
            Ok((schema_and_rows(c)?, v, n, ok))
        })
        .await
        .unwrap();
    assert_eq!(version, 26);
    assert_eq!(version, USER_MIGRATIONS.len() as u32);
    assert_eq!(violations, 0, "foreign_key_check не пуст");
    assert_eq!(integrity, "ok");
    assert_eq!(after.1, before.1, "строки entries на месте");
    assert_eq!(
        after.2, before.2,
        "строки ревизий, файлов, рёбер и пространств на месте"
    );
    let is_entries = |o: &&Obj| o.0 == "table" && o.1 == "entries";
    assert_eq!(
        after
            .0
            .iter()
            .filter(|o| !is_entries(o))
            .collect::<Vec<_>>(),
        before
            .0
            .iter()
            .filter(|o| !is_entries(o))
            .collect::<Vec<_>>(),
        "объекты схемы, кроме entries (индексы и триггеры пересозданы дословно)"
    );
    let sql_of = |s: &Snapshot| s.0.iter().find(is_entries).unwrap().2.clone();
    assert!(!sql_of(&before).contains("'returned'"));
    assert!(sql_of(&after).contains("'returned'"), "{}", sql_of(&after));
    // пять триггеров отчёта — прямым SQL
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

    // счётчики статусов WebUI по мигрированной базе
    let c = counts_as(&a, &tok).await;
    assert_eq!(
        c,
        json!({"new": 2, "proposed": 1, "accepted": 1, "deferred": 1, "declined": 1, "done": 1,
               "returned": 0}),
        "{c}"
    );
    // `new` человека — сразу очередь разбора; агента и `proposed` с открытым вопросом — нет
    let (fresh, agent_new, proposed) = (
        ids.fresh.to_string(),
        ids.agent_new.to_string(),
        ids.proposed.to_string(),
    );
    let j = start_as(&a, &tok, "mig-1").await;
    assert_eq!(
        section_lines(&j, REVIEW_SECTION),
        Some(vec![
            review_hint(1),
            idea_line("Идея на разбор", "normal", &fresh)
        ]),
        "{j}"
    );
    assert!(
        !text(&j).contains(&agent_new) && !text(&j).contains(&proposed),
        "{j}"
    );
    let get_as = |id: String| {
        let (a, tok) = (&a, &tok);
        async move {
            let r = a.tok(tok, Method::GET, &ent(&id), None).await;
            ok(&r, StatusCode::OK, &id);
            r.json()
        }
    };
    assert_eq!(get_as(proposed.clone()).await["status"], "proposed");
    assert_eq!(
        get_as(ids.question.to_string()).await["status"],
        "open",
        "вопрос-предложение ждёт ответа человека"
    );
    // идея «Начать» M5.7 — задача из идеи (строка плана считает и её)
    let acc = get_as(ids.accepted.to_string()).await;
    assert_eq!(acc["status"], "accepted", "{acc}");
    let plan_task = acc["data"]["task_id"].as_str().unwrap();
    assert!(
        has_line(
            &j,
            &format!("План из идей: 1 открытых, следующая — k:{plan_task} «Начатая идея» [normal]")
        ),
        "{j}"
    );
    // разбор `return` — статус `returned` проходит CHECK 0026; правка человека — снова `new`
    let r = a
        .tok(
            &tok,
            Method::POST,
            &format!("{}/review", ent(&fresh)),
            Some(
                json!({"outcome": "return", "summary": summary("Не хватает подробностей."),
                        "missing": "Что именно разобрать?"}),
            ),
        )
        .await;
    ok(&r, StatusCode::OK, "review return");
    assert_eq!(r.json()["entry"]["status"], "returned", "{}", r.text());
    assert_eq!(counts_as(&a, &tok).await["returned"], 1);
    let r = a
        .tok_rev(
            &tok,
            Method::PATCH,
            &ent(&fresh),
            Some(json!({"append_body": "Человек: разобрать экспорт.", "by": "human"})),
        )
        .await;
    ok(&r, StatusCode::OK, "правка человеком");
    assert_eq!(r.json()["status"], "new", "{}", r.text());
}

// ---------- п. 6: режим propose ----------

/// §11 п. 6: `ideas.mode = propose` со страницы «Подключение агента» — поведение M5.7 без
/// изменений: раздел `### Идеи` со строкой-указанием `propose`, подсказка к запросу «предложи»,
/// `propose` вопросом (MCP), «Начать» на странице «Вопросы» — задача одной транзакцией; разбора
/// нет (REST и MCP — 409 `idea-mode`, пакет — «разбор выключен»), строки плана нет. Обратно
/// `review`: идея `new` — очередь разбора, `proposed` с открытым вопросом ждёт ответа человека.
#[tokio::test]
async fn acceptance_propose_mode_is_m57() {
    let a = world("propose").await;
    set_mode_web(&a, "propose").await;
    let r = a.adm(Method::GET, SETTINGS, None, None).await;
    assert_eq!(
        r.json()["ideas"],
        json!({"proactive": true, "mode": "propose"})
    );
    let x_title = "Тёмная тема интерфейса";
    let y_title = "Экспорт отчётов в PDF";
    let x = idea_web(
        &a,
        &sol(),
        x_title,
        "Тёмная тема для вечерней работы.",
        json!({"priority": "high"}),
    )
    .await;
    let y = idea_web(
        &a,
        &sol(),
        y_title,
        "Выгружать отчёт продаж в PDF.",
        json!({}),
    )
    .await;

    // дайджест — раздел M5.7
    let j = start(&a, "p1").await;
    assert!(!text(&j).contains(REVIEW_SECTION), "{j}");
    assert_eq!(
        section_lines(&j, SECTION),
        Some(vec![
            HINT.to_string(),
            idea_line(x_title, "high", &x),
            idea_line(y_title, "normal", &y)
        ]),
        "{j}"
    );
    assert_eq!(items(&j, "idea"), [w(&x), w(&y)], "{j}");
    assert!(items(&j, "idea_review").is_empty(), "{j}");

    // предложение вопросом (MCP), как в M5.7
    let (t, err) = mcp(
        &a,
        "p1",
        "memory_update_entry",
        json!({"id": w(&x), "action": "propose", "text": "подходит к текущей задаче"}),
    )
    .await;
    assert!(!err, "{t}");
    let qid = after(&t, &format!("Предложено w:{x} → вопрос q:"));
    let e = get(&a, &x).await;
    assert_eq!(e["status"], "proposed", "{e}");
    assert_eq!(e["data"]["question_id"], qid.as_str(), "{e}");
    assert_eq!(get(&a, &qid).await["status"], "open");

    // разбора нет: REST и MCP — 409 idea-mode, пакет — уведомление
    let plan = json!({"outcome": "plan", "summary": summary("Решение — в план.")});
    let r = review(&a, &y, plan).await;
    problem(
        &r,
        StatusCode::CONFLICT,
        "idea-mode",
        "review в режиме propose",
    );
    let off_detail = detail_of(&r);
    assert!(
        off_detail.contains("ideas.mode = propose") && off_detail.contains("action=propose"),
        "{off_detail}"
    );
    let (t, err) = mcp(
        &a,
        "p1",
        "memory_update_entry",
        json!({"id": w(&y), "action": "review", "outcome": "plan",
               "summary": summary("Решение — в план.")}),
    )
    .await;
    mcp_refusal(&t, err, 409, &off_detail);
    let (t, err) = mcp(&a, "p1", "memory_recall", json!({"event": "idea_review"})).await;
    assert!(!err, "{t}");
    assert!(
        t.starts_with("Разбор идей выключен: ideas.mode = propose"),
        "{t}"
    );
    assert_eq!(get(&a, &y).await["status"], "new");

    // подсказка к запросу — «предложи» (M5.7), не «на разбор»
    let j = recall_as(
        &a,
        &a.token,
        Some("p2"),
        json!({"event": "prompt", "solution": G, "prompt": "как сделать экспорт отчётов в PDF"}),
    )
    .await;
    assert!(text(&j).contains(&w(&y)), "{j}");
    assert!(text(&j).contains("action=propose"), "{j}");
    assert!(!text(&j).contains("на разбор"), "{j}");
    assert_eq!(items(&j, "idea"), [w(&y)], "{j}");

    // «Начать» на странице «Вопросы» — задача, идея `accepted`, вопрос отвечен
    let r = post_web(
        &a,
        &qid,
        "answer",
        json!({"option": "accept", "channel": "webui"}),
    )
    .await;
    ok(&r, StatusCode::OK, "answer accept");
    let e = get(&a, &x).await;
    assert_eq!(e["status"], "accepted", "{e}");
    let task = e["data"]["task_id"].as_str().unwrap().to_string();
    assert_eq!(get(&a, &task).await["status"], "open");
    assert_eq!(get(&a, &qid).await["status"], "answered");
    // строки плана в режиме propose нет; ответ человека — в дайджесте
    let j = start(&a, "p3").await;
    assert!(!text(&j).contains("План из идей"), "{j}");
    assert!(text(&j).contains("Ответы человека"), "{j}");

    // обратно review: предложенная с открытым вопросом ждёт ответа человека, `new` — очередь
    let z = idea_web(
        &a,
        &sol(),
        "Горячие клавиши ленты",
        "Листать ленту идей клавишами.",
        json!({}),
    )
    .await;
    let (t, err) = mcp(
        &a,
        "p3",
        "memory_update_entry",
        json!({"id": w(&z), "action": "propose", "text": "к работе над лентой"}),
    )
    .await;
    assert!(!err, "{t}");
    let qz = after(&t, &format!("Предложено w:{z} → вопрос q:"));
    set_mode_web(&a, "review").await;
    let j = start(&a, "p4").await;
    assert_eq!(
        section_lines(&j, REVIEW_SECTION),
        Some(vec![review_hint(1), idea_line(y_title, "normal", &y)]),
        "{j}"
    );
    assert_eq!(get(&a, &z).await["status"], "proposed");
    assert_eq!(get(&a, &qz).await["status"], "open");
}
