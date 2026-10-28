//! Вопросы к человеку (M4.5 §4, задача 5): ответ из WebUI и чата становится решением
//! (`axiom` у человека, `hypothesis` у агента, ребро `derived_from`), повторный и параллельный
//! ответы, дубликаты вопросов, устаревание с причиной, разделы дайджеста, общее решение.
//! Сервер без моделей (поиск — FTS).
mod common;

use axum::body::Body;
use axum::http::{Method, StatusCode, header};
use common::api::{Api, Resp, api, req};
use serde_json::{Value, json};
use tower::ServiceExt;
use zam_server::config::ServerConfig;

const KN: &str = "/api/v1/spaces/knowledge/entries";
const RECALL: &str = "/api/v1/recall";
const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e77";

fn roomy(c: &mut ServerConfig) {
    c.recall.timeout_ms = 30_000;
}

fn id(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("нет id: {v}"))
        .to_string()
}

fn options() -> Value {
    json!({"options": [{"key": "a", "text": "Оставить SQLite"}, {"key": "b", "text": "Взять Postgres"}],
           "recommendation": "a"})
}

/// Задаёт вопрос в Knowledge токеном администратора.
async fn ask(a: &Api, title: &str, data: Value) -> Resp {
    a.t(
        Method::POST,
        KN,
        Some(json!({"kind": "question", "title": title, "body": "Контекст вопроса", "data": data})),
    )
    .await
}

async fn new_q(a: &Api, title: &str, data: Value) -> String {
    let r = ask(a, title, data).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    id(&r.json())
}

