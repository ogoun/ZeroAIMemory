//! Сканер секретов перед индексацией (задача 11 M3, §7.6): значения не попадают в чанки (FTS,
//! эмбеддинги, Qdrant), исходник заметки читается без изменений, ответ записи предупреждает
//! `secrets_detected` без значений. Без моделей (FTS).
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, api};
use serde_json::{Value, json};
use zam_core::Id;
use zam_store::{chunks_for_index, entry_chunks_for_index};

/// «Ключ» собирается из частей: в исходниках нет строки, похожей на настоящий.
fn secret() -> String {
    format!("gh{}_{}", "p", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1eF3hJ5")
}

async fn search(a: &Api, q: &str) -> Value {
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": q, "mode": "chunks"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

fn count(j: &Value) -> usize {
    j["items"].as_array().unwrap().len()
}

#[tokio::test]
async fn secret_in_note_not_in_fts_nor_qdrant_text() {
    let a = api("secrets-note", |_| {}).await;
    let s = secret();
    let content = format!(
        "# Деплой\n\nЗапуск выполняется скриптом deploy.\n\nGITHUB_TOKEN={s}\n\npassword = hunter2hunter2\n"
    );
    let n = a.note("knowledge", "ops/deploy", &content).await;
    let id: Id = n["id"].as_str().unwrap().parse().unwrap();
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    // чанки, которые уходят в эмбеддер и Qdrant: без значений, с маркером
    let chunks = chunks_for_index(&db, id).await.unwrap();
    assert!(!chunks.is_empty());
    for c in &chunks {
        assert!(
            !c.text.contains(&s) && !c.text.contains("hunter2hunter2"),
            "{}",
            c.text
        );
    }
    assert!(chunks.iter().any(|c| c.text.contains("‹secret›")));
    // FTS: по значению ничего, по обычному тексту заметка находится
    assert_eq!(count(&search(&a, &s).await), 0);
    assert_eq!(count(&search(&a, "hunter2hunter2").await), 0);
    assert!(count(&search(&a, "deploy").await) >= 1);
    // и в самой базе (таблица FTS) значения нет
    let hits: i64 = db
        .read({
            let s = s.clone();
            move |c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM chunks WHERE instr(text, ?1) > 0",
                    [s],
                    |r| r.get(0),
                )?)
            }
        })
        .await
        .unwrap();
    assert_eq!(hits, 0);
}

#[tokio::test]
async fn note_read_returns_original() {
    let a = api("secrets-read", |_| {}).await;
    let s = secret();
    let content = format!("# Ключи\n\ntoken: {s}\n");
    let n = a.note("knowledge", "ops/keys", &content).await;
    let id = n["id"].as_str().unwrap();
    let r = a.t(Method::GET, &format!("/api/v1/nodes/{id}"), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["content"], content);
}

#[tokio::test]
async fn write_response_has_secrets_detected() {
    let a = api("secrets-resp", |_| {}).await;
    let s = secret();
    let r = a
        .t(
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "knowledge", "path": "ops/k", "kind": "note",
                        "content": format!("# K\n\nстрока\ntoken: {s}\n")}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = r.json();
    assert!(
        j["warnings"]
            .as_array()
            .unwrap()
            .contains(&json!("secrets_detected"))
    );
    let d = j["secrets_detected"].as_array().unwrap();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["line"], 4);
    assert_eq!(d[0]["kind"], "known_prefix");
    assert!(
        j["secrets_hint"]
            .as_str()
            .unwrap()
            .contains("zam vault set")
    );
    // значение в ответе не повторяется
    assert!(!r.text().contains(&s));
    // чистая заметка — без предупреждения
    let n = a
        .note(
            "knowledge",
            "ops/clean",
            "# Чисто\n\ncommit 9fceb02d0ae598e95dc970b74767f19372d61af8\n",
        )
        .await;
    assert!(
        n.get("secrets_detected").is_none() && n.get("warnings").is_none(),
        "{n}"
    );
    // PATCH тоже предупреждает
    let id = j["id"].as_str().unwrap();
    let r = a
        .t(
            Method::PATCH,
            &format!("/api/v1/nodes/{id}"),
            Some(json!({"base_rev": 1, "ops": [{"op": "append", "content": "\npassword=abcdef123\n"}]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["secrets_detected"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn entry_body_secret_masked() {
    let a = api("secrets-entry", |_| {}).await;
    let s = secret();
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(json!({"kind": "insight", "title": "Утечка токена в логах",
                        "body": format!("В логе нашёлся токен:\nAPI_TOKEN={s}\nисправлено."),
                        "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = r.json();
    assert!(
        j["warnings"]
            .as_array()
            .unwrap()
            .contains(&json!("secrets_detected"))
    );
    assert_eq!(j["secrets_detected"][0]["kind"], "known_prefix");
    // строка отсчитывается в документе записи `# <title>\n\n<body>`
    assert_eq!(j["secrets_detected"][0]["line"], 4);
    // находки и подсказка значения не содержат (само тело записи в ответе — исходное)
    assert!(!j["secrets_detected"].to_string().contains(&s));
    assert!(!j["secrets_hint"].to_string().contains(&s));
    // исходное тело записи не меняется
    assert!(j["body"].as_str().unwrap().contains(&s));
    let id: Id = j["id"].as_str().unwrap().parse().unwrap();
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let chunks = entry_chunks_for_index(&db, id).await.unwrap();
    assert!(!chunks.is_empty());
    assert!(chunks.iter().all(|c| !c.text.contains(&s)));
    assert!(chunks.iter().any(|c| c.text.contains("‹secret›")));
    assert_eq!(count(&search(&a, &s).await), 0);
    assert!(count(&search(&a, "логах").await) >= 1);
    // PATCH записи: предупреждение по итоговому тексту
    let r = a
        .t(
            Method::PATCH,
            &format!("/api/v1/entries/{id}"),
            Some(json!({"append_body": "\npassword: another-secret-1"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["secrets_detected"].as_array().unwrap().len(), 2);
}
