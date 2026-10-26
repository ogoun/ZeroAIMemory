//! Ход перенарезки в `GET /index/status` (§8.2, задача 4 M4). Отдельный тестовый бинарник:
//! счётчик токенов и пороги чанкера — глобальные на процесс, а тесты `index_api.rs` с MiniLM
//! ставят точный токенизатор и ужимают пороги под окно модели — в одном процессе с ними
//! причина перенарезки зависела бы от порядка тестов. Без моделей (FTS).

mod common;

use axum::http::{Method, StatusCode};
use common::api::Api;
use serde_json::{Value, json};
use zam_core::Id;

const NOTE: &str =
    "# Сборка\n\nПроект собирается командой cargo build, тесты идут через cargo test.\n";

fn uid(a: &Api) -> Id {
    a.st.admin_id
}

/// Статус администратора без `X-Zam-User`: все базы.
async fn adm_status(a: &Api) -> Value {
    let r = a.adm(Method::GET, "/api/v1/index/status", None, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Перенарезка после смены настроек нарезки (§8.2): `dbs[].rechunk` — `{reason, changed,
/// total, done}` по каждой базе, пока идёт, затем `null`; токен видит свою базу,
/// администратор — все. Базы «нарезаны с λ = 0,5»: отпечаток и параметры в `meta` подменены.
/// Перенарезка задержана точкой остановки `zam_store::test_hooks::hold_rechunk`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn index_status_shows_rechunk_progress() {
    let c = common::cfg_in(common::tmp("ia-rechunk"));
    let a = common::api::api_with(c.clone()).await;
    a.note("knowledge", "a1", NOTE).await;
    a.note("knowledge", "a2", NOTE).await;
    let (bob, bob_token) = a.new_user("bob").await;
    let r = a
        .tok(
            &bob_token,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "knowledge", "path": "b1", "kind": "note", "content": NOTE})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let users: [Id; 2] = [uid(&a), bob.parse().unwrap()];
    for u in users {
        let db = a.st.users.get(u).await.unwrap();
        db.write(|tx| {
            let fp: String =
                tx.query_row("SELECT value FROM meta WHERE key = 'chunker'", [], |r| {
                    r.get(0)
                })?;
            let params: String = tx.query_row(
                "SELECT value FROM meta WHERE key = 'chunker_params'",
                [],
                |r| r.get(0),
            )?;
            let mut p: Value = serde_json::from_str(&params).unwrap();
            p["chunk_boundary_lambda"] = json!(0.5);
            tx.execute(
                "UPDATE meta SET value = ?1 WHERE key = 'chunker'",
                [fp.replace("lambda=0;", "lambda=0.5;")],
            )?;
            tx.execute(
                "UPDATE meta SET value = ?1 WHERE key = 'chunker_params'",
                [p.to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    }
    let s = adm_status(&a).await;
    assert!(
        s["dbs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["rechunk"].is_null()),
        "до перезапуска перенарезки нет: {s}"
    );
    a.st.shutdown().await;
    drop(a);

    let hold = zam_store::test_hooks::hold_rechunk().await;
    let b = common::api::api_with(c).await;
    let check = |r: &Value, total: u64| {
        assert_eq!(r["reason"], "params", "{r}");
        assert_eq!(
            (r["total"].as_u64(), r["done"].as_u64()),
            (Some(total), Some(0)),
            "{r}"
        );
        assert!(
            r["changed"]
                .as_array()
                .is_some_and(|c| c.contains(&json!("chunk_boundary_lambda"))),
            "{r}"
        );
    };
    // токен — только своя база
    let r = b
        .tok(&bob_token, Method::GET, "/api/v1/index/status", None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let dbs = r.json()["dbs"].clone();
    assert_eq!(dbs.as_array().map(Vec::len), Some(1), "{dbs}");
    assert_eq!(dbs[0]["user_id"], bob, "{dbs}");
    check(&dbs[0]["rechunk"], 1);
    // администратор без X-Zam-User — все базы
    let s = adm_status(&b).await;
    let dbs = s["dbs"].as_array().unwrap();
    assert_eq!(dbs.len(), 2, "{s}");
    for d in dbs {
        let total = if d["user_id"] == bob { 1 } else { 2 };
        check(&d["rechunk"], total);
    }
    drop(hold);
    for u in users {
        b.st.users.wait_rechunk(u).await;
    }
    let s = adm_status(&b).await;
    assert!(
        s["dbs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["rechunk"].is_null()),
        "перенарезка закончилась: {s}"
    );
    b.st.shutdown().await;
}
