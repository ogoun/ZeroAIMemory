//! Приёмка дайджеста тяжёлого профиля ТС-2 на гибриде (M6 §8, задача 8a; журнал задачи 4,
//! «→ 8a»): модель (MiniLM, CPU) даёт точный счётчик токенов выдачи — тот путь упаковки, на
//! котором в 0.0.10-m5.7 выпадали правила; мир `sh-05` (20 Rules, идеи включены) — в
//! `session_start` все обязательные строки ТС-2, строка карты кода и раздел «Ловушки». Без
//! ORT/модели/Qdrant — пропуск, в CI (`ZAM_REQUIRE_ML=1`) — падение. Отдельный файл: модель
//! ставит счётчик токенов процесса.
//!
//! Бюджет — `recall.digest_tokens` (1500) **в токенах bge-m3**, модели комплекта: токенизатор
//! MiniLM (английский WordPiece) считает кириллицу примерно вдвое дороже, поэтому бюджет запроса
//! пересчитывается долей «MiniLM / bge-m3» на тексте этого мира (токенизатор bge-m3 —
//! `tokenizers/bge-m3-int8`, как в CI). Решающая проверка с bge-m3 на CUDA — замер ТС-2.
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, api};
use common::heavy::{SOL, check_digest, load, spec};
use common::ml::{MINILM, bge_counter, external, have, ml, vector_ready};
use common::qdrant::TestQdrant;
use serde_json::{Value, json};
use zam_markdown::TokenCounter;

/// `recall.digest_tokens` по умолчанию, токенов bge-m3.
const DIGEST_TOKENS: usize = 1500;

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

/// Текст без раздела `### Правила` — часть, что упаковывается в бюджет.
fn without_rules(text: &str) -> String {
    let Some(start) = text.find("\n### Правила\n") else {
        return text.to_string();
    };
    let tail = &text[start + 1..];
    let end = tail.find("\n\n").map_or(tail.len(), |e| e + 1);
    format!("{}{}", &text[..start], &tail[end..])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn heavy_sh05_digest_complete_hybrid() {
    if !have(MINILM) {
        return;
    }
    let Some(bge) = bge_counter() else {
        return;
    };
    let s = spec("sh-05");
    let q = TestQdrant::start("recall-heavy-ml");
    let a = api("recall-heavy-ml", |c| {
        ml(c, MINILM);
        external(c, &q);
        c.recall.timeout_ms = 30_000;
    })
    .await;
    vector_ready(&a).await;
    assert_eq!(
        zam_store::budget_counter_kind(),
        zam_store::BudgetCounterKind::Model,
        "счётчик выдачи — не модель"
    );
    let w = load(&a, &s).await;
    // весь дайджест мира без предела (сессия не та: её первый старт остаётся первым)
    let full = recall(
        &a,
        "sh05-probe",
        json!({"event": "session_start", "solution": SOL, "budget_tokens": 20000,
               "record": false}),
    )
    .await;
    let full = without_rules(full["text"].as_str().unwrap());
    let ratio = zam_store::count_tokens(&full) as f64 / bge.count(&full) as f64;
    let budget = (DIGEST_TOKENS as f64 * ratio).floor() as usize;
    eprintln!(
        "MiniLM/bge-m3 = {ratio:.3}: бюджет {budget} токенов MiniLM ≈ {DIGEST_TOKENS} bge-m3"
    );
    let j = recall(
        &a,
        "sh05-ml",
        json!({"event": "session_start", "solution": SOL, "source": "startup",
               "budget_tokens": budget}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    check_digest("session_start (гибрид)", text, &w, s.code_map);
    assert!(text.contains("### Идеи"), "{text}");
    // в бюджете — всё, кроме Rules (точный счётчик модели)
    let rest = without_rules(text);
    assert!(zam_store::fits_budget(&rest, budget), "{rest}");
    assert!(j["rules_tokens"].as_u64().unwrap() > 0, "{j}");
    drop(q);
}
