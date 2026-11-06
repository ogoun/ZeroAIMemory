//! Приёмка дайджеста тяжёлого профиля ТС-2 без модели (M6 §8, задача 8a; журнал задачи 4, «→ 8a»):
//! мир `sh-05` (20 Rules, идеи включены) — в `session_start` все обязательные строки ТС-2, строка
//! карты кода и раздел «Ловушки»; в пакете восстановления — обязательные строки без идей.
//! Счётчик без модели — `tokenizer.json` bge-m3 комплекта, если он есть, иначе оценка. С моделью —
//! `recall_heavy_ml.rs` (модель ставит счётчик токенов процесса).
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, api};
use common::heavy::{SOL, check_digest, load, spec};
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
    check_digest("session_start", text, &w, s.code_map);
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
    check_digest("recovery", text, &wr, s.code_map);
}
