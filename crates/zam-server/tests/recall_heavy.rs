//! Приёмка дайджеста тяжёлого профиля ТС-2 без модели (M6 §8, задача 8a; журнал задачи 4, «→ 8a»):
//! мир `sh-05` (20 Rules, идеи включены) — в `session_start` все обязательные строки ТС-2, строка
//! карты кода и раздел «Ловушки»; в пакете восстановления — обязательные строки без идей.
//! Счётчик без модели — `tokenizer.json` bge-m3 комплекта, если он есть, иначе оценка. С моделью —
//! `recall_heavy_ml.rs` (модель ставит счётчик токенов процесса).
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, api};
use common::heavy::{SOL, check_digest, check_required, load, shape, spec};
use serde_json::{Value, json};

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

/// `sh-05`: 20 Rules, 6 задач, 6 вопросов, 2 запроса доступа, словарь, insight, идеи, карта кода.
#[tokio::test]
async fn heavy_sh05_digest_complete_fts() {
    let s = spec("sh-05");
    assert_eq!((s.profile.as_str(), s.rules.len()), ("heavy", 20));
    let a = api("recall-heavy-sh05", |c| c.recall.timeout_ms = 30_000).await;
    let w = load(&a, &s).await;
    assert_eq!(w.rules, 20);
    let j = recall(
        &a,
        "sh05-1",
        json!({"event": "session_start", "solution": SOL, "source": "startup"}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    // хук без `output_limit` — 7500 байт: обязательные строки на месте (карта кода и ловушки —
    // в остатке канала, повторное ревью 8a)
    check_required("session_start", text, &w);
    eprintln!("sh-05 session_start (7500 байт): {}", shape(text, &w));
    assert!(text.contains("### Идеи"), "{text}");
    // пакет восстановления — те же обязательные строки, кроме идей (§8)
    let mut rec = w.required.clone();
    rec.retain(|r| r.kind != "idea");
    let wr = common::heavy::World {
        required: rec,
        traps: w.traps.clone(),
        rules: w.rules,
    };
    let j = recall(
        &a,
        "sh05-1",
        json!({"event": "session_start", "solution": SOL, "source": "compact"}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    assert!(!text.contains("### Идеи"), "{text}");
    check_required("recovery", text, &wr);
}

/// Ревью 8a, C1: `sh-05` в пределе канала Codex (7500 байт) и Claude Code (9000 символов),
/// в том числе хук старого клиента без `output_limit` и восстановление с местом под сводку
/// журнала: весь текст в пределе, обязательные строки, карта кода и «Ловушки» — на месте.
#[tokio::test]
async fn heavy_sh05_fits_output_limit() {
    let s = spec("sh-05");
    let a = api("recall-heavy-sh05-limit", |c| c.recall.timeout_ms = 30_000).await;
    let w = load(&a, &s).await;
    let cases = [
        (
            "codex",
            Some(json!({"unit": "bytes", "max": 7500})),
            7500,
            true,
        ),
        (
            "claude",
            Some(json!({"unit": "chars", "max": 9000})),
            9000,
            false,
        ),
        ("old", None, 7500, true),
    ];
    for (who, limit, max, bytes) in cases {
        let size = |t: &str| if bytes { t.len() } else { t.chars().count() };
        let mut body = json!({"event": "session_start", "solution": SOL, "source": "startup",
                              "code_map": true});
        if let Some(l) = &limit {
            body["output_limit"] = l.clone();
        }
        let j = recall(&a, &format!("sh05-{who}"), body).await;
        let text = j["text"].as_str().unwrap();
        assert!(size(text) <= max, "{who}: {} > {max}", size(text));
        eprintln!("sh-05 {who}: {}", shape(text, &w));
        if who == "claude" {
            check_digest(who, text, &w, s.code_map);
        } else {
            check_required(who, text, &w);
        }
        // восстановление: клиент вычел 400 единиц под сводку журнала (обычная сводка — 150–350)
        let mut body = json!({"event": "session_start", "solution": SOL, "source": "compact",
                              "code_map": true});
        let unit = if bytes { "bytes" } else { "chars" };
        body["output_limit"] = json!({"unit": unit, "max": max - 400});
        let j = recall(&a, &format!("sh05-{who}"), body).await;
        let text = j["text"].as_str().unwrap();
        assert!(size(text) <= max - 400, "{who} recovery: {}", size(text));
        let mut wr = common::heavy::World {
            required: w.required.clone(),
            traps: w.traps.clone(),
            rules: w.rules,
        };
        wr.required.retain(|r| r.kind != "idea");
        eprintln!("sh-05 {who} recovery: {}", shape(text, &wr));
        if who == "claude" {
            check_digest(&format!("{who} recovery"), text, &wr, s.code_map);
        } else {
            check_required(&format!("{who} recovery"), text, &wr);
        }
    }
}
