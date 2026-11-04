//! Журнал «Идеи» через REST (M5.7, задача 2): создание и `origin`, правка и корзина идей
//! человека, `propose`, `decide`, `adopt`, ответ на вопрос-предложение, связи в `GET`, лента,
//! `/read w:`, права участников общего решения. Сервер без моделей (поиск — FTS).
mod common;

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, StatusCode, header};
use common::api::{Api, Resp, api, req};
use serde_json::{Value, json};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e57";
const KN: &str = "/api/v1/spaces/knowledge/entries";
const PASS: &str = "first-pass-123";
const NEW_PASS: &str = "second-pass-456";

fn sol() -> String {
    format!("/api/v1/spaces/{G}/entries")
}

fn ent(id: &str) -> String {
    format!("/api/v1/entries/{id}")
}

fn id(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("нет id: {v}"))
        .to_string()
}

fn ok(r: &Resp, s: StatusCode) {
    assert_eq!(r.status, s, "{}", r.text());
}

/// Ответ — problem с типом `/problems/<kind>` и статусом `s`.
fn problem(r: &Resp, s: StatusCode, kind: &str) {
    ok(r, s);
    assert_eq!(
        r.json()["type"],
        format!("/problems/{kind}"),
        "{}",
        r.text()
    );
}

/// Сервер с решением `G` администратора (`defer_days` = 7 — видно, что срок берётся из
/// `[ideas]`).
async fn setup(name: &str) -> Api {
    let a = api(&format!("ideas-{name}"), |c| c.ideas.defer_days = 7).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Проект"})),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    a
}

/// Идея токеном администратора в `path` с `origin`.
async fn idea_tok(a: &Api, path: &str, title: &str, origin: &str) -> String {
    let r = a
        .t(
            Method::POST,
            path,
            Some(
                json!({"kind": "idea", "title": title, "body": "Замысел идеи",
                        "tags": ["ux"], "data": {"origin": origin}}),
            ),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    id(&r.json())
}

/// Идея из WebUI (сессия администратора) в `path`.
async fn idea_web(a: &Api, path: &str, title: &str) -> String {
    let r = a
        .adm(
            Method::POST,
            path,
            Some(json!({"kind": "idea", "title": title, "body": "Замысел человека"})),
            None,
        )
        .await;
    ok(&r, StatusCode::CREATED);
    id(&r.json())
}

async fn get(a: &Api, id: &str) -> Value {
    let r = a.t(Method::GET, &ent(id), None).await;
    ok(&r, StatusCode::OK);
    r.json()
}

/// Запрос сессии WebUI администратора с `If-Match` текущей ревизии.
async fn adm_rev(a: &Api, m: Method, uri: &str, body: Option<Value>) -> Resp {
    let cur = a.adm(Method::GET, uri, None, None).await;
    let rev = cur.json()["rev"].to_string();
    a.adm_h(m, uri, body, &[("if-match", rev.as_str())]).await
}

async fn post_tok(a: &Api, id: &str, op: &str, body: Value) -> Resp {
    a.t(Method::POST, &format!("{}/{op}", ent(id)), Some(body))
        .await
}

async fn post_web(a: &Api, id: &str, op: &str, body: Value) -> Resp {
    a.adm(Method::POST, &format!("{}/{op}", ent(id)), Some(body), None)
        .await
}

async fn audit(a: &Api) -> Vec<zam_store::AuditRow> {
    a.st.system.list_audit(500, None).await.unwrap()
}

fn actions(rows: &[zam_store::AuditRow], action: &str) -> usize {
    rows.iter().filter(|r| r.action == action).count()
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

#[tokio::test]
async fn create_idea_spaces_and_origin() {
    let a = setup("create").await;
    let id1 = idea_tok(&a, &sol(), "Тёмная тема", "human").await;
    let e = get(&a, &id1).await;
    assert_eq!(e["kind"], "idea");
    assert_eq!(e["status"], "new");
    assert_eq!(e["data"]["origin"], "human");
    assert_eq!(e["data"]["priority"], "normal");
    let id2 = idea_tok(&a, KN, "Общая идея", "agent").await;
    assert_eq!(get(&a, &id2).await["data"]["origin"], "agent");
    // Rules — 422 (WebUI; токену запись в Rules закрыта раньше вида)
    let body = json!({"kind": "idea", "title": "Правило"});
    let r = a
        .adm(
            Method::POST,
            "/api/v1/spaces/rules/entries",
            Some(body.clone()),
            None,
        )
        .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    // Human: токену 404, WebUI — 422
    let human = "/api/v1/spaces/human/entries";
    let r = a
        .t(
            Method::POST,
            human,
            Some(json!({"kind": "idea", "title": "Личное", "data": {"origin": "human"}})),
        )
        .await;
    ok(&r, StatusCode::NOT_FOUND);
    let r = a.adm(Method::POST, human, Some(body.clone()), None).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    // токен без origin — 422 с подсказкой; неизвестное значение — так же
    for data in [
        json!({}),
        json!({"origin": "robot"}),
        json!({"origin": null}),
    ] {
        let r = a
            .t(
                Method::POST,
                &sol(),
                Some(json!({"kind": "idea", "title": "Без origin", "data": data})),
            )
            .await;
        ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(r.text().contains("data.origin"), "{}", r.text());
    }
    // WebUI: origin agent — 422; без поля — human
    let r = a
        .adm(
            Method::POST,
            &sol(),
            Some(json!({"kind": "idea", "title": "Агентская", "data": {"origin": "agent"}})),
            None,
        )
        .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    let w = idea_web(&a, &sol(), "Из WebUI").await;
    assert_eq!(get(&a, &w).await["data"]["origin"], "human");
    // поля сервера и прошедший horizon — 422
    for data in [
        json!({"origin": "human", "task_id": "x"}),
        json!({"origin": "human", "horizon": "2000-01-01"}),
        json!({"origin": "human", "priority": "urgent"}),
    ] {
        let r = a
            .t(
                Method::POST,
                &sol(),
                Some(json!({"kind": "idea", "title": "Плохая data", "data": data})),
            )
            .await;
        ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    }
    // повтор по Idempotency-Key — та же запись
    let body = json!({"kind": "idea", "title": "Повтор", "data": {"origin": "agent",
                      "horizon": "2099-01-01"}});
    let mut ids = Vec::new();
    for want in [StatusCode::CREATED, StatusCode::OK] {
        let r = a
            .tok_h(
                &a.token,
                Method::POST,
                &sol(),
                Some(body.clone()),
                &[("idempotency-key", "idea-1")],
            )
            .await;
        ok(&r, want);
        ids.push(id(&r.json()));
    }
    assert_eq!(ids[0], ids[1]);
    // ошибка структуры data на повторе — 422, как у первого запроса
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &sol(),
            Some(json!({"kind": "idea", "title": "Повтор",
                        "data": {"origin": "agent", "priority": "urgent"}})),
            &[("idempotency-key", "idea-1")],
        )
        .await;
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "unprocessable");
    assert!(r.text().contains("data.priority"), "{}", r.text());
}

