//! Словарь домена (M4.5 §2, задача 2): запись терминов `kind: term`, уникальность имён и
//! псевдонимов (409 `term-conflict`), замещение освобождает имя, предупреждения
//! `term_alias` при записи заметок и журнала, FTS по псевдониму, вики-ссылки `[[Имя]]` на
//! термины, версия словаря, MCP `memory_log kind=term`. Сервер без моделей (FTS-only).
mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};

const KN: &str = "/api/v1/spaces/knowledge/entries";

fn id(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("нет id: {v}"))
        .to_string()
}

fn term(title: &str, aliases: &[&str]) -> Value {
    json!({"kind": "term", "title": title, "body": format!("Подробности про {title}."),
           "data": {"definition": format!("Определение: {title}"), "aliases": aliases}})
}

async fn post(a: &Api, body: Value) -> Resp {
    a.t(Method::POST, KN, Some(body)).await
}

async fn created(a: &Api, body: Value) -> Value {
    let r = post(a, body).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()
}

async fn entry(a: &Api, id: &str) -> Value {
    let r = a
        .t(Method::GET, &format!("/api/v1/entries/{id}"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

async fn version(a: &Api) -> i64 {
    let r = a
        .t(Method::GET, "/api/v1/spaces/knowledge/glossary", None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["version"].as_i64().unwrap()
}

async fn search(a: &Api, q: &str, kinds: &[&str]) -> Vec<String> {
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": q, "filters": {"kinds": kinds}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["entry_id"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn term_create_and_read() {
    let a = api("glossary-create", |_| {}).await;
    let body = json!({"kind": "term", "title": "  Выжимка ", "body": "Подробности.",
        "data": {"definition": "  Короткая сводка записи  ",
                 "aliases": [" summary ", "", "summary", "digest"],
                 "not_to_confuse": ["Заметка"], "code": ["answer_cache"]}});
    let e = created(&a, body).await;
    assert_eq!(e["kind"], "term");
    assert_eq!(e["status"], "active");
    assert_eq!(e["title"], "Выжимка");
    assert_eq!(
        e["data"],
        json!({"definition": "Короткая сводка записи", "aliases": ["summary", "digest"],
               "not_to_confuse": ["Заметка"], "code": ["answer_cache"]})
    );
    assert!(e.get("dedup").is_none(), "{e}");
    assert!(e.get("warnings").is_none(), "{e}");
    let tid = id(&e);
    // карточка и ссылка t:
    assert_eq!(
        entry(&a, &tid).await["data"]["definition"],
        "Короткая сводка записи"
    );
    // L0 термина — определение
    let r = a
        .t(
            Method::POST,
            "/api/v1/read",
            Some(json!({"items": [{"id": format!("t:{tid}"), "tier": "L0"}]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert!(j.to_string().contains("Короткая сводка записи"), "{j}");
    assert_eq!(j["items"][0]["kind"], "term", "{j}");
    // список словаря и карточка по псевдониму (без регистра)
    let r = a
        .t(Method::GET, "/api/v1/spaces/knowledge/glossary", None)
        .await;
    let j = r.json();
    assert_eq!(j["items"][0]["title"], "Выжимка");
    assert_eq!(j["items"][0]["aliases"], json!(["summary", "digest"]));
    assert_eq!(j["items"][0]["definition"], "Короткая сводка записи");
    let r = a
        .t(
            Method::GET,
            "/api/v1/spaces/knowledge/glossary?term=SUMMARY",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(id(&r.json()), tid);
    let r = a
        .t(
            Method::GET,
            "/api/v1/spaces/knowledge/glossary?term=нет",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    // правка определения: PATCH с If-Match, нормализация сохраняется
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{tid}"),
            Some(json!({"data": {"definition": "Новая сводка", "aliases": ["summary"]}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["data"]["definition"], "Новая сводка");
    assert_eq!(r.json()["rev"], 2);
}

#[tokio::test]
async fn term_definition_required_422() {
    let a = api("glossary-422", |_| {}).await;
    let long = |n: usize| "я".repeat(n);
    let many = |n: usize| (0..n).map(|i| format!("a{i}")).collect::<Vec<_>>();
    for (body, field) in [
        (
            json!({"kind": "term", "title": "T", "data": {}}),
            "data.definition",
        ),
        (json!({"kind": "term", "title": "T"}), "data.definition"),
        (
            json!({"kind": "term", "title": "T", "data": {"definition": "  "}}),
            "data.definition",
        ),
        (
            json!({"kind": "term", "title": "T", "data": {"definition": long(301)}}),
            "data.definition",
        ),
        (
            json!({"kind": "term", "title": "T", "data": {"definition": "d", "aliases": many(33)}}),
            "data.aliases",
        ),
        (
            json!({"kind": "term", "title": "T", "data": {"definition": "d", "aliases": [long(201)]}}),
            "data.aliases",
        ),
        (
            json!({"kind": "term", "title": "T", "data": {"definition": "d", "not_to_confuse": many(17)}}),
            "data.not_to_confuse",
        ),
        (
            json!({"kind": "term", "title": "T", "data": {"definition": "d", "code": many(33)}}),
            "data.code",
        ),
        (
            json!({"kind": "term", "title": "T", "data": {"definition": "d", "extra": 1}}),
            "data.extra",
        ),
        (
            json!({"kind": "term", "title": long(201), "data": {"definition": "d"}}),
            "title",
        ),
        (
            json!({"kind": "term", "title": "—", "data": {"definition": "d"}}),
            "title",
        ),
    ] {
        let r = post(&a, body.clone()).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}: {}",
            r.text()
        );
        let d = r.json()["detail"].as_str().unwrap().to_string();
        assert!(d.starts_with(field), "{field}: {d}");
    }
    // граничные значения проходят
    let ok = json!({"kind": "term", "title": long(200),
        "data": {"definition": long(300), "aliases": many(32), "code": many(32)}});
    created(&a, ok).await;
    // триггеры и revisit у термина недопустимы
    let mut t = term("Другой", &[]);
    t["triggers"] = json!([{"type": "keyword", "pattern": "x"}]);
    assert_eq!(post(&a, t).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    let mut t = term("Третий", &[]);
    t["status"] = json!("revisit");
    assert_eq!(post(&a, t).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    // task и question пока не пишутся, но проверка доступа идёт раньше проверки вида
    let r = post(&a, json!({"kind": "task", "title": "T"})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
}

#[tokio::test]
async fn term_alias_conflict_409() {
    let a = api("glossary-conflict", |_| {}).await;
    let first = created(&a, term("Выжимка", &["summary"])).await;
    // псевдоним нового термина = имя существующего, регистр не важен
    let r = post(&a, term("Дайджест", &["ВЫЖИМКА"])).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.header("content-type"), "application/problem+json");
    let j = r.json();
    assert_eq!(j["type"], "/problems/term-conflict");
    assert_eq!(j["conflicts"][0]["id"], id(&first));
    assert_eq!(j["conflicts"][0]["title"], "Выжимка");
    assert_eq!(j["conflicts"][0]["matched"], "ВЫЖИМКА");
    // имя нового = псевдоним существующего (пунктуация и регистр не важны)
    let r = post(&a, term("Summary!", &[])).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.json()["conflicts"][0]["matched"], "Summary!");
    // псевдоним = псевдоним
    let r = post(&a, term("Дайджест", &["Summary"])).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    // несколько пересечений сразу — все в списке
    let second = created(&a, term("Чанк", &["chunk"])).await;
    let r = post(&a, term("Новый", &["summary", "chunk"])).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let ids: Vec<String> = r.json()["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        ids.contains(&id(&first)) && ids.contains(&id(&second)),
        "{ids:?}"
    );
    // свободное имя проходит; псевдоним, равный собственному имени, не конфликт
    let ok = created(&a, term("Дайджест", &["digest", "ДАЙДЖЕСТ"])).await;
    // правка: чужой псевдоним — 409, свой собственный — можно сохранить
    let uri = format!("/api/v1/entries/{}", id(&ok));
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &uri,
            Some(json!({"data": {"definition": "d", "aliases": ["summary"]}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &uri,
            Some(json!({"data": {"definition": "новое", "aliases": ["digest"]}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &uri,
            Some(json!({"title": "Chunk"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    // удалённый термин имя освобождает, восстановление при занятом имени — 409
    let del = created(&a, term("Временный", &["tmp"])).await;
    let duri = format!("/api/v1/entries/{}", id(&del));
    let r = a.tok_rev(&a.token, Method::DELETE, &duri, None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let again = created(&a, term("Временный", &["tmp"])).await;
    let r = a.t(Method::POST, &format!("{duri}/restore"), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.json()["conflicts"][0]["id"], id(&again));
}

#[tokio::test]
async fn term_supersede_frees_name() {
    let a = api("glossary-supersede", |_| {}).await;
    let old = created(&a, term("Выжимка", &["summary"])).await;
    let old_id = id(&old);
    // замещение тем же именем и псевдонимом: замещаемый термин не конфликтует
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{old_id}/supersede"),
            Some(term("Выжимка", &["summary", "digest"])),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = r.json();
    let new_id = id(&j["entry"]);
    assert_eq!(j["entry"]["kind"], "term");
    assert_eq!(j["entry"]["supersedes_id"], old_id);
    assert_eq!(j["superseded"]["status"], "superseded");
    // имя теперь держит новый термин
    let r = post(&a, term("Выжимка", &[])).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.json()["conflicts"][0]["id"], new_id);
    // замещённый не мешает: псевдоним, которого у нового термина нет, свободен
    // (в старом он был только в старом — пересекающихся имён нет)
    // замещение через supersedes_id при создании
    let mut third = term("Выжимка", &["summary"]);
    third["supersedes_id"] = json!(new_id);
    let r = post(&a, third).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let third_id = id(&r.json());
    assert_eq!(entry(&a, &new_id).await["status"], "superseded");
    // отзыв освобождает имя
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{third_id}/retract"),
            Some(json!({"reason": "термин устарел"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["status"], "retracted");
    created(&a, term("Выжимка", &["summary"])).await;
    // в списке словаря — только действующий
    let r = a
        .t(Method::GET, "/api/v1/spaces/knowledge/glossary", None)
        .await;
    assert_eq!(r.json()["items"].as_array().unwrap().len(), 1);
    // правка замещённого термина запрещена, как у insight
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{old_id}"),
            Some(json!({"title": "Иное"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
}

#[tokio::test]
async fn note_write_warns_alias() {
    let a = api("glossary-warn", |_| {}).await;
    created(&a, term("Выжимка", &["summary"])).await;
    let n = a
        .note("knowledge", "w1", "# Заметка\n\nНужна summary записи.\n")
        .await;
    assert_eq!(
        n["warnings"],
        json!(["term_alias: «summary» → «Выжимка»"]),
        "{n}"
    );
    // граница слова: summaryX и Xsummary не псевдоним
    let n = a
        .note("knowledge", "w3", "# Заметка\n\nsummaryX и Xsummary.\n")
        .await;
    assert!(n.get("warnings").is_none(), "{n}");
    // запись не отклоняется и при замене содержимого
    let nid = id(&n);
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{nid}"),
            Some(json!({"content": "# Заметка\n\nSUMMARY тут.\n"})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["warnings"],
        json!(["term_alias: «summary» → «Выжимка»"])
    );
    // журнал: insight с псевдонимом предупреждает, сам term — нет
    let r = post(
        &a,
        json!({"kind": "insight", "title": "Вывод про summary", "body": "x",
               "data": {"kind": "gotcha"}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        r.json()["warnings"],
        json!(["term_alias: «summary» → «Выжимка»"])
    );
    let t = created(&a, term("Сводка", &["сводочка"])).await;
    assert!(t.get("warnings").is_none(), "{t}");
    // замещённый или отозванный термин предупреждений не даёт
    let tid = id(&entry_by_title(&a, "Выжимка").await);
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{tid}/retract"),
            Some(json!({"reason": "r"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let n = a.note("knowledge", "w4", "# Заметка\n\nsummary.\n").await;
    assert!(n.get("warnings").is_none(), "{n}");
}

/// Действующий термин по имени (через карточку словаря).
async fn entry_by_title(a: &Api, name: &str) -> Value {
    let r = a
        .t(
            Method::GET,
            &format!(
                "/api/v1/spaces/knowledge/glossary?term={}",
                common::api::enc(name)
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

#[tokio::test]
async fn fts_finds_term_by_alias() {
    let a = api("glossary-fts", |_| {}).await;
    let t = created(
        &a,
        json!({"kind": "term", "title": "Выжимка", "body": "Подробности.",
               "data": {"definition": "Короткая сводка", "aliases": ["quokka"],
                        "code": ["answer_cache_entry"]}}),
    )
    .await;
    created(&a, term("Чанк", &[])).await;
    // слова нет ни в имени, ни в определении, ни в теле — только псевдоним
    assert_eq!(search(&a, "quokka", &["term"]).await, vec![id(&t)]);
    // определение индексируется
    assert_eq!(search(&a, "сводка", &["term"]).await, vec![id(&t)]);
    // идентификатор кода — через префикс контекста
    assert_eq!(
        search(&a, "answer_cache_entry", &["term"]).await,
        vec![id(&t)]
    );
    assert!(search(&a, "quokka", &["insight"]).await.is_empty());
    assert!(!search(&a, "Чанк", &["term"]).await.is_empty());
    // после замещения старый термин из выдачи по умолчанию уходит
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{}/retract", id(&t)),
            Some(json!({"reason": "r"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(search(&a, "quokka", &["term"]).await.is_empty());
}

/// Рёбра заметки `node` (`GET /nodes/{id}/links`).
async fn links(a: &Api, node: &str) -> Vec<Value> {
    let r = a
        .t(Method::GET, &format!("/api/v1/nodes/{node}/links"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["items"].as_array().unwrap().clone()
}

#[tokio::test]
async fn wikilink_resolves_to_term() {
    let a = api("glossary-wiki", |_| {}).await;
    // ссылка раньше термина: ожидает, при записи термина разрешается
    let early = id(&a
        .note(
            "knowledge",
            "early",
            "# Ранняя\n\n[[Выжимка]] и [[summary]] и [[Нет такого]]\n",
        )
        .await);
    let e = links(&a, &early).await;
    assert_eq!(e.len(), 3);
    assert!(e.iter().all(|x| x["dst"].is_null()), "{e:?}");
    let t = created(&a, term("Выжимка", &["summary"])).await;
    let tid = id(&t);
    let e = links(&a, &early).await;
    for raw in ["[[Выжимка]]", "[[summary]]"] {
        let d = e
            .iter()
            .find(|x| x["dst_raw"] == raw)
            .unwrap_or_else(|| panic!("{raw}: {e:?}"))["dst"]
            .as_str()
            .unwrap_or_else(|| panic!("{raw} не разрешена: {e:?}"))
            .to_string();
        assert!(d.ends_with(&tid), "{raw}: {d}");
        assert!(d.starts_with("t:") || d.starts_with("e:"), "{d}");
    }
    assert!(
        e.iter()
            .any(|x| x["dst_raw"] == "[[Нет такого]]" && x["dst"].is_null())
    );
    // ссылка после термина: разрешается сразу, регистр и пунктуация не важны
    let late = id(&a
        .note(
            "knowledge",
            "late",
            "# Поздняя\n\n[[ВЫЖИМКА]] и [[Summary]]\n",
        )
        .await);
    for x in links(&a, &late).await {
        assert!(x["dst"].as_str().is_some_and(|d| d.ends_with(&tid)), "{x}");
    }
    // обратные ссылки термина
    let r = a
        .t(
            Method::GET,
            &format!("/api/v1/entries/{tid}/backlinks"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["items"].as_array().unwrap().len(),
        4,
        "{}",
        r.text()
    );
    // заметка с таким именем приоритетнее термина
    let note = id(&a.note("knowledge", "Выжимка", "# Выжимка\n").await);
    let x = links(&a, &early).await;
    let d = x.iter().find(|x| x["dst_raw"] == "[[Выжимка]]").unwrap()["dst"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(d.ends_with(&note), "{d}");
    // отзыв термина делает его ссылки неразрешёнными
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{tid}/retract"),
            Some(json!({"reason": "r"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let x = links(&a, &late).await;
    assert!(x.iter().all(|x| x["dst"].is_null()), "{x:?}");
}

#[tokio::test]
async fn glossary_version_increments() {
    let a = api("glossary-version", |_| {}).await;
    assert_eq!(version(&a).await, 0);
    let t = created(&a, term("Выжимка", &[])).await;
    let v1 = version(&a).await;
    assert!(v1 > 0);
    // запись заметки и insight версию не меняет
    a.note("knowledge", "n", "# n\n").await;
    created(
        &a,
        json!({"kind": "insight", "title": "i", "data": {"kind": "gotcha"}}),
    )
    .await;
    assert_eq!(version(&a).await, v1);
    let uri = format!("/api/v1/entries/{}", id(&t));
    let r = a
        .tok_rev(&a.token, Method::PATCH, &uri, Some(json!({"body": "иное"})))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let v2 = version(&a).await;
    assert!(v2 > v1);
    let r = a.tok_rev(&a.token, Method::DELETE, &uri, None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let v3 = version(&a).await;
    assert!(v3 > v2);
    let r = a.t(Method::POST, &format!("{uri}/restore"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let v4 = version(&a).await;
    assert!(v4 > v3);
    let r = a
        .t(
            Method::POST,
            &format!("{uri}/retract"),
            Some(json!({"reason": "r"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(version(&a).await > v4);
    // отклонённая запись (конфликт имён) версию не меняет
    created(&a, term("Чанк", &["chunk"])).await;
    let v = version(&a).await;
    assert_eq!(
        post(&a, term("Чанк", &[])).await.status,
        StatusCode::CONFLICT
    );
    assert_eq!(version(&a).await, v);
}

#[tokio::test]
async fn human_space_term_422() {
    let a = api("glossary-human", |_| {}).await;
    // по токену Human не существует (404), как у insight; сессия WebUI до пространства
    // доходит, и там термины запрещены (422), Rules — тоже
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/human/entries",
            Some(term("Личное", &[])),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    for space in ["human", "rules"] {
        let r = a
            .adm(
                Method::POST,
                &format!("/api/v1/spaces/{space}/entries"),
                Some(term("Личное", &[])),
                None,
            )
            .await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{space}: {}",
            r.text()
        );
    }
}

async fn mcp(a: &Api, name: &str, args: Value) -> (String, bool) {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": name, "arguments": args}});
    let r = a
        .send(
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(header::HOST, "memory.lan:7700")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header(header::AUTHORIZATION, format!("Bearer {}", a.token))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let res = &j["result"];
    (
        res["content"][0]["text"].as_str().unwrap_or("").to_string(),
        res["isError"].as_bool() == Some(true),
    )
}

#[tokio::test]
async fn mcp_memory_log_term() {
    let a = api("glossary-mcp", |_| {}).await;
    let (text, err) = mcp(
        &a,
        "memory_log",
        json!({"kind": "term", "space": "knowledge", "title": "Выжимка",
               "body": "Подробности.",
               "data": {"definition": "Короткая сводка", "aliases": ["summary"]}}),
    )
    .await;
    assert!(!err, "{text}");
    assert!(text.contains("Записано t:"), "{text}");
    assert!(text.contains("term «Выжимка»"), "{text}");
    // конфликт имён — результат-ошибка с пояснением
    let (text, err) = mcp(
        &a,
        "memory_log",
        json!({"kind": "term", "space": "knowledge", "title": "Summary",
               "data": {"definition": "иное"}}),
    )
    .await;
    assert!(err, "{text}");
    assert!(text.contains("409"), "{text}");
    // без определения — ошибка проверки
    let (text, err) = mcp(
        &a,
        "memory_log",
        json!({"kind": "term", "space": "knowledge", "title": "Без определения"}),
    )
    .await;
    assert!(err, "{text}");
    assert!(text.contains("data.definition"), "{text}");
    // термин виден в словаре сервера
    let r = a
        .t(Method::GET, "/api/v1/spaces/knowledge/glossary", None)
        .await;
    assert_eq!(r.json()["items"].as_array().unwrap().len(), 1);
}
