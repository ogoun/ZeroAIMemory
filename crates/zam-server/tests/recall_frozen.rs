//! Детерминированность дайджеста (M6 §8, §17 п. 9; задача 8a): два `session_start` разных
//! сессий при неизменной базе в те же сутки UTC — побайтово одинаковый `text`; окна ответов
//! человека (`ANSWERS_DAYS`) и решений пакета восстановления (`RECOVERY_DECISIONS_DAYS`) — от
//! начала суток UTC; в тексте нет относительного возраста.
//!
//! Все тесты файла замораживают часы (`freeze_now` держит блокировку тестовых точек): часы общие
//! для процесса, поэтому тесты, чувствительные к сроку, рядом с замороженными не идут —
//! отдельный двоичный файл (журнал M6, задача 6). Повторный `freeze_now` — только после drop
//! прежнего guard.
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};
use zam_server::config::ServerConfig;
use zam_store::time::freeze_now;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e8a";
const KN: &str = "/api/v1/spaces/knowledge/entries";
const RECALL: &str = "/api/v1/recall";

fn roomy(c: &mut ServerConfig) {
    c.recall.timeout_ms = 30_000;
}

fn ts(s: &str) -> jiff::Timestamp {
    s.parse().unwrap()
}

async fn created(r: Resp) -> String {
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

async fn recall(a: &Api, session: &str, body: Value) -> Value {
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            RECALL,
            Some(body),
            &[("x-zam-session", session)],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Сервер с правилами, решением `G`, задачами, вопросом с ответом, идеями, ловушками, changelog,
/// проверенной командой и картой кода — все разделы дайджеста.
async fn world(name: &str) -> Api {
    let a = api(&format!("recall-frozen-{name}"), roomy).await;
    let r = a
        .adm(
            Method::PATCH,
            "/api/v1/admin/settings",
            Some(json!({"rules": {"agent_write": true}})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Детерминизм"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let sol = format!("/api/v1/spaces/{G}/entries");
    for i in 0..3 {
        a.note(
            "rules",
            &format!("правило-{i}"),
            &format!("# Правило {i}\n\nТекст правила {i}."),
        )
        .await;
    }
    a.note(
        G,
        "Команды/Сборка",
        "---\nverified_at: 2026-11-01\n---\n# Сборка\n\ncargo build\n",
    )
    .await;
    let task = created(
        a.t(
            Method::POST,
            KN,
            Some(json!({"kind": "task", "title": "Задача в работе",
                        "data": {"next_step": "дописать тест"}})),
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
    let q = created(
        a.t(
            Method::POST,
            KN,
            Some(
                json!({"kind": "question", "title": "Какой формат логов?", "body": "к",
                        "data": {"options": [{"key": "a", "text": "JSON"},
                                             {"key": "b", "text": "Текст"}]}}),
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
    for (i, t) in ["Кэш миниатюр", "Экспорт отчётов"].iter().enumerate() {
        created(
            a.t(
                Method::POST,
                &sol,
                Some(json!({"kind": "idea", "title": t, "body": "Замысел.",
                            "data": {"origin": "human", "priority": if i == 0 { "high" } else { "normal" }}})),
            )
            .await,
        )
        .await;
    }
    for i in 0..3 {
        created(
            a.t(
                Method::POST,
                &sol,
                Some(
                    json!({"kind": "insight", "title": format!("Ловушка {i}"), "body": "x",
                            "data": {"kind": "gotcha"}}),
                ),
            )
            .await,
        )
        .await;
        created(
            a.t(
                Method::POST,
                &sol,
                Some(
                    json!({"kind": "changelog", "title": format!("Изменение {i}"),
                            "body": "x", "data": {"kind": "fix"}}),
                ),
            )
            .await,
        )
        .await;
    }
    // карта кода: один файл, два символа
    let hash = format!("{:064x}", 1);
    let r = a
        .t(
            Method::POST,
            "/api/v1/code/sync",
            Some(
                json!({"solution": G, "commit": "abcdef1234", "branch": "main",
                        "machine": "box", "manifest": [{"path": "src/a.rs", "hash": hash}]}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let syms: Vec<Value> = [("alpha", 1u32), ("beta", 9)]
        .iter()
        .map(|(n, l)| {
            json!({"name": n, "qual": n, "kind": "function", "line": l, "end_line": l + 2,
                   "sig": format!("fn {n}()"), "body_hash": format!("{l:016x}"),
                   "exported": true})
        })
        .collect();
    let r = a
        .t(
            Method::PUT,
            "/api/v1/code/files",
            Some(
                json!({"solution": G, "commit": "abcdef1234", "branch": "main",
                        "machine": "box",
                        "files": [{"path": "src/a.rs", "lang": "rust", "hash": hash,
                                   "size": 10, "parse_state": "ok", "symbols": syms,
                                   "refs": []}]}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    a
}

/// §17 п. 9: два старта разных сессий в те же сутки UTC (часы сдвинуты на 9 ч) — побайтово
/// одинаковый `text`; в нём есть все разделы, в том числе строка карты кода.
#[tokio::test]
async fn digest_deterministic_two_sessions() {
    let a = {
        let _clock = freeze_now(ts("2026-10-10T01:00:00Z"));
        world("two").await
    };
    let body = json!({"event": "session_start", "solution": G});
    let first = {
        let _clock = freeze_now(ts("2026-10-10T02:00:00Z"));
        recall(&a, "det-1", body.clone()).await
    };
    let second = {
        let _clock = freeze_now(ts("2026-10-10T11:30:00Z"));
        recall(&a, "det-2", body.clone()).await
    };
    let text = first["text"].as_str().unwrap();
    for h in [
        "### Задачи",
        "### Ответы человека",
        "### Идеи",
        "### Правила",
        "### Ловушки и сбои",
        "### Последние изменения",
        "Проверенные команды:",
        "ZeroAIMemory: карта кода:",
    ] {
        assert!(text.contains(h), "{h}: {text}");
    }
    assert_eq!(
        first["text"], second["text"],
        "{text}\n---\n{}",
        second["text"]
    );
}

/// §8: окно ответов человека (`ANSWERS_DAYS` = 14) — от начала суток UTC: ответ, данный 14 суток
/// назад в 08:00, виден и в 07:00, и в 09:00 сегодняшних суток; 15 суток назад — нет.
#[tokio::test]
async fn answers_window_from_utc_day_start() {
    let a = {
        let _clock = freeze_now(ts("2026-10-01T06:00:00Z"));
        world("answers").await
    };
    let q = {
        let _clock = freeze_now(ts("2026-10-01T08:00:00Z"));
        let q = created(
            a.t(
                Method::POST,
                KN,
                Some(
                    json!({"kind": "question", "title": "Где хранить кэш?", "body": "к",
                            "data": {"options": [{"key": "a", "text": "Redis"},
                                                 {"key": "b", "text": "Память"}]}}),
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
        q
    };
    let body = json!({"event": "session_start", "solution": G});
    for (at, sid) in [
        ("2026-10-15T07:00:00Z", "aw-1"),
        ("2026-10-15T09:00:00Z", "aw-2"),
    ] {
        let _clock = freeze_now(ts(at));
        let j = recall(&a, sid, body.clone()).await;
        assert!(j["text"].as_str().unwrap().contains(&q), "{at}: {j}");
    }
    let _clock = freeze_now(ts("2026-10-16T00:00:00Z"));
    let j = recall(&a, "aw-3", body).await;
    assert!(!j["text"].as_str().unwrap().contains(&q), "{j}");
}

/// §8: окно решений пакета восстановления (`RECOVERY_DECISIONS_DAYS` = 7) — от начала суток UTC.
#[tokio::test]
async fn recovery_decisions_window_from_utc_day_start() {
    let a = {
        let _clock = freeze_now(ts("2026-10-01T06:00:00Z"));
        world("decisions").await
    };
    let d = {
        let _clock = freeze_now(ts("2026-10-01T08:00:00Z"));
        created(
            a.t(
                Method::POST,
                &format!("/api/v1/spaces/{G}/entries"),
                Some(
                    json!({"kind": "insight", "title": "Решение про кэш", "body": "обоснование",
                            "data": {"kind": "decision"}}),
                ),
            )
            .await,
        )
        .await
    };
    let body = json!({"event": "recovery", "solution": G});
    for (at, sid) in [
        ("2026-10-08T07:00:00Z", "dw-1"),
        ("2026-10-08T09:00:00Z", "dw-2"),
    ] {
        let _clock = freeze_now(ts(at));
        let j = recall(&a, sid, body.clone()).await;
        assert!(j["text"].as_str().unwrap().contains(&d), "{at}: {j}");
    }
    let _clock = freeze_now(ts("2026-10-09T00:00:00Z"));
    let j = recall(&a, "dw-3", body).await;
    assert!(!j["text"].as_str().unwrap().contains(&d), "{j}");
}
