//! `GET /triggers` при недоступном решении (§10.3, §12): как поиск — 200 с Knowledge и
//! `warnings`, а не отказ на весь ответ (решение владельца 2026-10-26). Серверы без моделей.
//!
//! Пользователи: A — владелец решения, B — участник, C — посторонний.
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e61";
/// Второе решение того же владельца (для отклонённого запроса).
const G_OTHER: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e62";

struct W {
    a: Api,
    a_tok: String,
    b_id: String,
    b_tok: String,
    c_tok: String,
}

async fn world(name: &str) -> W {
    let a = api(&format!("triggers-{name}"), |_| {}).await;
    let (_, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let (_, c_tok) = a.new_user("Вера").await;
    let w = W {
        a,
        a_tok,
        b_id,
        b_tok,
        c_tok,
    };
    for g in [G, G_OTHER] {
        assert_eq!(w.request(&w.a_tok, g).await.status, StatusCode::CREATED);
    }
    // Анна: триггер в решении и в личном Knowledge; Борис: свой Knowledge
    w.insight(&w.a_tok, G, "solution-trigger").await;
    w.insight(&w.a_tok, "knowledge", "anna-knowledge").await;
    w.insight(&w.b_tok, "knowledge", "boris-knowledge").await;
    w
}

impl W {
    async fn insight(&self, tok: &str, space: &str, pattern: &str) {
        let r = self
            .a
            .tok(
                tok,
                Method::POST,
                &format!("/api/v1/spaces/{space}/entries"),
                Some(json!({
                    "kind": "insight", "title": pattern, "body": "",
                    "data": {"kind": "gotcha"},
                    "triggers": [{"type": "keyword", "pattern": pattern}],
                })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }

    async fn triggers(&self, tok: &str, uri_tail: &str, headers: &[(&str, &str)]) -> Resp {
        self.a
            .tok_h(
                tok,
                Method::GET,
                &format!("/api/v1/triggers{uri_tail}"),
                None,
                headers,
            )
            .await
    }

    /// Регистрация решения или запрос доступа.
    async fn request(&self, tok: &str, g: &str) -> Resp {
        self.a
            .tok(
                tok,
                Method::POST,
                "/api/v1/solutions",
                Some(json!({"agent_solution_id": g, "name": "Общее"})),
            )
            .await
    }

    /// Решение владельца о доступе Бориса.
    async fn decide(&self, g: &str, action: &str, body: Option<Value>) {
        let r = self
            .a
            .tok(
                &self.a_tok,
                Method::POST,
                &format!("/api/v1/solutions/{g}/access/{}/{action}", self.b_id),
                body,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
}

fn patterns(j: &Value) -> Vec<&str> {
    j["triggers"]
        .as_array()
        .unwrap_or_else(|| panic!("{j}"))
        .iter()
        .map(|t| t["pattern"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn triggers_revoked_solution_returns_knowledge_with_warning() {
    let w = world("revoked").await;
    let q = format!("?solution={G}");
    // ожидание
    assert_eq!(w.request(&w.b_tok, G).await.status, StatusCode::ACCEPTED);
    let r = w.triggers(&w.b_tok, &q, &[]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["warnings"], json!(["solution access pending"]), "{j}");
    assert_eq!(patterns(&j), ["boris-knowledge"], "{j}");
    // одобрение: решение и Knowledge, предупреждений нет
    w.decide(G, "approve", Some(json!({}))).await;
    let j = w.triggers(&w.b_tok, &q, &[]).await.json();
    assert_eq!(j["warnings"], json!([]), "{j}");
    assert_eq!(patterns(&j), ["solution-trigger", "boris-knowledge"], "{j}");
    // отзыв: Knowledge остаётся, предупреждение
    w.decide(G, "revoke", None).await;
    let r = w.triggers(&w.b_tok, &q, &[]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["warnings"], json!(["solution access revoked"]), "{j}");
    assert_eq!(patterns(&j), ["boris-knowledge"], "{j}");
    assert!(j["etag"].as_str().is_some_and(|e| !e.is_empty()), "{j}");
    // то же через заголовок агента
    let j = w
        .triggers(&w.b_tok, "", &[("x-zam-solution", G)])
        .await
        .json();
    assert_eq!(j["warnings"], json!(["solution access revoked"]), "{j}");
    // отклонённый запрос
    assert_eq!(
        w.request(&w.b_tok, G_OTHER).await.status,
        StatusCode::ACCEPTED
    );
    w.decide(G_OTHER, "reject", Some(json!({"reason": "нет"})))
        .await;
    let j = w
        .triggers(&w.b_tok, &format!("?solution={G_OTHER}"), &[])
        .await
        .json();
    assert_eq!(j["warnings"], json!(["solution access rejected"]), "{j}");
    assert_eq!(patterns(&j), ["boris-knowledge"], "{j}");
}

#[tokio::test]
async fn triggers_unknown_solution_warning() {
    let w = world("unknown").await;
    // не зарегистрировано
    let unknown = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9eff";
    let r = w
        .triggers(&w.c_tok, &format!("?solution={unknown}"), &[])
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["warnings"], json!(["solution unknown"]));
    assert_eq!(patterns(&r.json()), Vec::<&str>::new());
    // зарегистрировано, но запроса доступа нет — существование не раскрывается
    let j = w
        .triggers(&w.c_tok, &format!("?solution={G}"), &[])
        .await
        .json();
    assert_eq!(j["warnings"], json!(["solution unknown"]), "{j}");
    // не GUID в параметре — 400; не GUID в заголовке — игнорируется
    let r = w.triggers(&w.c_tok, "?solution=knowledge", &[]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = w
        .triggers(&w.c_tok, "", &[("x-zam-solution", "garbage")])
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["warnings"], json!([]));
    // владелец: решение и Knowledge без предупреждений
    let j = w
        .triggers(&w.a_tok, &format!("?solution={G}"), &[])
        .await
        .json();
    assert_eq!(j["warnings"], json!([]), "{j}");
    assert_eq!(patterns(&j), ["solution-trigger", "anna-knowledge"], "{j}");
}

#[tokio::test]
async fn triggers_etag_changes_with_warnings() {
    let w = world("etag").await;
    let q = format!("?solution={G}");
    // посторонний: пустой состав, предупреждение unknown
    let r1 = w.triggers(&w.c_tok, &q, &[]).await;
    let e1 = r1.json()["etag"].as_str().unwrap().to_string();
    assert_eq!(r1.header("etag"), format!("\"{e1}\""));
    // без решения состав триггеров тот же, предупреждений нет — ETag другой
    let r0 = w.triggers(&w.c_tok, "", &[]).await;
    assert_ne!(r0.json()["etag"].as_str().unwrap(), e1);
    // If-None-Match с тем же ETag — 304
    let inm = format!("\"{e1}\"");
    let r = w.triggers(&w.c_tok, &q, &[("if-none-match", &inm)]).await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    // запрос доступа меняет предупреждение: ETag меняется, старый If-None-Match → 200
    assert_eq!(w.request(&w.c_tok, G).await.status, StatusCode::ACCEPTED);
    let r2 = w.triggers(&w.c_tok, &q, &[("if-none-match", &inm)]).await;
    assert_eq!(r2.status, StatusCode::OK, "{}", r2.text());
    assert_eq!(r2.json()["warnings"], json!(["solution access pending"]));
    assert_ne!(r2.json()["etag"].as_str().unwrap(), e1);
}
