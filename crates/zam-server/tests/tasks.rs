//! Журнал задач (M4.5 §3, задача 4): статусы и время, переоткрытие с причиной, захват
//! (свободна, истёк, та же сессия, 409, гонка), снятие (чужой — 403, человек — можно), аудит,
//! раздел `### Задачи` дайджеста, `claimed_tasks` сессии, MCP `status|claim|release`.
//! Сервер без моделей (поиск — FTS).
mod common;

use axum::body::Body;
use axum::http::{Method, StatusCode, header};
use common::api::{Api, Resp, api, req};
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};
use tower::ServiceExt;
use zam_server::config::ServerConfig;

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

fn sess(s: &str) -> [(&str, &str); 1] {
    [("x-zam-session", s)]
}

/// Создаёт задачу в Knowledge; её запись.
async fn new_task(a: &Api, title: &str, data: Value) -> Value {
    let r = a
        .t(
            Method::POST,
            KN,
            Some(
                json!({"kind": "task", "title": title, "body": format!("План: {title}"),
                        "data": data}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()
}

async fn get(a: &Api, id: &str) -> Value {
    let r = a
        .t(Method::GET, &format!("/api/v1/entries/{id}"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// PATCH токеном администратора с `If-Match` текущей ревизии.
async fn patch(a: &Api, id: &str, body: Value) -> Resp {
    a.tok_rev(
        &a.token,
        Method::PATCH,
        &format!("/api/v1/entries/{id}"),
        Some(body),
    )
    .await
}

async fn status(a: &Api, id: &str, s: &str) -> Value {
    let r = patch(a, id, json!({"status": s})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

async fn claim(a: &Api, id: &str, session: Option<&str>, body: Value) -> Resp {
    let h: Vec<(&str, &str)> = session.map(|s| ("x-zam-session", s)).into_iter().collect();
    a.tok_h(
        &a.token,
        Method::POST,
        &format!("/api/v1/entries/{id}/claim"),
        Some(body),
        &h,
    )
    .await
}

async fn release(a: &Api, id: &str, session: Option<&str>) -> Resp {
    let h: Vec<(&str, &str)> = session.map(|s| ("x-zam-session", s)).into_iter().collect();
    a.tok_h(
        &a.token,
        Method::POST,
        &format!("/api/v1/entries/{id}/release"),
        None,
        &h,
    )
    .await
}

/// Время `data.<поле>` записи.
fn ts(v: &Value, field: &str) -> Timestamp {
    v["data"][field]
        .as_str()
        .unwrap_or_else(|| panic!("нет data.{field}: {v}"))
        .parse()
        .unwrap()
}

#[tokio::test]
async fn task_lifecycle_times() {
    let a = api("tasks-lifecycle", |_| {}).await;
    let t = new_task(
        &a,
        "Миграция",
        json!({"next_step": " сделать схему ", "owner": "agent"}),
    )
    .await;
    let tid = id(&t);
    assert_eq!(t["kind"], "task");
    assert_eq!(t["status"], "open");
    assert_eq!(
        t["data"],
        json!({"next_step": "сделать схему", "owner": "agent"})
    );
    assert!(t.get("duration_min").is_none(), "{t}");

    // in_progress ставит started_at; следующий шаг — тем же PATCH
    let r = patch(
        &a,
        &tid,
        json!({"status": "in_progress", "next_step": "написать тест"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["status"], "in_progress");
    assert_eq!(j["data"]["next_step"], "написать тест");
    assert_eq!(j["rev"], 2, "новая ревизия");
    let started = ts(&j, "started_at");
    assert!(j["data"].get("done_at").is_none());

    // blocked → блокер; повторный in_progress время не меняет и снимает блокер
    let j = patch(
        &a,
        &tid,
        json!({"status": "blocked", "blocked_by": "ждём ревью"}),
    )
    .await
    .json();
    assert_eq!(j["data"]["blocked_by"], "ждём ревью");
    let j = status(&a, &tid, "in_progress").await;
    assert_eq!(ts(&j, "started_at"), started);
    assert!(j["data"].get("blocked_by").is_none(), "{j}");

    // поля сервера клиент не задаёт; неизвестные поля — 422
    for bad in [
        json!({"data": {"claimed_by": {"session": "x", "until": "2030-01-01T00:00:00Z"}}}),
        json!({"data": {"started_at": "2020-01-01T00:00:00Z"}}),
        json!({"data": {"foo": 1}}),
        json!({"data": {"next_step": "x".repeat(1001)}}),
        json!({"status": "active"}),
    ] {
        let r = patch(&a, &tid, bad.clone()).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad}: {}",
            r.text()
        );
    }

    // done ставит done_at и считает длительность
    let j = status(&a, &tid, "done").await;
    let done = ts(&j, "done_at");
    assert!(done >= started);
    let expect = done.duration_since(started).as_secs() / 60;
    assert_eq!(j["duration_min"], expect, "{j}");
    assert_eq!(get(&a, &tid).await["duration_min"], expect);

    // лента done-задач несёт duration_min, остальные виды — нет
    new_task(&a, "Открытая", json!({})).await;
    let r = a
        .t(Method::GET, &format!("{KN}?kind=task&status=done"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let items = r.json()["items"].clone();
    assert_eq!(items.as_array().unwrap().len(), 1);
    assert_eq!(items[0]["duration_min"], expect);
    let r = a
        .t(Method::GET, &format!("{KN}?kind=task&status=open"), None)
        .await;
    assert!(r.json()["items"][0].get("duration_min").is_none());

    // задача без started_at: duration_min = null
    let t2 = id(&new_task(&a, "Сразу готово", json!({})).await);
    let j = status(&a, &t2, "done").await;
    assert!(j["duration_min"].is_null(), "{j}");

    // задачи — не в Rules и не вне пространств журналов
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/rules/entries",
            Some(json!({"kind": "task", "title": "x"})),
        )
        .await;
    assert!(
        r.status == StatusCode::UNPROCESSABLE_ENTITY || r.status == StatusCode::FORBIDDEN,
        "{}",
        r.text()
    );
}

#[tokio::test]
async fn task_reopen_requires_reason() {
    let a = api("tasks-reopen", |_| {}).await;
    let tid = id(&new_task(&a, "Задача", json!({})).await);
    status(&a, &tid, "in_progress").await;
    let j = status(&a, &tid, "done").await;
    assert!(j["data"]["done_at"].is_string());
    // без причины — 422 reason-required
    let r = patch(&a, &tid, json!({"status": "open"})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert_eq!(r.json()["type"], "/problems/reason-required");
    let r = patch(&a, &tid, json!({"status": "open", "reason": "  "})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    // из done — только в open
    for s in ["in_progress", "blocked", "dropped"] {
        let r = patch(&a, &tid, json!({"status": s, "reason": "нашли баг"})).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{s}: {}",
            r.text()
        );
        assert_eq!(r.json()["type"], "/problems/unprocessable");
    }
    // с причиной — open, done_at снят, причина в ревизии, started_at сохранён
    let r = patch(&a, &tid, json!({"status": "open", "reason": "нашли баг"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["status"], "open");
    assert!(j["data"].get("done_at").is_none(), "{j}");
    assert!(j["data"]["started_at"].is_string());
    let revs = a
        .t(
            Method::GET,
            &format!("/api/v1/entries/{tid}/revisions"),
            None,
        )
        .await
        .json();
    assert_eq!(
        revs["items"].as_array().unwrap().last().unwrap()["reason"],
        "нашли баг"
    );
    // из любого статуса — в dropped; и из dropped — только в open
    status(&a, &tid, "dropped").await;
    let r = patch(&a, &tid, json!({"status": "in_progress", "reason": "r"})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = patch(&a, &tid, json!({"status": "open"})).await;
    assert_eq!(r.json()["type"], "/problems/reason-required");
    // устаревший If-Match — 409 с ревизией, без заголовка — 428
    let r = a
        .tok_h(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{tid}"),
            Some(json!({"status": "open", "reason": "r"})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert!(r.json()["rev"].is_number());
    let r = a
        .t(
            Method::PATCH,
            &format!("/api/v1/entries/{tid}"),
            Some(json!({"status": "open", "reason": "r"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED);
}

#[tokio::test]
async fn claim_free_expired_same_session() {
    let a = api("tasks-claim-free", |_| {}).await;
    let tid = id(&new_task(&a, "Захват", json!({})).await);
    let before = get(&a, &tid).await;
    // свободна — захват; ревизия и updated_at не меняются
    let r = claim(
        &a,
        &tid,
        Some("s1"),
        json!({"ttl_min": 30, "agent": "claude"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let c = &j["data"]["claimed_by"];
    assert_eq!(c["session"], "s1");
    assert_eq!(c["agent"], "claude");
    let until: Timestamp = c["until"].as_str().unwrap().parse().unwrap();
    let left = until.duration_since(Timestamp::now()).as_secs();
    assert!((29 * 60..=30 * 60).contains(&left), "{left}");
    assert_eq!(j["rev"], before["rev"]);
    assert_eq!(j["updated_at"], before["updated_at"]);
    let revs = a
        .t(
            Method::GET,
            &format!("/api/v1/entries/{tid}/revisions"),
            None,
        )
        .await
        .json();
    assert_eq!(revs["items"].as_array().unwrap().len(), 1, "ревизии нет");

    // та же сессия — продление (по умолчанию 120 мин)
    let j = claim(&a, &tid, Some("s1"), json!({})).await.json();
    let until2: Timestamp = j["data"]["claimed_by"]["until"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(until2 > until + SignedDuration::from_mins(80));
    // пустое тело тоже допустимо
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &format!("/api/v1/entries/{tid}/claim"),
            None,
            &sess("s1"),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // срок прежнего захвата истёк — захватывает другая сессия
    let t2 = id(&new_task(&a, "Просрочена", json!({})).await);
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let tid2: zam_core::Id = t2.parse().unwrap();
    zam_store::claim_task(
        &db,
        tid2,
        "old",
        None,
        1,
        Timestamp::now() - SignedDuration::from_hours(3),
    )
    .await
    .unwrap();
    let r = claim(&a, &t2, Some("s2"), json!({"ttl_min": 5})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["data"]["claimed_by"]["session"], "s2");

    // закрытую задачу не захватить
    status(&a, &t2, "done").await;
    assert_eq!(
        get(&a, &t2).await["data"].get("claimed_by"),
        None,
        "done снимает захват"
    );
    let r = claim(&a, &t2, Some("s2"), json!({})).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
}

#[tokio::test]
async fn claim_taken_409_with_claimed_by() {
    let a = api("tasks-claim-409", |_| {}).await;
    let tid = id(&new_task(&a, "Занята", json!({})).await);
    assert_eq!(
        claim(&a, &tid, Some("s1"), json!({"ttl_min": 60}))
            .await
            .status,
        StatusCode::OK
    );
    let r = claim(&a, &tid, Some("s2"), json!({})).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.header("content-type"), "application/problem+json");
    let j = r.json();
    assert_eq!(j["type"], "/problems/task-claimed");
    assert_eq!(j["claimed_by"]["session"], "s1");
    assert!(j["claimed_by"]["until"].is_string());
    // захват не изменился
    assert_eq!(get(&a, &tid).await["data"]["claimed_by"]["session"], "s1");
    // без сессии — 400; ttl вне 1..=1440 — 422; чужие поля тела — 400
    assert_eq!(
        claim(&a, &tid, None, json!({})).await.status,
        StatusCode::BAD_REQUEST
    );
    for ttl in [0, 1441] {
        let r = claim(&a, &tid, Some("s1"), json!({"ttl_min": ttl})).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{ttl}: {}",
            r.text()
        );
    }
    assert_eq!(
        claim(&a, &tid, Some("s1"), json!({"x": 1})).await.status,
        StatusCode::BAD_REQUEST
    );
    // 1440 допустимо
    assert_eq!(
        claim(&a, &tid, Some("s1"), json!({"ttl_min": 1440}))
            .await
            .status,
        StatusCode::OK
    );
    // не задача — 422; нет записи — 404
    let ins = a
        .t(
            Method::POST,
            KN,
            Some(json!({"kind": "insight", "title": "Не задача", "body": "x",
                        "data": {"kind": "discovery"}})),
        )
        .await
        .json();
    let r = claim(&a, &id(&ins), Some("s1"), json!({})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    let r = claim(&a, "01JZZZZZZZZZZZZZZZZZZZZZZZ", Some("s1"), json!({})).await;
    assert!(
        r.status == StatusCode::NOT_FOUND || r.status == StatusCode::BAD_REQUEST,
        "{}",
        r.text()
    );
    // токен `ro` захватить не может
    let ro = a.issue(&a.admin_id(), "ro").await;
    let r = a
        .tok_h(
            &ro,
            Method::POST,
            &format!("/api/v1/entries/{tid}/claim"),
            Some(json!({})),
            &sess("s9"),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_claim_one_wins() {
    let a = api("tasks-claim-race", |_| {}).await;
    let tid = id(&new_task(&a, "Гонка", json!({})).await);
    let mut hs = Vec::new();
    for i in 0..8 {
        let app = a.app.clone();
        let token = a.token.clone();
        let uri = format!("/api/v1/entries/{tid}/claim");
        hs.push(tokio::spawn(async move {
            let body = json!({"ttl_min": 10});
            let r = req(Method::POST, &uri, Some(&body))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header("x-zam-session", format!("race-{i}"))
                .body(Body::from(body.to_string()))
                .unwrap();
            let r = app.oneshot(r).await.unwrap();
            let status = r.status();
            let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
            (i, status, serde_json::from_slice::<Value>(&bytes).unwrap())
        }));
    }
    let mut winners = Vec::new();
    for h in hs {
        let (i, status, body) = h.await.unwrap();
        match status {
            StatusCode::OK => winners.push(i),
            StatusCode::CONFLICT => assert_eq!(body["type"], "/problems/task-claimed", "{body}"),
            s => panic!("{s}: {body}"),
        }
    }
    assert_eq!(winners.len(), 1, "захватила не одна сессия: {winners:?}");
    let cur = get(&a, &tid).await;
    assert_eq!(
        cur["data"]["claimed_by"]["session"],
        format!("race-{}", winners[0])
    );
}

#[tokio::test]
async fn release_foreign_403_human_ok() {
    let a = api("tasks-release", |_| {}).await;
    let tid = id(&new_task(&a, "Снятие", json!({})).await);
    // свободную снять — ничего не происходит
    let r = release(&a, &tid, Some("s2")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    claim(&a, &tid, Some("s1"), json!({})).await;
    // чужая сессия и запрос без сессии — 403, захват на месте
    for s in [Some("s2"), None] {
        let r = release(&a, &tid, s).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{s:?}: {}", r.text());
    }
    assert_eq!(get(&a, &tid).await["data"]["claimed_by"]["session"], "s1");
    // человек (WebUI) снимает чужой захват
    let r = a
        .adm_h(
            Method::POST,
            &format!("/api/v1/entries/{tid}/release"),
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["data"].get("claimed_by").is_none(), "{}", r.text());
    // своя сессия снимает свой захват
    claim(&a, &tid, Some("s1"), json!({})).await;
    let r = release(&a, &tid, Some("s1")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["data"].get("claimed_by").is_none());
    // аудит записи: захваты и снятия, без текста записи
    claim(&a, &tid, Some("s3"), json!({})).await;
    let audit = a
        .adm(Method::GET, "/api/v1/admin/audit?limit=100", None, None)
        .await
        .json();
    let rows: Vec<&Value> = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["target"] == tid.as_str())
        .collect();
    let acts: Vec<&str> = rows.iter().map(|r| r["action"].as_str().unwrap()).collect();
    assert_eq!(
        acts.iter().filter(|x| **x == "entry.claim").count(),
        3,
        "{acts:?}"
    );
    assert_eq!(
        acts.iter().filter(|x| **x == "entry.release").count(),
        2,
        "{acts:?}"
    );
    assert!(!audit.to_string().contains("План: Снятие"));
}

#[tokio::test]
async fn digest_tasks_section_order() {
    let a = api("tasks-digest", roomy).await;
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
    let mut ids = Vec::new();
    for (title, data) in [
        ("Моя задача", json!({"next_step": "дописать тест"})),
        ("Чужая задача", json!({"next_step": "ревью"})),
        ("Застряла", json!({"next_step": "ждать"})),
        ("Открытая", json!({})),
        ("Готовая", json!({})),
    ] {
        ids.push(id(&new_task(&a, title, data).await));
    }
    status(&a, &ids[0], "in_progress").await;
    status(&a, &ids[1], "in_progress").await;
    let r = patch(
        &a,
        &ids[2],
        json!({"status": "blocked", "blocked_by": "ждём API"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    status(&a, &ids[4], "done").await;
    claim(&a, &ids[0], Some("mine"), json!({})).await;

    let recall = |session: &'static str| {
        let a = &a;
        async move {
            let r = a
                .tok_h(
                    &a.token,
                    Method::POST,
                    RECALL,
                    Some(json!({"event": "session_start", "agent_block_version": 1})),
                    &sess(session),
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()
        }
    };
    let j = recall("mine").await;
    let text = j["text"].as_str().unwrap();
    let pos = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("нет «{needle}» в\n{text}"))
    };
    // после уведомлений, перед правилами
    assert!(pos("устарел") < pos("### Задачи"), "{text}");
    assert!(pos("### Задачи") < pos("### Правила"), "{text}");
    // строки
    assert!(
        text.contains(&format!(
            "ZeroAIMemory: [in_progress] Моя задача — далее: дописать тест — k:{}",
            ids[0]
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "ZeroAIMemory: [blocked] Застряла — далее: ждать — k:{} — блокер: ждём API",
            ids[2]
        )),
        "{text}"
    );
    // захваченная сессией — первой, затем недавно менявшиеся; open и done не показаны
    assert!(pos("Моя задача") < pos("Застряла"), "{text}");
    assert!(pos("Застряла") < pos("Чужая задача"), "{text}");
    assert!(
        !text.contains("Открытая") && !text.contains("Готовая"),
        "{text}"
    );
    let items = j["items"].as_array().unwrap();
    let task_items: Vec<&Value> = items.iter().filter(|i| i["reason"] == "task").collect();
    assert_eq!(task_items.len(), 3, "{j}");
    assert_eq!(task_items[0]["id"], format!("k:{}", ids[0]));
    assert_eq!(task_items[0]["kind"], "task");

    // другая сессия: порядок только по давности правки
    let j = recall("other").await;
    let text = j["text"].as_str().unwrap();
    let p = |n: &str| text.find(n).unwrap();
    assert!(
        p("Застряла") < p("Чужая задача") && p("Чужая задача") < p("Моя задача"),
        "{text}"
    );

    // не больше 5; захваченная — всё равно первой
    for i in 0..4 {
        let t = id(&new_task(&a, &format!("Ещё {i}"), json!({})).await);
        status(&a, &t, "in_progress").await;
    }
    let j = recall("mine").await;
    let text = j["text"].as_str().unwrap();
    assert_eq!(
        text.matches("ZeroAIMemory: [in_progress]").count()
            + text.matches("ZeroAIMemory: [blocked]").count(),
        5,
        "{text}"
    );
    assert!(
        text.find("Моя задача").unwrap() < text.find("Ещё 3").unwrap(),
        "{text}"
    );
}

#[tokio::test]
async fn session_reports_claimed_tasks() {
    let a = api("tasks-session", |_| {}).await;
    let t1 = id(&new_task(&a, "Одна", json!({})).await);
    let t2 = id(&new_task(&a, "Две", json!({})).await);
    status(&a, &t1, "in_progress").await;
    claim(&a, &t1, Some("cs"), json!({})).await;
    claim(&a, &t2, Some("cs"), json!({})).await;
    let r = a.t(Method::GET, "/api/v1/sessions/cs", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let ct = j["claimed_tasks"].as_array().unwrap();
    assert_eq!(ct.len(), 2, "{j}");
    let one = ct.iter().find(|t| t["id"] == t1.as_str()).unwrap();
    assert_eq!(one["title"], "Одна");
    assert_eq!(one["status"], "in_progress");
    assert_eq!(
        ct.iter().find(|t| t["id"] == t2.as_str()).unwrap()["status"],
        "open"
    );
    assert_eq!(j["memory_writes"], 2, "захват — запись в память");
    // закрытая задача и снятый захват из списка уходят; чужая сессия — пусто
    status(&a, &t1, "done").await;
    release(&a, &t2, Some("cs")).await;
    let j = a.t(Method::GET, "/api/v1/sessions/cs", None).await.json();
    assert_eq!(j["claimed_tasks"], json!([]));
    // сессия без захватов
    a.tok_h(
        &a.token,
        Method::POST,
        "/api/v1/search",
        Some(json!({"query": "ничего"})),
        &sess("empty"),
    )
    .await;
    let j = a
        .t(Method::GET, "/api/v1/sessions/empty", None)
        .await
        .json();
    assert_eq!(j["claimed_tasks"], json!([]));
}

/// `claimed_tasks` участника — только пространство решения сессии (финальное ревью M4.5, M-1):
/// задача Knowledge владельца, захваченная сессией с тем же id, участнику не видна; задача
/// решения — видна, пока доступ действует.
#[tokio::test]
async fn session_claimed_tasks_scoped_to_solution() {
    const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e33";
    let a = api("tasks-session-scope", |_| {}).await;
    let (_, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let reg = json!({"agent_solution_id": G, "name": "Общее"});
    let r = a
        .tok(&a_tok, Method::POST, "/api/v1/solutions", Some(reg.clone()))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = a
        .tok(&b_tok, Method::POST, "/api/v1/solutions", Some(reg))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let mut ids = Vec::new();
    for (space, title) in [(G, "Задача решения"), ("knowledge", "Личная задача Анны")]
    {
        let r = a
            .tok(
                &a_tok,
                Method::POST,
                &format!("/api/v1/spaces/{space}/entries"),
                Some(json!({"kind": "task", "title": title, "body": "план"})),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        let tid = id(&r.json());
        let r = a
            .tok_h(
                &a_tok,
                Method::POST,
                &format!("/api/v1/entries/{tid}/claim"),
                Some(json!({})),
                &sess("same"),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        ids.push(tid);
    }
    // сессия участника с тем же id и решением G
    let h = [("x-zam-session", "same"), ("x-zam-solution", G)];
    let r = a
        .tok_h(
            &b_tok,
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "ничего"})),
            &h,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = a
        .tok(&b_tok, Method::GET, "/api/v1/sessions/same", None)
        .await
        .json();
    let got: Vec<&str> = j["claimed_tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(got, [ids[0].as_str()], "{j}");
    // у владельца — обе
    let j = a
        .tok(&a_tok, Method::GET, "/api/v1/sessions/same", None)
        .await
        .json();
    assert_eq!(j["claimed_tasks"].as_array().unwrap().len(), 2, "{j}");
}

/// `next_step` и `blocked_by` в дайджесте и пакете восстановления маскируются (финальное ревью
/// M4.5, M-2): ключ из команды в шаге не уходит агенту.
#[tokio::test]
async fn digest_masks_task_texts() {
    let a = api("tasks-mask", roomy).await;
    // «ключ» собирается из частей: в исходниках нет строки, похожей на настоящий
    let key = format!("gh{}_{}", "p", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1eF3hJ5");
    let t1 = id(&new_task(
        &a,
        "Деплой",
        json!({"next_step": format!("запустить GITHUB_TOKEN={key} ./deploy.sh")}),
    )
    .await);
    status(&a, &t1, "in_progress").await;
    let t2 = id(&new_task(&a, "Выпуск", json!({})).await);
    let r = patch(
        &a,
        &t2,
        json!({"status": "blocked", "blocked_by": format!("нет доступа: token={key}")}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    for event in ["session_start", "recovery"] {
        let r = a
            .tok_h(
                &a.token,
                Method::POST,
                RECALL,
                Some(json!({"event": event})),
                &sess("mask"),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let text = r.json()["text"].as_str().unwrap().to_string();
        assert!(
            text.contains(&format!("k:{t1}")) && text.contains(&format!("k:{t2}")),
            "{text}"
        );
        assert!(!text.contains(&key), "{event}: {text}");
        assert!(text.contains("‹secret›"), "{event}: {text}");
    }
}

fn mcp_text(r: &Value) -> String {
    r["content"][0]["text"].as_str().unwrap_or("").to_string()
}

async fn mcp(a: &Api, name: &str, args: Value) -> (String, bool) {
    let b = axum::http::Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::HOST, "memory.lan:7700")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {}", a.token))
        .header("x-zam-session", "mcp-task");
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": name, "arguments": args}});
    let r = a.send(b.body(Body::from(body.to_string())).unwrap()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    (mcp_text(&j["result"]), j["result"]["isError"] == true)
}

#[tokio::test]
async fn mcp_update_entry_status_claim() {
    let a = api("tasks-mcp", |_| {}).await;
    let (t, err) = mcp(
        &a,
        "memory_log",
        json!({"kind": "task", "title": "Через MCP", "body": "план",
               "data": {"next_step": "начать"}, "space": "knowledge"}),
    )
    .await;
    assert!(!err, "{t}");
    assert!(
        t.contains("k:") && t.contains("task «Через MCP»") && t.contains("open"),
        "{t}"
    );
    let list = a
        .t(Method::GET, &format!("{KN}?kind=task"), None)
        .await
        .json();
    let tid = id(&list["items"][0]);
    let k = format!("k:{tid}");

    let (t, err) = mcp(
        &a,
        "memory_update_entry",
        json!({"id": k, "action": "claim", "ttl_min": 45}),
    )
    .await;
    assert!(!err, "{t}");
    let cur = get(&a, &tid).await;
    assert_eq!(cur["data"]["claimed_by"]["session"], "mcp-task");
    // status: нужна ревизия; next_step и blocked_by
    let (t, err) = mcp(
        &a,
        "memory_update_entry",
        json!({"id": k, "action": "status", "status": "in_progress"}),
    )
    .await;
    assert!(err, "без rev должна быть ошибка: {t}");
    let (t, err) = mcp(
        &a,
        "memory_update_entry",
        json!({"id": k, "action": "status", "rev": cur["rev"], "status": "blocked",
               "next_step": "ждать", "blocked_by": "ревью"}),
    )
    .await;
    assert!(!err, "{t}");
    assert!(t.contains("blocked"), "{t}");
    let cur = get(&a, &tid).await;
    assert_eq!(cur["status"], "blocked");
    assert_eq!(cur["data"]["next_step"], "ждать");
    assert_eq!(cur["data"]["blocked_by"], "ревью");
    assert_eq!(
        cur["data"]["claimed_by"]["session"], "mcp-task",
        "статус захват не снимает"
    );
    // null снимает поле
    let (t, err) = mcp(
        &a,
        "memory_update_entry",
        json!({"id": k, "action": "status", "rev": cur["rev"], "status": "in_progress",
               "next_step": null}),
    )
    .await;
    assert!(!err, "{t}");
    assert!(get(&a, &tid).await["data"].get("next_step").is_none());
    // чужой захват — isError с типом проблемы
    let other = a
        .tok_h(
            &a.token,
            Method::POST,
            &format!("/api/v1/entries/{tid}/claim"),
            Some(json!({})),
            &sess("someone-else"),
        )
        .await;
    assert_eq!(other.status, StatusCode::CONFLICT);
    let (t, err) = mcp(
        &a,
        "memory_update_entry",
        json!({"id": k, "action": "release"}),
    )
    .await;
    assert!(!err, "{t}");
    assert!(get(&a, &tid).await["data"].get("claimed_by").is_none());
    let (t, err) = mcp(
        &a,
        "memory_update_entry",
        json!({"id": k, "action": "bogus"}),
    )
    .await;
    assert!(err && t.contains("claim"), "{t}");
}

/// Захват — оперативное состояние: в снимки ревизий (`GET …/revisions`) не попадает, а правка,
/// отличающаяся только захватом, ревизии не создаёт.
#[tokio::test]
async fn claimed_by_not_in_revision_snapshots() {
    let a = api("tasks-claim-revisions", |_| {}).await;
    let tid = id(&new_task(&a, "Снимки", json!({"next_step": "шаг"})).await);
    let r = claim(&a, &tid, Some("s1"), json!({"ttl_min": 30})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // смена статуса при действующем захвате: захват в записи остаётся, в снимке его нет
    let j = status(&a, &tid, "in_progress").await;
    assert_eq!(j["data"]["claimed_by"]["session"], "s1", "{j}");
    let j = patch(&a, &tid, json!({"next_step": "новый шаг"}))
        .await
        .json();
    assert_eq!(j["data"]["claimed_by"]["session"], "s1");
    let revs = a
        .t(
            Method::GET,
            &format!("/api/v1/entries/{tid}/revisions"),
            None,
        )
        .await
        .json();
    let items = revs["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "{revs}");
    for r in items {
        assert!(r["data"].get("claimed_by").is_none(), "{r}");
    }
    assert_eq!(items[2]["data"]["next_step"], "новый шаг");
    // повторная правка без изменений содержимого — без новой ревизии
    let j = patch(&a, &tid, json!({"next_step": "новый шаг"}))
        .await
        .json();
    assert_eq!(j["rev"], 3);
    // done → done не сдвигает done_at
    let d1 = status(&a, &tid, "done").await;
    let done = d1["data"]["done_at"].clone();
    let d2 = status(&a, &tid, "done").await;
    assert_eq!(d2["data"]["done_at"], done);
    assert_eq!(d2["rev"], d1["rev"], "done → done без изменений");
}
