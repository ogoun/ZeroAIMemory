//! Changelog и Releases через REST (задача 7 M3, §7.2–7.4): CRUD записей, идемпотентность по
//! автору, ревизии, проверки `data`, сравнение релизов, доступ участника общего решения. Число
//! записей разделов `GET /spaces/{space}/counts` (M7 §5.3, задача 4 M7.2) — против длины лент
//! того же зрителя.
//!
//! Пользователи: A — владелец решения, B — участник. Серверы без моделей (FTS).
mod common;

use std::collections::BTreeSet;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};
use zam_core::Id;
use zam_store::{Attribution, AuthorKind, EntryKind, NewEntry, create_entry, get_entry};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e12";

struct W {
    a: Api,
    a_id: String,
    a_tok: String,
    b_id: String,
    b_tok: String,
}

/// Владелец A регистрирует решение; участник B запрашивает доступ, A одобряет с `access`
/// (если задан). Без `access` участника нет.
async fn world(name: &str, access: Option<&str>) -> W {
    let a = api(&format!("journals-{name}"), |_| {}).await;
    let (a_id, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let w = W {
        a,
        a_id,
        a_tok,
        b_id,
        b_tok,
    };
    let r =
        w.a.tok(
            &w.a_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Общее"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    if let Some(access) = access {
        let r =
            w.a.tok(
                &w.b_tok,
                Method::POST,
                "/api/v1/solutions",
                Some(json!({"agent_solution_id": G, "name": "Общее"})),
            )
            .await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
        let r =
            w.a.tok(
                &w.a_tok,
                Method::POST,
                &format!("/api/v1/solutions/{G}/access/{}/approve", w.b_id),
                Some(json!({ "access": access })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    w
}

impl W {
    async fn post(&self, tok: &str, body: Value, key: Option<&str>) -> Resp {
        let extra: Vec<(&str, &str)> = key.map(|k| ("idempotency-key", k)).into_iter().collect();
        self.a
            .tok_h(
                tok,
                Method::POST,
                &format!("/api/v1/spaces/{G}/entries"),
                Some(body),
                &extra,
            )
            .await
    }

    async fn changelog(&self, kind: &str, title: &str) -> Value {
        let r = self
            .post(
                &self.a_tok,
                json!({"kind": "changelog", "title": title, "body": "Подробности.",
                       "data": {"kind": kind}}),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        r.json()
    }

    async fn release(&self, version: &str, notes: &str) -> Value {
        let r = self
            .post(
                &self.a_tok,
                json!({"kind": "release", "title": format!("Релиз {version}"), "body": notes,
                       "data": {"version": version, "commits": [{"sha": "abc123", "subject": "feat: x"}]}}),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        r.json()
    }

    async fn get(&self, tok: &str, uri: &str) -> Resp {
        self.a.tok(tok, Method::GET, uri, None).await
    }

    async fn compare(&self, id: &str, with: Option<&str>) -> Value {
        let q = with.map(|w| format!("?with={w}")).unwrap_or_default();
        let r = self
            .get(&self.a_tok, &format!("/api/v1/releases/{id}/compare{q}"))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        r.json()
    }
}

fn id_of(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

fn titles(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|e| e["title"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn changelog_crud_idempotency_and_revisions() {
    let w = world("crud", None).await;
    let body = json!({"kind": "changelog", "title": "Добавлен экспорт", "body": "Текст.",
                      "tags": ["export"], "area": "web",
                      "data": {"kind": "feat", "refs": {"commits": ["abc"], "files": ["a.rs"]}}});
    let r = w.post(&w.a_tok, body.clone(), Some("k-1")).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let e = r.json();
    let id = id_of(&e);
    assert_eq!(e["rev"], 1);
    assert_eq!(e["kind"], "changelog");
    // повтор — 200 и та же запись
    let r = w.post(&w.a_tok, body.clone(), Some("k-1")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(id_of(&r.json()), id);
    // тот же ключ с другим запросом — 422 idempotency-mismatch
    let mut other = body.clone();
    other["title"] = json!("Другое");
    let r = w.post(&w.a_tok, other, Some("k-1")).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.json()["type"], "/problems/idempotency-mismatch");

    // чтение и лента
    let r = w.get(&w.a_tok, &format!("/api/v1/entries/{id}")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["data"]["refs"]["files"][0], "a.rs");
    let r = w
        .get(
            &w.a_tok,
            &format!("/api/v1/spaces/{G}/entries?kind=changelog&tag=export&area=web&limit=10"),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(titles(&r.json()["items"]), ["Добавлен экспорт"]);
    assert!(r.json()["next_cursor"].is_null());
    let r = w
        .get(
            &w.a_tok,
            &format!("/api/v1/spaces/{G}/entries?kind=release"),
        )
        .await;
    assert!(r.json()["items"].as_array().unwrap().is_empty());
    let r = w
        .get(&w.a_tok, &format!("/api/v1/spaces/{G}/entries?kind=oops"))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // PATCH с If-Match
    let uri = format!("/api/v1/entries/{id}");
    let r =
        w.a.tok_h(
            &w.a_tok,
            Method::PATCH,
            &uri,
            Some(json!({"title": "Экспорт добавлен", "append_body": "Ещё."})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["rev"], 2);
    assert_eq!(r.json()["body"], "Текст.\n\nЕщё.");
    let r =
        w.a.tok_h(
            &w.a_tok,
            Method::PATCH,
            &uri,
            Some(json!({"title": "Устарело"})),
            &[("if-match", "\"1\"")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    // без If-Match — 428 (§11.3, M4), с ревизией из GET — правка
    let r =
        w.a.tok(&w.a_tok, Method::PATCH, &uri, Some(json!({"tags": ["x"]})))
            .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED, "{}", r.text());
    let r =
        w.a.tok_rev(
            &w.a_tok,
            Method::PATCH,
            &uri,
            Some(json!({"tags": ["export", "v2"], "area": null})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["rev"], 3);
    assert!(r.json()["area"].is_null());
    // неверный data при правке — 422
    let r =
        w.a.tok_rev(
            &w.a_tok,
            Method::PATCH,
            &uri,
            Some(json!({"data": {"kind": "bogus"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    // ревизии
    let r = w.get(&w.a_tok, &format!("{uri}/revisions")).await;
    assert_eq!(r.status, StatusCode::OK);
    let revs = r.json()["items"].as_array().unwrap().clone();
    assert_eq!(revs.len(), 3);
    assert_eq!(revs[0]["title"], "Добавлен экспорт");
    assert_eq!(revs[2]["tags"], json!(["export", "v2"]));

    // удаление: запись не читается, повтор создания по ключу — 409, восстановление возвращает
    let r = w.a.tok_rev(&w.a_tok, Method::DELETE, &uri, None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(w.get(&w.a_tok, &uri).await.status, StatusCode::NOT_FOUND);
    let r = w.post(&w.a_tok, body.clone(), Some("k-1")).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let r =
        w.a.tok(&w.a_tok, Method::POST, &format!("{uri}/restore"), None)
            .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(w.get(&w.a_tok, &uri).await.status, StatusCode::OK);
    // неизвестное поле тела — 400
    let r = w
        .post(
            &w.a_tok,
            json!({"kind": "changelog", "title": "x", "oops": 1}),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn feed_filters_by_data_kind_across_pages() {
    let w = world("datakind", None).await;
    // feat, fix, feat, fix, feat — новые первыми: feat5, fix4, feat3, fix2, feat1
    let mut feats = Vec::new();
    for i in 1..=5 {
        let kind = if i % 2 == 1 { "feat" } else { "fix" };
        let e = w.changelog(kind, &format!("{kind} {i}")).await;
        if kind == "feat" {
            feats.push(id_of(&e));
        }
    }
    feats.reverse();
    // страница из двух записей: третья feat — на второй странице, без пропусков
    let base = format!("/api/v1/spaces/{G}/entries?kind=changelog&data_kind=feat&limit=2");
    let r = w.get(&w.a_tok, &base).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let p1 = r.json();
    let mut got: Vec<String> = p1["items"].as_array().unwrap().iter().map(id_of).collect();
    let cursor = p1["next_cursor"].as_str().expect("курсор").to_string();
    let r = w
        .get(
            &w.a_tok,
            &format!(
                "{base}&cursor={}",
                cursor.replace('|', "%7C").replace(':', "%3A")
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let p2 = r.json();
    got.extend(p2["items"].as_array().unwrap().iter().map(id_of));
    assert!(p2["next_cursor"].is_null());
    assert_eq!(got, feats);
    // неизвестный вид — 400
    let r = w
        .get(
            &w.a_tok,
            &format!("/api/v1/spaces/{G}/entries?data_kind=oops"),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn changelog_kind_validated_422() {
    let w = world("kind", None).await;
    for data in [
        json!({}),
        json!({"kind": "oops"}),
        json!({"kind": "feat", "refs": {"commits": "abc"}}),
        json!({"kind": "feat", "refs": {"issues": vec!["x"; 201]}}),
        json!({"kind": "feat", "refs": {"urls": []}}),
    ] {
        let r = w
            .post(
                &w.a_tok,
                json!({"kind": "changelog", "title": "t", "data": data}),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{data}");
    }
    // release: версия обязательна, дата — YYYY-MM-DD
    for data in [
        json!({}),
        json!({"version": ""}),
        json!({"version": "1", "date": "2026-13-01"}),
    ] {
        let r = w
            .post(
                &w.a_tok,
                json!({"kind": "release", "title": "t", "data": data}),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{data}");
    }
    // дата по умолчанию — сегодня
    let r = w.release("1.0.0", "").await;
    assert_eq!(r["data"]["date"].as_str().unwrap().len(), 10);
    // insight через /entries — со своими правилами data (вид инсайта, а не changelog)
    let r = w
        .post(
            &w.a_tok,
            json!({"kind": "insight", "title": "t", "data": {"kind": "fix"}}),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn release_compare_groups_by_kind_and_diffs_notes() {
    let w = world("compare", None).await;
    w.changelog("feat", "до первого").await;
    let r1 = w.release("1.0.0", "общее\nстарое\n").await;
    w.changelog("feat", "Новая функция").await;
    w.changelog("fix", "Починка").await;
    w.changelog("feat", "Ещё функция").await;
    let r2 = w.release("1.1.0", "общее\nновое\n").await;
    w.changelog("docs", "после второго").await;

    let c = w.compare(&id_of(&r2), None).await;
    assert_eq!(c["from"]["id"], r1["id"]);
    assert_eq!(c["to"]["id"], r2["id"]);
    assert_eq!(
        titles(&c["changelog"]["feat"]),
        ["Новая функция", "Ещё функция"]
    );
    assert_eq!(titles(&c["changelog"]["fix"]), ["Починка"]);
    assert!(c["changelog"].get("docs").is_none());
    let diff = c["notes_diff"].as_str().unwrap();
    assert!(
        diff.contains("-старое") && diff.contains("+новое"),
        "{diff}"
    );
    assert!(diff.contains("1.0.0") && diff.contains("1.1.0"), "{diff}");
    assert_eq!(c["commits"][0]["sha"], "abc123");
    // with=prev равно умолчанию
    let c2 = w.compare(&id_of(&r2), Some("prev")).await;
    assert_eq!(c2["from"]["id"], c["from"]["id"]);
    // не релиз и неизвестная запись
    let ch = w.changelog("fix", "x").await;
    let r = w
        .get(
            &w.a_tok,
            &format!("/api/v1/releases/{}/compare", id_of(&ch)),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = w
        .get(&w.a_tok, &format!("/api/v1/releases/{}/compare", Id::new()))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn compare_with_explicit_id_and_first_release() {
    let w = world("compare-id", None).await;
    w.changelog("ops", "самое раннее").await;
    let r1 = w.release("1.0.0", "a\n").await;
    w.changelog("fix", "между 1 и 2").await;
    let r2 = w.release("2.0.0", "b\n").await;
    w.changelog("feat", "между 2 и 3").await;
    let r3 = w.release("3.0.0", "c\n").await;

    // первый релиз: from = null, журнал с начала, дифф от пустого
    let c = w.compare(&id_of(&r1), None).await;
    assert!(c["from"].is_null());
    assert_eq!(titles(&c["changelog"]["ops"]), ["самое раннее"]);
    assert!(c["notes_diff"].as_str().unwrap().contains("+a"));
    // явный id: r3 против r1 — оба промежутка
    let c = w.compare(&id_of(&r3), Some(&id_of(&r1))).await;
    assert_eq!(c["from"]["id"], r1["id"]);
    assert_eq!(titles(&c["changelog"]["fix"]), ["между 1 и 2"]);
    assert_eq!(titles(&c["changelog"]["feat"]), ["между 2 и 3"]);
    // r3 против r2 — только свой промежуток
    let c = w.compare(&id_of(&r3), Some(&id_of(&r2))).await;
    assert!(c["changelog"].get("fix").is_none());
    assert_eq!(titles(&c["changelog"]["feat"]), ["между 2 и 3"]);
    // запись другого вида или другого пространства в with — не релиз
    let ch = w.changelog("fix", "y").await;
    let r = w
        .get(
            &w.a_tok,
            &format!(
                "/api/v1/releases/{}/compare?with={}",
                id_of(&r3),
                id_of(&ch)
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = w
        .get(
            &w.a_tok,
            &format!("/api/v1/releases/{}/compare?with={}", id_of(&r3), Id::new()),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn release_version_unique_409() {
    let w = world("unique", None).await;
    let r1 = w.release("1.0.0", "").await;
    let r = w
        .post(
            &w.a_tok,
            json!({"kind": "release", "title": "дубль", "data": {"version": " 1.0.0 "}}),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    // повтор исходного запроса по ключу — не конфликт версии
    let body = json!({"kind": "release", "title": "Релиз 2", "data": {"version": "2.0.0"}});
    assert_eq!(
        w.post(&w.a_tok, body.clone(), Some("rel-2")).await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        w.post(&w.a_tok, body, Some("rel-2")).await.status,
        StatusCode::OK
    );
    // смена версии правкой на занятую — 409, на свободную — 200
    let r2 = w.release("3.0.0", "").await;
    let uri = format!("/api/v1/entries/{}", id_of(&r2));
    let r =
        w.a.tok_rev(
            &w.a_tok,
            Method::PATCH,
            &uri,
            Some(json!({"data": {"version": "1.0.0"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r =
        w.a.tok_rev(
            &w.a_tok,
            Method::PATCH,
            &uri,
            Some(json!({"data": {"version": "3.0.1"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // удалённый релиз освобождает версию
    let r =
        w.a.tok_rev(
            &w.a_tok,
            Method::DELETE,
            &format!("/api/v1/entries/{}", id_of(&r1)),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    w.release("1.0.0", "").await;
}

#[tokio::test]
async fn changelog_outside_solution_422() {
    let w = world("outside", None).await;
    for space in ["knowledge", "rules"] {
        let r =
            w.a.tok(
                &w.a_tok,
                Method::POST,
                &format!("/api/v1/spaces/{space}/entries"),
                Some(json!({"kind": "changelog", "title": "t", "data": {"kind": "fix"}})),
            )
            .await;
        // rules токену по умолчанию на запись закрыт; knowledge — 422
        let want = if space == "rules" {
            StatusCode::FORBIDDEN
        } else {
            StatusCode::UNPROCESSABLE_ENTITY
        };
        assert_eq!(r.status, want, "{space}: {}", r.text());
    }
    let r =
        w.a.tok(
            &w.a_tok,
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(json!({"kind": "release", "title": "t", "data": {"version": "1"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    // Human токену не существует
    let r =
        w.a.tok(
            &w.a_tok,
            Method::POST,
            "/api/v1/spaces/human/entries",
            Some(json!({"kind": "changelog", "title": "t", "data": {"kind": "fix"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ro_member_cannot_log_403() {
    let w = world("ro", Some("ro")).await;
    let body = json!({"kind": "changelog", "title": "t", "data": {"kind": "fix"}});
    let r = w.post(&w.b_tok, body.clone(), None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // читать участник ro может
    let e = w.changelog("fix", "виден участнику").await;
    let r = w
        .get(&w.b_tok, &format!("/api/v1/entries/{}", id_of(&e)))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = w
        .get(&w.b_tok, &format!("/api/v1/spaces/{G}/entries"))
        .await;
    assert_eq!(titles(&r.json()["items"]), ["виден участнику"]);
    // править и удалять — нельзя
    let uri = format!("/api/v1/entries/{}", id_of(&e));
    let r =
        w.a.tok_rev(&w.b_tok, Method::PATCH, &uri, Some(json!({"title": "z"})))
            .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = w.a.tok_rev(&w.b_tok, Method::DELETE, &uri, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // токен ro владельца
    let ro = w.a.issue(&w.a_id, "ro").await;
    let r = w.post(&ro, body, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn member_entry_written_to_owner_db_attributed_to_member() {
    let w = world("member", Some("rw")).await;
    let body = json!({"kind": "changelog", "title": "Запись участника", "body": "От Бориса.",
                      "data": {"kind": "feat"}});
    // один и тот же ключ у владельца и участника — разные записи
    let own = w.post(&w.a_tok, body.clone(), Some("same-key")).await;
    assert_eq!(own.status, StatusCode::CREATED);
    let r = w.post(&w.b_tok, body.clone(), Some("same-key")).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let e = r.json();
    assert_ne!(e["id"], own.json()["id"]);
    assert_eq!(e["author"]["user_id"], w.b_id);
    assert_eq!(e["author"]["kind"], "agent");
    // повтор участника — его же запись
    let r = w.post(&w.b_tok, body, Some("same-key")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["id"], e["id"]);

    // запись лежит в базе владельца, а в базе участника записей нет
    let id: Id = id_of(&e).parse().unwrap();
    let odb = w.a.st.users.get(w.a_id.parse().unwrap()).await.unwrap();
    let in_owner = get_entry(&odb, id).await.unwrap();
    assert_eq!(in_owner.author.user_id, Some(w.b_id.parse().unwrap()));
    let bdb = w.a.st.users.get(w.b_id.parse().unwrap()).await.unwrap();
    assert!(get_entry(&bdb, id).await.is_err());

    // владелец видит запись, участник правит свою по id, ревизия — от участника
    let uri = format!("/api/v1/entries/{}", id_of(&e));
    assert_eq!(w.get(&w.a_tok, &uri).await.status, StatusCode::OK);
    let r =
        w.a.tok_rev(
            &w.b_tok,
            Method::PATCH,
            &uri,
            Some(json!({"title": "Правка Бориса"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = w.get(&w.a_tok, &format!("{uri}/revisions")).await;
    assert_eq!(r.json()["items"][1]["author"]["user_id"], w.b_id);
    // после отзыва доступа — 403 уже на следующем запросе
    let r =
        w.a.tok(
            &w.a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{}/revoke", w.b_id),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(w.get(&w.b_tok, &uri).await.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn member_cannot_read_owner_knowledge_entry_404() {
    let w = world("isolation", Some("rw")).await;
    // запись владельца вне пространства решения (Knowledge)
    let odb = w.a.st.users.get(w.a_id.parse().unwrap()).await.unwrap();
    let spaces = zam_store::list_spaces(&odb).await.unwrap();
    let knowledge = spaces
        .iter()
        .find(|s| s.kind == zam_store::SpaceKind::Knowledge)
        .unwrap()
        .id;
    let (secret, _) = create_entry(
        &odb,
        NewEntry {
            refs: None,
            space_id: knowledge,
            kind: EntryKind::Changelog,
            title: "Личная запись владельца".into(),
            body: "секрет".into(),
            data: json!({"kind": "fix"}),
            area: None,
            tags: Vec::new(),
            status: None,
            certainty: None,
            idempotency_key: None,
            triggers: Vec::new(),
            links: Vec::new(),
        },
        &Attribution::new(AuthorKind::Human),
    )
    .await
    .unwrap();
    let uri = format!("/api/v1/entries/{}", secret.id);
    // владелец видит
    assert_eq!(w.get(&w.a_tok, &uri).await.status, StatusCode::OK);
    // участник — нет ни по id, ни через правку, ни через историю, ни сравнением
    assert_eq!(w.get(&w.b_tok, &uri).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        w.get(&w.b_tok, &format!("{uri}/revisions")).await.status,
        StatusCode::NOT_FOUND
    );
    let r =
        w.a.tok_rev(&w.b_tok, Method::PATCH, &uri, Some(json!({"title": "z"})))
            .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(
        w.a.tok_rev(&w.b_tok, Method::DELETE, &uri, None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        w.get(&w.b_tok, &format!("/api/v1/releases/{}/compare", secret.id))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // чужое пространство Knowledge по id пространства — тоже 404
    let r = w
        .get(&w.b_tok, &format!("/api/v1/spaces/{knowledge}/entries"))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn restore_release_with_taken_version_409() {
    let w = world("restore-ver", None).await;
    let r1 = w.release("1.0.0", "старый").await;
    let uri = format!("/api/v1/entries/{}", id_of(&r1));
    let r = w.a.tok_rev(&w.a_tok, Method::DELETE, &uri, None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r2 = w.release("1.0.0", "новый").await;
    let r =
        w.a.tok(&w.a_tok, Method::POST, &format!("{uri}/restore"), None)
            .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    // удалённая остаётся удалённой, живая одна
    assert_eq!(w.get(&w.a_tok, &uri).await.status, StatusCode::NOT_FOUND);
    // освободили версию — восстановление проходит
    let r =
        w.a.tok_rev(
            &w.a_tok,
            Method::DELETE,
            &format!("/api/v1/entries/{}", id_of(&r2)),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r =
        w.a.tok(&w.a_tok, Method::POST, &format!("{uri}/restore"), None)
            .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

#[tokio::test]
async fn release_patch_merges_data_and_retry_without_date_is_idempotent() {
    let w = world("merge", None).await;
    let body = json!({"kind": "release", "title": "Релиз", "body": "",
        "data": {"version": "1.0.0", "git_tag": "v1", "commits": [{"sha": "a", "subject": "s"}]}});
    let r = w.post(&w.a_tok, body.clone(), Some("rel-k")).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let e = r.json();
    let today = e["data"]["date"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/entries/{}", id_of(&e));
    // PATCH меняет только версию: date, git_tag и commits сохраняются
    let r =
        w.a.tok_rev(
            &w.a_tok,
            Method::PATCH,
            &uri,
            Some(json!({"data": {"version": "1.0.1"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let d = &r.json()["data"];
    assert_eq!(d["version"], "1.0.1");
    assert_eq!(d["date"], today.as_str());
    assert_eq!(d["git_tag"], "v1");
    assert_eq!(d["commits"][0]["sha"], "a");
    // смена даты правкой не мешает повтору исходного запроса (как после полуночи UTC)
    let r =
        w.a.tok_rev(
            &w.a_tok,
            Method::PATCH,
            &uri,
            Some(json!({"data": {"date": "2000-01-01"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = w.post(&w.a_tok, body, Some("rel-k")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["id"], e["id"]);
}

#[tokio::test]
async fn empty_title_is_422_and_compare_not_truncated() {
    let w = world("title", None).await;
    for t in ["", "   "] {
        let r = w
            .post(
                &w.a_tok,
                json!({"kind": "changelog", "title": t, "data": {"kind": "fix"}}),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{t:?}");
    }
    let e = w.changelog("fix", "ok").await;
    let r =
        w.a.tok_rev(
            &w.a_tok,
            Method::PATCH,
            &format!("/api/v1/entries/{}", id_of(&e)),
            Some(json!({"title": " "})),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r1 = w.release("1.0.0", "").await;
    assert_eq!(w.compare(&id_of(&r1), None).await["truncated"], false);
}

// ---------- число записей разделов (M7 §5.3, задача 4 M7.2) ----------

/// Журналы WebUI решения и Knowledge — `journalsOf` (`web/src/lib/journals.ts`).
const SOLUTION_JOURNALS: [&str; 10] = [
    "changelog",
    "releases",
    "reports",
    "code",
    "insights",
    "glossary",
    "tasks",
    "ideas",
    "questions",
    "vault",
];
const KNOWLEDGE_JOURNALS: [&str; 6] = [
    "insights",
    "glossary",
    "tasks",
    "ideas",
    "questions",
    "vault",
];

/// Живых записей каждого вида в решении мира чисел ([`counts_world`]); ещё одна — в корзине.
/// Числа разные: перепутанный журнал не совпадёт случайно.
const LIVE: [(&str, usize); 8] = [
    ("changelog", 2),
    ("release", 3),
    ("report", 2),
    ("insight", 2),
    ("term", 3),
    ("task", 4),
    ("idea", 2),
    ("question", 3),
];

/// Зритель: токен агента или сессия WebUI.
#[derive(Clone, Copy)]
enum Who<'a> {
    Tok(&'a str),
    Web(&'a common::reports::Web),
}

async fn get_as(a: &Api, who: Who<'_>, uri: &str) -> Resp {
    match who {
        Who::Tok(t) => a.tok(t, Method::GET, uri, None).await,
        Who::Web(w) => common::reports::web(a, w, Method::GET, uri, None).await,
    }
}

async fn get_ok_as(a: &Api, who: Who<'_>, uri: &str) -> Value {
    let r = get_as(a, who, uri).await;
    assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
    r.json()
}

/// Вид записей журнала WebUI (`JOURNAL_OF_KIND`, `web/src/lib/entryRefs.ts`); у Кода и Vault
/// своих записей журнала нет.
fn kind_of(journal: &str) -> Option<&'static str> {
    Some(match journal {
        "changelog" => "changelog",
        "releases" => "release",
        "reports" => "report",
        "insights" => "insight",
        "glossary" => "term",
        "tasks" => "task",
        "ideas" => "idea",
        "questions" => "question",
        _ => return None,
    })
}

/// Длина ленты журнала `journal` пространства `space` у зрителя `who`: записи — `GET
/// …/entries?kind=` одной страницей, Vault — `GET …/vault`, Код — `files` из `GET /code/status`.
async fn feed_len(a: &Api, who: Who<'_>, space: &str, journal: &str) -> u64 {
    let (uri, key) = match (journal, kind_of(journal)) {
        ("vault", _) => (format!("/api/v1/spaces/{space}/vault"), "items"),
        ("code", _) => (format!("/api/v1/code/status?solution={space}"), "files"),
        (_, Some(k)) => (
            format!("/api/v1/spaces/{space}/entries?kind={k}&limit=200"),
            "items",
        ),
        (j, None) => panic!("журнал {j}"),
    };
    let v = get_ok_as(a, who, &uri).await;
    match &v[key] {
        Value::Array(items) => {
            assert!(v["next_cursor"].is_null(), "{uri}: лента в одну страницу");
            items.len() as u64
        }
        n => n.as_u64().unwrap_or_else(|| panic!("{uri}: {v}")),
    }
}

/// `GET /spaces/{space}/counts` зрителя `who`: ключи журналов — ровно `journals`, число каждого
/// — длина его ленты у того же зрителя ([`feed_len`]).
async fn counts_match_feeds(a: &Api, who: Who<'_>, space: &str, journals: &[&str]) -> Value {
    let c = get_ok_as(a, who, &format!("/api/v1/spaces/{space}/counts")).await;
    let keys: BTreeSet<&str> = c["journals"]
        .as_object()
        .unwrap_or_else(|| panic!("{c}"))
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        journals.iter().copied().collect::<BTreeSet<_>>(),
        "{space}: {c}"
    );
    for j in journals {
        let feed = feed_len(a, who, space, j).await;
        assert_eq!(c["journals"][j].as_u64(), Some(feed), "{space} {j}: {c}");
    }
    c
}

/// Запись вида `kind` с номером `i` токеном `tok` в пространство `space`; id.
async fn log_entry(a: &Api, tok: &str, space: &str, kind: &str, i: usize) -> String {
    let word = ["альфа", "бета", "гамма", "дельта", "эпсилон", "дзета"][i];
    let title = format!("{kind} {word}");
    let body = match kind {
        "changelog" => json!({"kind": kind, "title": title, "body": "Подробности.",
                              "data": {"kind": "feat"}}),
        "release" => json!({"kind": kind, "title": title, "body": "Заметки.",
                            "data": {"version": format!("1.{i}.0"),
                                     "commits": [{"sha": "abc123", "subject": "feat: x"}]}}),
        "insight" => json!({"kind": kind, "title": title, "body": "Суть.",
                            "data": {"kind": "gotcha"}}),
        "term" => json!({"kind": kind, "title": format!("Термин-{word}"), "body": "Подробности.",
                         "data": {"definition": format!("Определение: {word}"), "aliases": []}}),
        "idea" => json!({"kind": kind, "title": title, "body": "Замысел.",
                         "data": {"origin": "agent"}}),
        _ => json!({"kind": kind, "title": title, "body": "Контекст."}),
    };
    let r = a
        .tok(
            tok,
            Method::POST,
            &format!("/api/v1/spaces/{space}/entries"),
            Some(body),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{kind}: {}", r.text());
    id_of(&r.json())
}

/// Статус записи правкой с текущей ревизией.
async fn set_status(a: &Api, tok: &str, id: &str, status: &str) {
    let r = a
        .tok_rev(
            tok,
            Method::PATCH,
            &format!("/api/v1/entries/{id}"),
            Some(json!({"status": status, "reason": "по ходу работы"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{status}: {}", r.text());
}

/// Запись — в корзину сессией владельца (отчёт удаляет только он в WebUI).
async fn trash_entry(a: &Api, web: &common::reports::Web, id: &str) {
    let uri = format!("/api/v1/entries/{id}");
    let rev = get_ok_as(a, Who::Web(web), &uri).await["rev"].to_string();
    let extra = [("if-match", rev.as_str())];
    let r = common::reports::web_h(a, web, Method::DELETE, &uri, None, &extra).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
}

/// Запись Vault окружения (тип `host`, без значения) в области `space`; id.
async fn vault_host(a: &Api, tok: &str, space: &str, title: &str) -> String {
    let r = a
        .tok(
            tok,
            Method::POST,
            &format!("/api/v1/spaces/{space}/vault"),
            Some(json!({"title": title, "data": {"type": "host", "address": "192.0.2.10"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    id_of(&r.json())
}

/// Узел `kind` (`note` | `folder`) по пути `path` пространства `space` зрителем `who`; id.
async fn node(a: &Api, who: Who<'_>, space: &str, path: &str, kind: &str) -> String {
    let mut body = json!({"space": space, "path": path, "kind": kind});
    if kind == "note" {
        body["content"] = json!("Текст заметки.");
    }
    let r = match who {
        Who::Tok(t) => a.tok(t, Method::POST, "/api/v1/nodes", Some(body)).await,
        Who::Web(w) => common::reports::web(a, w, Method::POST, "/api/v1/nodes", Some(body)).await,
    };
    assert_eq!(r.status, StatusCode::CREATED, "{path}: {}", r.text());
    id_of(&r.json())
}

/// Карта кода решения `G` из файлов `paths` (синхронизация манифеста и загрузка).
async fn code_map(a: &Api, tok: &str, paths: &[&str]) {
    let hash = |i: usize| format!("{i:064x}");
    let manifest: Vec<Value> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| json!({"path": p, "hash": hash(i)}))
        .collect();
    let r = a
        .tok(
            tok,
            Method::POST,
            "/api/v1/code/sync",
            Some(json!({"solution": G, "commit": "abcdef1", "branch": "main",
                        "manifest": manifest})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let files: Vec<Value> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            json!({"path": p, "lang": "rust", "hash": hash(i), "size": 100,
                   "parse_state": "ok", "symbols": [], "refs": []})
        })
        .collect();
    let r = a
        .tok(
            tok,
            Method::PUT,
            "/api/v1/code/files",
            Some(json!({"solution": G, "commit": "abcdef1", "branch": "main",
                        "machine": "box", "files": files})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// Мир чисел разделов: владелец `o` решения `G`, участники `rw` и `ro` (у всех — токен и сессия
/// WebUI). В решении — записи каждого вида по [`LIVE`] и ещё по одной в корзине; задачи `open`,
/// `in_progress`, `blocked`, `done`; вопросы `open`, `open`, `obsolete`; карта кода из 3 файлов;
/// Vault владельца 2 и одна в корзине; 4 заметки, папка и заметка в корзине. В Knowledge — по
/// записи каждого вида Knowledge и выполненная задача, Vault 1, заметка 1; в Human — заметка.
async fn counts_world(name: &str) -> common::reports::W {
    let w = common::reports::world(&format!("journals-counts-{name}")).await;
    let (a, tok, web) = (&w.a, w.o.tok.as_str(), &w.o.web);
    for (kind, live) in LIVE {
        let mut ids = Vec::new();
        for i in 0..=live {
            ids.push(if kind == "report" {
                let r = w
                    .report_as(tok, G, &format!("Отчёт {i}"), "# Итог\n\nТекст.", &[])
                    .await;
                assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
                id_of(&r.json())
            } else {
                log_entry(a, tok, G, kind, i).await
            });
        }
        // последняя — в корзину (статус живых не меняется)
        trash_entry(a, web, &ids[live]).await;
        let statuses: &[&str] = match kind {
            "task" => &["open", "in_progress", "blocked", "done"],
            "question" => &["open", "open", "obsolete"],
            _ => &[],
        };
        for (id, s) in ids.iter().zip(statuses) {
            if *s != "open" {
                set_status(a, tok, id, s).await;
            }
        }
    }
    code_map(a, tok, &["src/a.rs", "src/b.rs", "src/c.rs"]).await;
    vault_host(a, tok, G, "стенд").await;
    vault_host(a, tok, G, "сборка").await;
    let gone = vault_host(a, tok, G, "старый стенд").await;
    let r = a
        .tok(tok, Method::DELETE, &format!("/api/v1/vault/{gone}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    node(a, Who::Tok(tok), G, "папка", "folder").await;
    for p in ["папка/первая", "папка/вторая", "третья", "четвёртая"]
    {
        node(a, Who::Tok(tok), G, p, "note").await;
    }
    let gone = node(a, Who::Tok(tok), G, "пятая", "note").await;
    let r = a
        .tok(tok, Method::DELETE, &format!("/api/v1/nodes/{gone}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    // Knowledge: номера 4–5 — термины и прочие заголовки не совпадают с живыми записями решения
    for (kind, i) in [
        ("insight", 5),
        ("term", 5),
        ("task", 4),
        ("task", 5),
        ("idea", 5),
        ("question", 5),
    ] {
        log_entry(a, tok, "knowledge", kind, i).await;
    }
    let tasks = get_ok_as(
        a,
        Who::Tok(tok),
        "/api/v1/spaces/knowledge/entries?kind=task",
    )
    .await;
    set_status(a, tok, &id_of(&tasks["items"][0]), "done").await;
    vault_host(a, tok, "knowledge", "личный стенд").await;
    node(a, Who::Tok(tok), "knowledge", "заметка", "note").await;
    node(a, Who::Web(web), "human", "человеку", "note").await;
    w
}

/// Числа разделов (M7 §5.3 п. 1): у каждого журнала решения и Knowledge число равно длине ленты
/// того же зрителя (сессия владельца), корзина не считается, папка — не заметка; открытые вопросы
/// — `open`, задачи — `open`, `in_progress`, `blocked`. У Human журналов нет — только заметки.
#[tokio::test]
async fn space_counts_by_journal() {
    let w = counts_world("by-journal").await;
    let (a, me) = (&w.a, Who::Web(&w.o.web));
    let c = counts_match_feeds(a, me, G, &SOLUTION_JOURNALS).await;
    assert_eq!(
        c["journals"],
        json!({"changelog": 2, "releases": 3, "reports": 2, "code": 3, "insights": 2,
               "glossary": 3, "tasks": 4, "ideas": 2, "questions": 3, "vault": 2})
    );
    assert_eq!(c["open"], json!({"questions": 2, "tasks": 3}), "{c}");
    assert_eq!(c["notes"], 4, "{c}");
    // то же по id пространства решения
    let spaces = get_ok_as(a, me, "/api/v1/spaces").await;
    let sol = spaces["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["agent_solution_id"] == G)
        .unwrap();
    let by_id = get_ok_as(
        a,
        me,
        &format!("/api/v1/spaces/{}/counts", sol["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(by_id, c);
    let k = counts_match_feeds(a, me, "knowledge", &KNOWLEDGE_JOURNALS).await;
    assert_eq!(
        k["journals"],
        json!({"insights": 1, "glossary": 1, "tasks": 2, "ideas": 1, "questions": 1, "vault": 1})
    );
    assert_eq!(k["open"], json!({"questions": 1, "tasks": 1}), "{k}");
    assert_eq!(k["notes"], 1, "{k}");
    let h = get_ok_as(a, me, "/api/v1/spaces/human/counts").await;
    assert_eq!(
        h,
        json!({"journals": {}, "open": {"questions": 0, "tasks": 0}, "notes": 1})
    );
    let r = get_as(a, me, "/api/v1/spaces/nope/counts").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
}

/// Токену отчёты не видны нигде (M5.5 §7): ключа `reports` нет, прочие числа — как у его лент;
/// сессии того же пользователя — с отчётами; Human токену — 404, как его лента.
#[tokio::test]
async fn space_counts_token_without_reports() {
    let w = counts_world("token").await;
    let a = &w.a;
    let no_reports: Vec<&str> = SOLUTION_JOURNALS
        .into_iter()
        .filter(|j| *j != "reports")
        .collect();
    for tok in [w.o.tok.as_str(), w.ro.tok.as_str()] {
        let c = counts_match_feeds(a, Who::Tok(tok), G, &no_reports).await;
        assert_eq!(c["journals"]["insights"], 2, "{c}");
        assert_eq!(c["open"], json!({"questions": 2, "tasks": 3}), "{c}");
        let r = get_as(a, Who::Tok(tok), "/api/v1/spaces/human/counts").await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    }
    // лента отчётов токену — 400, как и раньше
    let r = get_as(
        a,
        Who::Tok(&w.o.tok),
        &format!("/api/v1/spaces/{G}/entries?kind=report"),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let web = get_ok_as(a, Who::Web(&w.o.web), &format!("/api/v1/spaces/{G}/counts")).await;
    assert_eq!(web["journals"]["reports"], 2, "{web}");
}

/// Участнику — только пространство решения (M7 §5.3 п. 1): числа журналов — как у владельца,
/// Vault — свой (Vault участника лежит в его базе: число равно длине его `GET …/vault`), личные
/// пространства владельца по id — 404, `knowledge` участника — его собственное.
#[tokio::test]
async fn space_counts_member_only_solution() {
    let w = counts_world("member").await;
    let a = &w.a;
    vault_host(a, &w.rw.tok, G, "стенд участника").await;
    let owner = get_ok_as(a, Who::Web(&w.o.web), &format!("/api/v1/spaces/{G}/counts")).await;
    assert_eq!(owner["journals"]["vault"], 2, "{owner}");
    for (who, human, vault) in [
        (Who::Web(&w.ro.web), true, 0),
        (Who::Tok(&w.ro.tok), false, 0),
        (Who::Web(&w.rw.web), true, 1),
    ] {
        let journals: Vec<&str> = SOLUTION_JOURNALS
            .into_iter()
            .filter(|j| human || *j != "reports")
            .collect();
        let c = counts_match_feeds(a, who, G, &journals).await;
        assert_eq!(c["journals"]["vault"], vault, "{c}");
        for j in journals.iter().filter(|j| **j != "vault") {
            assert_eq!(c["journals"][j], owner["journals"][j], "{j}: {c}");
        }
        assert_eq!((&c["open"], &c["notes"]), (&owner["open"], &owner["notes"]));
    }
    // личные пространства владельца по id: владельцу — 200, участнику — 404
    let spaces = get_ok_as(a, Who::Web(&w.o.web), "/api/v1/spaces").await;
    let personal: Vec<&Value> = spaces["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["kind"] != "solution")
        .collect();
    assert_eq!(personal.len(), 3, "{spaces}");
    for s in personal {
        let uri = format!("/api/v1/spaces/{}/counts", s["id"].as_str().unwrap());
        assert_eq!(
            get_as(a, Who::Web(&w.o.web), &uri).await.status,
            StatusCode::OK
        );
        for who in [Who::Web(&w.ro.web), Who::Tok(&w.ro.tok)] {
            let r = get_as(a, who, &uri).await;
            assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}: {}", r.text());
        }
    }
    // `knowledge` участника — его собственное: записей владельца там нет
    let k = counts_match_feeds(a, Who::Web(&w.ro.web), "knowledge", &KNOWLEDGE_JOURNALS).await;
    assert_eq!(k["journals"]["insights"], 0, "{k}");
    assert_eq!(k["notes"], 0, "{k}");
}

/// Карта кода выключена (`code.enabled = false`): её маршруты отвечают 404, журнал «Код» пуст —
/// `code` 0, хотя снимок с файлами в базе остался (выключили после загрузки); ключ журнала есть.
#[tokio::test]
async fn space_counts_code_disabled_zero() {
    let a = api("journals-counts-code-off", |c| c.code.enabled = false).await;
    let (uid, tok) = a.new_user("Анна").await;
    let r = a
        .tok(
            &tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Общее"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let db = a.st.users.get(uid.parse().unwrap()).await.unwrap();
    let at = zam_store::time::fmt_ts(zam_store::time::now());
    db.write(move |tx| {
        tx.execute(
            "INSERT INTO code_snapshots (solution_guid, updated_at, files) VALUES (?1, ?2, 3)",
            [G, at.as_str()],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let r = a
        .tok(
            &tok,
            Method::GET,
            &format!("/api/v1/code/status?solution={G}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let r = a
        .tok(
            &tok,
            Method::GET,
            &format!("/api/v1/spaces/{G}/counts"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["journals"]["code"], 0, "{}", r.text());
}