#[tokio::test]
async fn patch_status_rejected_and_content_by_state() {
    let a = setup("patch").await;
    let i = idea_web(&a, &sol(), "Правка идеи").await;
    let r = adm_rev(
        &a,
        Method::PATCH,
        &ent(&i),
        Some(json!({"status": "accepted"})),
    )
    .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.text().contains("propose, decide, adopt"), "{}", r.text());
    // в new содержимое правится; data — слиянием
    let r = adm_rev(
        &a,
        Method::PATCH,
        &ent(&i),
        Some(json!({"title": "Правка идеи 2", "data": {"priority": "high"}})),
    )
    .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["data"]["priority"], "high");
    assert_eq!(r.json()["data"]["origin"], "human");
    let r = adm_rev(
        &a,
        Method::PATCH,
        &ent(&i),
        Some(json!({"data": {"origin": "agent"}})),
    )
    .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    ok(
        &post_web(&a, &i, "decide", json!({"decision": "accept"})).await,
        StatusCode::OK,
    );
    let r = adm_rev(
        &a,
        Method::PATCH,
        &ent(&i),
        Some(json!({"title": "Поздно"})),
    )
    .await;
    ok(&r, StatusCode::CONFLICT);
}

#[tokio::test]
async fn token_cannot_adopt_rewrite_or_delete_human_idea() {
    let a = setup("human-owned").await;
    let i = idea_tok(&a, &sol(), "Идея человека", "human").await;
    for body in [
        json!({"title": "Переписал"}),
        json!({"body": "Другой замысел"}),
        json!({"data": {"priority": "low"}}),
        json!({"tags": ["x"]}),
        json!({"area": "web"}),
        json!({"append_body": "и ещё", "title": "Переписал"}),
    ] {
        let r = a
            .tok_rev(&a.token, Method::PATCH, &ent(&i), Some(body))
            .await;
        problem(&r, StatusCode::FORBIDDEN, "idea-human-owned");
    }
    let r = a.tok_rev(&a.token, Method::DELETE, &ent(&i), None).await;
    problem(&r, StatusCode::FORBIDDEN, "idea-human-owned");
    let e = get(&a, &i).await;
    assert_eq!(e["rev"], 1, "ревизия прежняя");
    assert!(e["deleted_at"].is_null());
    // adopt токену — 403 и для идеи агента
    let ag = idea_tok(&a, &sol(), "Идея агента", "agent").await;
    for x in [&i, &ag] {
        ok(
            &post_tok(&a, x, "adopt", json!({})).await,
            StatusCode::FORBIDDEN,
        );
    }
    assert_eq!(get(&a, &ag).await["data"]["origin"], "agent");
    // чистое дополнение — 200
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &ent(&i),
            Some(json!({"append_body": "Дополнение агента"})),
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["rev"], 2);
    // идею агента токен правит и удаляет
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &ent(&ag),
            Some(json!({"title": "Своя"})),
        )
        .await;
    ok(&r, StatusCode::OK);
    let r = a.tok_rev(&a.token, Method::DELETE, &ent(&ag), None).await;
    ok(&r, StatusCode::NO_CONTENT);
    let rows = audit(&a).await;
    assert_eq!(actions(&rows, "entry.idea.adopt"), 0);
    assert_eq!(actions(&rows, "entry.delete"), 1, "только идея агента");
    // идею человека из корзины возвращает человек: токену 403
    ok(
        &adm_rev(&a, Method::DELETE, &ent(&i), None).await,
        StatusCode::NO_CONTENT,
    );
    let r = a
        .t(Method::POST, &format!("{}/restore", ent(&i)), None)
        .await;
    problem(&r, StatusCode::FORBIDDEN, "idea-human-owned");
    let r = a.adm(Method::GET, &ent(&i), None, None).await;
    ok(&r, StatusCode::NOT_FOUND);
    assert_eq!(actions(&audit(&a).await, "entry.restore"), 0);
    let r = a
        .adm(Method::POST, &format!("{}/restore", ent(&i)), None, None)
        .await;
    ok(&r, StatusCode::OK);
    // свою идею агент возвращает сам
    let r = a
        .t(Method::POST, &format!("{}/restore", ent(&ag)), None)
        .await;
    ok(&r, StatusCode::OK);
}

