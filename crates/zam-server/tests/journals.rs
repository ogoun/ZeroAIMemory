//! Changelog и Releases через REST (задача 7 M3, §7.2–7.4): CRUD записей, идемпотентность по
//! автору, ревизии, проверки `data`, сравнение релизов, доступ участника общего решения.
//!
//! Пользователи: A — владелец решения, B — участник. Серверы без моделей (FTS).
mod common;

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
    // без If-Match правка допустима
    let r =
        w.a.tok(
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
        w.a.tok(
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
    let r = w.a.tok(&w.a_tok, Method::DELETE, &uri, None).await;
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
        w.a.tok(
            &w.a_tok,
            Method::PATCH,
            &uri,
            Some(json!({"data": {"version": "1.0.0"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r =
        w.a.tok(
            &w.a_tok,
            Method::PATCH,
            &uri,
            Some(json!({"data": {"version": "3.0.1"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // удалённый релиз освобождает версию
    let r =
        w.a.tok(
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
        w.a.tok(&w.b_tok, Method::PATCH, &uri, Some(json!({"title": "z"})))
            .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = w.a.tok(&w.b_tok, Method::DELETE, &uri, None).await;
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
        w.a.tok(
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
        w.a.tok(&w.b_tok, Method::PATCH, &uri, Some(json!({"title": "z"})))
            .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(
        w.a.tok(&w.b_tok, Method::DELETE, &uri, None).await.status,
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
    let r = w.a.tok(&w.a_tok, Method::DELETE, &uri, None).await;
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
        w.a.tok(
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
        w.a.tok(
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
        w.a.tok(
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
        w.a.tok(
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
