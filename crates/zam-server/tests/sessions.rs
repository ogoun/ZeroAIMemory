//! Сессии агентов, `session_seen` в поиске и оценки полезности (§6.3, §9.3, §10.5, §12;
//! задача 5 M4). Сервер без моделей (поиск — FTS).
//!
//! Пользователи общего решения: A — владелец, B — участник, C — посторонний.
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};
use zam_core::{Id, TargetKind, TargetRef};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e52";
const SEARCH: &str = "/api/v1/search";

fn sess(id: &str) -> [(&str, &str); 1] {
    [("x-zam-session", id)]
}

async fn session(a: &Api, tok: &str, id: &str) -> Resp {
    a.tok(tok, Method::GET, &format!("/api/v1/sessions/{id}"), None)
        .await
}

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

/// Первый поиск с `X-Zam-Session` создаёт сессию; записи в память с заголовком (узел, запись
/// журнала, связь) увеличивают `memory_writes`, неудачная запись и чтение — нет; явные
/// `POST`/`PATCH`/`GET /sessions`; чужая сессия — 404.
#[tokio::test]
async fn implicit_session_on_header() {
    let a = api("sessions-implicit", |_| {}).await;
    let (_, other) = a.new_user("Пётр").await;
    let h = sess("agent-1");
    assert_eq!(
        session(&a, &a.token, "agent-1").await.status,
        StatusCode::NOT_FOUND
    );
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            SEARCH,
            Some(json!({"query": "ничего"})),
            &[("x-zam-session", "agent-1"), ("x-zam-machine", "box")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let s = session(&a, &a.token, "agent-1").await;
    assert_eq!(s.status, StatusCode::OK, "{}", s.text());
    let j = s.json();
    assert_eq!(j["id"], "agent-1");
    assert_eq!(j["machine"], "box");
    assert_eq!(j["memory_writes"], 0);
    assert!(j["ended_at"].is_null());
    for k in ["agent", "solution", "area", "started_at", "last_seen_at"] {
        assert!(j.get(k).is_some(), "{k}: {j}");
    }

    // записи в память
    let n = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "knowledge", "path": "a", "kind": "note", "content": "# A\n"})),
            &h,
        )
        .await;
    assert_eq!(n.status, StatusCode::CREATED, "{}", n.text());
    let n2 = a.note("knowledge", "b", "# B\n").await;
    let e = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(
                json!({"kind": "insight", "title": "LF в скриптах", "body": "x",
                        "data": {"kind": "gotcha"}, "scope": "global"}),
            ),
            &h,
        )
        .await;
    assert_eq!(e.status, StatusCode::CREATED, "{}", e.text());
    let edge = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/edges",
            Some(json!({"src": format!("n:{}", id(&n.json())),
                        "dst": format!("n:{}", id(&n2)), "type": "relates_to"})),
            &h,
        )
        .await;
    assert_eq!(edge.status, StatusCode::CREATED, "{}", edge.text());
    // неудачная запись и чтение не считаются
    let bad = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "knowledge", "path": "a", "kind": "note", "content": "# A\n"})),
            &h,
        )
        .await;
    assert!(!bad.status.is_success());
    let read = a
        .tok_h(
            &a.token,
            Method::GET,
            &format!("/api/v1/nodes/{}", id(&n2)),
            None,
            &h,
        )
        .await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(
        session(&a, &a.token, "agent-1").await.json()["memory_writes"],
        3
    );
    // запись с новой сессией создаёт её
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "knowledge", "path": "c", "kind": "note", "content": "# C\n"})),
            &sess("agent-2"),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(
        session(&a, &a.token, "agent-2").await.json()["memory_writes"],
        1
    );

    // чужая сессия не видна
    assert_eq!(
        session(&a, &other, "agent-1").await.status,
        StatusCode::NOT_FOUND
    );

    // явное создание: без id — UUIDv7
    let r = a
        .t(
            Method::POST,
            "/api/v1/sessions",
            Some(json!({"agent": "codex", "solution": G, "area": "svc"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let new_id = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(uuid::Uuid::parse_str(&new_id).unwrap().get_version_num(), 7);
    let j = session(&a, &a.token, &new_id).await.json();
    assert_eq!(
        (j["agent"].as_str(), j["solution"].as_str()),
        (Some("codex"), Some(G))
    );
    let r = a
        .t(
            Method::POST,
            "/api/v1/sessions",
            Some(json!({"id": "agent-3"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.json()["id"], "agent-3");
    for bad in [
        json!({"id": "x".repeat(129)}),
        json!({"solution": "не-guid"}),
    ] {
        let r = a.t(Method::POST, "/api/v1/sessions", Some(bad)).await;
        assert!(r.status.is_client_error(), "{}", r.text());
    }

    // конец сессии
    let r = a
        .t(
            Method::PATCH,
            "/api/v1/sessions/agent-1",
            Some(json!({"ended": true})),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    assert!(session(&a, &a.token, "agent-1").await.json()["ended_at"].is_string());
    let r = a
        .tok(
            &other,
            Method::PATCH,
            "/api/v1/sessions/agent-1",
            Some(json!({"ended": true})),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // список своих: фильтр по решению и предел
    let r = a.t(Method::GET, "/api/v1/sessions?limit=10", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["items"].as_array().unwrap().len(), 4);
    let r = a
        .t(Method::GET, &format!("/api/v1/sessions?solution={G}"), None)
        .await;
    let items = r.json()["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], new_id.as_str());
    let r = a.tok(&other, Method::GET, "/api/v1/sessions", None).await;
    assert!(r.json()["items"].as_array().unwrap().is_empty());
}

async fn search_as(a: &Api, session: &str, mode: &str, format: &str) -> Resp {
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            SEARCH,
            Some(json!({"query": "квазар", "mode": mode, "format": format})),
            &sess(session),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r
}

/// Выдача `mode=chunks` отмечает текст выданным сессии; та же ревизия в следующем поиске —
/// `seen: true` и `(seen)` вместо фрагмента; после правки узла — снова фрагмент.
#[tokio::test]
async fn skip_seen_marks_same_rev_only() {
    let a = api("sessions-seen-rev", |_| {}).await;
    let n = a
        .note("knowledge", "Астро", "# Астро\n\nКвазар светит ярко.\n")
        .await;
    // brief не отмечает: выдана только карточка
    let j = search_as(&a, "s", "brief", "json").await.json();
    assert_eq!(j["items"][0]["seen"], false, "{j}");
    let j = search_as(&a, "s", "brief", "json").await.json();
    assert_eq!(j["items"][0]["seen"], false, "{j}");
    // chunks — текст выдан
    let j = search_as(&a, "s", "chunks", "json").await.json();
    assert!(
        j["items"][0]["text"].as_str().unwrap().contains("Квазар"),
        "{j}"
    );
    let j = search_as(&a, "s", "brief", "json").await.json();
    assert_eq!(j["items"][0]["seen"], true, "{j}");
    assert!(j["items"][0].get("text").is_none());
    let md = search_as(&a, "s", "brief", "md").await.text();
    assert!(md.contains("(seen)"), "{md}");
    assert!(!md.contains("Квазар"), "{md}");
    // повтор chunks той же ревизии: текста нет
    let j = search_as(&a, "s", "chunks", "json").await.json();
    assert_eq!(j["items"][0]["seen"], true, "{j}");
    assert!(j["items"][0].get("text").is_none(), "{j}");
    // skip_seen: false — текст снова отдаётся, пометка остаётся
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            SEARCH,
            Some(json!({"query": "квазар", "mode": "chunks", "skip_seen": false})),
            &sess("s"),
        )
        .await;
    let j = r.json();
    assert!(
        j["items"][0]["text"].as_str().unwrap().contains("Квазар"),
        "{j}"
    );
    assert_eq!(j["items"][0]["seen"], false, "{j}");

    // новая ревизия — снова фрагмент
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{}", id(&n)),
            Some(json!({"content": "# Астро\n\nКвазар светит ещё ярче.\n"})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = search_as(&a, "s", "brief", "json").await.json();
    assert_eq!(j["items"][0]["rev"], 2);
    assert_eq!(j["items"][0]["seen"], false, "{j}");
    let md = search_as(&a, "s", "brief", "md").await.text();
    assert!(!md.contains("(seen)") && md.contains("Квазар"), "{md}");
    // `session_id` из тела равносилен заголовку
    let r = a
        .t(
            Method::POST,
            SEARCH,
            Some(json!({"query": "квазар", "mode": "chunks", "session_id": "body-s"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let j = search_as(&a, "body-s", "brief", "json").await.json();
    assert_eq!(j["items"][0]["seen"], true, "{j}");
}

/// Выданное одной сессии не помечается в другой.
#[tokio::test]
async fn seen_isolated_between_sessions() {
    let a = api("sessions-seen-iso", |_| {}).await;
    a.note("knowledge", "Астро", "# Астро\n\nКвазар светит ярко.\n")
        .await;
    search_as(&a, "a", "chunks", "json").await;
    let j = search_as(&a, "a", "brief", "json").await.json();
    assert_eq!(j["items"][0]["seen"], true, "{j}");
    let j = search_as(&a, "b", "brief", "json").await.json();
    assert_eq!(j["items"][0]["seen"], false, "{j}");
    // без сессии — без пометок
    let r = a
        .t(Method::POST, SEARCH, Some(json!({"query": "квазар"})))
        .await;
    assert_eq!(r.json()["items"][0]["seen"], false);
}

struct W {
    a: Api,
    a_id: String,
    a_tok: String,
    b_id: String,
    b_tok: String,
    c_tok: String,
}

/// A регистрирует решение, B запрашивает доступ; одобрение — `approve`.
async fn world(name: &str, approve: bool) -> W {
    let a = api(&format!("sessions-{name}"), |_| {}).await;
    let (a_id, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let (_, c_tok) = a.new_user("Вера").await;
    let reg = json!({ "agent_solution_id": G, "name": "Общее" });
    let r = a
        .tok(&a_tok, Method::POST, "/api/v1/solutions", Some(reg.clone()))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = a
        .tok(&b_tok, Method::POST, "/api/v1/solutions", Some(reg))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    if approve {
        let r = a
            .tok(
                &a_tok,
                Method::POST,
                &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
                Some(json!({"access": "ro"})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    W {
        a,
        a_id,
        a_tok,
        b_id,
        b_tok,
        c_tok,
    }
}

impl W {
    /// Insight владельца в общем решении.
    async fn insight(&self) -> String {
        let r = self
            .a
            .tok(
                &self.a_tok,
                Method::POST,
                &format!("/api/v1/spaces/{G}/entries"),
                Some(json!({"kind": "insight", "title": "LF в bash", "body": "x",
                            "data": {"kind": "gotcha"}})),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        id(&r.json())
    }
    async fn feedback(&self, tok: &str, entry: &str, body: Value) -> Resp {
        self.a
            .tok_h(
                tok,
                Method::POST,
                &format!("/api/v1/entries/{entry}/feedback"),
                Some(body),
                &sess("fb-session"),
            )
            .await
    }
}

/// Участник решения (даже с `ro`) оценивает запись владельца: событие ложится в базу
/// владельца, `GET /entries/{id}` отдаёт счётчики; в базе участника событий нет.
#[tokio::test]
async fn feedback_records_usage_in_owner_db() {
    let w = world("fb-owner", true).await;
    let e = w.insight().await;
    let r = w.feedback(&w.b_tok, &e, json!({"value": "useful"})).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let r = w
        .feedback(
            &w.a_tok,
            &e,
            json!({"value": "useless", "session_id": "own"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let r = w.feedback(&w.b_tok, &e, json!({"value": "useful"})).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    for tok in [&w.a_tok, &w.b_tok] {
        let j =
            w.a.tok(tok, Method::GET, &format!("/api/v1/entries/{e}"), None)
                .await
                .json();
        assert_eq!(
            j["usage"],
            json!({"shown": 0, "useful": 2, "useless": 1}),
            "{j}"
        );
    }
    let target = TargetRef {
        kind: TargetKind::Entry,
        id: e.parse().unwrap(),
    };
    let owner_db =
        w.a.st
            .users
            .get(w.a_id.parse::<Id>().unwrap())
            .await
            .unwrap();
    let c = zam_store::usage_counts(&owner_db, target).await.unwrap();
    assert_eq!((c.useful, c.useless), (2, 1));
    let member_db =
        w.a.st
            .users
            .get(w.b_id.parse::<Id>().unwrap())
            .await
            .unwrap();
    let c = zam_store::usage_counts(&member_db, target).await.unwrap();
    assert_eq!((c.useful, c.useless), (0, 0));
    // кто и в какой сессии оценил — в строках базы владельца
    let rows: Vec<(String, Option<String>, String)> = owner_db
        .read(|c| {
            let mut st = c.prepare(
                "SELECT user_id, session_id, source FROM usage WHERE event = 'useful' ORDER BY id",
            )?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|r| r.0 == w.b_id && r.1.as_deref() == Some("fb-session") && r.2 == "agent")
    );
}

/// Оценка требует права чтения цели: посторонний — 404, участник без одобрения — 403;
/// неверное значение — 400, удалённая запись — 404.
#[tokio::test]
async fn feedback_requires_read_access() {
    let w = world("fb-access", false).await;
    let e = w.insight().await;
    let r = w.feedback(&w.c_tok, &e, json!({"value": "useful"})).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let r = w.feedback(&w.b_tok, &e, json!({"value": "useful"})).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    for bad in [
        json!({"value": "meh"}),
        json!({}),
        json!({"value": "useful", "session_id": ""}),
    ] {
        let r = w.feedback(&w.a_tok, &e, bad.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text());
    }
    let r =
        w.a.tok_rev(
            &w.a_tok,
            Method::DELETE,
            &format!("/api/v1/entries/{e}"),
            None,
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let r = w.feedback(&w.a_tok, &e, json!({"value": "useful"})).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let target = TargetRef {
        kind: TargetKind::Entry,
        id: e.parse().unwrap(),
    };
    let owner_db =
        w.a.st
            .users
            .get(w.a_id.parse::<Id>().unwrap())
            .await
            .unwrap();
    let c = zam_store::usage_counts(&owner_db, target).await.unwrap();
    assert_eq!((c.useful, c.useless), (0, 0));
}

/// Обслуживание удаляет сессии без обращений дольше 30 дней (во всех базах), свежие остаются.
#[tokio::test]
async fn sessions_gc_after_30_days() {
    let a = api("sessions-gc", |_| {}).await;
    let (_, other) = a.new_user("Пётр").await;
    for tok in [&a.token, &other] {
        let r = a
            .tok(
                tok,
                Method::POST,
                "/api/v1/sessions",
                Some(json!({"id": "old"})),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED);
    }
    let day = SignedDuration::from_hours(24);
    let n = zam_server::maintenance::gc_sessions(&a.st, Timestamp::now() + day * 29).await;
    assert_eq!(n, 0);
    assert_eq!(session(&a, &a.token, "old").await.status, StatusCode::OK);
    let n = zam_server::maintenance::gc_sessions(&a.st, Timestamp::now() + day * 31).await;
    assert_eq!(n, 2);
    for tok in [&a.token, &other] {
        assert_eq!(session(&a, tok, "old").await.status, StatusCode::NOT_FOUND);
    }
}
