//! Журнал задач (M4.5 §3, задача 4): статусы и время, переоткрытие с причиной, захват
//! (свободна, истёк, та же сессия, 409, гонка), снятие (чужой — 403, человек — можно), аудит,
//! раздел `### Задачи` дайджеста, `claimed_tasks` сессии, MCP `status|claim|release`; подзадачи
//! `part_of` (M7 §5.4, задача 5 M7.2): сводка, `parent`, `stale`, фильтры ленты и счётчиков.
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

/// Ревизии записи (`GET …/revisions`, `items`), от первой к последней.
async fn revisions(a: &Api, id: &str) -> Vec<Value> {
    let r = a
        .t(
            Method::GET,
            &format!("/api/v1/entries/{id}/revisions"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["items"].as_array().unwrap().clone()
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
    assert_eq!(
        revisions(&a, &tid).await.last().unwrap()["reason"],
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
    assert_eq!(revisions(&a, &tid).await.len(), 1, "ревизии нет");

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
    // захваченная сессией — первой, затем недавно менявшиеся; open и done строками задач не
    // показаны — open только в строке «Открыто задач» (M6.6 §4, задача 3)
    assert!(pos("Моя задача") < pos("Застряла"), "{text}");
    assert!(pos("Застряла") < pos("Чужая задача"), "{text}");
    assert!(
        !text.contains("] Открытая") && !text.contains("Готовая"),
        "{text}"
    );
    assert!(text.contains("Открыто задач: 1 — «Открытая»"), "{text}");
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
/// M4.5, M-2): ключ из команды в шаге не уходит агенту. Заголовок задачи — тоже (финальное
/// ревью M6.6, Minor 3, как в строке «Открыто задач»).
#[tokio::test]
async fn digest_masks_task_texts() {
    let a = api("tasks-mask", roomy).await;
    // «ключ» собирается из частей: в исходниках нет строки, похожей на настоящий
    let key = format!("gh{}_{}", "p", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1eF3hJ5");
    let aws = format!("{}{}", "AK", "IAABCDEFGHIJKLMNOP");
    let t3 = id(&new_task(&a, &format!("Ротация {aws}"), json!({})).await);
    status(&a, &t3, "in_progress").await;
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
        // заголовок с ключом — маска в строке задачи
        let line = format!("ZeroAIMemory: [in_progress] Ротация ‹secret› — k:{t3}");
        assert!(text.lines().any(|l| l == line), "{event}: {text}");
        assert!(!text.contains(&aws[..8]), "{event}: {text}");
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

/// Первая строка ответа MCP (без `~N tok`).
fn first(t: &str) -> String {
    t.lines().next().unwrap_or("").to_string()
}

/// Текст 422 статуса задачи при создании (M6.6 §8, замечание 33, решение координатора 19).
const STATUS_ON_CREATE: &str =
    "data.status: статус задаётся правкой — memory_update_entry action=status (zam task status)";

/// Задача с `data.status` при создании — 422 с подсказкой, как задать статус (REST и MCP —
/// один текст); задача не создаётся. Без `data.status` — создаётся `open`, как прежде.
#[tokio::test]
async fn task_create_status_422_names_action() {
    let a = api("tasks-create-status", |_| {}).await;
    for status in [json!("in_progress"), json!("open"), Value::Null] {
        let r = a
            .t(
                Method::POST,
                KN,
                Some(json!({"kind": "task", "title": "Со статусом",
                            "data": {"status": status, "next_step": "начать"}})),
            )
            .await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
        assert_eq!(r.json()["detail"], STATUS_ON_CREATE, "{status}");
    }
    let (t, err) = mcp(
        &a,
        "memory_log",
        json!({"kind": "task", "title": "Через MCP со статусом", "space": "knowledge",
               "data": {"status": "in_progress"}}),
    )
    .await;
    assert!(err, "{t}");
    assert!(t.contains(STATUS_ON_CREATE), "{t}");
    let list = a
        .t(Method::GET, &format!("{KN}?kind=task"), None)
        .await
        .json();
    assert_eq!(list["items"].as_array().unwrap().len(), 0, "{list}");
    assert_eq!(
        new_task(&a, "Без статуса", json!({})).await["status"],
        "open"
    );
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
    // M6.6 §8 (замечание 19): срок захвата — время агента (M6 §4.4), сессия, ревизия, статус
    let until = |e: &Value| {
        zam_core::time::agent_ts(e["data"]["claimed_by"]["until"].as_str().unwrap()).unwrap()
    };
    assert_eq!(
        first(&t),
        format!(
            "Захвачена {k} до {} (сессия mcp-task) · rev1 · open",
            until(&cur)
        )
    );
    // продление той же сессией с агентом — агент в скобках
    let (t, err) = mcp(
        &a,
        "memory_update_entry",
        json!({"id": k, "action": "claim", "ttl_min": 45, "agent": "claude"}),
    )
    .await;
    assert!(!err, "{t}");
    let cur = get(&a, &tid).await;
    assert_eq!(
        first(&t),
        format!(
            "Захвачена {k} до {} (сессия mcp-task, агент claude) · rev1 · open",
            until(&cur)
        )
    );
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
    let cur = get(&a, &tid).await;
    assert!(cur["data"].get("claimed_by").is_none());
    assert_eq!(
        first(&t),
        format!("Захват снят {k} · rev{} · in_progress", cur["rev"])
    );
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
    let items = revisions(&a, &tid).await;
    assert_eq!(items.len(), 3, "{items:?}");
    for r in &items {
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

/// `data` ревизий задачи, от первой к последней.
async fn revision_data(a: &Api, id: &str) -> Vec<Value> {
    revisions(a, id)
        .await
        .into_iter()
        .map(|x| x["data"].clone())
        .collect()
}

/// M6.6, замечание 35: смена статуса на `done` без `next_step` снимает «следующий шаг» (и
/// блокер) — у выполненной задачи в лентах нет «далее: …»; прежнее значение — в истории.
#[tokio::test]
async fn task_done_clears_next_step() {
    let a = api("tasks-done-next", |_| {}).await;
    let tid = id(&new_task(&a, "Ревью", json!({"next_step": "повторное ревью"})).await);
    let j = patch(
        &a,
        &tid,
        json!({"status": "blocked", "blocked_by": "ждём ревьюера"}),
    )
    .await
    .json();
    assert_eq!(j["data"]["next_step"], "повторное ревью", "{j}");
    let j = status(&a, &tid, "done").await;
    assert_eq!(j["status"], "done");
    assert!(j["data"].get("next_step").is_none(), "{j}");
    assert!(j["data"].get("blocked_by").is_none(), "{j}");
    assert!(j["data"].get("done_at").is_some(), "{j}");
    let cur = get(&a, &tid).await;
    assert!(cur["data"].get("next_step").is_none(), "{cur}");
    // лента выполненных — без шага
    let items = a
        .t(Method::GET, &format!("{KN}?kind=task&status=done"), None)
        .await
        .json()["items"]
        .clone();
    assert!(items[0]["data"].get("next_step").is_none(), "{items}");
    // история хранит прежний шаг и блокер
    let revs = revision_data(&a, &tid).await;
    assert_eq!(revs.len(), 3, "{revs:?}");
    assert_eq!(revs[1]["next_step"], "повторное ревью", "{revs:?}");
    assert_eq!(revs[1]["blocked_by"], "ждём ревьюера", "{revs:?}");
    assert!(revs[2].get("next_step").is_none(), "{revs:?}");

    // блокер, заданный задаче не в `blocked`, закрытие тоже снимает
    let t2 = id(&new_task(
        &a,
        "Без блока",
        json!({"next_step": "шаг", "blocked_by": "x"}),
    )
    .await);
    let j = status(&a, &t2, "done").await;
    assert!(j["data"].get("next_step").is_none(), "{j}");
    assert!(j["data"].get("blocked_by").is_none(), "{j}");
    // переоткрытие шаг не возвращает
    let j = patch(
        &a,
        &t2,
        json!({"status": "open", "reason": "тесты красные"}),
    )
    .await
    .json();
    assert_eq!(j["status"], "open", "{j}");
    assert!(j["data"].get("next_step").is_none(), "{j}");
}

/// Явный `next_step` (и `blocked_by`) той же правки сильнее: остаётся у выполненной задачи;
/// повтор `done → done` — не переход, заданный после закрытия шаг не снимает.
#[tokio::test]
async fn task_done_with_explicit_next_keeps_it() {
    let a = api("tasks-done-explicit", |_| {}).await;
    let tid = id(&new_task(&a, "Выпуск", json!({"next_step": "собрать"})).await);
    status(&a, &tid, "in_progress").await;
    let r = patch(
        &a,
        &tid,
        json!({"status": "done", "next_step": "передать в выпуск"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["data"]["next_step"], "передать в выпуск", "{j}");
    // то же полем `data`
    let t2 = id(&new_task(&a, "Через data", json!({"next_step": "старый"})).await);
    let r = patch(
        &a,
        &t2,
        json!({"status": "dropped", "data": {"next_step": "новый", "blocked_by": "решение"}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["data"]["next_step"], "новый", "{j}");
    assert_eq!(j["data"]["blocked_by"], "решение", "{j}");
    // повтор done без next_step: шаг остаётся, ревизии нет
    let before = get(&a, &tid).await;
    let j = status(&a, &tid, "done").await;
    assert_eq!(j["data"]["next_step"], "передать в выпуск", "{j}");
    assert_eq!(j["rev"], before["rev"], "{j}");
    // шаг задачи, закрытой раньше, снимает пустая строка (`zam task status <id> done --next ""`)
    let r = patch(&a, &tid, json!({"status": "done", "next_step": ""})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert!(j["data"].get("next_step").is_none(), "{j}");
    assert_eq!(j["status"], "done", "{j}");
}

/// `dropped` снимает шаг так же, как `done`.
#[tokio::test]
async fn task_dropped_clears_next_step() {
    let a = api("tasks-dropped-next", |_| {}).await;
    let tid = id(&new_task(&a, "Снята", json!({"next_step": "сделать"})).await);
    status(&a, &tid, "in_progress").await;
    let r = patch(&a, &tid, json!({"status": "dropped", "reason": "не нужно"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["status"], "dropped");
    assert!(j["data"].get("next_step").is_none(), "{j}");
    let revs = revision_data(&a, &tid).await;
    assert_eq!(revs[revs.len() - 2]["next_step"], "сделать", "{revs:?}");
}

/// MCP `memory_update_entry action=status` идёт тем же `PATCH`: `done` снимает шаг, явный
/// `next_step` вызова остаётся.
#[tokio::test]
async fn mcp_status_done_clears_next_step() {
    let a = api("tasks-mcp-done-next", |_| {}).await;
    let tid = id(&new_task(&a, "Через MCP", json!({"next_step": "ревью"})).await);
    let k = format!("k:{tid}");
    let cur = get(&a, &tid).await;
    let (t, err) = mcp(
        &a,
        "memory_update_entry",
        json!({"id": k, "action": "status", "rev": cur["rev"], "status": "done"}),
    )
    .await;
    assert!(!err, "{t}");
    assert!(get(&a, &tid).await["data"].get("next_step").is_none());
    let t2 = id(&new_task(&a, "С шагом", json!({"next_step": "ревью"})).await);
    let cur = get(&a, &t2).await;
    let (t, err) = mcp(
        &a,
        "memory_update_entry",
        json!({"id": format!("k:{t2}"), "action": "status", "rev": cur["rev"],
               "status": "done", "next_step": "сообщить владельцу"}),
    )
    .await;
    assert!(!err, "{t}");
    assert_eq!(
        get(&a, &t2).await["data"]["next_step"],
        "сообщить владельцу"
    );
}

// ---------- Подзадачи `part_of` (M7 §5.4, задача 5 M7.2) ----------

/// Связь `part_of` шага с общей задачей (`POST /edges`); порядок вызовов — порядок шагов.
async fn part_of(a: &Api, step: &str, parent: &str) {
    let r = a
        .t(
            Method::POST,
            "/api/v1/edges",
            Some(json!({"src": format!("e:{step}"), "type": "part_of",
                        "dst": format!("e:{parent}")})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

/// Лента задач пространства `space` (`knowledge` или GUID) с параметрами `q`: ответ целиком.
async fn feed_in(a: &Api, space: &str, q: &str) -> Value {
    let r = a
        .t(
            Method::GET,
            &format!("/api/v1/spaces/{space}/entries?kind=task{q}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{q}: {}", r.text());
    r.json()
}

async fn feed(a: &Api, q: &str) -> Value {
    feed_in(a, "knowledge", q).await
}

fn ids_of(v: &Value) -> Vec<String> {
    v["items"].as_array().unwrap().iter().map(id).collect()
}

/// Запись `id` из ленты `v`.
fn item<'a>(v: &'a Value, id: &str) -> &'a Value {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == id)
        .unwrap_or_else(|| panic!("нет {id} в ленте: {v}"))
}

/// Общая задача «Этап» и 13 шагов «Шаг i»: связи `part_of` — в порядке `(5·i) mod 13`, не в
/// порядке создания задач (и не в обратном); статусы по порядку шагов — 8 `done`, 1 `dropped`,
/// 2 `in_progress`, 2 `open`. Возвращает общую и шаги в порядке рёбер.
async fn umbrella_13(a: &Api) -> (String, Vec<String>) {
    let p = id(&new_task(a, "Этап", json!({})).await);
    let mut steps = Vec::new();
    for i in 0..13 {
        steps.push(id(&new_task(a, &format!("Шаг {i}"), json!({})).await));
    }
    let order: Vec<String> = (0..13).map(|i| steps[(5 * i) % 13].clone()).collect();
    for s in &order {
        part_of(a, s, &p).await;
    }
    for (k, s) in order.iter().enumerate() {
        let st = match k {
            0..=7 => "done",
            8 => "dropped",
            9 | 10 => "in_progress",
            _ => continue,
        };
        status(a, s, st).await;
    }
    (p, order)
}

/// Сводка `subtasks` у общей: всего и по статусам; строки `items` — только в `GET /entries/{id}`
/// в порядке рёбер (не создания задач), в ленте и эхо правки — без них; MCP `memory_read` общей
/// в Markdown — без строк о подзадачах (решение 31: Markdown-выдача агенту в волне не меняется).
#[tokio::test]
async fn subtasks_summary_and_items_order() {
    let a = api("tasks-subtasks-summary", |_| {}).await;
    let (p, order) = umbrella_13(&a).await;
    let j = get(&a, &p).await;
    let s = &j["subtasks"];
    for (k, n) in [
        ("total", 13),
        ("done", 8),
        ("dropped", 1),
        ("open", 2),
        ("in_progress", 2),
        ("blocked", 0),
    ] {
        assert_eq!(s[k], n, "{k}: {j}");
    }
    let items = s["items"].as_array().unwrap_or_else(|| panic!("{j}"));
    let got: Vec<String> = items.iter().map(id).collect();
    assert_eq!(got, order, "порядок рёбер: {j}");
    assert_eq!(s["truncated"], false, "{j}");
    assert_eq!(items[0]["status"], "done");
    assert_eq!(items[8]["status"], "dropped");
    assert_eq!(items[12]["status"], "open");
    let first = get(&a, &order[0]).await;
    assert_eq!(items[0]["title"], first["title"]);
    assert_eq!(items[0]["claimed"], Value::Null, "{j}");
    assert_eq!(ts_of(&items[0], "updated_at"), ts_of(&first, "updated_at"));
    assert!(j["parent"].is_null(), "у общей родителя нет: {j}");
    assert!(j.get("stale").is_none(), "{j}");
    // захват шага — в строке: агент и срок
    let r = claim(
        &a,
        &order[11],
        Some("cs-sub"),
        json!({"agent": "claude", "ttl_min": 60}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = get(&a, &p).await;
    let c = &j["subtasks"]["items"][11]["claimed"];
    assert_eq!(c["agent"], "claude", "{j}");
    assert!(ts_of(c, "until") > Timestamp::now(), "{j}");
    // лента: сводка без строк
    let f = feed(&a, "").await;
    let s = &item(&f, &p)["subtasks"];
    assert_eq!(s["total"], 13, "{f}");
    assert!(
        s.get("items").is_none() && s.get("truncated").is_none(),
        "{f}"
    );
    // эхо правки — тоже без строк
    let e = status(&a, &p, "in_progress").await;
    assert_eq!(e["subtasks"]["done"], 8, "{e}");
    assert!(e["subtasks"].get("items").is_none(), "{e}");
    // MCP memory_read в Markdown — прежний: ни сводки, ни заголовков шагов
    let (t, err) = mcp(
        &a,
        "memory_read",
        json!({"items": [{"id": format!("k:{p}")}]}),
    )
    .await;
    assert!(!err, "{t}");
    assert!(t.contains("План: Этап"), "{t}");
    for bad in ["Шаг ", "subtasks", "подзадач", "8/12", "last_activity"] {
        assert!(!t.contains(bad), "{bad}: {t}");
    }
}

/// У подзадачи — `parent {id, title, status, subtasks {total, done, dropped}}` (сводка родителя
/// для «(8/13)» без второго запроса); `top_level=1` — без подзадач, порядок прежний (новые
/// первыми); `parent=<id>` — ровно прямые подзадачи в порядке рёбер, курсор — позиция, с
/// фильтрами ленты; `top_level` и `parent` — только у `kind=task` (422), вместе — 422.
#[tokio::test]
async fn subtask_parent_and_top_level_filter() {
    let a = api("tasks-subtasks-parent", |_| {}).await;
    let (p, order) = umbrella_13(&a).await;
    let other = id(&new_task(&a, "Отдельная", json!({})).await);
    let j = get(&a, &order[3]).await;
    assert_eq!(
        j["parent"],
        json!({"id": p, "title": "Этап", "status": "open",
               "subtasks": {"total": 13, "done": 8, "dropped": 1}}),
        "{j}"
    );
    assert!(j.get("subtasks").is_none(), "у шага подзадач нет: {j}");
    assert!(j["last_activity_at"].as_str().is_some(), "{j}");
    // в ленте у шага — тот же parent
    let f = feed(&a, "").await;
    assert_eq!(item(&f, &order[3])["parent"], j["parent"], "{f}");
    assert!(item(&f, &other)["parent"].is_null(), "{f}");
    // верхний уровень: без шагов, новые первыми
    let top = feed(&a, "&top_level=1").await;
    assert_eq!(ids_of(&top), [other.clone(), p.clone()], "{top}");
    let top = feed(&a, "&top_level=1&status=open").await;
    assert_eq!(ids_of(&top), [other.clone(), p.clone()], "{top}");
    // прямые подзадачи — в порядке рёбер
    let kids = feed(&a, &format!("&parent={p}")).await;
    assert_eq!(ids_of(&kids), order, "{kids}");
    assert!(kids["next_cursor"].is_null(), "{kids}");
    // курсор — позиция: 5 + 5 + 3
    let mut got = Vec::new();
    let mut cursor: Option<String> = None;
    for want in [5, 5, 3] {
        let q = match &cursor {
            Some(c) => format!("&parent={p}&limit=5&cursor={c}"),
            None => format!("&parent={p}&limit=5"),
        };
        let page = feed(&a, &q).await;
        assert_eq!(ids_of(&page).len(), want, "{page}");
        got.extend(ids_of(&page));
        cursor = page["next_cursor"].as_str().map(str::to_string);
    }
    assert!(cursor.is_none());
    assert_eq!(got, order);
    // фильтр статуса — в том же порядке
    let done = feed(&a, &format!("&parent={p}&status=done")).await;
    assert_eq!(ids_of(&done), order[..8], "{done}");
    // у отдельной задачи подзадач нет; ссылка k: тоже принимается
    assert!(ids_of(&feed(&a, &format!("&parent={other}")).await).is_empty());
    assert_eq!(ids_of(&feed(&a, &format!("&parent=k:{p}")).await), order);
    // не своего вида и вместе — 422; неверный id, флаг и курсор — 400
    for (q, code) in [
        (
            "?kind=idea&top_level=1".to_string(),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            format!("?kind=idea&parent={p}"),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        ("?top_level=1".to_string(), StatusCode::UNPROCESSABLE_ENTITY),
        (
            format!("?kind=task&top_level=1&parent={p}"),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        ("?kind=task&parent=xyz".to_string(), StatusCode::BAD_REQUEST),
        (
            "?kind=task&top_level=2".to_string(),
            StatusCode::BAD_REQUEST,
        ),
        (
            format!("?kind=task&parent={p}&cursor=x"),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let r = a.t(Method::GET, &format!("{KN}{q}"), None).await;
        assert_eq!(r.status, code, "{q}: {}", r.text());
    }
}

/// Цикл `part_of` любой длины (2, 3, 10 задач): все задачи цикла — верхнего уровня, ни у одной
/// нет `parent` и подзадач из цикла; ребро задачи цикла к общей вне цикла тоже не учитывается
/// (решение 21); задача вне цикла (`D part_of A`) — подзадача задачи цикла.
#[tokio::test]
async fn part_of_cycle_keeps_top_level() {
    let a = api("tasks-subtasks-cycle", |_| {}).await;
    let mut cycles: Vec<Vec<String>> = Vec::new();
    for n in [2, 3, 10] {
        let mut c = Vec::new();
        for i in 0..n {
            c.push(id(&new_task(&a, &format!("Цикл {n}.{i}"), json!({})).await));
        }
        for (i, t) in c.iter().enumerate() {
            part_of(&a, t, &c[(i + 1) % n]).await;
        }
        cycles.push(c);
    }
    let x = id(&new_task(&a, "Общая вне цикла", json!({})).await);
    let y = id(&new_task(&a, "Шаг общей", json!({})).await);
    part_of(&a, &y, &x).await;
    part_of(&a, &cycles[1][0], &x).await;
    let d = id(&new_task(&a, "D", json!({})).await);
    let a0 = cycles[0][0].clone();
    part_of(&a, &d, &a0).await;
    let top = ids_of(&feed(&a, "&top_level=1").await);
    for c in &cycles {
        for t in c {
            assert!(top.contains(t), "задача цикла на верхнем уровне: {t}");
            let j = get(&a, t).await;
            assert!(j["parent"].is_null(), "{j}");
            if *t != a0 {
                assert!(j.get("subtasks").is_none(), "{j}");
            }
        }
    }
    assert!(top.contains(&x) && !top.contains(&y) && !top.contains(&d));
    assert_eq!(top.len(), 2 + 3 + 10 + 1, "{top:?}");
    // у A — только D
    let j = get(&a, &a0).await;
    assert_eq!(j["subtasks"]["total"], 1, "{j}");
    assert_eq!(j["subtasks"]["items"][0]["id"], d.as_str(), "{j}");
    assert_eq!(get(&a, &d).await["parent"]["id"], a0.as_str());
    assert_eq!(ids_of(&feed(&a, &format!("&parent={a0}")).await), [d]);
    // у общей вне цикла — только её шаг, задача цикла не в счёт
    let j = get(&a, &x).await;
    assert_eq!(j["subtasks"]["total"], 1, "{j}");
    assert_eq!(ids_of(&feed(&a, &format!("&parent={x}")).await), [y]);
}

/// Подзадача в другом пространстве (Knowledge → общая в решении): в сводку не входит, у неё
/// нет `parent`, в Knowledge она — верхнего уровня.
#[tokio::test]
async fn subtasks_other_space_not_counted() {
    const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e35";
    let a = api("tasks-subtasks-space", |_| {}).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Подзадачи"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let mut sol = Vec::new();
    for title in ["Общая решения", "Шаг решения"] {
        let r = a
            .t(
                Method::POST,
                &format!("/api/v1/spaces/{G}/entries"),
                Some(json!({"kind": "task", "title": title, "body": "план"})),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        sol.push(id(&r.json()));
    }
    let (u, s1) = (sol[0].clone(), sol[1].clone());
    let s2 = id(&new_task(&a, "Шаг в Knowledge", json!({})).await);
    part_of(&a, &s1, &u).await;
    part_of(&a, &s2, &u).await;
    let j = get(&a, &u).await;
    assert_eq!(j["subtasks"]["total"], 1, "{j}");
    assert_eq!(j["subtasks"]["items"][0]["id"], s1.as_str(), "{j}");
    assert!(get(&a, &s2).await["parent"].is_null());
    assert_eq!(get(&a, &s1).await["parent"]["id"], u.as_str());
    assert!(ids_of(&feed(&a, "&top_level=1").await).contains(&s2));
    assert_eq!(ids_of(&feed_in(&a, G, "&top_level=1").await), [u.as_str()]);
    assert_eq!(ids_of(&feed_in(&a, G, &format!("&parent={u}")).await), [s1]);
}

/// `updated_at` записи `id` — `hours` часов назад (прямо в базе: часы `freeze_now` общие для
/// процесса и сдвинули бы сроки захватов соседних тестов). Возвращает записанное время.
async fn age(a: &Api, id: &str, hours: i64) -> Timestamp {
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let ts = zam_store::time::fmt_ts(Timestamp::now() - SignedDuration::from_hours(hours));
    let id = id.to_string();
    let at = ts.clone();
    db.write(move |c| {
        c.execute(
            "UPDATE entries SET updated_at = ?2 WHERE id = ?1",
            [&id, &at],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    ts.parse().unwrap()
}

/// Время в поле `field` значения `v`.
fn ts_of(v: &Value, field: &str) -> Timestamp {
    v[field]
        .as_str()
        .unwrap_or_else(|| panic!("нет {field}: {v}"))
        .parse()
        .unwrap()
}

/// «Давно без обновлений»: `in_progress`, захвата нет, задача и её подзадачи не менялись больше
/// 12 ч — `stale: true` (карточка и лента); `last_activity_at` — позднейшее из `updated_at`
/// задачи и подзадач; правка подзадачи снимает `stale`, `last_activity_at` — её время; захват с
/// истёкшим сроком — не захват.
#[tokio::test]
async fn stale_in_progress_after_12h_without_claim() {
    let a = api("tasks-stale", |_| {}).await;
    let p = id(&new_task(&a, "Давняя", json!({})).await);
    let s = id(&new_task(&a, "Её шаг", json!({})).await);
    part_of(&a, &s, &p).await;
    status(&a, &p, "in_progress").await;
    let fresh = get(&a, &p).await;
    assert!(fresh.get("stale").is_none(), "{fresh}");
    let tp = age(&a, &p, 14).await;
    let ts = age(&a, &s, 13).await;
    let j = get(&a, &p).await;
    assert_eq!(j["stale"], true, "{j}");
    assert_eq!(ts_of(&j, "last_activity_at"), ts.max(tp), "{j}");
    let f = feed(&a, "").await;
    assert_eq!(item(&f, &p)["stale"], true, "{f}");
    assert!(item(&f, &s).get("stale").is_none(), "шаг open: {f}");
    // правка подзадачи — свежая активность общей
    let e = status(&a, &s, "in_progress").await;
    let j = get(&a, &p).await;
    assert!(j.get("stale").is_none(), "{j}");
    assert_eq!(
        ts_of(&j, "last_activity_at"),
        ts_of(&e, "updated_at"),
        "{j}"
    );
    // без подзадач: истёкший захват — как его нет
    let q = id(&new_task(&a, "Одна", json!({})).await);
    status(&a, &q, "in_progress").await;
    let r = claim(&a, &q, Some("cs-old"), json!({"ttl_min": 1})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let qid = q.clone();
    let past = zam_store::time::fmt_ts(Timestamp::now() - SignedDuration::from_hours(1));
    db.write(move |c| {
        c.execute(
            "UPDATE entries SET data = json_set(data, '$.claimed_by.until', ?2) WHERE id = ?1",
            [&qid, &past],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let tq = age(&a, &q, 13).await;
    let j = get(&a, &q).await;
    assert_eq!(j["stale"], true, "{j}");
    assert_eq!(ts_of(&j, "last_activity_at"), tq, "{j}");
}

/// Живой захват снимает «давно без обновлений» при том же возрасте; не `in_progress` — тоже.
#[tokio::test]
async fn stale_not_with_live_claim() {
    let a = api("tasks-stale-claim", |_| {}).await;
    let p = id(&new_task(&a, "Захваченная", json!({})).await);
    status(&a, &p, "in_progress").await;
    let r = claim(&a, &p, Some("cs-live"), json!({"ttl_min": 120})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    age(&a, &p, 14).await;
    let j = get(&a, &p).await;
    assert!(j.get("stale").is_none(), "{j}");
    assert!(j["last_activity_at"].is_string(), "{j}");
    assert!(item(&feed(&a, "").await, &p).get("stale").is_none());
    // тот же возраст без захвата — «давно без обновлений»: дело именно в захвате
    let r = release(&a, &p, Some("cs-live")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(get(&a, &p).await["stale"], true);
    // открытая и заблокированная задача того же возраста — не «давно без обновлений»
    for st in ["open", "blocked"] {
        let t = id(&new_task(&a, &format!("Старая {st}"), json!({})).await);
        if st != "open" {
            status(&a, &t, st).await;
        }
        age(&a, &t, 14).await;
        let j = get(&a, &t).await;
        assert!(j.get("stale").is_none(), "{st}: {j}");
    }
}

/// Счётчики статусов доски — `entries/counts?kind=task&top_level=1` без подзадач,
/// `parent=<id>` — по прямым подзадачам; не у задач — 422.
#[tokio::test]
async fn counts_top_level() {
    let a = api("tasks-subtasks-counts", |_| {}).await;
    let p = id(&new_task(&a, "Общая", json!({})).await);
    let other = id(&new_task(&a, "Другая", json!({})).await);
    status(&a, &other, "in_progress").await;
    let mut steps = Vec::new();
    for i in 0..3 {
        let s = id(&new_task(&a, &format!("Шаг {i}"), json!({})).await);
        part_of(&a, &s, &p).await;
        steps.push(s);
    }
    status(&a, &steps[0], "done").await;
    let counts = |q: String| {
        let a = &a;
        async move {
            let r = a
                .t(
                    Method::GET,
                    &format!("/api/v1/spaces/knowledge/entries/counts{q}"),
                    None,
                )
                .await;
            (r.status, r.json())
        }
    };
    let all = json!({"open": 3, "in_progress": 1, "blocked": 0, "done": 1, "dropped": 0});
    assert_eq!(counts("?kind=task".into()).await, (StatusCode::OK, all));
    let top = json!({"open": 1, "in_progress": 1, "blocked": 0, "done": 0, "dropped": 0});
    assert_eq!(
        counts("?kind=task&top_level=1".into()).await,
        (StatusCode::OK, top)
    );
    let kids = json!({"open": 2, "in_progress": 0, "blocked": 0, "done": 1, "dropped": 0});
    assert_eq!(
        counts(format!("?kind=task&parent={p}")).await,
        (StatusCode::OK, kids)
    );
    for q in ["?kind=idea&top_level=1", "?kind=question&parent=x"] {
        let (code, j) = counts(q.into()).await;
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY, "{q}: {j}");
    }
}