async fn get(a: &Api, id: &str) -> Value {
    let r = a
        .t(Method::GET, &format!("/api/v1/entries/{id}"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Ответ токеном администратора (агент).
async fn answer_tok(a: &Api, qid: &str, body: Value) -> Resp {
    a.t(
        Method::POST,
        &format!("/api/v1/entries/{qid}/answer"),
        Some(body),
    )
    .await
}

/// Ответ из WebUI (сессия администратора).
async fn answer_webui(a: &Api, qid: &str, body: Value) -> Resp {
    a.adm(
        Method::POST,
        &format!("/api/v1/entries/{qid}/answer"),
        Some(body),
        None,
    )
    .await
}

async fn patch(a: &Api, id: &str, body: Value) -> Resp {
    a.tok_rev(
        &a.token,
        Method::PATCH,
        &format!("/api/v1/entries/{id}"),
        Some(body),
    )
    .await
}

#[tokio::test]
async fn answer_webui_creates_axiom_decision() {
    let a = api("questions-webui", |_| {}).await;
    let qid = new_q(&a, "Какую базу взять?", options()).await;
    let q = get(&a, &qid).await;
    assert_eq!(q["kind"], "question");
    assert_eq!(q["status"], "open");
    assert_eq!(q["data"]["recommendation"], "a");

    let r = answer_webui(
        &a,
        &qid,
        json!({"option": "b", "text": "Нагрузка вырастет"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let did = j["decision_id"].as_str().unwrap().to_string();
    assert_eq!(j["decision"]["id"], did.as_str());
    let e = &j["entry"];
    assert_eq!(e["status"], "answered");
    assert_eq!(e["rev"], 2, "новая ревизия");
    let ans = &e["data"]["answer"];
    assert_eq!(ans["option"], "b");
    assert_eq!(ans["text"], "Нагрузка вырастет");
    assert_eq!(ans["by"], "human");
    assert_eq!(ans["channel"], "webui");
    assert_eq!(ans["decision_id"], did.as_str());
    assert!(ans["answered_at"].is_string());
    // варианты и рекомендация сохранились
    assert_eq!(e["data"]["options"].as_array().unwrap().len(), 2);

    let d = get(&a, &did).await;
    assert_eq!(d["kind"], "insight");
    assert_eq!(d["certainty"], "axiom");
    assert_eq!(d["data"]["kind"], "decision");
    assert_eq!(d["data"]["decision"]["decided_by"], "human");
    assert_eq!(d["title"], "Какую базу взять? → Взять Postgres");
    assert_eq!(d["status"], "active");
    let body = d["body"].as_str().unwrap();
    for part in [
        "Какую базу взять?",
        "Контекст вопроса",
        "- a: Оставить SQLite (рекомендовано)",
        "- b: Взять Postgres",
        "b — Взять Postgres",
        "Нагрузка вырастет",
        "человек",
        "webui",
    ] {
        assert!(body.contains(part), "нет «{part}» в\n{body}");
    }
    assert_eq!(d["author"]["kind"], "human");

    // ребро derived_from решение → вопрос
    let links = a
        .t(Method::GET, &format!("/api/v1/entries/{did}/links"), None)
        .await
        .json();
    let to_q: Vec<&Value> = links["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "derived_from" && e["dst"].as_str().unwrap().ends_with(&qid))
        .collect();
    assert_eq!(to_q.len(), 1, "{links}");

    // ревизия вопроса: вторая ревизия — ответ; решение в ленте Knowledge
    let revs = a
        .t(
            Method::GET,
            &format!("/api/v1/entries/{qid}/revisions"),
            None,
        )
        .await
        .json();
    assert_eq!(revs["items"].as_array().unwrap().len(), 2);
    let list = a
        .t(Method::GET, &format!("{KN}?kind=insight"), None)
        .await
        .json();
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    // аудит без текста ответа
    let audit = a
        .adm(Method::GET, "/api/v1/admin/audit?limit=50", None, None)
        .await;
    assert_eq!(audit.status, StatusCode::OK, "{}", audit.text());
    let at = audit.text();
    assert!(at.contains("entry.answer"), "{at}");
    assert!(!at.contains("Нагрузка вырастет"), "текст ответа в аудите");
}

#[tokio::test]
async fn answer_from_chat_by_agent_token_is_human_axiom() {
    let a = api("questions-chat", |_| {}).await;
    let qid = new_q(&a, "Нужен ли кэш?", json!({})).await;
    let r = answer_tok(
        &a,
        &qid,
        json!({"text": "Да, но только для чтения", "channel": "chat"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["entry"]["data"]["answer"]["by"], "human");
    assert_eq!(j["entry"]["data"]["answer"]["channel"], "chat");
    assert!(j["entry"]["data"]["answer"]["option"].is_null());
    let d = &j["decision"];
    assert_eq!(d["certainty"], "axiom");
    assert_eq!(d["data"]["decision"]["decided_by"], "human");
    assert_eq!(d["title"], "Нужен ли кэш? → Да, но только для чтения");
    // автор ревизии — агент (токен), а не человек: он записал слова человека
    assert_eq!(d["author"]["kind"], "agent");

    // channel по умолчанию у токена — chat; webui токену недоступен
    let q2 = new_q(&a, "Второй вопрос", json!({})).await;
    let r = answer_tok(&a, &q2, json!({"text": "ок", "channel": "webui"})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    let r = answer_tok(&a, &q2, json!({"text": "ок"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["entry"]["data"]["answer"]["channel"], "chat");
    // cli
    let q3 = new_q(&a, "Третий вопрос", json!({})).await;
    let r = answer_tok(&a, &q3, json!({"text": "ок", "channel": "cli"})).await;
    assert_eq!(r.json()["entry"]["data"]["answer"]["channel"], "cli");
}

#[tokio::test]
async fn answer_by_agent_hypothesis() {
    let a = api("questions-agent", |_| {}).await;
    let qid = new_q(&a, "Какой порт?", options()).await;
    let r = answer_tok(
        &a,
        &qid,
        json!({"option": "a", "channel": "chat", "by": "agent"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["entry"]["data"]["answer"]["by"], "agent");
    assert_eq!(j["decision"]["certainty"], "hypothesis");
    assert_eq!(j["decision"]["data"]["decision"]["decided_by"], "agent");
    assert_eq!(j["decision"]["title"], "Какой порт? → Оставить SQLite");
}

#[tokio::test]
async fn answer_twice_409() {
    let a = api("questions-twice", |_| {}).await;
    let qid = new_q(&a, "Дважды?", options()).await;
    let first = answer_webui(&a, &qid, json!({"option": "a"})).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text());
    let did = first.json()["decision_id"].clone();
    let r = answer_tok(&a, &qid, json!({"text": "иначе", "channel": "chat"})).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.header("content-type"), "application/problem+json");
    let j = r.json();
    assert_eq!(j["type"], "/problems/already-answered");
    assert_eq!(j["decision_id"], did);
    // первое решение не тронуто; второго нет
    let list = a
        .t(Method::GET, &format!("{KN}?kind=insight"), None)
        .await
        .json();
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(get(&a, &qid).await["data"]["answer"]["option"], "a");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_answer_one_decision() {
    let a = api("questions-race", |_| {}).await;
    let qid = new_q(&a, "Гонка ответов?", options()).await;
    let mut hs = Vec::new();
    for i in 0..8 {
        let app = a.app.clone();
        let token = a.token.clone();
        let uri = format!("/api/v1/entries/{qid}/answer");
        hs.push(tokio::spawn(async move {
            let body = json!({"text": format!("ответ {i}"), "channel": "chat"});
            let r = req(Method::POST, &uri, Some(&body))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(body.to_string()))
                .unwrap();
            let r = app.oneshot(r).await.unwrap();
            let status = r.status();
            let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap())
        }));
    }
    let mut ok = Vec::new();
    let mut dup_ids = Vec::new();
    for h in hs {
        let (status, body) = h.await.unwrap();
        match status {
            StatusCode::OK => ok.push(body),
            StatusCode::CONFLICT => {
                assert_eq!(body["type"], "/problems/already-answered", "{body}");
                dup_ids.push(body["decision_id"].clone());
            }
            s => panic!("{s}: {body}"),
        }
    }
    assert_eq!(ok.len(), 1, "ровно один ответ принят");
    assert_eq!(dup_ids.len(), 7);
    assert!(dup_ids.iter().all(|d| *d == ok[0]["decision_id"]));
    let list = a
        .t(Method::GET, &format!("{KN}?kind=insight"), None)
        .await
        .json();
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "одно решение");
}

#[tokio::test]
async fn answer_unknown_option_422() {
    let a = api("questions-422", |_| {}).await;
    let qid = new_q(&a, "Вариант?", options()).await;
    for bad in [
        json!({"option": "zzz"}),
        json!({}),
        json!({"text": "  ", "option": " "}),
        json!({"text": "x", "channel": "telegram"}),
        json!({"text": "x", "by": "robot"}),
    ] {
        let r = answer_tok(&a, &qid, bad.clone()).await;
        assert!(
            r.status == StatusCode::UNPROCESSABLE_ENTITY || r.status == StatusCode::BAD_REQUEST,
            "{bad}: {} {}",
            r.status,
            r.text()
        );
    }
    let r = answer_tok(&a, &qid, json!({"option": "zzz"})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = answer_tok(&a, &qid, json!({})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    // ничего не записано
    assert_eq!(get(&a, &qid).await["status"], "open");
    let list = a
        .t(Method::GET, &format!("{KN}?kind=insight"), None)
        .await
        .json();
    assert!(list["items"].as_array().unwrap().is_empty());
    // не вопрос — 422; токен ro — 403
    let ins = a
        .t(
            Method::POST,
            KN,
            Some(json!({"kind": "insight", "title": "Не вопрос", "body": "x",
                        "data": {"kind": "discovery"}})),
        )
        .await
        .json();
    let r = answer_tok(&a, &id(&ins), json!({"text": "x"})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    let ro = a.issue(&a.admin_id(), "ro").await;
    let r = a
        .tok(
            &ro,
            Method::POST,
            &format!("/api/v1/entries/{qid}/answer"),
            Some(json!({"text": "x"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // варианты: правила data вопроса
    for bad in [
        json!({"options": [{"key": "a", "text": "x"}, {"key": "a", "text": "y"}]}),
        json!({"options": [{"key": "a", "text": "x"}], "recommendation": "z"}),
        json!({"answer": {"text": "подделка"}}),
        json!({"foo": 1}),
    ] {
        let r = ask(&a, "Плохой вопрос", bad.clone()).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad}: {}",
            r.text()
        );
    }
    let many: Vec<Value> = (0..9)
        .map(|i| json!({"key": format!("k{i}"), "text": "t"}))
        .collect();
    let r = ask(&a, "Много вариантов", json!({ "options": many })).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    // вопрос не создаётся в Rules
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/rules/entries",
            Some(json!({"kind": "question", "title": "x"})),
        )
        .await;
    assert!(
        r.status == StatusCode::UNPROCESSABLE_ENTITY || r.status == StatusCode::FORBIDDEN,
        "{}",
        r.text()
    );
}

#[tokio::test]
async fn ask_duplicate_of_answered_returns_answer() {
    let a = api("questions-dup", |_| {}).await;
    let qid = new_q(&a, "Нужен ли Docker?", json!({})).await;
    // дубликат открытого вопроса (регистр и пунктуация не важны)
    let r = ask(&a, "нужен ли docker", json!({})).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let j = r.json();
    assert_eq!(j["type"], "/problems/possible-duplicate");
    assert_eq!(j["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(j["candidates"][0]["id"], qid.as_str());
    assert_eq!(j["candidates"][0]["status"], "open");
    assert!(j["candidates"][0].get("answer").is_none(), "{j}");

    // после ответа кандидат несёт ответ
    let r = answer_webui(&a, &qid, json!({"text": "Нет, без Docker"})).await;
    assert_eq!(r.status, StatusCode::OK);
    let did = r.json()["decision_id"].clone();
    let r = ask(&a, "Нужен ли Docker?!", json!({})).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let c = &r.json()["candidates"][0];
    assert_eq!(c["status"], "answered");
    assert_eq!(c["answer"]["text"], "Нет, без Docker");
    assert_eq!(c["answer"]["by"], "human");
    assert_eq!(c["answer"]["decision_id"], did);

    // force записывает всё равно
    let r = a
        .t(
            Method::POST,
            KN,
            Some(json!({"kind": "question", "title": "Нужен ли Docker?", "force": true})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    // повтор по Idempotency-Key дубликатом не считается
    let body = json!({"kind": "question", "title": "Другой вопрос"});
    let k = [("idempotency-key", "q-key-1")];
    let r1 = a
        .tok_h(&a.token, Method::POST, KN, Some(body.clone()), &k)
        .await;
    assert_eq!(r1.status, StatusCode::CREATED, "{}", r1.text());
    let r2 = a.tok_h(&a.token, Method::POST, KN, Some(body), &k).await;
    assert_eq!(r2.status, StatusCode::OK, "{}", r2.text());

    // устаревший вопрос дублем не считается
    let q2 = new_q(&a, "Старый вопрос", json!({})).await;
    let r = patch(
        &a,
        &q2,
        json!({"status": "obsolete", "reason": "неактуален"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = ask(&a, "Старый вопрос", json!({})).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

#[tokio::test]
async fn obsolete_requires_reason() {
    let a = api("questions-obsolete", |_| {}).await;
    let qid = new_q(&a, "Устареет?", options()).await;
    let r = patch(&a, &qid, json!({"status": "obsolete"})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert_eq!(r.json()["type"], "/problems/reason-required");
    let r = patch(&a, &qid, json!({"status": "obsolete", "reason": "  "})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    // answered ставится только ответом
    let r = patch(&a, &qid, json!({"status": "answered", "reason": "r"})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    // правка data открытого вопроса — слияние; ответ клиент не задаёт
    let r = patch(&a, &qid, json!({"data": {"recommendation": "b"}})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["data"]["recommendation"], "b");
    assert_eq!(r.json()["data"]["options"].as_array().unwrap().len(), 2);
    for bad in [
        json!({"data": {"answer": {"text": "x"}}}),
        json!({"data": {"recommendation": "zzz"}}),
        json!({"data": {"options": [{"key": "a"}]}}),
    ] {
        let r = patch(&a, &qid, bad.clone()).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad}: {}",
            r.text()
        );
    }
    // с причиной — устарел; причина в ревизии; отвечать нельзя; назад не вернуть
    let r = patch(
        &a,
        &qid,
        json!({"status": "obsolete", "reason": "решили иначе"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["status"], "obsolete");
    let revs = a
        .t(
            Method::GET,
            &format!("/api/v1/entries/{qid}/revisions"),
            None,
        )
        .await
        .json();
    assert_eq!(
        revs["items"].as_array().unwrap().last().unwrap()["reason"],
        "решили иначе"
    );
    let r = answer_tok(&a, &qid, json!({"option": "a"})).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_ne!(r.json()["type"], "/problems/already-answered");
    let r = patch(&a, &qid, json!({"status": "open", "reason": "r"})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());

    // отвеченный тоже можно объявить устаревшим; его содержимое не правится
    let q2 = new_q(&a, "Отвеченный", options()).await;
    answer_webui(&a, &q2, json!({"option": "a"})).await;
    let r = patch(&a, &q2, json!({"data": {"recommendation": "b"}})).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let r = patch(&a, &q2, json!({"status": "open", "reason": "r"})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    let r = patch(&a, &q2, json!({"status": "obsolete", "reason": "r"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["data"]["answer"]["decision_id"].is_string());
}

async fn recall(a: &Api, session: &str) -> Value {
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            RECALL,
            Some(json!({"event": "session_start", "agent_block_version": 1})),
            &[("x-zam-session", session)],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

#[tokio::test]
async fn digest_answers_once_per_session() {
    let a = api("questions-digest-answers", roomy).await;
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
    let t = a
        .t(
            Method::POST,
            KN,
            Some(json!({"kind": "task", "title": "Активная задача"})),
        )
        .await
        .json();
    let r = patch(&a, &id(&t), json!({"status": "in_progress"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let qid = new_q(&a, "Какой формат логов?", options()).await;
    let r = answer_webui(&a, &qid, json!({"option": "b"})).await;
    let did = r.json()["decision_id"].as_str().unwrap().to_string();
    let line =
        format!("ZeroAIMemory: ответ на «Какой формат логов?»: Взять Postgres — q:{qid} → i:{did}");

    let j = recall(&a, "s1").await;
    let text = j["text"].as_str().unwrap();
    assert!(text.contains(&line), "{text}");
    let pos = |n: &str| {
        text.find(n)
            .unwrap_or_else(|| panic!("нет «{n}» в\n{text}"))
    };
    assert!(pos("### Задачи") < pos("### Ответы человека"), "{text}");
    assert!(pos("### Ответы человека") < pos("### Правила"), "{text}");
    let items = j["items"].as_array().unwrap();
    let it = items.iter().find(|i| i["reason"] == "answer").unwrap();
    assert_eq!(it["id"], format!("q:{qid}"));
    assert_eq!(it["kind"], "question");

    // та же сессия (после compact) — ответ не повторяется; задачи и правила повторяются
    let j = recall(&a, "s1").await;
    let text = j["text"].as_str().unwrap();
    assert!(!text.contains("### Ответы человека"), "{text}");
    assert!(!text.contains("ответ на «"), "{text}");
    assert!(
        text.contains("### Задачи") && text.contains("### Правила"),
        "{text}"
    );

    // другая сессия видит ответ
    let j = recall(&a, "s2").await;
    assert!(j["text"].as_str().unwrap().contains(&line));

    // ответ старше 14 дней не показывается
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let old = zam_store::days_ago(jiff::Timestamp::now(), 15);
    let n = db
        .write(move |tx| {
            Ok(tx.execute(
                "UPDATE entries SET data = json_set(data, '$.answer.answered_at', ?1)
                 WHERE kind = 'question'",
                [zam_store::time::fmt_ts(old)],
            )?)
        })
        .await
        .unwrap();
    assert_eq!(n, 1);
    let j = recall(&a, "s3").await;
    assert!(!j["text"].as_str().unwrap().contains("ответ на «"));

    // не больше 5 ответов
    for i in 0..7 {
        let q = new_q(&a, &format!("Вопрос {i}"), json!({})).await;
        answer_webui(&a, &q, json!({"text": format!("ответ {i}")})).await;
    }
    let j = recall(&a, "s4").await;
    let text = j["text"].as_str().unwrap();
    assert_eq!(
        text.matches("ZeroAIMemory: ответ на «").count(),
        5,
        "{text}"
    );
    // новые первыми
    assert!(
        text.contains("Вопрос 6") && !text.contains("Вопрос 0"),
        "{text}"
    );
}

#[tokio::test]
async fn digest_open_questions_line() {
    let a = api("questions-digest-open", roomy).await;
    // нет вопросов — раздела нет
    let j = recall(&a, "s0").await;
    assert!(!j["text"].as_str().unwrap().contains("### Открытые вопросы"));

    let t = a
        .t(
            Method::POST,
            KN,
            Some(json!({"kind": "task", "title": "Задача в работе"})),
        )
        .await
        .json();
    patch(&a, &id(&t), json!({"status": "in_progress"})).await;
    for i in 1..=4 {
        new_q(&a, &format!("Открытый {i}"), json!({})).await;
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }
    let answered = new_q(&a, "Отвеченный", json!({})).await;
    answer_webui(&a, &answered, json!({"text": "да"})).await;
    let obsolete = new_q(&a, "Устаревший", json!({})).await;
    patch(&a, &obsolete, json!({"status": "obsolete", "reason": "r"})).await;

    let j = recall(&a, "s1").await;
    let text = j["text"].as_str().unwrap();
    assert!(
        text.contains("### Открытые вопросы\n4 открытых: «Открытый 4», «Открытый 3», «Открытый 2»"),
        "{text}"
    );
    assert!(!text.contains("«Открытый 1»"), "{text}");
    let pos = |n: &str| text.find(n).unwrap();
    assert!(pos("### Задачи") < pos("### Ответы человека"), "{text}");
    assert!(
        pos("### Ответы человека") < pos("### Открытые вопросы"),
        "{text}"
    );
    // строка — не подсказка записи: в items её нет, и она повторяется в той же сессии
    let items = j["items"].as_array().unwrap();
    assert!(items.iter().all(|i| i["reason"] != "open_questions"), "{j}");
    let j = recall(&a, "s1").await;
    assert!(j["text"].as_str().unwrap().contains("4 открытых: "));
    assert!(!j["text"].as_str().unwrap().contains("ответ на «"));
}

#[tokio::test]
async fn participant_answers_in_shared_solution() {
    let a = api("questions-shared", |_| {}).await;
    let (_, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Общее"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = a
        .tok(
            &b_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Общее"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
            Some(json!({"access": "rw"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let sol = format!("/api/v1/spaces/{G}/entries");
    // участник задаёт вопрос и отвечает на него: всё лежит в базе владельца
    let r = a
        .tok(
            &b_tok,
            Method::POST,
            &sol,
            Some(json!({"kind": "question", "title": "Кто ревьюит релиз?", "data": options()})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let qid = id(&r.json());
    // владелец видит вопрос в своей ленте
    let list = a
        .tok(&a_tok, Method::GET, &format!("{sol}?kind=question"), None)
        .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.text());
    assert_eq!(list.json()["items"][0]["id"], qid.as_str());
    // дубликат ловится и у владельца
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &sol,
            Some(json!({"kind": "question", "title": "Кто ревьюит релиз?"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());

    // ro-участник ответить не может; rw — может
    let ro = {
        let r = a
            .tok(
                &a_tok,
                Method::POST,
                &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
                Some(json!({"access": "ro"})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let r = a
            .tok(
                &b_tok,
                Method::POST,
                &format!("/api/v1/entries/{qid}/answer"),
                Some(json!({"option": "a"})),
            )
            .await;
        r.status
    };
    assert!(
        ro == StatusCode::FORBIDDEN || ro == StatusCode::NOT_FOUND,
        "ro-участник: {ro}"
    );
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
            Some(json!({"access": "rw"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = a
        .tok(
            &b_tok,
            Method::POST,
            &format!("/api/v1/entries/{qid}/answer"),
            Some(json!({"option": "a", "channel": "chat"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let did = j["decision_id"].as_str().unwrap().to_string();
    assert_eq!(j["decision"]["certainty"], "axiom");

    // решение — в базе владельца: он читает его и видит в ленте решения
    let r = a
        .tok(&a_tok, Method::GET, &format!("/api/v1/entries/{did}"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["data"]["decision"]["decided_by"], "human");
    let list = a
        .tok(&a_tok, Method::GET, &format!("{sol}?kind=insight"), None)
        .await
        .json();
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
    // в личной базе участника решения нет
    let own = a
        .tok(
            &b_tok,
            Method::GET,
            "/api/v1/spaces/knowledge/entries?kind=insight",
            None,
        )
        .await
        .json();
    assert!(own["items"].as_array().unwrap().is_empty(), "{own}");
}
