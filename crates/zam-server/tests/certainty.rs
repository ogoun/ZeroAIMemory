//! Эпистемический статус `certainty` (задача 9 M3, задача 2 M4, §7.5, Review Focus 3): матрица
//! прав «агент / человек / system» (решение владельца 2026-10-26) на каждом пути записи —
//! insight (создание, PATCH, supersede, retract, delete, restore, триггеры) и заметки
//! (frontmatter при создании, PUT, PATCH, revert, move, delete, restore, вложение),
//! доказательства и причины, упразднённый человеческий токен, поиск (`certainty` в выдаче и
//! карточке `md`, `filters.certainty`, бусты) и дозаполнение `nodes.certainty` из frontmatter.
//!
//! Агент — любой токен (первый токен администратора), человек — cookie-сессия WebUI. Все
//! записи — в Knowledge администратора; серверы без моделей (поиск — FTS).
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

/// Код нарушения матрицы: 422 и `code` problem.
fn assert_certainty(r: &Resp, code: &str) {
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    let v = r.json();
    assert_eq!(v["type"], "/problems/certainty", "{v}");
    assert_eq!(v["code"], code, "{v}");
}

fn assert_ok(r: &Resp, status: StatusCode) {
    assert_eq!(r.status, status, "{}", r.text());
}

/// Клиент «человек» или «агент» поверх [`Api`].
#[derive(Clone, Copy)]
enum Who<'a> {
    /// Первый токен администратора (агент).
    Agent,
    /// Cookie-сессия WebUI (человек).
    Web,
    /// Другой токен (тоже агент).
    Token(&'a str),
}

async fn call(a: &Api, who: Who<'_>, m: Method, uri: &str, body: Option<Value>) -> Resp {
    call_h(a, who, m, uri, body, &[]).await
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
        Who::Token(t) => a.tok_h(t, m, uri, body, extra).await,
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
    assert_ok(&r, StatusCode::CREATED);
    r.json()
}

/// Текущая ревизия записи глазами `who` (CLI и WebUI берут её из `GET /entries/{id}`).
async fn current_rev(a: &Api, who: Who<'_>, id: &str) -> String {
    let r = call(a, who, Method::GET, &format!("/api/v1/entries/{id}"), None).await;
    assert_ok(&r, StatusCode::OK);
    r.json()["rev"].to_string()
}

/// `PATCH /entries/{id}` с `If-Match` текущей ревизии (§11.3).
async fn patch_entry(a: &Api, who: Who<'_>, id: &str, body: Value) -> Resp {
    let rev = current_rev(a, who, id).await;
    call_h(
        a,
        who,
        Method::PATCH,
        &format!("/api/v1/entries/{id}"),
        Some(body),
        &[("if-match", rev.as_str())],
    )
    .await
}

/// `DELETE /entries/{id}` с `If-Match` текущей ревизии.
async fn delete_entry(a: &Api, who: Who<'_>, id: &str) -> Resp {
    let rev = current_rev(a, who, id).await;
    call_h(
        a,
        who,
        Method::DELETE,
        &format!("/api/v1/entries/{id}"),
        None,
        &[("if-match", rev.as_str())],
    )
    .await
}

