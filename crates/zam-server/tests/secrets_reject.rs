//! Отказ записи при секрете (M7.5 §8.9 п. 5, признак `reject_secrets`): `reject_secrets: true` в
//! `POST /nodes`, `PUT`/`PATCH /nodes/{id}` — новые находки сканера (как `secrets_detected`)
//! дают 422 `/problems/secrets-detected` с `[{kind, line}]`, ничего не пишется; импорт с
//! `reject_secrets=true` — конфликт `secrets` на файл, остальные пишутся; MCP — поле
//! `reject_secrets` у `memory_write`; без поля — как 0.0.17. Без моделей (FTS).
mod common;

use std::io::{Cursor, Write};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};

/// «Ключ» собирается из частей: в исходниках нет строки, похожей на настоящий.
fn secret() -> String {
    format!("gh{}_{}", "p", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1eF3hJ5")
}

/// Пароль `ключ=значение` — второй вид находки.
fn password() -> String {
    format!("password={}{}", "abcdef", "123")
}

/// 422 отказа: тип, код, находки `[{kind, line}]` без значений.
fn assert_rejected(r: &Resp, kind: &str, line: Option<u64>) {
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    let j = r.json();
    assert_eq!(j["type"], "/problems/secrets-detected", "{j}");
    assert_eq!(j["code"], "secrets_detected", "{j}");
    let d = j["secrets_detected"].as_array().unwrap();
    assert!(
        d.iter()
            .any(|h| h["kind"] == kind && line.is_none_or(|l| h["line"] == l)),
        "{kind}:{line:?} — {j}"
    );
    assert!(
        !r.text().contains(&secret()) && !r.text().contains("abcdef123"),
        "{j}"
    );
    assert!(
        j["detail"].as_str().unwrap().contains("ничего не записано"),
        "{j}"
    );
}

async fn node(a: &Api, id: &str) -> Value {
    let r = a
        .adm(Method::GET, &format!("/api/v1/nodes/{id}"), None, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// `POST`, `PUT` и `PATCH` с `reject_secrets: true` и новым секретом — 422 до записи: заметки
/// нет (тот же путь свободен), ревизия не растёт, текст прежний. Агенту по маске секрет, что был в
/// тексте и раньше, — не новость: правка рядом с ним проходит; сессии WebUI — все находки.
#[tokio::test]
async fn write_reject_secrets_422_nothing_written() {
    let a = api("secrets-reject-write", |_| {}).await;
    let s = secret();
    let body = format!("# K\n\nстрока\ntoken: {s}\n");
    let r = a
        .t(
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "knowledge", "path": "ops/k", "kind": "note",
                        "content": &body, "reject_secrets": true}),
            ),
        )
        .await;
    assert_rejected(&r, "known_prefix", Some(4));
    // путь свободен — заметки нет
    let n = a.note("knowledge", "ops/k", "# K\n\nчисто\n").await;
    let id = n["id"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/nodes/{id}");
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &uri,
            Some(json!({"content": &body, "reject_secrets": true})),
            &[("if-match", "1")],
        )
        .await;
    assert_rejected(&r, "known_prefix", Some(4));
    let r = a
        .t(
            Method::PATCH,
            &uri,
            Some(json!({"base_rev": 1, "reject_secrets": true,
                        "ops": [{"op": "append", "content": format!("\n{}\n", password())}]})),
        )
        .await;
    assert_rejected(&r, "key_value", None);
    let cur = node(&a, &id).await;
    assert_eq!(cur["rev"], 1, "{cur}");
    assert_eq!(cur["content"], "# K\n\nчисто\n", "{cur}");

    // секрет уже в тексте (записан без отказа): агенту — не новость
    let n = a.note("knowledge", "ops/old", &body).await;
    let id = n["id"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/nodes/{id}");
    let r = a
        .t(
            Method::PATCH,
            &uri,
            Some(json!({"base_rev": 1, "reject_secrets": true,
                        "ops": [{"op": "append", "content": "\nещё строка\n"}]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // PUT видом с метками: значение возвращается из-под метки — тоже не новость
    let view = a.t(Method::GET, &uri, None).await.json();
    let marked = view["content"].as_str().unwrap().to_string();
    assert!(!marked.contains(&s), "{marked}");
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &uri,
            Some(json!({"content": format!("{marked}и ещё\n"), "reject_secrets": true})),
            &[("if-match", "2")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        node(&a, &id).await["content"]
            .as_str()
            .unwrap()
            .contains(&s)
    );
    // сессия WebUI правит исходник: находки — все, как в `secrets_detected`
    let r = a
        .adm(
            Method::PATCH,
            &uri,
            Some(json!({"base_rev": 3, "reject_secrets": true,
                        "ops": [{"op": "append", "content": "\nстрока человека\n"}]})),
            None,
        )
        .await;
    assert_rejected(&r, "known_prefix", Some(4));
    assert_eq!(node(&a, &id).await["rev"], 3);
}

/// Без поля или с `reject_secrets: false` — как 0.0.17: записано, находки — в
/// `secrets_detected`; импорт без параметра пишет заметку с секретом и предупреждает. Признаки
/// `recall_m75` и `reject_secrets` — в `/health.features`.
#[tokio::test]
async fn reject_secrets_absent_keeps_017() {
    let a = api("secrets-reject-absent", |_| {}).await;
    free(&a);
    let body = format!("# K\n\nстрока\ntoken: {}\n", secret());
    for (path, flag) in [("ops/a", None), ("ops/b", Some(false))] {
        let mut req = json!({"space": "knowledge", "path": path, "kind": "note", "content": &body});
        if let Some(f) = flag {
            req["reject_secrets"] = json!(f);
        }
        let r = a.t(Method::POST, "/api/v1/nodes", Some(req)).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        let j = r.json();
        assert_eq!(j["secrets_detected"][0]["kind"], "known_prefix", "{j}");
        let id = j["id"].as_str().unwrap();
        let r = a
            .t(
                Method::PATCH,
                &format!("/api/v1/nodes/{id}"),
                Some(json!({"base_rev": 1, "ops": [{"op": "append",
                            "content": format!("\n{}\n", password())}]})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(r.json()["secrets_detected"][0]["kind"], "key_value");
    }
    let zip = zip_of(&[("c.md", body.as_bytes())]);
    let rep = import(&a, "", zip).await;
    assert_eq!(rep["created"], 1, "{rep}");
    assert!(rep["conflicts"].as_array().unwrap().is_empty(), "{rep}");
    assert!(
        rep["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().starts_with("secrets_detected")),
        "{rep}"
    );
    let h = a.t(Method::GET, "/api/v1/health", None).await.json();
    let f = h["features"].as_array().unwrap();
    for want in ["recall_m75", "reject_secrets"] {
        assert!(f.contains(&json!(want)), "{want}: {h}");
    }
}

// ---------- импорт ----------

/// Свободное место импорта — подменённое (резерв места не зависит от диска раннера).
fn free(a: &Api) {
    zam_server::backup::test_hooks::set_free_space(&a.st.cfg.data_dir, Some(1 << 40));
}

fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in files {
        z.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap().into_inner()
}

/// Импорт «папки `.md`» в Knowledge токеном с параметрами `q` (`?…` или пусто): итог задания.
async fn import(a: &Api, q: &str, zip: Vec<u8>) -> Value {
    let r = Request::post(format!("/api/v1/spaces/knowledge/import{q}"))
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::AUTHORIZATION, format!("Bearer {}", a.token))
        .body(Body::from(zip))
        .unwrap();
    let r = a.send(r).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let status = format!("/api/v1{}", r.json()["status"].as_str().unwrap());
    let t0 = std::time::Instant::now();
    loop {
        let j = a.t(Method::GET, &status, None).await.json();
        match j["state"].as_str().unwrap() {
            "running" => {
                assert!(t0.elapsed() < std::time::Duration::from_secs(300), "{j}");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            "done" => return j["report"].clone(),
            _ => panic!("импорт не выполнен: {j}"),
        }
    }
}

/// Живая заметка по пути в базе администратора.
async fn has_note(a: &Api, path: &str) -> bool {
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let p = path.to_string();
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT count(*) FROM nodes WHERE path = ?1 AND deleted_at IS NULL",
            [p],
            |r| r.get::<_, i64>(0),
        )?)
    })
    .await
    .unwrap()
        > 0
}

/// Импорт с `reject_secrets=true`: файл с секретом — конфликт `secrets: <вид> (строка N)` (без
/// значения), не пишется; остальные пишутся; пробный импорт показывает тот же конфликт.
#[tokio::test]
async fn import_reject_secrets_conflict_per_file() {
    let a = api("secrets-reject-import", |_| {}).await;
    free(&a);
    let leak = format!("# Журнал\n\nстрока\ntoken: {}\n", secret());
    let pass = format!("# Стенд\n\n{}\n", password());
    let zip = || {
        zip_of(&[
            ("журнал.md", leak.as_bytes()),
            ("чисто.md", "# Чисто\n\nбез секретов\n".as_bytes()),
            ("стенд/вход.md", pass.as_bytes()),
        ])
    };
    let dry = import(&a, "?reject_secrets=true&dry_run=true", zip()).await;
    assert_eq!(dry["conflicts"].as_array().unwrap().len(), 2, "{dry}");
    let rep = import(&a, "?reject_secrets=true", zip()).await;
    assert_eq!(rep["created"], 1, "{rep}");
    let conflicts = rep["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 2, "{rep}");
    let reason = |path: &str| {
        conflicts
            .iter()
            .find(|c| c["path"] == path)
            .and_then(|c| c["reason"].as_str())
            .unwrap_or_else(|| panic!("нет конфликта {path}: {rep}"))
            .to_string()
    };
    assert_eq!(
        reason("журнал"),
        "secrets: known_prefix (строка 4)",
        "{rep}"
    );
    assert_eq!(
        reason("стенд/вход"),
        "secrets: key_value (строка 3)",
        "{rep}"
    );
    assert!(!rep.to_string().contains(&secret()) && !rep.to_string().contains("abcdef123"));
    assert!(has_note(&a, "чисто").await);
    assert!(!has_note(&a, "журнал").await && !has_note(&a, "стенд/вход").await);
}

// ---------- MCP ----------

/// MCP `tools/call` `memory_write` (решение не задано — Knowledge): ответ JSON-RPC.
async fn mcp_write(a: &Api, args: Value) -> Value {
    let b = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::HOST, "memory.lan:7700")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {}", a.token))
        .header("x-zam-session", "m1");
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": "memory_write", "arguments": args}});
    let r = a.send(b.body(Body::from(body.to_string())).unwrap()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// MCP `memory_write {reject_secrets: true}` с секретом — ошибка с видом и строкой находки,
/// заметки нет; без поля — записано, как прежде; у схемы инструмента — поле `reject_secrets`.
#[tokio::test]
async fn mcp_memory_write_reject_secrets() {
    let a = api("secrets-reject-mcp", |_| {}).await;
    let content = format!("# K\n\nстрока\ntoken: {}\n", secret());
    let j = mcp_write(
        &a,
        json!({"path": "knowledge:/ops/mcp", "content": &content, "reject_secrets": true}),
    )
    .await;
    assert_eq!(j["result"]["isError"], true, "{j}");
    let t = j["result"]["content"][0]["text"].as_str().unwrap();
    assert!(t.contains("known_prefix (строка 4)"), "{t}");
    assert!(!t.contains(&secret()), "{t}");
    assert!(!has_note(&a, "ops/mcp").await);
    let j = mcp_write(
        &a,
        json!({"path": "knowledge:/ops/mcp", "content": &content}),
    )
    .await;
    assert_ne!(j["result"]["isError"], true, "{j}");
    assert!(has_note(&a, "ops/mcp").await);
    let b = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::HOST, "memory.lan:7700")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {}", a.token));
    let body = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let r = a.send(b.body(Body::from(body.to_string())).unwrap()).await;
    let tools = r.json()["result"]["tools"].clone();
    let w = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "memory_write")
        .unwrap();
    assert_eq!(
        w["inputSchema"]["properties"]["reject_secrets"]["type"],
        "boolean"
    );
}