#[tokio::test]
async fn token_by_agent_cannot_decide_human_idea() {
    let a = setup("by-agent").await;
    let i = idea_tok(&a, &sol(), "Решает человек", "human").await;
    for body in [
        json!({"decision": "accept", "by": "agent"}),
        json!({"decision": "defer", "by": "agent", "horizon": "2099-01-01"}),
        json!({"decision": "reopen", "by": "agent", "reason": "снова"}),
        json!({"decision": "decline", "by": "agent"}),
    ] {
        let r = post_tok(&a, &i, "decide", body.clone()).await;
        problem(&r, StatusCode::FORBIDDEN, "idea-human-decision");
        let mut no_by = body;
        no_by.as_object_mut().unwrap().remove("by");
        let r = post_tok(&a, &i, "decide", no_by).await;
        problem(&r, StatusCode::BAD_REQUEST, "by-required");
    }
    let e = get(&a, &i).await;
    assert_eq!(
        (e["rev"].clone(), e["status"].clone()),
        (json!(1), json!("new"))
    );
    let rows = audit(&a).await;
    assert_eq!(actions(&rows, "entry.idea.decide"), 0, "{rows:?}");
    // channel webui у токена — 422; неизвестное решение — 422
    let r = post_tok(
        &a,
        &i,
        "decide",
        json!({"decision": "decline", "by": "human", "channel": "webui"}),
    )
    .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    let r = post_tok(
        &a,
        &i,
        "decide",
        json!({"decision": "start", "by": "human"}),
    )
    .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    // сессия WebUI — всегда человек: явный by: agent — 422
    let ag = idea_web(&a, &sol(), "WebUI не агент").await;
    for x in [&i, &ag] {
        let r = post_web(
            &a,
            x,
            "decide",
            json!({"decision": "decline", "by": "agent"}),
        )
        .await;
        ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    }
}