async fn get_entry(a: &Api, id: &str) -> Value {
    let r = a
        .adm(Method::GET, &format!("/api/v1/entries/{id}"), None, None)
        .await;
    assert_ok(&r, StatusCode::OK);
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
    assert_ok(&r, StatusCode::OK);
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
    assert_ok(&r, StatusCode::CREATED);
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
    assert_ok(&r, StatusCode::OK);
    r.json()["node"].clone()
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
async fn agent_creates_and_changes_axiom_with_reason() {
    let a = api("cert-agent-axiom", |_| {}).await;
    // явный axiom от агента (по указанию пользователя) — 201
    let e = created_insight(
        &a,
        Who::Agent,
        json!({"kind": "insight", "title": "Аксиома по указанию", "data": {"kind": "decision"},
               "certainty": "axiom"}),
    )
    .await;
    assert_eq!(
        (&e["certainty"], &e["author"]["kind"]),
        (&json!("axiom"), &json!("agent"))
    );
    let id = id_of(&e);
    // axiom → hypothesis без причины — 422, с причиной — 200; история хранит причину
    let r = patch_entry(&a, Who::Agent, &id, json!({"certainty": "hypothesis"})).await;
    assert_certainty(&r, "reason-required");
    assert_eq!(get_entry(&a, &id).await["rev"], 1, "ничего не записано");
    let r = patch_entry(
        &a,
        Who::Agent,
        &id,
        json!({"certainty": "hypothesis", "reason": "пользователь отменил правило"}),
    )
    .await;
    assert_ok(&r, StatusCode::OK);
    assert_eq!(r.json()["certainty"], "hypothesis");
    // обратно в axiom и снятие статуса — тоже с причиной
    let r = patch_entry(&a, Who::Agent, &id, json!({"certainty": "axiom"})).await;
    assert_certainty(&r, "reason-required");
    let r = patch_entry(
        &a,
        Who::Agent,
        &id,
        json!({"certainty": "axiom", "reason": "пользователь подтвердил"}),
    )
    .await;
    assert_ok(&r, StatusCode::OK);
    let r = patch_entry(
        &a,
        Who::Agent,
        &id,
        json!({"certainty": null, "reason": "не о надёжности"}),
    )
    .await;
    assert_ok(&r, StatusCode::OK);
    assert_eq!(r.json()["certainty"], Value::Null);
    let revs = entry_revisions(&a, &id).await;
    let got: Vec<(&Value, &Value, &Value)> = revs
        .iter()
        .map(|r| (&r["certainty"], &r["reason"], &r["author"]["kind"]))
        .collect();
    assert_eq!(
        got,
        vec![
            (&json!("axiom"), &Value::Null, &json!("agent")),
            (
                &json!("hypothesis"),
                &json!("пользователь отменил правило"),
                &json!("agent")
            ),
            (
                &json!("axiom"),
                &json!("пользователь подтвердил"),
                &json!("agent")
            ),
            (&Value::Null, &json!("не о надёжности"), &json!("agent")),
        ]
    );
    // замещение аксиомой через supersedes_id — 201
    let h = created_insight(&a, Who::Agent, insight("Гипотеза")).await;
    assert_eq!(h["certainty"], "hypothesis", "по умолчанию у агента: {h}");
    let mut body = insight("Замена аксиомой");
    body["supersedes_id"] = json!(id_of(&h));
    body["certainty"] = json!("axiom");
    let r = create_insight(&a, Who::Agent, body).await;
    assert_ok(&r, StatusCode::CREATED);
    assert_eq!(r.json()["certainty"], "axiom", "{}", r.text());
    // неизвестное значение статуса заметки — 400
    let r = create_note(&a, Who::Agent, "a/bad", &note(Some("certain"), None, "x")).await;
    assert_ok(&r, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn agent_theorem_without_evidence_422() {
    let a = api("cert-agent-theorem", |_| {}).await;
    let mut body = insight("Теорема без доказательства");
    body["certainty"] = json!("theorem");
    let r = create_insight(&a, Who::Agent, body.clone()).await;
    assert_certainty(&r, "evidence-required");
    // неполное доказательство не засчитывается
    body["data"]["evidence"] = json!({"checked": "a", "method": "", "result": "c"});
    let r = create_insight(&a, Who::Agent, body.clone()).await;
    assert_certainty(&r, "evidence-required");
    body["data"]["evidence"] = evidence();
    let e = created_insight(&a, Who::Agent, body).await;
    assert_eq!(e["certainty"], "theorem");
    // заметка: theorem агента без evidence: — 422, с ним — 201
    let r = create_note(&a, Who::Agent, "t/no", &note(Some("theorem"), None, "x")).await;
    assert_certainty(&r, "evidence-required");
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
    assert_certainty(&r, "evidence-required");
    // с доказательством, но без причины — 422
    let data = json!({"kind": "discovery", "evidence": evidence()});
    let r = patch_entry(
        &a,
        Who::Agent,
        &id,
        json!({"certainty": "theorem", "data": data}),
    )
    .await;
    assert_certainty(&r, "reason-required");
    assert_eq!(get_entry(&a, &id).await["rev"], 1, "ничего не записано");
    // доказательство и причина — 200, ревизия с причиной
    let r = patch_entry(
        &a,
        Who::Agent,
        &id,
        json!({"certainty": "theorem", "data": data, "reason": "перепроверка подтвердила"}),
    )
    .await;
    assert_ok(&r, StatusCode::OK);
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
    assert_ok(&r, StatusCode::OK);
    let r = patch_entry(
        &a,
        Who::Agent,
        &id,
        json!({"certainty": "theorem", "reason": "снова подтверждено"}),
    )
    .await;
    assert_ok(&r, StatusCode::OK);

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
    assert_certainty(&r, "reason-required");
    let r = put_note(
        &a,
        Who::Agent,
        &n,
        &note(Some("theorem"), None, "x"),
        Some("подтверждено"),
    )
    .await;
    assert_certainty(&r, "evidence-required");
    let r = put_note(
        &a,
        Who::Agent,
        &n,
        &note(Some("theorem"), Some("прогон CI"), "x"),
        Some("подтверждено прогоном"),
    )
    .await;
    assert_ok(&r, StatusCode::OK);
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
    assert_certainty(&r, "reason-required");
    let r = patch_entry(&a, Who::Agent, &id, json!({"certainty": null})).await;
    assert_certainty(&r, "reason-required");
    let r = patch_entry(
        &a,
        Who::Agent,
        &id,
        json!({"certainty": "hypothesis", "reason": "параметры были неверны"}),
    )
    .await;
    assert_ok(&r, StatusCode::OK);
    assert_eq!(r.json()["certainty"], "hypothesis");
    let revs = entry_revisions(&a, &id).await;
    assert_eq!(revs.last().unwrap()["reason"], "параметры были неверны");
    // снять статус — с причиной (с M4 — у любого статуса)
    let r = patch_entry(
        &a,
        Who::Agent,
        &id,
        json!({"certainty": null, "reason": "не о надёжности"}),
    )
    .await;
    assert_ok(&r, StatusCode::OK);
    assert_eq!(r.json()["certainty"], Value::Null);

    // заметка: theorem → hypothesis с сообщением, снятие theorem — с сообщением
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
    assert_certainty(&r, "reason-required");
    let r = put_note(
        &a,
        Who::Agent,
        &n,
        &note(Some("hypothesis"), None, "x"),
        Some("пересмотр"),
    )
    .await;
    assert_ok(&r, StatusCode::OK);
    assert_eq!(r.json()["certainty"], "hypothesis");
    let t = created_note(
        &a,
        Who::Agent,
        "d/t",
        &note(Some("theorem"), Some("CI"), "x"),
    )
    .await;
    let r = put_note(&a, Who::Agent, &t, &note(None, None, "x"), None).await;
    assert_certainty(&r, "reason-required");
    let r = put_note(&a, Who::Agent, &t, &note(None, None, "x"), Some("снять")).await;
    assert_ok(&r, StatusCode::OK);
    assert_eq!(r.json()["certainty"], Value::Null);
}

#[tokio::test]
async fn agent_modifies_axiom_entry() {
    let a = api("cert-axiom-entry", |_| {}).await;
    // insight человека без явного статуса — без статуса; аксиому человек ставит явно
    let e = created_insight(&a, Who::Web, insight("Заметка владельца")).await;
    assert_eq!(
        (&e["certainty"], &e["author"]["kind"]),
        (&Value::Null, &json!("human")),
        "{e}"
    );
    let mut body = insight("Правило владельца");
    body["certainty"] = json!("axiom");
    let e = created_insight(&a, Who::Web, body).await;
    assert_eq!(e["certainty"], "axiom", "{e}");
    let id = id_of(&e);
    // агент правит запись axiom: тело, заголовок, теги, статус, триггеры
    for body in [
        json!({"body": "переписано агентом по указанию"}),
        json!({"append_body": "дописано"}),
        json!({"title": "Новый заголовок"}),
        json!({"tags": ["x"]}),
        json!({"status": "revisit", "reason": "r"}),
        json!({"triggers": [{"type": "keyword", "pattern": "x"}]}),
    ] {
        assert_ok(
            &patch_entry(&a, Who::Agent, &id, body).await,
            StatusCode::OK,
        );
    }
    let cur = get_entry(&a, &id).await;
    assert_eq!(cur["certainty"], "axiom", "статус не изменился: {cur}");
    assert_eq!(cur["triggers"].as_array().map(Vec::len), Some(1), "{cur}");
    // удаляет и восстанавливает
    assert_ok(
        &delete_entry(&a, Who::Agent, &id).await,
        StatusCode::NO_CONTENT,
    );
    let r = call(
        &a,
        Who::Agent,
        Method::POST,
        &format!("/api/v1/entries/{id}/restore"),
        None,
    )
    .await;
    assert_ok(&r, StatusCode::OK);
    // замещает (новая запись агента — hypothesis) и отзывает замещённую
    let r = call(
        &a,
        Who::Agent,
        Method::POST,
        &format!("/api/v1/entries/{id}/supersede"),
        Some(json!({"title": "Замена", "data": {"kind": "discovery"}})),
    )
    .await;
    assert_ok(&r, StatusCode::CREATED);
    assert_eq!(r.json()["entry"]["certainty"], "hypothesis");
    let r = call(
        &a,
        Who::Agent,
        Method::POST,
        &format!("/api/v1/entries/{id}/retract"),
        Some(json!({"reason": "пользователь отменил"})),
    )
    .await;
    assert_ok(&r, StatusCode::OK);
    assert_eq!(get_entry(&a, &id).await["status"], "retracted");
}

#[tokio::test]
async fn agent_note_axiom_frontmatter_allowed() {
    let a = api("cert-axiom-note", |_| {}).await;
    // заметка с certainty: axiom во frontmatter при создании — 201
    let n = created_note(&a, Who::Agent, "a/axiom", &note(Some("axiom"), None, "x")).await;
    assert_eq!(n["certainty"], "axiom", "{n}");
    // перевод своей заметки в axiom правкой frontmatter — с сообщением ревизии
    let h = created_note(
        &a,
        Who::Agent,
        "own/h",
        &note(Some("hypothesis"), None, "x"),
    )
    .await;
    let r = put_note(&a, Who::Agent, &h, &note(Some("axiom"), None, "x"), None).await;
    assert_certainty(&r, "reason-required");
    let r = put_note(
        &a,
        Who::Agent,
        &h,
        &note(Some("axiom"), None, "x"),
        Some("пользователь утвердил"),
    )
    .await;
    assert_ok(&r, StatusCode::OK);
    assert_eq!(r.json()["certainty"], "axiom");
    // PATCH секций, добавляющий frontmatter в начало, — тоже переход
    let n0 = created_note(&a, Who::Agent, "own/plain", &note(None, None, "x")).await;
    let r = call(
        &a,
        Who::Agent,
        Method::PATCH,
        &format!("/api/v1/nodes/{}", id_of(&n0)),
        Some(json!({"base_rev": 1, "message": "по указанию пользователя",
                    "ops": [{"op": "prepend", "content": "---\ncertainty: axiom\n---\n"}]})),
    )
    .await;
    assert_ok(&r, StatusCode::OK);
    assert_eq!(get_node(&a, &id_of(&n0)).await["certainty"], "axiom");

    // заметка axiom человека в папке: агент правит, откатывает, переносит, удаляет,
    // восстанавливает, прикладывает файл
    let ax = created_note(
        &a,
        Who::Web,
        "rules/core/ax",
        &note(Some("axiom"), None, "Правило."),
    )
    .await;
    let ax_id = id_of(&ax);
    let r = put_note(
        &a,
        Who::Agent,
        &ax,
        &note(Some("axiom"), None, "Уточнено."),
        None,
    )
    .await;
    assert_ok(&r, StatusCode::OK);
    // устаревшая ревизия — 409 (раньше прав)
    let r = put_note(&a, Who::Agent, &ax, "x", None).await;
    assert_ok(&r, StatusCode::CONFLICT);
    let r = call(
        &a,
        Who::Agent,
        Method::PATCH,
        &format!("/api/v1/nodes/{ax_id}"),
        Some(json!({"base_rev": 2, "ops": [{"op": "append", "content": "\nдописано\n"}]})),
    )
    .await;
    assert_ok(&r, StatusCode::OK);
    let r = call(
        &a,
        Who::Agent,
        Method::POST,
        &format!("/api/v1/nodes/{ax_id}/revert"),
        Some(json!({"rev": 1})),
    )
    .await;
    assert_ok(&r, StatusCode::OK);
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
    assert_ok(&r, StatusCode::CREATED);
    let r = call(
        &a,
        Who::Agent,
        Method::POST,
        &format!("/api/v1/nodes/{ax_id}/move"),
        Some(json!({"path": "elsewhere/ax"})),
    )
    .await;
    assert_ok(&r, StatusCode::OK);
    // папка с заметкой axiom: перенос и удаление поддерева, восстановление
    let folder = a
        .t(
            Method::GET,
            "/api/v1/nodes/by-path?space=knowledge&path=own&format=json",
            None,
        )
        .await
        .json();
    let fid = id_of(&folder["node"]);
    let r = call(
        &a,
        Who::Agent,
        Method::POST,
        &format!("/api/v1/nodes/{fid}/move"),
        Some(json!({"path": "mine"})),
    )
    .await;
    assert_ok(&r, StatusCode::OK);
    let r = call(
        &a,
        Who::Agent,
        Method::DELETE,
        &format!("/api/v1/nodes/{fid}"),
        None,
    )
    .await;
    assert_ok(&r, StatusCode::NO_CONTENT);
    let r = call(
        &a,
        Who::Agent,
        Method::POST,
        &format!("/api/v1/nodes/{fid}/restore"),
        None,
    )
    .await;
    assert_ok(&r, StatusCode::OK);
    assert_eq!(get_node(&a, &id_of(&h)).await["certainty"], "axiom");
}

#[tokio::test]
async fn agent_edge_from_axiom_allowed() {
    let a = api("cert-axiom-edge", |_| {}).await;
    let ax = id_of(&created_note(&a, Who::Web, "e/ax", &note(Some("axiom"), None, "x")).await);
    let mut body = insight("Аксиома-запись");
    body["certainty"] = json!("axiom");
    let ax_entry = id_of(&created_insight(&a, Who::Web, body).await);
    let plain = id_of(&created_note(&a, Who::Agent, "e/plain", &note(None, None, "y")).await);
    // агент связывает аксиомы (заметку и запись) с другими целями и удаляет связи
    for (src, dst) in [
        (format!("n:{ax}"), format!("n:{plain}")),
        (format!("e:{ax_entry}"), format!("n:{plain}")),
        (format!("n:{plain}"), format!("n:{ax}")),
    ] {
        let r = call(
            &a,
            Who::Agent,
            Method::POST,
            "/api/v1/edges",
            Some(json!({"src": src, "type": "relates_to", "dst": dst})),
        )
        .await;
        assert_ok(&r, StatusCode::CREATED);
        let eid = id_of(&r.json());
        let r = call(
            &a,
            Who::Agent,
            Method::DELETE,
            &format!("/api/v1/edges/{eid}"),
            None,
        )
        .await;
        assert_ok(&r, StatusCode::NO_CONTENT);
    }
}

#[tokio::test]
async fn webui_human_change_without_reason() {
    let a = api("cert-webui", |_| {}).await;
    let e = created_insight(&a, Who::Agent, insight("Вывод агента")).await;
    let id = id_of(&e);
    // человеку причина необязательна, доказательство для theorem — тоже
    let r = patch_entry(&a, Who::Web, &id, json!({"certainty": "axiom"})).await;
    assert_ok(&r, StatusCode::OK);
    assert_eq!(r.json()["certainty"], "axiom");
    let r = patch_entry(&a, Who::Web, &id, json!({"certainty": "theorem"})).await;
    assert_ok(&r, StatusCode::OK);
    let r = patch_entry(
        &a,
        Who::Web,
        &id,
        json!({"certainty": "axiom", "reason": "проверено владельцем"}),
    )
    .await;
    assert_ok(&r, StatusCode::OK);
    let revs = entry_revisions(&a, &id).await;
    let got: Vec<(&Value, &Value, &Value)> = revs
        .iter()
        .skip(1)
        .map(|r| (&r["certainty"], &r["reason"], &r["author"]["kind"]))
        .collect();
    assert_eq!(
        got,
        vec![
            (&json!("axiom"), &Value::Null, &json!("human")),
            (&json!("theorem"), &Value::Null, &json!("human")),
            (
                &json!("axiom"),
                &json!("проверено владельцем"),
                &json!("human")
            ),
        ]
    );
    // заметка: перевод в axiom в редакторе WebUI — без сообщения ревизии
    let n = created_note(&a, Who::Agent, "w/n", &note(Some("hypothesis"), None, "x")).await;
    let r = put_note(&a, Who::Web, &n, &note(Some("axiom"), None, "x"), None).await;
    assert_ok(&r, StatusCode::OK);
    assert_eq!(r.json()["certainty"], "axiom");
    // /auth/me — без признака human (сессия WebUI всегда человек)
    let me = a
        .adm(Method::GET, "/api/v1/auth/me", None, None)
        .await
        .json();
    assert!(me.get("human").is_none(), "{me}");
    assert!(me["csrf"].is_string(), "{me}");
}

#[tokio::test]
async fn human_token_flag_rejected_400() {
    let a = api("cert-issue", |_| {}).await;
    let (uid, utok) = a.new_user("Анна").await;
    let uri = format!("/api/v1/admin/users/{uid}/tokens");
    // по токену выпуск — 403, как и прежде (администраторский маршрут)
    for tok in [&a.token, &utok] {
        let r = a
            .tok(
                tok,
                Method::POST,
                &uri,
                Some(json!({"name": "x", "human": true})),
            )
            .await;
        assert_ok(&r, StatusCode::FORBIDDEN);
    }
    // администратор WebUI: human: true — 400 с пояснением, токена нет
    let before = a.adm(Method::GET, &uri, None, None).await.json();
    let r = a
        .adm(
            Method::POST,
            &uri,
            Some(json!({"name": "ноутбук владельца", "human": true})),
            None,
        )
        .await;
    assert_ok(&r, StatusCode::BAD_REQUEST);
    assert_eq!(
        r.json()["detail"],
        "человеческие токены упразднены (решение владельца 2026-10-26): подтверждайте в WebUI"
    );
    let after = a.adm(Method::GET, &uri, None, None).await.json();
    assert_eq!(before, after, "токен не выпущен");
    // human: false — совместимость: обычный токен; поля human в ответах нет
    let r = a
        .adm(
            Method::POST,
            &uri,
            Some(json!({"name": "ноутбук", "human": false})),
            None,
        )
        .await;
    assert_ok(&r, StatusCode::CREATED);
    let v = r.json();
    assert!(v["info"].get("human").is_none(), "{v}");
    let plain = v["plaintext"].as_str().unwrap().to_string();
    let list = a.adm(Method::GET, &uri, None, None).await.json();
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .all(|t| t.get("human").is_none()),
        "{list}"
    );
    // аудит token_issue без human
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
    assert!(!issues.is_empty(), "{audit}");
    assert!(
        issues.iter().all(|r| r["details"].get("human").is_none()),
        "{audit}"
    );
    // GET /spaces — без human по токену и в WebUI
    for tok in [&plain, &utok] {
        let s = a.tok(tok, Method::GET, "/api/v1/spaces", None).await.json();
        assert!(s.get("human").is_none(), "{s}");
    }
    let s = a
        .adm(Method::GET, "/api/v1/spaces", None, None)
        .await
        .json();
    assert!(s.get("human").is_none(), "{s}");
}

#[tokio::test]
async fn old_human_token_writes_as_agent() {
    let dir = common::tmp("cert-old-human");
    let c = common::cfg_in(dir);
    let a = api_with(c.clone()).await;
    let ht = a.issue(&a.admin_id(), "rw").await;
    let sys = a.st.data_dir.system_db();
    a.st.shutdown().await;
    drop(a);
    // токен, выпущенный человеческим в M3 (tokens.human = 1)
    let db = zam_store::Db::open(&sys, zam_store::SYSTEM_MIGRATIONS).unwrap();
    db.write(|tx| {
        tx.execute("UPDATE tokens SET human = 1 WHERE name = 't-rw'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    drop(db);

    let b = api_with(c).await;
    let who = Who::Token(&ht);
    // запись по нему — агентская: axiom создаётся, смена статуса — только с причиной
    let mut body = insight("Аксиома по старому токену");
    body["certainty"] = json!("axiom");
    let e = created_insight(&b, who, body).await;
    assert_eq!(e["author"]["kind"], "agent", "{e}");
    let id = id_of(&e);
    let r = patch_entry(&b, who, &id, json!({"certainty": "hypothesis"})).await;
    assert_certainty(&r, "reason-required");
    let n = created_note(&b, who, "old/n", &note(None, None, "x")).await;
    let r = b
        .tok(
            &ht,
            Method::GET,
            &format!("/api/v1/nodes/{}/revisions", id_of(&n)),
            None,
        )
        .await;
    assert_eq!(r.json()[0]["author_kind"], "agent", "{}", r.text());
    // флаг в базе остался (история только дописывается), но в ответах его нет
    let list = b
        .adm(
            Method::GET,
            &format!("/api/v1/admin/users/{}/tokens", b.admin_id()),
            None,
            None,
        )
        .await
        .json();
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .all(|t| t.get("human").is_none()),
        "{list}"
    );
    // аудит при старте видит его префикс
    let prefix = list
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "t-rw")
        .map(|t| t["prefix"].as_str().unwrap().to_string())
        .unwrap();
    let prefixes = b.st.system.legacy_human_token_prefixes().await.unwrap();
    assert_eq!(prefixes, vec![prefix]);
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
        // балл в ответе не выше 1 (деление на лучший после бустов), порядок — по полному баллу
        let s = r.json();
        assert!(s["items"][0]["score"].as_f64().unwrap() <= 1.0);
    }
    // insight axiom выше равного insight hypothesis
    let hy = created_insight(&a, Who::Agent, insight("квазиморфизм вывод")).await;
    let mut body = insight("квазиморфизм вывод");
    body["certainty"] = json!("axiom");
    let ax = created_insight(&a, Who::Web, body).await;
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
    // аксиома человека в корзине
    let axgone =
        id_of(&created_note(&a, Who::Web, "b/axgone", &note(Some("axiom"), None, "x")).await);
    let r = a
        .adm(
            Method::DELETE,
            &format!("/api/v1/nodes/{axgone}"),
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    // заметка, которой агент до задачи 9 записал `certainty: axiom`
    let fake = id_of(
        &created_note(
            &a,
            Who::Agent,
            "b/fake",
            &note(Some("hypothesis"), None, "x"),
        )
        .await,
    );
    // база «до миграции 0009»: столбец пуст, флаг дозаполнения стоит, очередь пуста
    let admin = a.st.admin_id;
    let db = a.st.users.get(admin).await.unwrap();
    let fake_id: Id = fake.parse().unwrap();
    db.write(move |tx| {
        let text = note(Some("axiom"), None, "агент сам объявил аксиому");
        tx.execute(
            "INSERT INTO revisions (node_id, rev, content, content_hash, size, author_kind, ts)
             VALUES (?1, 2, ?2, 'h2', ?3, 'agent', '2026-10-20T00:00:00.000000Z')",
            (fake_id, &text, text.len() as i64),
        )?;
        tx.execute(
            "UPDATE nodes SET current_rev = 2, content_hash = 'h2' WHERE id = ?1",
            [fake_id],
        )?;
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
    // axiom не от человека понижен до hypothesis
    assert_eq!(get_node(&b, &fake).await["certainty"], "hypothesis");
    let db = b.st.users.get(b.st.admin_id).await.unwrap();
    let ids = [gone.clone(), axgone.clone()];
    let (flag, jobs, deleted): (bool, Vec<String>, Vec<Option<String>>) = db
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
            let mut deleted = Vec::new();
            for id in &ids {
                deleted.push(c.query_row(
                    "SELECT certainty FROM nodes WHERE id = ?1",
                    [id.parse::<Id>().unwrap()],
                    |r| r.get(0),
                )?);
            }
            Ok((flag, jobs, deleted))
        })
        .await
        .unwrap();
    assert!(!flag, "флаг снят");
    // upsert — живым заметкам со статусом (payload Qdrant); удалённые без задания
    let mut want = vec![ax.clone(), th.clone(), fake.clone()];
    want.sort();
    assert_eq!(jobs, want);
    // удалённые заметки дозаполнены тоже: столбец описывает текущую ревизию
    assert_eq!(
        deleted,
        vec![Some("hypothesis".to_string()), Some("axiom".to_string())]
    );
    // понижённая заметка: правка агентом с axiom во frontmatter — переход hypothesis → axiom,
    // только с причиной (с M4 агент ставит axiom по указанию пользователя)
    let n = get_node(&b, &fake).await;
    let put = async |msg: Option<&str>| {
        let mut body = json!({"content": note(Some("axiom"), None, "y")});
        if let Some(m) = msg {
            body["message"] = json!(m);
        }
        b.tok_h(
            &agent,
            Method::PUT,
            &format!("/api/v1/nodes/{fake}"),
            Some(body),
            &[("if-match", n["current_rev"].to_string().as_str())],
        )
        .await
    };
    assert_certainty(&put(None).await, "reason-required");
    let r = put(Some("пользователь подтвердил")).await;
    assert_ok(&r, StatusCode::OK);
    assert_eq!(r.json()["certainty"], "axiom");
    // аксиомы после дозаполнения агент удаляет и восстанавливает (матрица M4)
    let r = b
        .tok(&agent, Method::DELETE, &format!("/api/v1/nodes/{ax}"), None)
        .await;
    assert_ok(&r, StatusCode::NO_CONTENT);
    let r = b
        .tok(
            &agent,
            Method::POST,
            &format!("/api/v1/nodes/{axgone}/restore"),
            None,
        )
        .await;
    assert_ok(&r, StatusCode::OK);
}

#[tokio::test]
async fn agent_theorem_loses_evidence_422() {
    let a = api("cert-theorem-final", |_| {}).await;
    // заметка theorem: агентская правка без evidence: — 422, с ним — 200
    let n = created_note(
        &a,
        Who::Agent,
        "f/th",
        &note(Some("theorem"), Some("прогон CI"), "x"),
    )
    .await;
    let r = put_note(&a, Who::Agent, &n, &note(Some("theorem"), None, "y"), None).await;
    assert_certainty(&r, "evidence-required");
    let r = put_note(
        &a,
        Who::Agent,
        &n,
        &note(Some("theorem"), Some("прогон CI"), "y"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // theorem человека без доказательства: агент правит только с доказательством
    let h = created_note(&a, Who::Web, "f/human", &note(Some("theorem"), None, "x")).await;
    let r = put_note(&a, Who::Agent, &h, &note(Some("theorem"), None, "y"), None).await;
    assert_certainty(&r, "evidence-required");
    // запись theorem: PATCH data без evidence — 422
    let mut body = insight("Теорема с доказательством");
    body["certainty"] = json!("theorem");
    body["data"]["evidence"] = evidence();
    let id = id_of(&created_insight(&a, Who::Agent, body).await);
    let r = patch_entry(&a, Who::Agent, &id, json!({"data": {"kind": "discovery"}})).await;
    assert_certainty(&r, "evidence-required");
    let r = patch_entry(
        &a,
        Who::Agent,
        &id,
        json!({"data": {"kind": "discovery", "evidence": {"checked": "c", "method": "", "result": "r"}}}),
    )
    .await;
    assert_certainty(&r, "evidence-required");
    // прочие правки с сохранённым доказательством — можно
    let r = patch_entry(&a, Who::Agent, &id, json!({"append_body": "уточнение"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // человек может убрать доказательство
    let r = patch_entry(&a, Who::Web, &id, json!({"data": {"kind": "discovery"}})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

#[tokio::test]
async fn note_write_warns_on_unrecognized_certainty_key() {
    let a = api("cert-warn", |_| {}).await;
    let r = create_note(
        &a,
        Who::Agent,
        "w/q",
        "---\n\"certainty\": theorem\nEvidence: x\n---\n# Q\n",
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let v = r.json();
    assert_eq!(v["warnings"], json!(["certainty_not_recognized"]), "{v}");
    assert_eq!(
        v["certainty"],
        Value::Null,
        "ключ в кавычках статус не задаёт"
    );
    let ok = created_note(&a, Who::Agent, "w/ok", &note(Some("hypothesis"), None, "x")).await;
    assert!(ok.get("warnings").is_none(), "{ok}");
    // PUT и PATCH — тоже
    let r = put_note(
        &a,
        Who::Agent,
        &ok,
        "---\nCertainty: axiom\n---\n# X\n",
        None,
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "снятие hypothesis без причины"
    );
    let r = put_note(
        &a,
        Who::Agent,
        &ok,
        "---\nCertainty: axiom\n---\n# X\n",
        Some("снять"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["warnings"], json!(["certainty_not_recognized"]));
    let r = call(
        &a,
        Who::Agent,
        Method::PATCH,
        &format!("/api/v1/nodes/{}", id_of(&ok)),
        Some(json!({"base_rev": 2, "ops": [{"op": "append", "content": "\nещё\n"}]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["warnings"], json!(["certainty_not_recognized"]));
}

const SHARED: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e77";

#[tokio::test]
async fn member_agent_changes_owner_axiom_with_reason() {
    let a = api("cert-member", |_| {}).await;
    let (owner, otok) = a.new_user("Владелец").await;
    let (member, mtok) = a.new_user("Участник").await;
    let reg = json!({"agent_solution_id": SHARED, "name": "Общее"});
    let r = a
        .tok(&otok, Method::POST, "/api/v1/solutions", Some(reg.clone()))
        .await;
    assert_ok(&r, StatusCode::CREATED);
    let r = a
        .tok(&mtok, Method::POST, "/api/v1/solutions", Some(reg))
        .await;
    assert_ok(&r, StatusCode::ACCEPTED);
    let r = a
        .tok(
            &otok,
            Method::POST,
            &format!("/api/v1/solutions/{SHARED}/access/{member}/approve"),
            Some(json!({})),
        )
        .await;
    assert_ok(&r, StatusCode::OK);
    let entries = format!("/api/v1/spaces/{SHARED}/entries");
    // аксиома владельца — из WebUI администратора в базе владельца
    let mut body = insight("Правило решения");
    body["certainty"] = json!("axiom");
    let r = a
        .adm(Method::POST, &entries, Some(body), Some(&owner))
        .await;
    assert_ok(&r, StatusCode::CREATED);
    let id = id_of(&r.json());
    let who = Who::Token(&mtok);
    // участник (агент) правит её и меняет статус — с причиной
    let r = patch_entry(&a, who, &id, json!({"append_body": "участник"})).await;
    assert_ok(&r, StatusCode::OK);
    let r = patch_entry(&a, who, &id, json!({"certainty": "hypothesis"})).await;
    assert_certainty(&r, "reason-required");
    let r = patch_entry(
        &a,
        who,
        &id,
        json!({"certainty": "hypothesis", "reason": "владелец попросил пересмотреть"}),
    )
    .await;
    assert_ok(&r, StatusCode::OK);
    let revs = a
        .tok(
            &otok,
            Method::GET,
            &format!("/api/v1/entries/{id}/revisions"),
            None,
        )
        .await
        .json();
    let last = revs["items"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(
        (&last["reason"], &last["author"]["kind"]),
        (&json!("владелец попросил пересмотреть"), &json!("agent"))
    );
    // и создаёт axiom в решении владельца; запись без статуса — hypothesis
    let mut body = insight("Аксиома участника");
    body["certainty"] = json!("axiom");
    let r = a.tok(&mtok, Method::POST, &entries, Some(body)).await;
    assert_ok(&r, StatusCode::CREATED);
    assert_eq!(r.json()["certainty"], "axiom");
    let r = a
        .tok(
            &mtok,
            Method::POST,
            &entries,
            Some(insight("Вывод участника")),
        )
        .await;
    assert_ok(&r, StatusCode::CREATED);
    assert_eq!(r.json()["certainty"], "hypothesis");
}
