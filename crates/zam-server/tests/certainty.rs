//! Эпистемический статус `certainty` (задача 9 M3, §7.5, Review Focus 3): матрица прав
//! «агент / человек» на каждом пути записи — insight (создание, PATCH, supersede, retract,
//! delete, restore, триггеры) и заметки (frontmatter при создании, PUT, PATCH, revert, move,
//! delete, restore, вложение), доказательства и причины, человеческий токен, поиск
//! (`certainty` в выдаче и карточке `md`, `filters.certainty`, бусты) и дозаполнение
//! `nodes.certainty` из frontmatter.
//!
//! Агент — первый токен администратора (без флага `human`), человек — cookie-сессия WebUI
//! или токен `{human: true}`. Все записи — в Knowledge администратора; серверы без моделей
//! (поиск — FTS).
mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::api::{Api, Resp, api, api_with};
use serde_json::{Value, json};
use zam_core::Id;

const K: &str = "knowledge";

fn insight(title: &str) -> Value {
    json!({"kind": "insight", "title": title, "body": "Тело вывода.", "data": {"kind": "discovery"},
           "force": true})
}

fn evidence() -> Value {
    json!({"checked": "скрипт на CI", "method": "прогон 3 раза", "result": "воспроизводится"})
}

fn id_of(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("нет id: {v}"))
        .to_string()
}

/// Код нарушения матрицы: статус и `code` problem.
fn assert_certainty(r: &Resp, status: StatusCode, code: &str) {
    assert_eq!(r.status, status, "{}", r.text());
    let v = r.json();
    assert_eq!(v["type"], "/problems/certainty", "{v}");
    assert_eq!(v["code"], code, "{v}");
}

/// Клиент «человек» или «агент» поверх [`Api`].
#[derive(Clone, Copy)]
enum Who<'a> {
    /// Первый токен администратора (агент).
    Agent,
    /// Cookie-сессия WebUI.
    Web,
    /// Человеческий токен.
    Human(&'a str),
}

async fn call(a: &Api, who: Who<'_>, m: Method, uri: &str, body: Option<Value>) -> Resp {
    match who {
        Who::Agent => a.t(m, uri, body).await,
        Who::Web => a.adm(m, uri, body, None).await,
        Who::Human(t) => a.tok(t, m, uri, body).await,
    }
}

async fn call_h(
    a: &Api,
    who: Who<'_>,
    m: Method,
    uri: &str,
    body: Option<Value>,
    extra: &[(&str, &str)],
) -> Resp {
    match who {
        Who::Agent => a.tok_h(&a.token, m, uri, body, extra).await,
        Who::Human(t) => a.tok_h(t, m, uri, body, extra).await,
        Who::Web => {
            let mut b = common::api::req(m, uri, body.as_ref())
                .header(header::COOKIE, &a.cookie)
                .header("x-csrf-token", &a.csrf);
            for (k, v) in extra {
                b = b.header(*k, *v);
            }
            let body = body.map_or_else(Body::empty, |v| Body::from(v.to_string()));
            a.send(b.body(body).unwrap()).await
        }
    }
}

async fn create_insight(a: &Api, who: Who<'_>, body: Value) -> Resp {
    call(
        a,
        who,
        Method::POST,
        &format!("/api/v1/spaces/{K}/entries"),
        Some(body),
    )
    .await
}

async fn created_insight(a: &Api, who: Who<'_>, body: Value) -> Value {
    let r = create_insight(a, who, body).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()
}

async fn patch_entry(a: &Api, who: Who<'_>, id: &str, body: Value) -> Resp {
    call(
        a,
        who,
        Method::PATCH,
        &format!("/api/v1/entries/{id}"),
        Some(body),
    )
    .await
}