#[tokio::test]
async fn agent_declines_own_idea() {
    let a = setup("agent-decline").await;
    let i = idea_tok(&a, &sol(), "Своя мысль", "agent").await;
    // «начать» идею агента нельзя ни агенту, ни человеку до «принять как свою»
    let r = post_tok(
        &a,
        &i,
        "decide",
        json!({"decision": "accept", "by": "agent"}),
    )
    .await;
    problem(&r, StatusCode::FORBIDDEN, "idea-human-decision");
    let r = post_web(&a, &i, "decide", json!({"decision": "accept"})).await;
    problem(&r, StatusCode::CONFLICT, "idea-agent-origin");
    let r = post_tok(&a, &i, "propose", json!({})).await;
    problem(&r, StatusCode::CONFLICT, "idea-agent-origin");
    let r = post_tok(
        &a,
        &i,
        "decide",
        json!({"decision": "decline", "by": "agent", "reason": "устарело"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    let j = r.json();
    assert_eq!(j["entry"]["status"], "declined");
    assert_eq!(j["entry"]["data"]["decided"]["by"], "agent");
    assert!(j.get("task").is_none());
    // повторный decline — 409 transition-not-allowed
    let r = post_tok(
        &a,
        &i,
        "decide",
        json!({"decision": "decline", "by": "agent"}),
    )
    .await;
    ok(&r, StatusCode::CONFLICT);
    assert_eq!(r.json()["code"], "transition-not-allowed");
    assert_eq!(r.json()["from"], "declined");
}

#[tokio::test]
async fn client_derived_from_to_idea_rejected() {
    let a = setup("derived").await;
    let i = idea_tok(&a, &sol(), "Цель связи", "human").await;
    let r = a
        .t(
            Method::POST,
            &sol(),
            Some(json!({"kind": "task", "title": "Чужая задача"})),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    let t = id(&r.json());
    let edge = |kind: &str| json!({"src": format!("e:{t}"), "type": kind, "dst": format!("e:{i}")});
    let r = a
        .t(Method::POST, "/api/v1/edges", Some(edge("derived_from")))
        .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.text().contains("«начать»"), "{}", r.text());
    // другие связи с идеей разрешены
    let r = a
        .t(Method::POST, "/api/v1/edges", Some(edge("relates_to")))
        .await;
    ok(&r, StatusCode::CREATED);
    // links записи
    let r = a
        .t(
            Method::POST,
            &sol(),
            Some(
                json!({"kind": "insight", "title": "Вывод из идеи", "body": "Текст.",
                        "data": {"kind": "discovery"},
                        "links": [{"type": "derived_from", "target": format!("entry:{i}")}]}),
            ),
        )
        .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.text().contains("«начать»"), "{}", r.text());
}

#[tokio::test]
async fn webui_propose_422_token_propose_ok() {
    let a = setup("propose").await;
    let i = idea_web(&a, &sol(), "Предложить идею").await;
    let r = post_web(&a, &i, "propose", json!({"text": "сейчас"})).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.text().contains("действие агента"), "{}", r.text());
    let r = post_tok(
        &a,
        &i,
        "propose",
        json!({"text": "Подходит к текущей задаче"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    let j = r.json();
    assert_eq!(j["entry"]["status"], "proposed");
    let q = &j["question"];
    let qid = id(q);
    assert_eq!(q["kind"], "question");
    assert_eq!(q["status"], "open");
    assert_eq!(q["data"]["idea"], i.as_str());
    let keys: Vec<&str> = q["data"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["accept", "defer", "decline"]);
    assert_eq!(j["entry"]["data"]["question_id"], qid.as_str());
    // повтор — тот же вопрос, без новой ревизии и аудита; пустое тело тоже допустимо
    let r = a
        .t(Method::POST, &format!("{}/propose", ent(&i)), None)
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["question"]["id"], qid.as_str());
    assert_eq!(r.json()["entry"]["rev"], j["entry"]["rev"]);
    let rows = audit(&a).await;
    assert_eq!(actions(&rows, "entry.idea.propose"), 1);
    // связь relates_to вопрос → идея
    let r = a
        .t(Method::GET, &format!("{}/links", ent(&qid)), None)
        .await;
    ok(&r, StatusCode::OK);
    assert!(
        r.text().contains(&i) && r.text().contains("relates_to"),
        "{}",
        r.text()
    );
    // текст длиннее 2000 — 422
    let j2 = idea_tok(&a, &sol(), "Длинное предложение", "human").await;
    let r = post_tok(&a, &j2, "propose", json!({"text": "я".repeat(2001)})).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
}

/// Пространство решения `G`: id из записи в нём.
async fn solution_space(a: &Api) -> String {
    let r = a
        .t(
            Method::POST,
            &sol(),
            Some(json!({"kind": "task", "title": "Опорная задача"})),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    r.json()["space_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn propose_space_default_request_solution() {
    let a = setup("propose-space").await;
    let space = solution_space(&a).await;
    let i = idea_tok(&a, KN, "Общая идея для проекта", "human").await;
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &format!("{}/propose", ent(&i)),
            Some(json!({})),
            &[("x-zam-solution", G)],
        )
        .await;
    ok(&r, StatusCode::OK);
    let q = r.json()["question"].clone();
    assert_eq!(q["data"]["task_space"], space.as_str());
    // «начать» на странице «Вопросы» — задача в решении, а не в Knowledge
    let r = post_web(&a, &id(&q), "answer", json!({"option": "accept"})).await;
    ok(&r, StatusCode::OK);
    let j = r.json();
    assert_eq!(j["task"]["space_id"], space.as_str());
    assert_eq!(j["idea"]["status"], "accepted");
    // явное space вне правил — 422 (у идеи решения задача только в её пространстве)
    let k = idea_tok(&a, &sol(), "Идея решения", "human").await;
    let r = post_tok(&a, &k, "propose", json!({"space": "knowledge"})).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    let r = post_tok(&a, &k, "propose", json!({"space": "nope"})).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn decide_accept_returns_task_and_audit() {
    let a = setup("accept").await;
    let i = idea_web(&a, &sol(), "Секретный заголовок идеи").await;
    let r = post_web(&a, &i, "decide", json!({"decision": "accept"})).await;
    ok(&r, StatusCode::OK);
    let j = r.json();
    let tid = id(&j["task"]);
    assert_eq!(j["task"]["kind"], "task");
    assert_eq!(j["task"]["status"], "open");
    assert_eq!(j["task"]["title"], "Секретный заголовок идеи");
    assert_eq!(j["entry"]["status"], "accepted");
    assert_eq!(j["entry"]["data"]["task_id"], tid.as_str());
    assert_eq!(j["entry"]["data"]["decided"]["channel"], "webui");
    assert!(j.get("question").is_none(), "открытого вопроса не было");
    let r = a
        .t(Method::GET, &format!("{}/links", ent(&tid)), None)
        .await;
    ok(&r, StatusCode::OK);
    assert!(
        r.text().contains(&i) && r.text().contains("derived_from"),
        "{}",
        r.text()
    );
    let rows = audit(&a).await;
    let row = rows
        .iter()
        .find(|r| r.action == "entry.idea.decide")
        .expect("аудит decide");
    assert_eq!(row.target.as_deref(), Some(i.as_str()));
    assert_eq!(row.details["decision"], "accept");
    assert_eq!(row.details["task"], tid.as_str());
    let all = serde_json::to_string(&rows.iter().map(|r| &r.details).collect::<Vec<_>>()).unwrap();
    assert!(!all.contains("Секретный"), "текст идеи в аудите: {all}");
    assert!(!all.contains("Замысел"), "текст идеи в аудите: {all}");
}

#[tokio::test]
async fn accept_twice_409_already_decided_with_task_id() {
    let a = setup("accept-twice").await;
    let i = idea_tok(&a, &sol(), "Дважды", "human").await;
    let r = post_tok(
        &a,
        &i,
        "decide",
        json!({"decision": "accept", "by": "human"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    let tid = id(&r.json()["task"]);
    assert_eq!(r.json()["entry"]["data"]["decided"]["channel"], "chat");
    let r = post_tok(
        &a,
        &i,
        "decide",
        json!({"decision": "accept", "by": "human"}),
    )
    .await;
    problem(&r, StatusCode::CONFLICT, "already-decided");
    assert_eq!(r.json()["task_id"], tid.as_str());
    let tasks = a
        .t(Method::GET, &format!("{}?kind=task", sol()), None)
        .await;
    assert_eq!(tasks.json()["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn defer_twice_200_new_horizon() {
    let a = setup("defer").await;
    let i = idea_web(&a, &sol(), "Отложить").await;
    for h in ["2099-01-01", "2099-02-01"] {
        let r = post_web(&a, &i, "decide", json!({"decision": "defer", "horizon": h})).await;
        ok(&r, StatusCode::OK);
        assert_eq!(r.json()["entry"]["status"], "deferred");
        assert_eq!(r.json()["entry"]["data"]["horizon"], h);
    }
    // без даты — сегодня + [ideas] defer_days (7)
    let r = post_web(&a, &i, "decide", json!({"decision": "defer"})).await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["entry"]["data"]["horizon"], today_plus(7));
    for bad in [
        json!({"decision": "defer", "horizon": "2000-01-01"}),
        json!({"decision": "defer", "horizon": "01.01.2099"}),
        json!({"decision": "decline", "horizon": "2099-01-01"}),
        json!({"decision": "reopen"}),
    ] {
        let r = post_web(&a, &i, "decide", bad.clone()).await;
        ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let r = post_web(&a, &i, "decide", json!({"decision": "reopen"})).await;
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "reason-required");
    let r = post_web(
        &a,
        &i,
        "decide",
        json!({"decision": "reopen", "reason": "вернёмся"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["entry"]["status"], "new");
    assert!(r.json()["entry"]["data"]["horizon"].is_null());
}

#[tokio::test]
async fn answer_proposal_option_required_and_horizon() {
    let a = setup("answer").await;
    let i = idea_tok(&a, &sol(), "Ответ из чата", "human").await;
    let r = post_tok(&a, &i, "propose", json!({})).await;
    let qid = id(&r.json()["question"]);
    let r = post_tok(&a, &qid, "answer", json!({"text": "давай", "by": "human"})).await;
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "option-required");
    // horizon у обычного вопроса — 422
    let r = a
        .t(
            Method::POST,
            &sol(),
            Some(json!({"kind": "question", "title": "Обычный вопрос?",
                        "data": {"options": [{"key": "a", "text": "да"}]}})),
        )
        .await;
    let plain = id(&r.json());
    let r = post_tok(
        &a,
        &plain,
        "answer",
        json!({"option": "a", "by": "human", "horizon": "2099-01-01"}),
    )
    .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    // defer с horizon из чата
    let r = post_tok(
        &a,
        &qid,
        "answer",
        json!({"option": "defer", "horizon": "2099-03-01", "by": "human", "channel": "chat"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    let j = r.json();
    assert_eq!(j["entry"]["status"], "answered");
    assert!(j["decision_id"].is_string());
    assert_eq!(j["idea"]["status"], "deferred");
    assert_eq!(j["idea"]["data"]["horizon"], "2099-03-01");
    assert!(j.get("task").is_none());
    // defer без даты — срок из [ideas] defer_days
    let k = idea_tok(&a, &sol(), "Отложить без даты", "human").await;
    let r = post_tok(&a, &k, "propose", json!({})).await;
    let q2 = id(&r.json()["question"]);
    let r = post_tok(&a, &q2, "answer", json!({"option": "defer", "by": "human"})).await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["idea"]["data"]["horizon"], today_plus(7));
    // accept — задача в ответе
    let m = idea_tok(&a, &sol(), "Начать из чата", "human").await;
    let r = post_tok(&a, &m, "propose", json!({})).await;
    let q3 = id(&r.json()["question"]);
    let r = post_tok(
        &a,
        &q3,
        "answer",
        json!({"option": "accept", "by": "human"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    let j = r.json();
    assert_eq!(j["idea"]["status"], "accepted");
    assert_eq!(j["idea"]["data"]["task_id"], j["task"]["id"]);
}

#[tokio::test]
async fn answer_proposal_by_agent_403() {
    let a = setup("answer-agent").await;
    let i = idea_tok(&a, &sol(), "Агент не решает", "human").await;
    let r = post_tok(&a, &i, "propose", json!({})).await;
    let qid = id(&r.json()["question"]);
    let r = post_tok(
        &a,
        &qid,
        "answer",
        json!({"option": "accept", "by": "agent"}),
    )
    .await;
    problem(&r, StatusCode::FORBIDDEN, "idea-human-decision");
    assert_eq!(get(&a, &qid).await["status"], "open");
    assert_eq!(get(&a, &i).await["status"], "proposed");
}

#[tokio::test]
async fn get_idea_has_task_and_question_links() {
    let a = setup("get-links").await;
    let i = idea_tok(&a, &sol(), "Связи идеи", "human").await;
    assert!(get(&a, &i).await.get("task").is_none());
    let r = post_tok(&a, &i, "propose", json!({})).await;
    let qid = id(&r.json()["question"]);
    let e = get(&a, &i).await;
    assert_eq!(e["question"], json!({"id": qid, "status": "open"}));
    let r = post_web(&a, &i, "decide", json!({"decision": "accept"})).await;
    ok(&r, StatusCode::OK);
    let j = r.json();
    let tid = id(&j["task"]);
    assert_eq!(
        j["question"]["status"], "answered",
        "открытый вопрос отвечен"
    );
    let e = get(&a, &i).await;
    assert_eq!(
        e["task"],
        json!({"id": tid, "status": "open", "title": "Связи идеи"})
    );
    assert_eq!(e["question"]["status"], "answered");
    // задача удалена — пометка deleted
    let r = a.tok_rev(&a.token, Method::DELETE, &ent(&tid), None).await;
    ok(&r, StatusCode::NO_CONTENT);
    let e = get(&a, &i).await;
    assert_eq!(e["task"], json!({"id": tid, "deleted": true}));
    assert_eq!(e["status"], "accepted");
    // выход из accepted при удалённой задаче — reopen, следующий accept даёт новую задачу
    let r = post_web(
        &a,
        &i,
        "decide",
        json!({"decision": "reopen", "reason": "заново"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    let r = post_web(&a, &i, "decide", json!({"decision": "accept"})).await;
    ok(&r, StatusCode::OK);
    assert_ne!(id(&r.json()["task"]), tid);
}

#[tokio::test]
async fn list_filters_origin_priority_status() {
    let a = setup("list").await;
    for (title, origin, prio) in [
        ("Высокая", "human", "high"),
        ("Низкая", "human", "low"),
        ("Агентская", "agent", "normal"),
    ] {
        let r = a
            .t(
                Method::POST,
                &sol(),
                Some(json!({"kind": "idea", "title": title,
                            "data": {"origin": origin, "priority": prio}})),
            )
            .await;
        ok(&r, StatusCode::CREATED);
    }
    let count = |q: &str| {
        let a = &a;
        let q = q.to_string();
        async move {
            let r = a.t(Method::GET, &format!("{}?{q}", sol()), None).await;
            ok(&r, StatusCode::OK);
            r.json()["items"].as_array().unwrap().len()
        }
    };
    assert_eq!(count("kind=idea").await, 3);
    assert_eq!(count("kind=idea&origin=human").await, 2);
    assert_eq!(count("kind=idea&origin=agent").await, 1);
    assert_eq!(count("kind=idea&priority=high").await, 1);
    assert_eq!(count("kind=idea&origin=human&priority=low").await, 1);
    assert_eq!(count("kind=idea&status=new").await, 3);
    assert_eq!(count("kind=idea&status=accepted").await, 0);
    for bad in ["origin=human", "priority=high", "kind=task&origin=agent"] {
        let r = a.t(Method::GET, &format!("{}?{bad}", sol()), None).await;
        ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    }
    for bad in ["kind=idea&origin=robot", "kind=idea&priority=urgent"] {
        let r = a.t(Method::GET, &format!("{}?{bad}", sol()), None).await;
        ok(&r, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn delete_cascade_rest() {
    let a = setup("cascade").await;
    // идея с открытым вопросом удалена → вопрос obsolete
    let i = idea_web(&a, &sol(), "Удаляемая идея").await;
    let r = post_tok(&a, &i, "propose", json!({})).await;
    let qid = id(&r.json()["question"]);
    let r = adm_rev(&a, Method::DELETE, &ent(&i), None).await;
    ok(&r, StatusCode::NO_CONTENT);
    assert_eq!(get(&a, &qid).await["status"], "obsolete");
    // ответ на её вопрос — 409
    let r = post_web(&a, &qid, "answer", json!({"option": "accept"})).await;
    ok(&r, StatusCode::CONFLICT);
    // вопрос-предложение удалён → идея new без question_id
    let k = idea_tok(&a, &sol(), "Идея без вопроса", "human").await;
    let r = post_tok(&a, &k, "propose", json!({})).await;
    let q2 = id(&r.json()["question"]);
    let r = a.tok_rev(&a.token, Method::DELETE, &ent(&q2), None).await;
    ok(&r, StatusCode::NO_CONTENT);
    let e = get(&a, &k).await;
    assert_eq!(e["status"], "new");
    assert!(e["data"]["question_id"].is_null(), "{e}");
    // восстановление вопроса не возвращает предложение: он устаревает
    let r = a
        .t(Method::POST, &format!("{}/restore", ent(&q2)), None)
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(get(&a, &q2).await["status"], "obsolete");
}

#[tokio::test]
async fn read_w_prefix() {
    let a = setup("read").await;
    let i = idea_tok(&a, &sol(), "Читаемая идея", "human").await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/read",
            Some(json!({"items": [{"id": format!("w:{i}")}, {"id": format!("e:{i}")}]})),
        )
        .await;
    ok(&r, StatusCode::OK);
    let j = r.json();
    for k in 0..2 {
        assert!(j["items"][k]["error"].is_null(), "{j}");
        assert!(j["items"][k].to_string().contains("Замысел идеи"), "{j}");
    }
}

/// Сессия пользователя WebUI: cookie, CSRF, адрес клиента.
struct Web {
    cookie: String,
    csrf: String,
    ip: String,
}

fn with_ip(mut r: axum::http::Request<Body>, ip: &str) -> axum::http::Request<Body> {
    r.extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip.parse().unwrap(), 40_000)));
    r
}

async fn web(a: &Api, w: &Web, m: Method, uri: &str, body: Option<Value>) -> Resp {
    web_h(a, w, m, uri, body, &[]).await
}

async fn web_h(
    a: &Api,
    w: &Web,
    m: Method,
    uri: &str,
    body: Option<Value>,
    extra: &[(&str, &str)],
) -> Resp {
    let mut b = req(m, uri, body.as_ref())
        .header(header::COOKIE, &w.cookie)
        .header("x-csrf-token", &w.csrf);
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let body = body.map_or_else(Body::empty, |v| Body::from(v.to_string()));
    a.send(with_ip(b.body(body).unwrap(), &w.ip)).await
}

/// Правка сессией с `If-Match` текущей ревизии.
async fn web_rev(a: &Api, w: &Web, m: Method, uri: &str, body: Option<Value>) -> Resp {
    let cur = web(a, w, Method::GET, uri, None).await;
    let rev = cur.json()["rev"].to_string();
    web_h(a, w, m, uri, body, &[("if-match", rev.as_str())]).await
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
    ok(&r, StatusCode::OK);
    let body = json!({ "login": name, "password": PASS });
    let r = req(Method::POST, "/api/v1/auth/login", Some(&body))
        .body(Body::from(body.to_string()))
        .unwrap();
    let r = a.send(with_ip(r, ip)).await;
    ok(&r, StatusCode::OK);
    let w = Web {
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
        &w,
        Method::POST,
        "/api/v1/auth/password",
        Some(json!({ "old": PASS, "new": NEW_PASS })),
    )
    .await;
    ok(&r, StatusCode::NO_CONTENT);
    (uid, tok, w)
}

/// Режим участника `access` у владельца `owner_tok`.
async fn set_access(a: &Api, owner_tok: &str, member: &str, access: &str) {
    let r = a
        .tok(
            owner_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{member}/approve"),
            Some(json!({ "access": access })),
        )
        .await;
    ok(&r, StatusCode::OK);
}

/// Матрица прав §9: владелец (WebUI), участник `rw` (WebUI и токен), участник `ro` (WebUI и
/// токен); задача «начать» — в базе владельца.
#[tokio::test]
async fn shared_solution_rights_matrix() {
    let a = api("ideas-shared", |_| {}).await;
    let (o_id, o_tok, o_web) = ready_user(&a, "Ольга", "10.7.0.1").await;
    let (m_id, m_tok, m_web) = ready_user(&a, "Марк", "10.7.0.2").await;
    let r = a
        .tok(
            &o_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Общее"})),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    let r = a
        .tok(
            &m_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Общее"})),
        )
        .await;
    ok(&r, StatusCode::ACCEPTED);
    set_access(&a, &o_tok, &m_id, "rw").await;
    let create = |title: &str, origin: Option<&str>| {
        let mut b = json!({"kind": "idea", "title": title, "body": "Замысел"});
        if let Some(o) = origin {
            b["data"] = json!({"origin": o});
        }
        b
    };
    // владелец в WebUI: создаёт, «начать» — задача в его базе
    let r = web(
        &a,
        &o_web,
        Method::POST,
        &sol(),
        Some(create("Владелец", None)),
    )
    .await;
    ok(&r, StatusCode::CREATED);
    let own = id(&r.json());
    let r = web(
        &a,
        &o_web,
        Method::POST,
        &format!("{}/propose", ent(&own)),
        Some(json!({})),
    )
    .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    let r = web(
        &a,
        &o_web,
        Method::POST,
        &format!("{}/decide", ent(&own)),
        Some(json!({"decision": "accept"})),
    )
    .await;
    ok(&r, StatusCode::OK);
    let tid: zam_core::Id = id(&r.json()["task"]).parse().unwrap();
    let o_db = a.st.users.get(o_id.parse().unwrap()).await.unwrap();
    assert!(
        zam_store::get_entry(&o_db, tid).await.is_ok(),
        "задача в базе владельца"
    );
    let m_db = a.st.users.get(m_id.parse().unwrap()).await.unwrap();
    assert!(zam_store::get_entry(&m_db, tid).await.is_err());

    // участник rw в WebUI: создаёт, правит, решает, принимает как свою
    let r = web(
        &a,
        &m_web,
        Method::POST,
        &sol(),
        Some(create("Участник", None)),
    )
    .await;
    ok(&r, StatusCode::CREATED);
    let mine = id(&r.json());
    let r = web_rev(
        &a,
        &m_web,
        Method::PATCH,
        &ent(&mine),
        Some(json!({"title": "Участник 2"})),
    )
    .await;
    ok(&r, StatusCode::OK);
    let r = web(
        &a,
        &m_web,
        Method::POST,
        &format!("{}/propose", ent(&mine)),
        Some(json!({})),
    )
    .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    let r = web(
        &a,
        &m_web,
        Method::POST,
        &format!("{}/decide", ent(&mine)),
        Some(json!({"decision": "defer", "horizon": "2099-01-01"})),
    )
    .await;
    ok(&r, StatusCode::OK);
    // токен участника rw: origin обязателен, идею человека не переписывает и не удаляет
    let r = a
        .tok(&m_tok, Method::POST, &sol(), Some(create("Токен", None)))
        .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    let r = a
        .tok(
            &m_tok,
            Method::POST,
            &sol(),
            Some(create("Агентская", Some("agent"))),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    let agent = id(&r.json());
    let r = a
        .tok_rev(
            &m_tok,
            Method::PATCH,
            &ent(&own),
            Some(json!({"title": "Чужое"})),
        )
        .await;
    problem(&r, StatusCode::FORBIDDEN, "idea-human-owned");
    let r = a
        .tok_rev(
            &m_tok,
            Method::PATCH,
            &ent(&agent),
            Some(json!({"title": "Своя"})),
        )
        .await;
    ok(&r, StatusCode::OK);
    let r = a.tok_rev(&m_tok, Method::DELETE, &ent(&mine), None).await;
    problem(&r, StatusCode::FORBIDDEN, "idea-human-owned");
    let r = a
        .tok(
            &m_tok,
            Method::POST,
            &format!("{}/adopt", ent(&agent)),
            None,
        )
        .await;
    ok(&r, StatusCode::FORBIDDEN);
    let r = a
        .tok(
            &m_tok,
            Method::POST,
            &format!("{}/propose", ent(&mine)),
            Some(json!({"text": "пора"})),
        )
        .await;
    ok(&r, StatusCode::OK);
    let r = a
        .tok(
            &m_tok,
            Method::POST,
            &format!("{}/decide", ent(&mine)),
            Some(json!({"decision": "decline", "by": "human"})),
        )
        .await;
    ok(&r, StatusCode::OK);
    // участник rw в WebUI принимает идею агента как свою
    let r = web(
        &a,
        &m_web,
        Method::POST,
        &format!("{}/adopt", ent(&agent)),
        None,
    )
    .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["data"]["origin"], "human");
    assert_eq!(r.json()["data"]["adopted_by"], m_id.as_str());
    let r = web(
        &a,
        &m_web,
        Method::POST,
        &format!("{}/adopt", ent(&agent)),
        None,
    )
    .await;
    problem(&r, StatusCode::CONFLICT, "already-adopted");
    // корзина: идею человека возвращает человек, токен участника — 403
    let r = web_rev(&a, &o_web, Method::DELETE, &ent(&own), None).await;
    ok(&r, StatusCode::NO_CONTENT);
    let r = a
        .tok(
            &m_tok,
            Method::POST,
            &format!("{}/restore", ent(&own)),
            None,
        )
        .await;
    problem(&r, StatusCode::FORBIDDEN, "idea-human-owned");
    let r = web(
        &a,
        &o_web,
        Method::POST,
        &format!("{}/restore", ent(&own)),
        None,
    )
    .await;
    ok(&r, StatusCode::OK);

    // ro: читает, но не пишет — ни WebUI, ни токен
    set_access(&a, &o_tok, &m_id, "ro").await;
    let m_ro = a.issue(&m_id, "ro").await;
    for t in [&m_tok, &m_ro] {
        ok(
            &a.tok(t, Method::GET, &ent(&own), None).await,
            StatusCode::OK,
        );
        let r = a
            .tok(t, Method::POST, &sol(), Some(create("ro", Some("agent"))))
            .await;
        ok(&r, StatusCode::FORBIDDEN);
        for (op, body) in [
            ("propose", json!({})),
            ("decide", json!({"decision": "decline", "by": "human"})),
            ("adopt", json!({})),
        ] {
            let r = a
                .tok(
                    t,
                    Method::POST,
                    &format!("{}/{op}", ent(&agent)),
                    Some(body),
                )
                .await;
            ok(&r, StatusCode::FORBIDDEN);
        }
        let r = a.tok_rev(t, Method::DELETE, &ent(&agent), None).await;
        ok(&r, StatusCode::FORBIDDEN);
    }
    ok(
        &web(&a, &m_web, Method::GET, &ent(&own), None).await,
        StatusCode::OK,
    );
    let r = web(
        &a,
        &m_web,
        Method::POST,
        &sol(),
        Some(create("ro web", None)),
    )
    .await;
    ok(&r, StatusCode::FORBIDDEN);
    let r = web(
        &a,
        &m_web,
        Method::POST,
        &format!("{}/decide", ent(&agent)),
        Some(json!({"decision": "decline"})),
    )
    .await;
    ok(&r, StatusCode::FORBIDDEN);
    let r = web_rev(
        &a,
        &m_web,
        Method::PATCH,
        &ent(&agent),
        Some(json!({"title": "ro"})),
    )
    .await;
    ok(&r, StatusCode::FORBIDDEN);
    // идеи участника лежат в базе владельца
    assert!(
        zam_store::get_entry(&o_db, mine.parse().unwrap())
            .await
            .is_ok()
    );
}

/// Отзыв доступа участника (§9): его токен больше не предлагает и не решает идеи общего решения.
#[tokio::test]
async fn revoked_member_cannot_decide_or_propose() {
    let a = api("ideas-revoked", |_| {}).await;
    let (_, o_tok) = a.new_user("Ольга").await;
    let (m_id, m_tok) = a.new_user("Марк").await;
    let r = a
        .tok(
            &o_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Общее"})),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    let r = a
        .tok(
            &m_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Общее"})),
        )
        .await;
    ok(&r, StatusCode::ACCEPTED);
    set_access(&a, &o_tok, &m_id, "rw").await;
    let mut ids = Vec::new();
    for t in ["Первая", "Вторая"] {
        let r = a
            .tok(
                &m_tok,
                Method::POST,
                &sol(),
                Some(json!({"kind": "idea", "title": t, "data": {"origin": "human"}})),
            )
            .await;
        ok(&r, StatusCode::CREATED);
        ids.push(id(&r.json()));
    }
    // до отзыва — работает
    let r = a
        .tok(
            &m_tok,
            Method::POST,
            &format!("{}/propose", ent(&ids[0])),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    let r = a
        .tok(
            &o_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{m_id}/revoke"),
            None,
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    for (x, op, body) in [
        (&ids[1], "propose", json!({})),
        (
            &ids[0],
            "decide",
            json!({"decision": "accept", "by": "human"}),
        ),
        (
            &ids[1],
            "decide",
            json!({"decision": "decline", "by": "human"}),
        ),
    ] {
        let r = a
            .tok(
                &m_tok,
                Method::POST,
                &format!("{}/{op}", ent(x)),
                Some(body),
            )
            .await;
        problem(&r, StatusCode::FORBIDDEN, "solution-access");
    }
    // владелец видит идеи без изменений
    for (x, st) in [(&ids[0], "proposed"), (&ids[1], "new")] {
        let r = a.tok(&o_tok, Method::GET, &ent(x), None).await;
        ok(&r, StatusCode::OK);
        assert_eq!(r.json()["status"], st);
    }
}

/// Задача 7 (§16): идея `accepted` со снятой задачей — `propose` из WebUI 422, токеном без
/// `text` 422, с `text` — вопрос «вернуть» (`data.proposal: reopen`, без `task_space`), идея
/// остаётся `accepted` с `question_id`, аудит `entry.idea.propose`; ответ: без варианта —
/// `option-required` с `reopen`, `accept` — 422, `by: agent` — 403, `reopen` человека — идея
/// `new`; `GET` идеи показывает открытый вопрос.
#[tokio::test]
async fn reopen_proposal_rest() {
    let a = setup("reopen").await;
    let i = idea_tok(&a, &sol(), "Кэш выжимок", "human").await;
    let r = post_tok(
        &a,
        &i,
        "decide",
        json!({"decision": "accept", "by": "human"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    let task = id(&r.json()["task"]);
    // живая задача — 409
    let r = post_tok(&a, &i, "propose", json!({"text": "пора"})).await;
    ok(&r, StatusCode::CONFLICT);
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &ent(&task),
            Some(json!({"status": "dropped", "reason": "не до того"})),
        )
        .await;
    ok(&r, StatusCode::OK);
    let r = post_web(&a, &i, "propose", json!({"text": "пора"})).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    let r = post_tok(&a, &i, "propose", json!({})).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        r.json()["detail"]
            .as_str()
            .unwrap()
            .contains("почему идею пора вернуть"),
        "{}",
        r.text()
    );
    let before = actions(&audit(&a).await, "entry.idea.propose");
    let r = post_tok(&a, &i, "propose", json!({"text": "кэш снова тормозит"})).await;
    ok(&r, StatusCode::OK);
    let j = r.json();
    assert_eq!(j["entry"]["status"], "accepted", "{j}");
    let qid = id(&j["question"]);
    assert_eq!(j["entry"]["data"]["question_id"], qid.as_str(), "{j}");
    assert_eq!(j["question"]["title"], "Вернуть идею «Кэш выжимок»?", "{j}");
    assert_eq!(j["question"]["data"]["proposal"], "reopen", "{j}");
    assert!(j["question"]["data"]["task_space"].is_null(), "{j}");
    assert_eq!(actions(&audit(&a).await, "entry.idea.propose"), before + 1);
    // повтор — тот же вопрос, без аудита
    let r = post_tok(&a, &i, "propose", json!({})).await;
    ok(&r, StatusCode::OK);
    assert_eq!(id(&r.json()["question"]), qid);
    assert_eq!(actions(&audit(&a).await, "entry.idea.propose"), before + 1);
    let e = get(&a, &i).await;
    assert_eq!(e["question"]["id"], qid.as_str(), "{e}");
    assert_eq!(e["question"]["status"], "open", "{e}");
    assert_eq!(e["task"]["status"], "dropped", "{e}");
    // ответы
    let r = post_tok(&a, &qid, "answer", json!({"text": "давай", "by": "human"})).await;
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "option-required");
    assert!(r.text().contains("reopen"), "{}", r.text());
    for key in ["accept", "defer"] {
        let r = post_tok(&a, &qid, "answer", json!({"option": key, "by": "human"})).await;
        ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let r = post_tok(
        &a,
        &qid,
        "answer",
        json!({"option": "reopen", "by": "human", "horizon": today_plus(3)}),
    )
    .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    let r = post_tok(
        &a,
        &qid,
        "answer",
        json!({"option": "reopen", "by": "agent"}),
    )
    .await;
    problem(&r, StatusCode::FORBIDDEN, "idea-human-decision");
    let r = post_tok(
        &a,
        &qid,
        "answer",
        json!({"option": "reopen", "by": "human"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    let j = r.json();
    assert_eq!(j["idea"]["status"], "new", "{j}");
    assert_eq!(j["idea"]["data"]["prev_task_ids"], json!([task]), "{j}");
    assert!(j["idea"]["data"]["task_id"].is_null(), "{j}");
    assert!(j.get("task").is_none(), "{j}");
}
