//! Обязательная ревизия записей журналов (задача 2 M4, §11.3, решение владельца 2026-10-26):
//! `PATCH`/`DELETE /entries/{id}` без `If-Match` — 428 `/problems/precondition-required`,
//! расхождение — 409 с `rev` текущей; `supersede`, `retract`, `restore` — `If-Match`
//! необязателен, при наличии проверяется; ревизия проверяется раньше прав `certainty`.
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};

const K: &str = "knowledge";

async fn insight(a: &Api, title: &str) -> Value {
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/spaces/{K}/entries"),
            Some(json!({"kind": "insight", "title": title, "body": "Тело.",
                        "data": {"kind": "discovery"}, "force": true})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()
}

fn id_of(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

/// Запрос первым токеном (агент) с `If-Match: rev` (если задан).
async fn call(a: &Api, m: Method, uri: &str, body: Option<Value>, rev: Option<i64>) -> Resp {
    let rev = rev.map(|r| r.to_string());
    let extra: Vec<(&str, &str)> = rev.iter().map(|r| ("if-match", r.as_str())).collect();
    a.tok_h(&a.token, m, uri, body, &extra).await
}

async fn get(a: &Api, id: &str) -> Value {
    let r = a
        .t(Method::GET, &format!("/api/v1/entries/{id}"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

fn assert_428(r: &Resp) {
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED, "{}", r.text());
    let v = r.json();
    assert_eq!(v["type"], "/problems/precondition-required", "{v}");
    assert_eq!(
        v["detail"], "передайте If-Match: <rev> текущей ревизии",
        "{v}"
    );
}

fn assert_409_rev(r: &Resp, current: i64) {
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let v = r.json();
    assert_eq!(v["type"], "/problems/conflict", "{v}");
    assert_eq!(v["rev"], current, "{v}");
}

#[tokio::test]
async fn entry_patch_without_if_match_428() {
    let a = api("rev-patch-428", |_| {}).await;
    let id = id_of(&insight(&a, "Вывод").await);
    let uri = format!("/api/v1/entries/{id}");
    let r = call(
        &a,
        Method::PATCH,
        &uri,
        Some(json!({"append_body": "x"})),
        None,
    )
    .await;
    assert_428(&r);
    // и смена certainty, и триггеры — тоже
    for body in [
        json!({"certainty": "theorem", "reason": "r"}),
        json!({"triggers": [{"type": "keyword", "pattern": "x"}]}),
    ] {
        assert_428(&call(&a, Method::PATCH, &uri, Some(body), None).await);
    }
    assert_eq!(get(&a, &id).await["rev"], 1, "ничего не записано");
    // с ревизией — 200
    let r = call(
        &a,
        Method::PATCH,
        &uri,
        Some(json!({"append_body": "x"})),
        Some(1),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["rev"], 2);
    // WebUI (сессия) — те же правила
    let r = a
        .adm(Method::PATCH, &uri, Some(json!({"append_body": "y"})), None)
        .await;
    assert_428(&r);
}

#[tokio::test]
async fn entry_delete_without_if_match_428() {
    let a = api("rev-delete-428", |_| {}).await;
    let id = id_of(&insight(&a, "Удаляемый").await);
    let uri = format!("/api/v1/entries/{id}");
    assert_428(&call(&a, Method::DELETE, &uri, None, None).await);
    assert_eq!(get(&a, &id).await["deleted_at"], Value::Null);
    // устаревшая — 409, текущая — 204
    assert_409_rev(&call(&a, Method::DELETE, &uri, None, Some(5)).await, 1);
    let r = call(&a, Method::DELETE, &uri, None, Some(1)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    // restore: If-Match необязателен; устаревший — 409
    let restore = format!("/api/v1/entries/{id}/restore");
    assert_409_rev(&call(&a, Method::POST, &restore, None, Some(9)).await, 1);
    let r = call(&a, Method::POST, &restore, None, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

#[tokio::test]
async fn entry_patch_stale_if_match_409_with_current_rev() {
    let a = api("rev-stale-409", |_| {}).await;
    let id = id_of(&insight(&a, "Гипотеза").await);
    let uri = format!("/api/v1/entries/{id}");
    let r = call(
        &a,
        Method::PATCH,
        &uri,
        Some(json!({"tags": ["a"]})),
        Some(1),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = call(
        &a,
        Method::PATCH,
        &uri,
        Some(json!({"tags": ["b"]})),
        Some(1),
    )
    .await;
    assert_409_rev(&r, 2);
    assert_eq!(get(&a, &id).await["tags"], json!(["a"]));
    // ревизия проверяется раньше прав: смена статуса без причины при устаревшей ревизии — 409,
    // а не 422
    let r = call(
        &a,
        Method::PATCH,
        &uri,
        Some(json!({"certainty": "axiom"})),
        Some(1),
    )
    .await;
    assert_409_rev(&r, 2);
    let r = call(
        &a,
        Method::PATCH,
        &uri,
        Some(json!({"certainty": "axiom"})),
        Some(2),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    // supersede и retract: без If-Match — можно, устаревший — 409
    let sup = format!("/api/v1/entries/{id}/supersede");
    let r = call(
        &a,
        Method::POST,
        &sup,
        Some(json!({"title": "Замена", "data": {"kind": "discovery"}})),
        Some(1),
    )
    .await;
    assert_409_rev(&r, 2);
    let other = id_of(&insight(&a, "Другой вывод").await);
    let retract = format!("/api/v1/entries/{other}/retract");
    let r = call(
        &a,
        Method::POST,
        &retract,
        Some(json!({"reason": "r"})),
        Some(3),
    )
    .await;
    assert_409_rev(&r, 1);
    let r = call(
        &a,
        Method::POST,
        &retract,
        Some(json!({"reason": "r"})),
        Some(1),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = call(
        &a,
        Method::POST,
        &sup,
        Some(json!({"title": "Замена", "data": {"kind": "discovery"}})),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

#[tokio::test]
async fn parallel_patches_one_wins_other_409() {
    let a = api("rev-parallel", |_| {}).await;
    let id = id_of(&insight(&a, "Общая запись").await);
    let uri = format!("/api/v1/entries/{id}");
    for round in 0..5 {
        let rev = get(&a, &id).await["rev"].as_i64().unwrap();
        let (r1, r2) = tokio::join!(
            call(
                &a,
                Method::PATCH,
                &uri,
                Some(json!({"append_body": format!("сессия 1, круг {round}")})),
                Some(rev),
            ),
            call(
                &a,
                Method::PATCH,
                &uri,
                Some(json!({"append_body": format!("сессия 2, круг {round}")})),
                Some(rev),
            ),
        );
        let mut codes = [r1.status, r2.status];
        codes.sort();
        assert_eq!(
            codes,
            [StatusCode::OK, StatusCode::CONFLICT],
            "{} / {}",
            r1.text(),
            r2.text()
        );
        let loser = if r1.status == StatusCode::CONFLICT {
            &r1
        } else {
            &r2
        };
        assert_409_rev(loser, rev + 1);
        assert_eq!(get(&a, &id).await["rev"], rev + 1);
    }
}
