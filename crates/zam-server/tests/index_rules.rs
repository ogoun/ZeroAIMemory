//! `rules` в `GET /index/status` (M6 §8, задача 8b) — без моделей.
//!
//! Отдельный набор: счётчик токенов выдачи общий для процесса, и тест с моделью в том же
//! процессе (`index_api.rs`) переключил бы его с оценки на токенизатор модели посреди
//! сравнения сумм L1.

mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, api};
use serde_json::{Value, json};
use zam_core::Id;

fn uid(a: &Api) -> Id {
    a.st.admin_id
}

async fn status(a: &Api) -> Value {
    let r = a.t(Method::GET, "/api/v1/index/status", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Статус администратора без `X-Zam-User`: все пользователи.
async fn adm_status(a: &Api) -> Value {
    let r = a.adm(Method::GET, "/api/v1/index/status", None, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// M6 §8 (задача 8b): `rules` в `GET /index/status` — Rules пользователя запроса `{solution,
/// count, l1_tokens, warning}`, `rules_heavy` — при больше 10 правил или L1 в сумме больше 1500
/// токенов; то же у базы в `dbs[]` (администратору без `X-Zam-User` — только там, `rules` —
/// `null`); `?only=rules` — лёгкий ответ. Без моделей.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn index_status_rules_heavy_shape() {
    let a = api("ia-rules", |_| {}).await;
    let r = a
        .adm(
            Method::PATCH,
            "/api/v1/admin/settings",
            Some(json!({"rules": {"agent_write": true}})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let only = async |sol: Option<&str>| {
        let h: Vec<(&str, &str)> = sol.map(|g| ("x-zam-solution", g)).into_iter().collect();
        let r = a
            .tok_h(
                &a.token,
                Method::GET,
                "/api/v1/index/status?only=rules",
                None,
                &h,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        r.json()
    };
    // правил нет
    assert_eq!(
        only(None).await,
        json!({"rules": {"solution": null, "count": 0, "l1_tokens": 0, "warning": null}})
    );
    // 10 коротких — ещё не много
    for i in 0..10 {
        a.note(
            "rules",
            &format!("п{i}"),
            &format!("# П{i}\n\nКороткое правило {i}.\n"),
        )
        .await;
    }
    let j = only(None).await;
    assert_eq!(j["rules"]["count"], 10, "{j}");
    assert_eq!(j["rules"]["warning"], Value::Null, "{j}");
    let small = j["rules"]["l1_tokens"].as_u64().unwrap();
    assert!(small > 0 && small < 1500, "{j}");
    // кэш L1 по снимку ревизий (ревью 8b): повтор — то же из кэша; правка правила меняет снимок,
    // сумма пересчитывается и совпадает со счётом заново
    let db = a.st.users.get(uid(&a)).await.unwrap();
    let rules_space = zam_store::list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == zam_store::SpaceKind::Rules)
        .unwrap()
        .id;
    let tok = zam_store::budget_tokenizer_id();
    assert_eq!(
        zam_server::api::index::rules_l1_cached(rules_space),
        Some((tok.clone(), small as usize))
    );
    assert_eq!(only(None).await, j);
    // запись другого счётчика (смена оценки на токенизатор модели) при том же снимке ревизий —
    // не отдаётся: сумма пересчитывается текущим счётчиком (повторное ревью 8b)
    zam_server::api::index::rules_l1_cache_poison(rules_space, "другой-счётчик", 1);
    assert_eq!(only(None).await, j);
    assert_eq!(
        zam_server::api::index::rules_l1_cached(rules_space),
        Some((tok.clone(), small as usize))
    );
    let p0 = a
        .tok(
            &a.token,
            Method::GET,
            "/api/v1/nodes/by-path?space=rules&path=п0&format=json",
            None,
        )
        .await
        .json();
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{}", p0["node"]["id"].as_str().unwrap()),
            Some(json!({"content": "# П0\n\nКороткое правило 0, теперь с длинной оговоркой про исключения.\n"})),
            &[("if-match", &p0["rev"].to_string())],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j2 = only(None).await;
    let grown = j2["rules"]["l1_tokens"].as_u64().unwrap();
    assert!(grown > small, "{j2}");
    assert_eq!(
        zam_server::api::index::rules_l1_cached(rules_space),
        Some((tok.clone(), grown as usize))
    );
    // перенос без новой ревизии текста (ревью 8c, п. 10): заголовок «П1» повторял путь «п1» и в L1
    // не входил; у нового пути он не повтор — L1 растёт, снимок кэша (с путём) пересчитывается
    let p1 = a
        .tok(
            &a.token,
            Method::GET,
            "/api/v1/nodes/by-path?space=rules&path=п1&format=json",
            None,
        )
        .await
        .json();
    let r = a
        .tok(
            &a.token,
            Method::POST,
            &format!("/api/v1/nodes/{}/move", p1["node"]["id"].as_str().unwrap()),
            Some(json!({"path": "другое-имя"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j3 = only(None).await;
    let moved = j3["rules"]["l1_tokens"].as_u64().unwrap();
    assert!(moved > grown, "{grown} → {j3}");
    assert_eq!(
        zam_server::api::index::rules_l1_cached(rules_space),
        Some((tok, moved as usize))
    );
    // 11-е — rules_heavy; GUID решения из заголовка — в `solution`
    a.note("rules", "п10", "# П10\n\nКороткое правило 10.\n")
        .await;
    let g = "11111111-2222-4333-8444-555555555555";
    let j = only(Some(g)).await;
    assert_eq!(j["rules"]["count"], 11, "{j}");
    assert_eq!(j["rules"]["warning"], "rules_heavy", "{j}");
    assert_eq!(j["rules"]["solution"], g, "{j}");
    // полный статус: `rules` и `dbs[].rules`
    let s = status(&a).await;
    assert_eq!(s["rules"]["warning"], "rules_heavy", "{s}");
    assert_eq!(s["dbs"][0]["rules"]["count"], 11, "{s}");
    // администратор по всем базам — `rules: null`, у базы — свои
    let s = adm_status(&a).await;
    assert_eq!(s["rules"], Value::Null, "{s}");
    let mine = s["dbs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["user_id"] == json!(uid(&a)))
        .unwrap();
    assert_eq!(mine["rules"]["warning"], "rules_heavy", "{s}");
    // 10 правил (не больше порога), но L1 в сумме больше 1500 токенов — тоже rules_heavy
    let b = api("ia-rules-long", |_| {}).await;
    let r = b
        .adm(
            Method::PATCH,
            "/api/v1/admin/settings",
            Some(json!({"rules": {"agent_write": true}})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let long = "Длинное пояснение правила с подробностями и примерами. ".repeat(40);
    for i in 0..10 {
        b.note("rules", &format!("д{i}"), &format!("# Д{i}\n\n{long}\n"))
            .await;
    }
    let r = b
        .t(Method::GET, "/api/v1/index/status?only=rules", None)
        .await;
    let j = r.json();
    assert_eq!(j["rules"]["count"], 10, "{j}");
    assert!(j["rules"]["l1_tokens"].as_u64().unwrap() > 1500, "{j}");
    assert_eq!(j["rules"]["warning"], "rules_heavy", "{j}");
    // неизвестный `only` — 400
    let r = b.t(Method::GET, "/api/v1/index/status?only=zz", None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}