async fn get_entry(a: &Api, id: &str) -> Value {
    let r = a
        .adm(Method::GET, &format!("/api/v1/entries/{id}"), None, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

async fn entry_revisions(a: &Api, id: &str) -> Vec<Value> {
    let r = a
        .adm(
            Method::GET,
            &format!("/api/v1/entries/{id}/revisions"),
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["items"].as_array().unwrap().clone()
}

async fn create_note(a: &Api, who: Who<'_>, path: &str, content: &str) -> Resp {
    call(
        a,
        who,
        Method::POST,
        "/api/v1/nodes",
        Some(json!({"space": K, "path": path, "kind": "note", "content": content})),
    )
    .await
}

async fn created_note(a: &Api, who: Who<'_>, path: &str, content: &str) -> Value {
    let r = create_note(a, who, path, content).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()
}

async fn put_note(a: &Api, who: Who<'_>, n: &Value, content: &str, msg: Option<&str>) -> Resp {
    let rev = n["current_rev"].to_string();
    let mut body = json!({ "content": content });
    if let Some(m) = msg {
        body["message"] = json!(m);
    }
    call_h(
        a,
        who,
        Method::PUT,
        &format!("/api/v1/nodes/{}", id_of(n)),
        Some(body),
        &[("if-match", rev.as_str())],
    )
    .await
}

async fn get_node(a: &Api, id: &str) -> Value {
    let r = a
        .adm(
            Method::GET,
            &format!("/api/v1/nodes/{id}?format=json"),
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["node"].clone()
}

/// Человеческий токен администратора, выпущенный в WebUI.
async fn human_token(a: &Api) -> String {
    let r = a
        .adm(
            Method::POST,
            &format!("/api/v1/admin/users/{}/tokens", a.admin_id()),
            Some(json!({"name": "человек", "human": true})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let v = r.json();
    assert_eq!(v["info"]["human"], true, "{v}");
    v["plaintext"].as_str().unwrap().to_string()
}

fn note(certainty: Option<&str>, evidence: Option<&str>, body: &str) -> String {
    let mut fm = String::new();
    if let Some(c) = certainty {
        fm.push_str(&format!("certainty: {c}\n"));
    }
    if let Some(e) = evidence {
        fm.push_str(&format!("evidence: {e}\n"));
    }
    if fm.is_empty() {
        format!("# Заметка\n\n{body}\n")
    } else {
        format!("---\n{fm}---\n# Заметка\n\n{body}\n")
    }
}

#[tokio::test]
async fn agent_cannot_create_axiom_403() {
    let a = api("cert-agent-axiom", |_| {}).await;
    // явный axiom от агента — 403
    let r = create_insight(
        &a,
        Who::Agent,
        json!({"kind": "insight", "title": "Аксиома агента", "data": {"kind": "decision"},
               "certainty": "axiom"}),
    )
    .await;
    assert_certainty(&r, StatusCode::FORBIDDEN, "axiom-requires-human");
    // и через supersedes_id: новая запись axiom
    let h = created_insight(&a, Who::Agent, insight("Гипотеза")).await;
    assert_eq!(h["certainty"], "hypothesis", "по умолчанию у агента: {h}");
    let mut body = insight("Замена аксиомой");
    body["supersedes_id"] = json!(id_of(&h));
    body["certainty"] = json!("axiom");
    let r = create_insight(&a, Who::Agent, body).await;
    assert_certainty(&r, StatusCode::FORBIDDEN, "axiom-requires-human");
    assert_eq!(get_entry(&a, &id_of(&h)).await["status"], "active");
    // заметка с certainty: axiom во frontmatter — 403, ничего не создано
    let r = create_note(&a, Who::Agent, "a/axiom", &note(Some("axiom"), None, "x")).await;
    assert_certainty(&r, StatusCode::FORBIDDEN, "axiom-requires-human");
    let r = a
        .t(
            Method::GET,
            "/api/v1/nodes/by-path?space=knowledge&path=a/axiom",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    // неизвестное значение статуса — 400
    let r = create_note(&a, Who::Agent, "a/bad", &note(Some("certain"), None, "x")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

#[tokio::test]
async fn agent_theorem_without_evidence_422() {
    let a = api("cert-agent-theorem", |_| {}).await;
    let mut body = insight("Теорема без доказательства");
    body["certainty"] = json!("theorem");
    let r = create_insight(&a, Who::Agent, body.clone()).await;
    assert_certainty(&r, StatusCode::UNPROCESSABLE_ENTITY, "evidence-required");
    // неполное доказательство не засчитывается
    body["data"]["evidence"] = json!({"checked": "a", "method": "", "result": "c"});
    let r = create_insight(&a, Who::Agent, body.clone()).await;
    assert_certainty(&r, StatusCode::UNPROCESSABLE_ENTITY, "evidence-required");
    body["data"]["evidence"] = evidence();
    let e = created_insight(&a, Who::Agent, body).await;
    assert_eq!(e["certainty"], "theorem");
    // заметка: theorem агента без evidence: — 422, с ним — 201
    let r = create_note(&a, Who::Agent, "t/no", &note(Some("theorem"), None, "x")).await;
    assert_certainty(&r, StatusCode::UNPROCESSABLE_ENTITY, "evidence-required");
    let n = created_note(
        &a,
        Who::Agent,
        "t/yes",
        &note(Some("theorem"), Some("прогон тестов, 3 из 3"), "x"),
    )
    .await;
    assert_eq!(n["certainty"], "theorem", "{n}");
    // человеку доказательство не нужно
    let n = created_note(&a, Who::Web, "t/web", &note(Some("theorem"), None, "x")).await;
    assert_eq!(n["certainty"], "theorem", "{n}");
}

#[tokio::test]
async fn agent_promotes_with_evidence_history_has_reason() {
    let a = api("cert-promote", |_| {}).await;
    let e = created_insight(&a, Who::Agent, insight("Метод из статьи нерабочий")).await;
    let id = id_of(&e);
    assert_eq!(e["certainty"], "hypothesis");
    // без доказательства — 422
    let r = patch_entry(
        &a,
        Who::Agent,
        &id,
        json!({"certainty": "theorem", "reason": "перепроверил"}),
    )
    .await;
    assert_certainty(&r, StatusCode::UNPROCESSABLE_ENTITY, "evidence-required");
    // с доказательством, но без причины — 422
    let data = json!({"kind": "discovery", "evidence": evidence()});
    let r = patch_entry(
        &a,
        Who::Agent,
        &id,
        json!({"certainty": "theorem", "data": data}),
    )
    .await;
    assert_certainty(&r, StatusCode::UNPROCESSABLE_ENTITY, "reason-required");
    assert_eq!(get_entry(&a, &id).await["rev"], 1, "ничего не записано");
    // доказательство и причина — 200, ревизия с причиной
    let r = patch_entry(
        &a,
        Who::Agent,
        &id,
        json!({"certainty": "theorem", "data": data, "reason": "перепроверка подтвердила"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["certainty"], "theorem");
    let revs = entry_revisions(&a, &id).await;
    let last = revs.last().unwrap();
    assert_eq!(last["certainty"], "theorem");
    assert_eq!(last["reason"], "перепроверка подтвердила");
    assert_eq!(last["author"]["kind"], "agent");
    // доказательство уже в data: следующий перевод его не требует заново (theorem → hypothesis
    // → theorem)
    let r = patch_entry(
        &a,
        Who::Agent,
        &id,
        json!({"certainty": "hypothesis", "reason": "сомнение"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = patch_entry(
        &a,
        Who::Agent,
        &id,
        json!({"certainty": "theorem", "reason": "снова подтверждено"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // заметка: hypothesis → theorem — evidence: и сообщение ревизии
    let n = created_note(&a, Who::Agent, "p/n", &note(Some("hypothesis"), None, "x")).await;
    let r = put_note(
        &a,
        Who::Agent,
        &n,
        &note(Some("theorem"), Some("прогон CI"), "x"),
        None,
    )
    .await;
    assert_certainty(&r, StatusCode::UNPROCESSABLE_ENTITY, "reason-required");
    let r = put_note(
        &a,
        Who::Agent,
        &n,
        &note(Some("theorem"), None, "x"),
        Some("подтверждено"),
    )
    .await;
    assert_certainty(&r, StatusCode::UNPROCESSABLE_ENTITY, "evidence-required");
    let r = put_note(
        &a,
        Who::Agent,
        &n,
        &note(Some("theorem"), Some("прогон CI"), "x"),
        Some("подтверждено прогоном"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["certainty"], "theorem");
    let r = a
        .t(
            Method::GET,
            &format!("/api/v1/nodes/{}/revisions", id_of(&n)),
            None,
        )
        .await;
    let revs = r.json();
    assert_eq!(revs[0]["message"], "подтверждено прогоном", "{revs}");
}

#[tokio::test]
async fn agent_demotes_theorem_with_reason() {
    let a = api("cert-demote", |_| {}).await;
    let mut body = insight("Теорема");
    body["certainty"] = json!("theorem");
    body["data"]["evidence"] = evidence();
    let id = id_of(&created_insight(&a, Who::Agent, body).await);
    let r = patch_entry(&a, Who::Agent, &id, json!({"certainty": "hypothesis"})).await;
    assert_certainty(&r, StatusCode::UNPROCESSABLE_ENTITY, "reason-required");
    // снять статус у theorem агент не может
    let r = patch_entry(
        &a,
        Who::Agent,
        &id,
        json!({"certainty": null, "reason": "r"}),
    )
    .await;
    assert_certainty(&r, StatusCode::UNPROCESSABLE_ENTITY, "invalid-transition");
    let r = patch_entry(
        &a,
        Who::Agent,
        &id,
        json!({"certainty": "hypothesis", "reason": "параметры были неверны"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["certainty"], "hypothesis");
    let revs = entry_revisions(&a, &id).await;
    assert_eq!(revs.last().unwrap()["reason"], "параметры были неверны");
    // у hypothesis — снимает, с причиной
    let r = patch_entry(
        &a,
        Who::Agent,
        &id,
        json!({"certainty": null, "reason": "не о надёжности"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["certainty"], Value::Null);

    // заметка: theorem → hypothesis с сообщением
    let n = created_note(
        &a,
        Who::Agent,
        "d/n",
        &note(Some("theorem"), Some("CI"), "x"),
    )
    .await;
    let r = put_note(
        &a,
        Who::Agent,
        &n,
        &note(Some("hypothesis"), None, "x"),
        None,
    )
    .await;
    assert_certainty(&r, StatusCode::UNPROCESSABLE_ENTITY, "reason-required");
    let r = put_note(&a, Who::Agent, &n, &note(None, None, "x"), Some("снять")).await;
    assert_certainty(&r, StatusCode::UNPROCESSABLE_ENTITY, "invalid-transition");
    let r = put_note(
        &a,
        Who::Agent,
        &n,
        &note(Some("hypothesis"), None, "x"),
        Some("пересмотр"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["certainty"], "hypothesis");
}

#[tokio::test]
async fn agent_cannot_patch_supersede_retract_axiom_403() {
    let a = api("cert-axiom-entry", |_| {}).await;
    // insight человека без явного статуса — axiom
    let e = created_insight(&a, Who::Web, insight("Правило владельца")).await;
    assert_eq!(e["certainty"], "axiom", "{e}");
    assert_eq!(e["author"]["kind"], "human");
    let id = id_of(&e);
    let forbidden = |r: Resp| assert_certainty(&r, StatusCode::FORBIDDEN, "axiom-requires-human");
    for body in [
        json!({"body": "переписано агентом"}),
        json!({"append_body": "дописано"}),
        json!({"title": "Новый заголовок"}),
        json!({"tags": ["x"]}),
        json!({"status": "revisit", "reason": "r"}),
        json!({"certainty": "hypothesis", "reason": "r"}),
        json!({"certainty": null, "reason": "r"}),
        json!({"triggers": [{"type": "keyword", "pattern": "x"}]}),
        json!({}),
    ] {
        forbidden(patch_entry(&a, Who::Agent, &id, body).await);
    }
    // замещение — эндпоинтом и через supersedes_id
    forbidden(
        call(
            &a,
            Who::Agent,
            Method::POST,
            &format!("/api/v1/entries/{id}/supersede"),
            Some(json!({"title": "Замена", "data": {"kind": "discovery"}})),
        )
        .await,
    );
    let mut body = insight("Замена");
    body["supersedes_id"] = json!(id);
    forbidden(create_insight(&a, Who::Agent, body).await);
    // отзыв и удаление
    forbidden(
        call(
            &a,
            Who::Agent,
            Method::POST,
            &format!("/api/v1/entries/{id}/retract"),
            Some(json!({"reason": "кажется, неверно"})),
        )
        .await,
    );
    forbidden(
        call(
            &a,
            Who::Agent,
            Method::DELETE,
            &format!("/api/v1/entries/{id}"),
            None,
        )
        .await,
    );
    let cur = get_entry(&a, &id).await;
    assert_eq!(
        (&cur["rev"], &cur["status"], &cur["certainty"]),
        (&json!(1), &json!("active"), &json!("axiom")),
        "запись не изменилась: {cur}"
    );
    assert_eq!(cur["triggers"], json!([]));
    // удалённую человеком axiom агент не восстанавливает
    let r = call(
        &a,
        Who::Web,
        Method::DELETE,
        &format!("/api/v1/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    forbidden(
        call(
            &a,
            Who::Agent,
            Method::POST,
            &format!("/api/v1/entries/{id}/restore"),
            None,
        )
        .await,
    );
    // человек — может: восстановить, изменить, заместить
    let r = call(
        &a,
        Who::Web,
        Method::POST,
        &format!("/api/v1/entries/{id}/restore"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = patch_entry(&a, Who::Web, &id, json!({"append_body": "уточнение"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = call(
        &a,
        Who::Web,
        Method::POST,
        &format!("/api/v1/entries/{id}/supersede"),
        Some(json!({"title": "Новое правило", "data": {"kind": "decision"}})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["entry"]["certainty"], "axiom");
    // замещённую axiom агент не отзывает (отзыв superseded разрешён — но не агенту)
    forbidden(
        call(
            &a,
            Who::Agent,
            Method::POST,
            &format!("/api/v1/entries/{id}/retract"),
            Some(json!({"reason": "r"})),
        )
        .await,
    );
}

#[tokio::test]
async fn agent_note_axiom_frontmatter_403_and_axiom_note_write_move_delete_403() {
    let a = api("cert-axiom-note", |_| {}).await;
    let forbidden = |r: Resp| assert_certainty(&r, StatusCode::FORBIDDEN, "axiom-requires-human");
    // перевод своей заметки в axiom правкой frontmatter — 403
    let h = created_note(
        &a,
        Who::Agent,
        "own/h",
        &note(Some("hypothesis"), None, "x"),
    )
    .await;
    forbidden(
        put_note(
            &a,
            Who::Agent,
            &h,
            &note(Some("axiom"), None, "x"),
            Some("я уверен"),
        )
        .await,
    );
    let n0 = created_note(&a, Who::Agent, "own/plain", &note(None, None, "x")).await;
    forbidden(
        put_note(
            &a,
            Who::Agent,
            &n0,
            &note(Some("axiom"), None, "x"),
            Some("m"),
        )
        .await,
    );
    // PATCH секций, добавляющий frontmatter в начало, — тоже
    let r = call(
        &a,
        Who::Agent,
        Method::PATCH,
        &format!("/api/v1/nodes/{}", id_of(&n0)),
        Some(json!({"base_rev": 1, "message": "m",
                    "ops": [{"op": "prepend", "content": "---\ncertainty: axiom\n---\n"}]})),
    )
    .await;
    assert!(
        r.status == StatusCode::FORBIDDEN || r.status == StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        r.text()
    );
    assert_eq!(get_node(&a, &id_of(&n0)).await["certainty"], Value::Null);

    // заметка axiom человека в папке
    let ax = created_note(
        &a,
        Who::Web,
        "rules/core/ax",
        &note(Some("axiom"), None, "Правило."),
    )
    .await;
    assert_eq!(ax["certainty"], "axiom");
    let ax_id = id_of(&ax);
    forbidden(
        put_note(
            &a,
            Who::Agent,
            &ax,
            &note(Some("axiom"), None, "Иначе."),
            None,
        )
        .await,
    );
    forbidden(
        put_note(
            &a,
            Who::Agent,
            &ax,
            &note(Some("hypothesis"), None, "x"),
            Some("m"),
        )
        .await,
    );
    // устаревшая ревизия тоже 403 (проверка до конфликта ревизий)
    let mut stale = ax.clone();
    stale["current_rev"] = json!(7);
    forbidden(put_note(&a, Who::Agent, &stale, "x", None).await);
    forbidden(
        call(
            &a,
            Who::Agent,
            Method::PATCH,
            &format!("/api/v1/nodes/{ax_id}"),
            Some(json!({"base_rev": 1, "ops": [{"op": "append", "content": "\nдописано\n"}]})),
        )
        .await,
    );
    forbidden(
        call(
            &a,
            Who::Agent,
            Method::POST,
            &format!("/api/v1/nodes/{ax_id}/move"),
            Some(json!({"path": "elsewhere/ax"})),
        )
        .await,
    );
    forbidden(
        call(
            &a,
            Who::Agent,
            Method::DELETE,
            &format!("/api/v1/nodes/{ax_id}"),
            None,
        )
        .await,
    );
    forbidden(
        call(
            &a,
            Who::Agent,
            Method::POST,
            &format!("/api/v1/nodes/{ax_id}/revert"),
            Some(json!({"rev": 1})),
        )
        .await,
    );
    // папка с заметкой axiom: перенос и удаление поддерева — 403
    let folder = a
        .t(
            Method::GET,
            "/api/v1/nodes/by-path?space=knowledge&path=rules&format=json",
            None,
        )
        .await
        .json();
    let fid = id_of(&folder["node"]);
    forbidden(
        call(
            &a,
            Who::Agent,
            Method::POST,
            &format!("/api/v1/nodes/{fid}/move"),
            Some(json!({"path": "old-rules"})),
        )
        .await,
    );
    forbidden(
        call(
            &a,
            Who::Agent,
            Method::DELETE,
            &format!("/api/v1/nodes/{fid}"),
            None,
        )
        .await,
    );
    // вложение к заметке axiom — 403
    const B: &str = "zam-cert-boundary";
    let mp = format!(
        "--{B}\r\nContent-Disposition: form-data; name=\"node\"\r\n\r\n{ax_id}\r\n--{B}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\n\
         Content-Type: text/plain\r\n\r\nx\r\n--{B}--\r\n"
    );
    let r = a
        .send(
            Request::post("/api/v1/blobs")
                .header(header::AUTHORIZATION, format!("Bearer {}", a.token))
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={B}"),
                )
                .body(Body::from(mp))
                .unwrap(),
        )
        .await;
    forbidden(r);
    let cur = get_node(&a, &ax_id).await;
    assert_eq!(
        (&cur["current_rev"], &cur["path"], &cur["deleted_at"]),
        (&json!(1), &json!("rules/core/ax"), &Value::Null),
        "{cur}"
    );
    // удалённую человеком заметку axiom агент не восстанавливает
    let r = call(
        &a,
        Who::Web,
        Method::DELETE,
        &format!("/api/v1/nodes/{ax_id}"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    forbidden(
        call(
            &a,
            Who::Agent,
            Method::POST,
            &format!("/api/v1/nodes/{ax_id}/restore"),
            None,
        )
        .await,
    );
    // человек — восстанавливает, правит и переносит
    let r = call(
        &a,
        Who::Web,
        Method::POST,
        &format!("/api/v1/nodes/{ax_id}/restore"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = put_note(
        &a,
        Who::Web,
        &ax,
        &note(Some("axiom"), None, "Уточнено."),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = call(
        &a,
        Who::Web,
        Method::POST,
        &format!("/api/v1/nodes/{ax_id}/move"),
        Some(json!({"path": "rules/ax"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // заметки без axiom агент по-прежнему правит, переносит и удаляет
    let r = call(
        &a,
        Who::Agent,
        Method::POST,
        &format!("/api/v1/nodes/{}/move", id_of(&h)),
        Some(json!({"path": "own/h2"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = call(
        &a,
        Who::Agent,
        Method::DELETE,
        &format!("/api/v1/nodes/{}", id_of(&n0)),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
}

#[tokio::test]
async fn human_token_can_set_and_change_axiom() {
    let a = api("cert-human-token", |_| {}).await;
    let ht = human_token(&a).await;
    let who = Who::Human(&ht);
    let mut body = insight("Аксиома по человеческому токену");
    body["certainty"] = json!("axiom");
    let e = created_insight(&a, who, body).await;
    assert_eq!(
        (&e["certainty"], &e["author"]["kind"]),
        (&json!("axiom"), &json!("human"))
    );
    let id = id_of(&e);
    let r = patch_entry(&a, who, &id, json!({"append_body": "уточнение"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // смена статуса — с причиной
    let r = patch_entry(&a, who, &id, json!({"certainty": "hypothesis"})).await;
    assert_certainty(&r, StatusCode::UNPROCESSABLE_ENTITY, "reason-required");
    let r = patch_entry(
        &a,
        who,
        &id,
        json!({"certainty": "hypothesis", "reason": "условия изменились"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = patch_entry(
        &a,
        who,
        &id,
        json!({"certainty": "axiom", "reason": "подтверждено владельцем"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let revs = entry_revisions(&a, &id).await;
    let last = revs.last().unwrap();
    assert_eq!(
        (&last["certainty"], &last["reason"], &last["author"]["kind"]),
        (
            &json!("axiom"),
            &json!("подтверждено владельцем"),
            &json!("human")
        )
    );
    // заметка axiom: создание, правка, перенос, удаление
    let n = created_note(&a, who, "h/ax", &note(Some("axiom"), None, "x")).await;
    assert_eq!(n["certainty"], "axiom");
    let r = put_note(&a, who, &n, &note(Some("axiom"), None, "y"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = call(
        &a,
        who,
        Method::POST,
        &format!("/api/v1/nodes/{}/move", id_of(&n)),
        Some(json!({"path": "h/ax2"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = call(
        &a,
        who,
        Method::DELETE,
        &format!("/api/v1/nodes/{}", id_of(&n)),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    // после отзыва человеческого токена — 401
    let tokens = a
        .adm(
            Method::GET,
            &format!("/api/v1/admin/users/{}/tokens", a.admin_id()),
            None,
            None,
        )
        .await
        .json();
    let tid = tokens
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["human"] == true)
        .map(|t| t["id"].as_str().unwrap().to_string())
        .unwrap();
    let r = a
        .adm(
            Method::DELETE,
            &format!("/api/v1/admin/tokens/{tid}"),
            None,
            None,
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let r = patch_entry(&a, who, &id, json!({"append_body": "z"})).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text());
}

#[tokio::test]
async fn webui_admin_can_set_axiom() {
    let a = api("cert-webui", |_| {}).await;
    let e = created_insight(&a, Who::Agent, insight("Вывод агента")).await;
    let id = id_of(&e);
    let r = patch_entry(&a, Who::Web, &id, json!({"certainty": "axiom"})).await;
    assert_certainty(&r, StatusCode::UNPROCESSABLE_ENTITY, "reason-required");
    let r = patch_entry(
        &a,
        Who::Web,
        &id,
        json!({"certainty": "axiom", "reason": "проверено владельцем"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["certainty"], "axiom");
    let revs = entry_revisions(&a, &id).await;
    assert_eq!(revs.last().unwrap()["author"]["kind"], "human");
    // теперь агент её не меняет
    let r = patch_entry(&a, Who::Agent, &id, json!({"append_body": "x"})).await;
    assert_certainty(&r, StatusCode::FORBIDDEN, "axiom-requires-human");
    // заметка: перевод в axiom в редакторе WebUI — с сообщением ревизии
    let n = created_note(&a, Who::Agent, "w/n", &note(Some("hypothesis"), None, "x")).await;
    let r = put_note(&a, Who::Web, &n, &note(Some("axiom"), None, "x"), None).await;
    assert_certainty(&r, StatusCode::UNPROCESSABLE_ENTITY, "reason-required");
    let r = put_note(
        &a,
        Who::Web,
        &n,
        &note(Some("axiom"), None, "x"),
        Some("утверждено"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["certainty"], "axiom");
    // /auth/me — человек
    let me = a
        .adm(Method::GET, "/api/v1/auth/me", None, None)
        .await
        .json();
    assert_eq!(me["human"], true, "{me}");
}

#[tokio::test]
async fn human_token_issued_only_by_admin() {
    let a = api("cert-issue", |_| {}).await;
    let (uid, utok) = a.new_user("Анна").await;
    // по токену (даже администратора) выпуск — 403
    for tok in [&a.token, &utok] {
        let r = a
            .tok(
                tok,
                Method::POST,
                &format!("/api/v1/admin/users/{uid}/tokens"),
                Some(json!({"name": "x", "human": true})),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    }
    // администратор WebUI — 201, токен человеческий; обычный — нет
    let r = a
        .adm(
            Method::POST,
            &format!("/api/v1/admin/users/{uid}/tokens"),
            Some(json!({"name": "ноутбук владельца", "human": true})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let v = r.json();
    assert_eq!(v["info"]["human"], true);
    let ht = v["plaintext"].as_str().unwrap().to_string();
    let plain = a.issue(&uid, "rw").await;
    // аудит token_issue с human
    let audit = a
        .adm(Method::GET, "/api/v1/admin/audit?limit=10", None, None)
        .await
        .json();
    let issues: Vec<&Value> = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["action"] == "token_issue")
        .collect();
    assert!(
        issues.iter().any(|r| r["details"]["human"] == true),
        "{audit}"
    );
    assert!(
        issues.iter().any(|r| r["details"]["human"] == false),
        "{audit}"
    );
    // GET /spaces: human по токену
    for (tok, want) in [(&ht, true), (&plain, false), (&utok, false)] {
        let s = a.tok(tok, Method::GET, "/api/v1/spaces", None).await.json();
        assert_eq!(s["human"], want, "{s}");
    }
    let s = a
        .adm(Method::GET, "/api/v1/spaces", None, None)
        .await
        .json();
    assert_eq!(s["human"], true, "{s}");
    // человеческий токен не даёт прав администратора
    let r = a.tok(&ht, Method::GET, "/api/v1/admin/users", None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
}

async fn search(a: &Api, body: Value) -> Resp {
    let r = a.t(Method::POST, "/api/v1/search", Some(body)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r
}

/// `(id цели, certainty)` элементов выдачи по порядку.
fn items(r: &Resp) -> Vec<(String, Value)> {
    r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            let id = i["node_id"].as_str().or(i["entry_id"].as_str()).unwrap();
            (id.to_string(), i["certainty"].clone())
        })
        .collect()
}

#[tokio::test]
async fn search_shows_certainty_and_filters() {
    let a = api("cert-search", |_| {}).await;
    let ax = created_note(
        &a,
        Who::Web,
        "s/ax",
        &note(Some("axiom"), None, "квазар правило"),
    )
    .await;
    let hy = created_note(
        &a,
        Who::Agent,
        "s/hy",
        &note(Some("hypothesis"), None, "квазар гипотеза"),
    )
    .await;
    let plain = created_note(&a, Who::Agent, "s/pl", &note(None, None, "квазар просто")).await;
    let mut body = insight("квазар вывод");
    body["certainty"] = json!("theorem");
    body["data"]["evidence"] = evidence();
    let th = created_insight(&a, Who::Agent, body).await;

    let r = search(&a, json!({"query": "квазар", "top_k": 20})).await;
    let got = items(&r);
    let find = |id: &str| got.iter().find(|(i, _)| i == id).map(|(_, c)| c.clone());
    assert_eq!(find(&id_of(&ax)), Some(json!("axiom")), "{got:?}");
    assert_eq!(find(&id_of(&hy)), Some(json!("hypothesis")));
    assert_eq!(find(&id_of(&plain)), Some(Value::Null));
    assert_eq!(find(&id_of(&th)), Some(json!("theorem")));

    // filters.certainty: только указанные статусы, цели без статуса не проходят
    let r = search(
        &a,
        json!({"query": "квазар", "top_k": 20, "filters": {"certainty": ["axiom", "theorem"]}}),
    )
    .await;
    let mut ids: Vec<String> = items(&r).into_iter().map(|(i, _)| i).collect();
    ids.sort();
    let mut want = vec![id_of(&ax), id_of(&th)];
    want.sort();
    assert_eq!(ids, want);
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "квазар", "filters": {"certainty": ["sure"]}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    // карточка md: статус заметки и записи
    let r = search(&a, json!({"query": "квазар", "top_k": 20, "format": "md"})).await;
    let md = r.text();
    let line = |path: &str| {
        md.lines()
            .find(|l| l.contains(path))
            .unwrap_or_else(|| panic!("нет {path}: {md}"))
            .to_string()
    };
    assert!(line("s/ax").contains("rev1 · axiom ·"), "{md}");
    assert!(line("s/hy").contains("rev1 · hypothesis ·"), "{md}");
    assert!(!line("s/pl").contains("axiom"), "{md}");
    assert!(line("квазар вывод").contains("theorem"), "{md}");

    // GET /triggers отдаёт certainty
    let mut body = insight("триггер квазара");
    body["triggers"] = json!([{"type": "keyword", "pattern": "квазар"}]);
    created_insight(&a, Who::Agent, body).await;
    let t = a.t(Method::GET, "/api/v1/triggers", None).await;
    assert_eq!(t.status, StatusCode::OK, "{}", t.text());
    let t = t.json();
    let item = t["triggers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["title"] == "триггер квазара")
        .cloned()
        .unwrap_or_else(|| panic!("{t}"));
    assert_eq!(item["certainty"], "hypothesis", "{item}");
}

#[tokio::test]
async fn axiom_boost_orders_above_equal_hypothesis() {
    let a = api("cert-boost", |_| {}).await;
    // одинаковые тексты и пути равной длины; порядок создания — в обе стороны
    for (word, axiom_first) in [("пульсар", true), ("магнетар", false)] {
        let body = format!("{word} одинаковый текст заметки");
        let make = async |who: Who<'_>, path: &str, c: &str| {
            id_of(&created_note(&a, who, path, &note(Some(c), None, &body)).await)
        };
        let (ax, hy) = if axiom_first {
            let ax = make(Who::Web, &format!("{word}/a1"), "axiom").await;
            let hy = make(Who::Agent, &format!("{word}/a2"), "hypothesis").await;
            (ax, hy)
        } else {
            let hy = make(Who::Agent, &format!("{word}/a1"), "hypothesis").await;
            let ax = make(Who::Web, &format!("{word}/a2"), "axiom").await;
            (ax, hy)
        };
        let r = search(&a, json!({"query": word, "top_k": 5})).await;
        let order: Vec<String> = items(&r).into_iter().map(|(i, _)| i).collect();
        assert_eq!(order, vec![ax.clone(), hy.clone()], "{word}: {}", r.text());
        // балл в ответе ограничен 1 (нормировка — задача 18), порядок — по полному баллу
        let s = r.json();
        assert!(s["items"][0]["score"].as_f64().unwrap() <= 1.0);
    }
    // insight человека (axiom) выше равного insight агента (hypothesis)
    let hy = created_insight(&a, Who::Agent, insight("квазиморфизм вывод")).await;
    let ax = created_insight(&a, Who::Web, insight("квазиморфизм вывод")).await;
    let r = search(&a, json!({"query": "квазиморфизм", "top_k": 5})).await;
    let order: Vec<String> = items(&r).into_iter().map(|(i, _)| i).collect();
    assert_eq!(order, vec![id_of(&ax), id_of(&hy)], "{}", r.text());
}

#[tokio::test]
async fn certainty_backfill_from_frontmatter() {
    let dir = common::tmp("cert-backfill");
    let c = common::cfg_in(dir);
    let a = api_with(c.clone()).await;
    let ax = id_of(&created_note(&a, Who::Web, "b/ax", &note(Some("axiom"), None, "x")).await);
    let th = id_of(
        &created_note(
            &a,
            Who::Agent,
            "b/th",
            &note(Some("theorem"), Some("CI"), "x"),
        )
        .await,
    );
    let pl = id_of(&created_note(&a, Who::Agent, "b/pl", &note(None, None, "x")).await);
    let gone = id_of(
        &created_note(
            &a,
            Who::Agent,
            "b/gone",
            &note(Some("hypothesis"), None, "x"),
        )
        .await,
    );
    let r = a
        .t(Method::DELETE, &format!("/api/v1/nodes/{gone}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    // база «до миграции 0009»: столбец пуст, флаг дозаполнения стоит, очередь пуста
    let admin = a.st.admin_id;
    let db = a.st.users.get(admin).await.unwrap();
    db.write(|tx| {
        tx.execute("UPDATE nodes SET certainty = NULL", [])?;
        tx.execute("DELETE FROM index_jobs", [])?;
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('certainty_backfill', 'pending')",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    a.st.shutdown().await;
    drop(db);
    drop(a);

    let b = api_with(c).await;
    // первый токен выдаётся только при первом старте: агенту — новый
    let agent = b.issue(&b.admin_id(), "rw").await;
    assert_eq!(get_node(&b, &ax).await["certainty"], "axiom");
    assert_eq!(get_node(&b, &th).await["certainty"], "theorem");
    assert_eq!(get_node(&b, &pl).await["certainty"], Value::Null);
    let db = b.st.users.get(b.st.admin_id).await.unwrap();
    let (flag, jobs, gone_c): (bool, Vec<String>, Option<String>) = db
        .read(move |c| {
            let flag = c
                .query_row(
                    "SELECT 1 FROM meta WHERE key = 'certainty_backfill'",
                    [],
                    |_| Ok(()),
                )
                .is_ok();
            let mut st = c.prepare(
                "SELECT target_id FROM index_jobs WHERE op = 'upsert' ORDER BY target_id",
            )?;
            let jobs = st
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<_, _>>()?;
            let g = c.query_row(
                "SELECT certainty FROM nodes WHERE id = ?1",
                [gone.parse::<Id>().unwrap()],
                |r| r.get(0),
            )?;
            Ok((flag, jobs, g))
        })
        .await
        .unwrap();
    assert!(!flag, "флаг снят");
    // upsert — заметкам со статусом (payload Qdrant), удалённые не трогаются
    let mut want = vec![ax.clone(), th.clone()];
    want.sort();
    assert_eq!(jobs, want);
    assert_eq!(gone_c, None);
    // после дозаполнения права действуют: агент не трогает axiom
    let r = b
        .tok(&agent, Method::DELETE, &format!("/api/v1/nodes/{ax}"), None)
        .await;
    assert_certainty(&r, StatusCode::FORBIDDEN, "axiom-requires-human");
}
