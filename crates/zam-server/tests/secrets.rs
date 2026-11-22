//! Сканер секретов перед индексацией (задача 11 M3, §7.6): значения не попадают в чанки (FTS,
//! эмбеддинги, Qdrant), ответ записи предупреждает `secrets_detected` без значений. Выдача агенту
//! (задача 5 M7.1, спецификация M7 §4.3.1): по токену текст заметок и записей — с `‹secret›`
//! вместо значения, сессии WebUI — исходник (он и хранится в базе). Без моделей (FTS).
mod common;
/// Шаблоны тестовых входов сканера (`zam-secrets`): вход упора правил M6 в предел повторов.
#[path = "../../zam-secrets/tests/common/mod.rs"]
mod scan_common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
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

/// Метка значения секрета в выдаче агенту.
const MARK: &str = "‹secret›";

/// `GET /nodes/{id}`: по токену — текст с маской (задача 5 M7.1; до неё — исходник), сессии
/// WebUI — исходник; ETag — ревизия у обоих (он же `If-Match` записи).
#[tokio::test]
async fn note_read_masked_for_token_original_for_session() {
    let a = api("secrets-read", |_| {}).await;
    let s = secret();
    let content = format!("# Ключи\n\ntoken: {s}\n");
    let n = a.note("knowledge", "ops/keys", &content).await;
    let id = n["id"].as_str().unwrap();
    let uri = format!("/api/v1/nodes/{id}");
    let r = a.t(Method::GET, &uri, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["content"], format!("# Ключи\n\ntoken: {MARK}\n"));
    let md = a.t(Method::GET, &format!("{uri}?format=md"), None).await;
    assert_eq!(md.text(), format!("# Ключи\n\ntoken: {MARK}\n"));
    assert_eq!(md.header("etag"), "\"1\"");
    let r = a.adm(Method::GET, &uri, None, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["content"], content);
    let md = a
        .adm(Method::GET, &format!("{uri}?format=md"), None, None)
        .await;
    assert_eq!(md.text(), content);
    assert_eq!(md.header("etag"), "\"1\"");
}

/// `POST /read` по токену: L2, L1, `section` и путь `<пространство>:/<путь>` — без значения
/// (маска всей ревизии до нарезки), в md и json; сессии WebUI — исходник.
#[tokio::test]
async fn agent_read_masks_l2_l1_section() {
    let a = api("secrets-agent-read", |_| {}).await;
    let s = secret();
    let content = format!(
        "# Ключи\n\nВход на стенд описан ниже.\n\n## Доступ\n\ntoken: {s}\n\n## Прочее\n\nтекст\n"
    );
    let n = a.note("knowledge", "ops/agent-keys", &content).await;
    let nid = format!("n:{}", n["id"].as_str().unwrap());
    let items = [
        json!({"id": &nid}),
        json!({"id": "knowledge:/ops/agent-keys"}),
        json!({"id": &nid, "tier": "L1"}),
        json!({"id": &nid, "section": "Доступ"}),
    ];
    for item in &items {
        for fmt in ["json", "md"] {
            let r = a
                .t(
                    Method::POST,
                    "/api/v1/read",
                    Some(json!({"items": [item], "format": fmt})),
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            assert!(!r.text().contains(&s), "{item} {fmt}: {}", r.text());
        }
    }
    let read = |item: Value| {
        let a = &a;
        async move {
            let r = a
                .t(Method::POST, "/api/v1/read", Some(json!({"items": [item]})))
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()["items"][0]["text"].as_str().unwrap().to_string()
        }
    };
    assert_eq!(
        read(items[0].clone()).await,
        zam_markdown::mask_text(&content)
    );
    assert_eq!(
        read(items[3].clone()).await,
        format!("## Доступ\n\ntoken: {MARK}\n\n")
    );
    // человеку — исходник
    let r = a
        .adm(
            Method::POST,
            "/api/v1/read",
            Some(json!({"items": [{"id": &nid}, {"id": &nid, "section": "Доступ"}]})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["items"][0]["text"], content.as_str(), "{j}");
    assert!(j["items"][1]["text"].as_str().unwrap().contains(&s), "{j}");
}

/// Дифф `since_rev` и история узла по токену: каждая ревизия маскируется до диффа — значения нет
/// ни в строках контекста, ни в добавленных; сессии WebUI — исходник.
#[tokio::test]
async fn agent_since_rev_diff_masked() {
    let a = api("secrets-since-rev", |_| {}).await;
    let s = secret();
    let p2 = format!("{}{}", "hunter2", "Xq9zLm");
    let v1 = format!("# Ключи\n\ntoken: {s}\n\nстрока 1\n");
    let v2 = format!("{v1}строка 2 и password={p2}\n");
    let n = a.note("knowledge", "ops/since", &v1).await;
    let id = n["id"].as_str().unwrap();
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{id}"),
            Some(json!({"content": &v2})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let clean = |t: &str| !t.contains(&s) && !t.contains(&p2);
    for fmt in ["json", "md"] {
        let r = a
            .t(
                Method::POST,
                "/api/v1/read",
                Some(json!({"items": [{"id": format!("n:{id}"), "since_rev": 1}], "format": fmt})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let t = r.text();
        assert!(clean(&t) && t.contains(MARK), "{fmt}: {t}");
    }
    for uri in [
        format!("/api/v1/nodes/{id}/diff?from=1&to=2"),
        format!("/api/v1/nodes/{id}/revisions/2"),
        format!("/api/v1/nodes/{id}/revisions/2?format=md"),
    ] {
        let r = a.t(Method::GET, &uri, None).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        assert!(
            clean(&r.text()) && r.text().contains(MARK),
            "{uri}: {}",
            r.text()
        );
        let r = a.adm(Method::GET, &uri, None, None).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        assert!(r.text().contains(&p2), "{uri}: {}", r.text());
    }
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
    // PATCH тоже предупреждает. Правка по токену — агент по маске: только новая находка (ключ
    // `token:` был в тексте и раньше — агенту он не новость; круг правок 1 задачи 6 M7.1, M4)
    let id = j["id"].as_str().unwrap();
    let r = a
        .t(
            Method::PATCH,
            &format!("/api/v1/nodes/{id}"),
            Some(json!({"base_rev": 1, "ops": [{"op": "append", "content": "\npassword=abcdef123\n"}]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let d = r.json()["secrets_detected"].as_array().unwrap().clone();
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0]["kind"], "key_value");
    // та же правка сессией WebUI — все находки текста, как прежде
    let r = a
        .adm_h(
            Method::PATCH,
            &format!("/api/v1/nodes/{id}"),
            Some(json!({"base_rev": 2, "ops": [{"op": "append", "content": "\nеще строка\n"}]})),
            &[],
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
    // находки и подсказка значения не содержат; эхо записи по токену — с маской (задача 5 M7.1)
    assert!(!j["secrets_detected"].to_string().contains(&s));
    assert!(!j["secrets_hint"].to_string().contains(&s));
    assert!(!j.to_string().contains(&s), "{j}");
    assert!(j["body"].as_str().unwrap().contains(MARK), "{j}");
    let id: Id = j["id"].as_str().unwrap().parse().unwrap();
    // исходное тело записи не меняется: сессия WebUI читает его как есть
    let r = a
        .adm(Method::GET, &format!("/api/v1/entries/{id}"), None, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["body"].as_str().unwrap().contains(&s));
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let chunks = entry_chunks_for_index(&db, id).await.unwrap();
    assert!(!chunks.is_empty());
    assert!(chunks.iter().all(|c| !c.text.contains(&s)));
    assert!(chunks.iter().any(|c| c.text.contains("‹secret›")));
    assert_eq!(count(&search(&a, &s).await), 0);
    assert!(count(&search(&a, "логах").await) >= 1);
    // PATCH записи по токену: только новая находка (`API_TOKEN` был и раньше — агенту не
    // новость; круг правок 1 задачи 6 M7.1, M4)
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{id}"),
            Some(json!({"append_body": "\npassword: another-secret-1"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["secrets_detected"].as_array().unwrap().len(), 1);
}

/// Секрет в заголовке записи и в теге не попадает ни в `chunks.context_prefix`, ни в FTS.
#[tokio::test]
async fn secret_in_title_and_tag_masked_in_prefix() {
    let a = api("secrets-prefix", |_| {}).await;
    let s = secret();
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(json!({"kind": "insight", "title": format!("Ключ {s} утёк"),
                        "body": "Обычный текст записи.", "tags": [format!("tok-{s}")],
                        "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id: Id = r.json()["id"].as_str().unwrap().parse().unwrap();
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let chunks = entry_chunks_for_index(&db, id).await.unwrap();
    assert!(!chunks.is_empty());
    for c in &chunks {
        assert!(
            !c.text.contains(&s) && !c.context_prefix.contains(&s),
            "{c:?}"
        );
    }
    let like = format!("%{s}%");
    let n: i64 = db
        .read(move |c| {
            Ok(c.query_row(
                "SELECT count(*) FROM chunks WHERE context_prefix LIKE ?1 OR text LIKE ?1",
                [like],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(n, 0);
    assert_eq!(count(&search(&a, &s).await), 0);

    // заметка: секрет в теге frontmatter и в имени
    let n = a
        .note(
            "knowledge",
            "ops/tags",
            &format!("---\ntags: [\"api_key={s}\"]\n---\n# Теги\n\nтекст заметки\n"),
        )
        .await;
    let nid: Id = n["id"].as_str().unwrap().parse().unwrap();
    for c in chunks_for_index(&db, nid).await.unwrap() {
        assert!(!c.context_prefix.contains(&s), "{}", c.context_prefix);
    }
    assert_eq!(count(&search(&a, &s).await), 0);
}

/// Записи журналов по токену (задача 5 M7.1): карточка `GET /entries/{id}`, лента, ревизии, эхо
/// `POST` и `PATCH`, `/read` записи (L0, L1, L2, дифф `since_rev`) — без значения ни в заголовке,
/// ни в теле, ни в строках `data`; заголовок с ключом — маской в карточке поиска и в подсказке
/// `/recall`. Сессии WebUI — исходник.
#[tokio::test]
async fn agent_entries_masked() {
    let a = api("secrets-agent-entries", |c| c.recall.timeout_ms = 30_000).await;
    let s = secret();
    let post = |body: Value| {
        let a = &a;
        async move {
            let r = a
                .t(Method::POST, "/api/v1/spaces/knowledge/entries", Some(body))
                .await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
            r
        }
    };
    let r = post(
        json!({"kind": "insight", "title": format!("Ключ {s} в логах сборки"),
                         "body": format!("В логе нашёлся токен:\nAPI_TOKEN={s}\n"),
                         "triggers": [{"type": "keyword", "pattern": "сборка"}],
                         "data": {"kind": "gotcha"}}),
    )
    .await;
    assert!(!r.text().contains(&s), "эхо POST: {}", r.text());
    let j = r.json();
    assert!(j["title"].as_str().unwrap().contains(MARK), "{j}");
    let gotcha = j["id"].as_str().unwrap().to_string();
    // секрет в строке `data`
    let r = post(json!({"kind": "insight", "title": "Опыт с ротацией ключей",
                         "body": "Проверили ротацию.",
                         "data": {"kind": "experiment",
                                  "experiment": {"method": format!("token: {s}"),
                                                 "result": "confirmed"}}}))
    .await;
    assert!(!r.text().contains(&s), "эхо POST data: {}", r.text());
    let exp = r.json()["id"].as_str().unwrap().to_string();
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{gotcha}"),
            Some(json!({"append_body": format!("\nещё раз: token: {s}")})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!r.text().contains(&s), "эхо PATCH: {}", r.text());
    for uri in [
        format!("/api/v1/entries/{gotcha}"),
        format!("/api/v1/entries/{exp}"),
        format!("/api/v1/entries/{gotcha}/revisions"),
        "/api/v1/spaces/knowledge/entries?kind=insight".to_string(),
        "/api/v1/spaces/knowledge/entries".to_string(),
        // заголовок — в узле графа и в триггере (его печатают хуки)
        format!("/api/v1/graph?entry={gotcha}"),
        "/api/v1/triggers".to_string(),
    ] {
        let r = a.t(Method::GET, &uri, None).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        assert!(!r.text().contains(&s), "{uri}: {}", r.text());
        assert!(r.text().contains(MARK), "{uri}: {}", r.text());
        // человеку — исходник
        let r = a.adm(Method::GET, &uri, None, None).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        assert!(r.text().contains(&s), "{uri}: {}", r.text());
    }
    // `/read` записи: L0 (заголовок), L1, L2 и дифф с ревизии 1
    for item in [
        json!({"id": format!("i:{gotcha}"), "tier": "L0"}),
        json!({"id": format!("i:{gotcha}"), "tier": "L1"}),
        json!({"id": format!("i:{gotcha}")}),
        json!({"id": format!("i:{gotcha}"), "since_rev": 1}),
        json!({"id": format!("i:{exp}"), "since_rev": 1}),
    ] {
        for fmt in ["json", "md"] {
            let r = a
                .t(
                    Method::POST,
                    "/api/v1/read",
                    Some(json!({"items": [&item], "format": fmt})),
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            assert!(!r.text().contains(&s), "{item} {fmt}: {}", r.text());
        }
    }
    // карточка поиска: заголовок записи — с маской
    for fmt in ["json", "md"] {
        let r = a
            .t(
                Method::POST,
                "/api/v1/search",
                Some(json!({"query": "логах сборки", "format": fmt})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert!(r.text().contains(&gotcha), "{fmt}: {}", r.text());
        assert!(!r.text().contains(&s), "{fmt}: {}", r.text());
        assert!(r.text().contains(MARK), "{fmt}: {}", r.text());
    }
    let r = a
        .adm(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "логах сборки"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains(&s), "человеку — исходник: {}", r.text());
    // подсказка `/recall`: ловушка с ключом в заголовке — строка без значения
    let r = a
        .t(
            Method::POST,
            "/api/v1/recall",
            Some(json!({"event": "session_start"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains(&gotcha), "{}", r.text());
    assert!(!r.text().contains(&s), "{}", r.text());
}

/// Решение тестов маски круга правок 1 (задача 5 M7.1): журналы релизов и ссылки на файлы
/// ведутся в решении.
const SOL: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e55";

async fn with_solution(name: &str) -> Api {
    let a = api(name, |_| {}).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": SOL, "name": "Маска"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    a
}

/// `POST` записи токеном → ответ (201).
async fn post_entry(a: &Api, space: &str, body: Value) -> Value {
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/spaces/{space}/entries"),
            Some(body),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()
}

/// `GET uri` по токену — без значения `s` и с меткой; тот же запрос из сессии WebUI — исходник.
async fn get_pair(a: &Api, uri: &str, s: &str) -> (Resp, Resp) {
    let t = a.t(Method::GET, uri, None).await;
    assert_eq!(t.status, StatusCode::OK, "{uri}: {}", t.text());
    assert!(
        !t.text().contains(s) && t.text().contains(MARK),
        "{uri}: {}",
        t.text()
    );
    let h = a.adm(Method::GET, uri, None, None).await;
    assert_eq!(h.status, StatusCode::OK, "{uri}: {}", h.text());
    assert!(
        h.text().contains(s),
        "человеку — исходник: {uri}: {}",
        h.text()
    );
    (t, h)
}

/// Ответ по токену без значения `s` (и с меткой, если `mark`).
fn clean(r: &Resp, s: &str, mark: bool, what: &str) {
    assert!(!r.text().contains(s), "{what}: {}", r.text());
    assert!(!mark || r.text().contains(MARK), "{what}: {}", r.text());
}

/// `GET /file-refs` (хук `post-tool` печатает заголовки агенту, круг правок 1): по токену —
/// заголовки с маской, `ETag` — по выданному (у маски свой, повтор с ним — 304); сессии WebUI —
/// исходник.
#[tokio::test]
async fn agent_file_refs_titles_masked() {
    let a = with_solution("secrets-file-refs").await;
    let s = secret();
    post_entry(
        &a,
        SOL,
        json!({"kind": "insight", "title": format!("Ключ {s} в конфиге"),
               "body": "Файл конфигурации.", "data": {"kind": "gotcha"},
               "refs": {"files": [{"path": "src/config.rs"}]}, "force": true}),
    )
    .await;
    let uri = format!("/api/v1/file-refs?solution={SOL}");
    let (t, h) = get_pair(&a, &uri, &s).await;
    assert!(t.text().contains("src/config.rs"), "{}", t.text());
    assert_ne!(t.header("etag"), h.header("etag"), "ETag — по выданному");
    let etag = t.header("etag").to_string();
    let r = a
        .tok_h(
            &a.token,
            Method::GET,
            &uri,
            None,
            &[("if-none-match", etag.as_str())],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED, "{}", r.text());
}

/// Словарь и 409 `term-conflict`: заголовок и определение термина агенту — с маской (и в
/// `conflicts[].title`, и в `detail`), сессии WebUI — исходник.
#[tokio::test]
async fn agent_glossary_and_term_conflict_masked() {
    let a = api("secrets-glossary", |_| {}).await;
    let s = secret();
    let j = post_entry(
        &a,
        "knowledge",
        json!({"kind": "term", "title": format!("Ключ {s}"), "body": "Термин.",
               "data": {"definition": format!("Определение: token: {s}"),
                        "aliases": ["деплой-ключ"]}}),
    )
    .await;
    assert!(!j.to_string().contains(&s), "эхо термина: {j}");
    get_pair(&a, "/api/v1/spaces/knowledge/glossary", &s).await;
    let dup = json!({"kind": "term", "title": "деплой-ключ", "body": "Другой.",
                     "data": {"definition": "Другое определение"}});
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(dup.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let p = r.json();
    assert_eq!(p["type"], "/problems/term-conflict", "{p}");
    clean(&r, &s, true, "409 term-conflict");
    assert!(
        p["conflicts"][0]["title"].as_str().unwrap().contains(MARK),
        "{p}"
    );
    let r = a
        .adm(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(dup),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(r.text().contains(&s), "человеку — исходник: {}", r.text());
}

/// 409 `possible-duplicate`: заголовки кандидатов агенту — с маской.
#[tokio::test]
async fn agent_dedup_candidates_masked() {
    let a = api("secrets-dedup", |_| {}).await;
    let s = secret();
    let title = format!("Ключ {s} в логах деплоя");
    post_entry(
        &a,
        "knowledge",
        json!({"kind": "insight", "title": &title, "body": "Первое.", "data": {"kind": "gotcha"}}),
    )
    .await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(
                json!({"kind": "insight", "title": &title, "body": "Второе.",
                        "data": {"kind": "gotcha"}}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let p = r.json();
    assert_eq!(p["type"], "/problems/possible-duplicate", "{p}");
    clean(&r, &s, true, "409 possible-duplicate");
}

/// Выжимки кэша (`GET /cache`, `GET /cache/{id}`) и шаг 0 поиска: запрос и текст выжимки агенту
/// — с маской, сессии WebUI — исходник.
#[tokio::test]
async fn agent_cache_summaries_masked() {
    let a = api("secrets-cache", |_| {}).await;
    let s = secret();
    let n = a
        .note(
            "knowledge",
            "ops/deploy-keys",
            "# Деплой\n\nКлючи деплоя лежат в Vault.\n",
        )
        .await;
    let body = json!({"query": "где ключи деплоя",
                      "summary": format!("Ключ деплоя: token: {s} (n:x)."),
                      "sources": [{"id": format!("n:{}", n["id"].as_str().unwrap()), "rev": 1}]});
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/cache",
            Some(body),
            &[("x-zam-session", "s-mask")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    clean(&r, &s, false, "эхо выжимки");
    let cid = r.json()["id"].as_str().unwrap().to_string();
    get_pair(&a, "/api/v1/cache", &s).await;
    get_pair(&a, &format!("/api/v1/cache/{cid}"), &s).await;
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "где ключи деплоя"})),
            &[("x-zam-session", "s-other")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    clean(&r, &s, false, "шаг 0 поиска");
}

/// Сравнение релизов: тексты релизов, дифф заметок, записи changelog и сообщения коммитов агенту
/// — с маской, сессии WebUI — исходник.
#[tokio::test]
async fn agent_release_compare_masked() {
    let a = with_solution("secrets-releases").await;
    let s = secret();
    let release = |version: &str, notes: String, commits: Value| {
        json!({"kind": "release", "title": format!("Релиз {version}"), "body": notes,
               "data": {"version": version, "commits": commits}})
    };
    post_entry(&a, SOL, release("1.0.0", "старое\n".into(), json!([]))).await;
    post_entry(
        &a,
        SOL,
        json!({"kind": "changelog", "title": format!("Починка ключа {s}"),
               "body": "Подробности.", "data": {"kind": "fix"}}),
    )
    .await;
    let r2 = post_entry(
        &a,
        SOL,
        release(
            "1.1.0",
            format!("новое\ntoken: {s}\n"),
            json!([{"sha": "abc123", "subject": format!("fix: ключ {s}")}]),
        ),
    )
    .await;
    let uri = format!("/api/v1/releases/{}/compare", r2["id"].as_str().unwrap());
    let (t, _) = get_pair(&a, &uri, &s).await;
    let j = t.json();
    assert!(
        j["notes_diff"]
            .as_str()
            .unwrap()
            .contains("+token: ‹secret›"),
        "{j}"
    );
    assert!(
        j["commits"][0]["subject"].as_str().unwrap().contains(MARK),
        "{j}"
    );
}

/// `claimed_tasks` сессии: заголовок захваченной задачи агенту — с маской.
#[tokio::test]
async fn agent_claimed_tasks_masked() {
    let a = api("secrets-claimed", |_| {}).await;
    let s = secret();
    let t = post_entry(
        &a,
        "knowledge",
        json!({"kind": "task", "title": format!("Ротация ключа {s}"), "body": "План.",
               "data": {}}),
    )
    .await;
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &format!("/api/v1/entries/{}/claim", t["id"].as_str().unwrap()),
            Some(json!({})),
            &[("x-zam-session", "cs-mask")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    clean(&r, &s, false, "эхо захвата");
    let r = a.t(Method::GET, "/api/v1/sessions/cs-mask", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["claimed_tasks"].as_array().unwrap().len(), 1, "{j}");
    clean(&r, &s, true, "claimed_tasks");
}

/// Узел и запись после операций — `revert`, `move`, удаление в корзину, корзина, `restore`: ответ
/// агенту без значения (frontmatter узла и текст записи — с маской).
#[tokio::test]
async fn agent_node_and_entry_ops_masked() {
    let a = api("secrets-ops", |_| {}).await;
    let s = secret();
    let content = format!("---\ndescription: \"ключ {s}\"\n---\n# Доступ\n\ntoken: {s}\n");
    let n = a.note("knowledge", "ops/ops-mask", &content).await;
    let nid = n["id"].as_str().unwrap().to_string();
    let node = format!("/api/v1/nodes/{nid}");
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &node,
            Some(json!({"content": format!("{content}\nстрока 2\n")})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let ops = [
        (
            Method::POST,
            format!("{node}/revert"),
            Some(json!({"rev": 1})),
        ),
        (
            Method::POST,
            format!("{node}/move"),
            Some(json!({"path": "ops/moved-mask"})),
        ),
    ];
    for (m, uri, body) in ops {
        let r = a.t(m, &uri, body).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        clean(&r, &s, true, &uri);
    }
    let r = a.t(Method::DELETE, &node, None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let r = a.t(Method::GET, "/api/v1/trash", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    clean(&r, &s, false, "корзина");
    let r = a.t(Method::POST, &format!("{node}/restore"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    clean(&r, &s, true, "восстановление узла");
    // запись: в корзину и обратно
    let e = post_entry(
        &a,
        "knowledge",
        json!({"kind": "insight", "title": format!("Ключ {s} в CI"), "body": format!("token: {s}"),
               "data": {"kind": "gotcha"}}),
    )
    .await;
    let entry = format!("/api/v1/entries/{}", e["id"].as_str().unwrap());
    let r = a
        .tok_h(&a.token, Method::DELETE, &entry, None, &[("if-match", "1")])
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let r = a.t(Method::POST, &format!("{entry}/restore"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    clean(&r, &s, true, "восстановление записи");
}

/// Авто-L0 заметки, где единственный секрет — флаг в инлайн-коде (круг правок 2 задачи 5 M7.1):
/// значения нет ни в L0 узла, ни в `/read` L0, ни в карточке поиска, ни в дереве пространства —
/// ни агенту, ни человеку (L0 строится по маскированному исходнику и так хранится).
#[tokio::test]
async fn auto_l0_inline_flag_masked_everywhere() {
    let a = api("secrets-l0-inline", |_| {}).await;
    let pw = ["S3c", "ret", "9x"].concat();
    let n = a
        .note(
            "knowledge",
            "ops/бд-доступ",
            &format!("# Доступ к БД\n\nПодключение: `mysql -uroot -p{pw} prod`.\n"),
        )
        .await;
    assert!(!n.to_string().contains(&pw), "эхо записи: {n}");
    let nid = n["id"].as_str().unwrap().to_string();
    let requests = [
        (Method::GET, format!("/api/v1/nodes/{nid}?tier=L0"), None),
        (Method::GET, format!("/api/v1/nodes/{nid}"), None),
        (
            Method::POST,
            "/api/v1/read".to_string(),
            Some(json!({"items": [{"id": format!("n:{nid}"), "tier": "L0"}]})),
        ),
        (
            Method::POST,
            "/api/v1/search".to_string(),
            Some(json!({"query": "Подключение prod"})),
        ),
        (
            Method::GET,
            "/api/v1/spaces/knowledge/tree?path=ops".to_string(),
            None,
        ),
    ];
    for (m, uri, body) in requests {
        let r = a.t(m.clone(), &uri, body.clone()).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        // поиск: сначала — карточка заметки в выдаче (иначе «значения нет» ничего не доказывает)
        if uri.ends_with("/search") {
            assert!(
                r.text().contains(&nid),
                "карточки заметки нет: {}",
                r.text()
            );
        }
        assert!(!r.text().contains(&pw), "{uri}: {}", r.text());
        if uri.contains("L0") || uri.contains("/read") {
            assert!(r.text().contains(MARK), "{uri}: {}", r.text());
        }
        // человеку: L0 хранится без значения (в `GET /nodes/{id}` — исходный текст заметки)
        if !uri.ends_with(&nid) {
            let h = a.adm(m, &uri, body, None).await;
            assert_eq!(h.status, StatusCode::OK, "{uri}: {}", h.text());
            if uri.ends_with("/search") {
                assert!(h.text().contains(&nid), "карточки нет: {}", h.text());
            }
            assert!(!h.text().contains(&pw), "человеку L0: {uri}: {}", h.text());
        }
    }
}

/// `range` из сессии WebUI режет исходник (смещения — по исходному тексту), по токену — маску
/// всей ревизии (смещения — по ней).
#[tokio::test]
async fn range_session_original_token_masked() {
    let a = api("secrets-range-pair", |_| {}).await;
    let s = secret();
    let content = format!("# Ключи\n\ntoken: {s}\n\nхвост\n");
    let n = a.note("knowledge", "ops/range-pair", &content).await;
    let nid = format!("n:{}", n["id"].as_str().unwrap());
    let masked = zam_markdown::mask_text(&content);
    let body = |len: usize| json!({"items": [{"id": &nid, "range": format!("r1:0-{len}")}]});
    let r = a
        .t(Method::POST, "/api/v1/read", Some(body(masked.len())))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["items"][0]["text"],
        masked.as_str(),
        "{}",
        r.text()
    );
    let r = a
        .adm(
            Method::POST,
            "/api/v1/read",
            Some(body(content.len())),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["items"][0]["text"],
        content.as_str(),
        "{}",
        r.text()
    );
}

/// Вход, на котором сканер правил M6 упирался в предел повторов (фикстура `zam-secrets`, задача
/// 15b M6).
fn depth_capped_text() -> String {
    scan_common::expand(
        include_str!("../../zam-secrets/tests/fixtures/scan_depth_capped.txt").trim_end(),
    )
}

/// Задача 3 M7.1 (спецификация M7 §4.2 п. 4): цепочка «пароль в URL ↔ ключ-значение», на которой
/// сканер правил M6 упирался в `FIXPOINT_MAX`, доходит до неподвижной точки — запись заметки и
/// записи журнала получает `secrets_detected`, но не `secrets_scan_depth`, а значения цепочки в
/// поиск не попадают. Обычный секрет — тоже только `secrets_detected`. (Упор теперь расширяет
/// находки, и известные входы до него не доходят: поиск `generate_depth_capped` — журнал M7.)
#[tokio::test]
async fn scan_depth_fixture_no_warning() {
    let a = api("secrets-depth", |_| {}).await;
    let t = depth_capped_text();
    let n = a
        .note("knowledge", "ops/depth", &format!("# Глубина\n\n{t}\n"))
        .await;
    assert_eq!(n["warnings"], json!(["secrets_detected"]), "{n}");
    let e = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(
                json!({"kind": "insight", "title": "Глубина", "body": t, "force": true,
                        "certainty": "hypothesis", "data": {"kind": "discovery"}}),
            ),
        )
        .await;
    assert_eq!(e.status, StatusCode::CREATED, "{}", e.text());
    let e = e.json();
    let w = e["warnings"].as_array().expect("warnings");
    assert!(w.contains(&json!("secrets_detected")), "{e}");
    assert!(!w.contains(&json!("secrets_scan_depth")), "{e}");
    // значения цепочки замаскированы в чанках заметки и записи
    for v in [scan_common::rnd(40, 101), scan_common::rnd(20, 301)] {
        assert_eq!(count(&search(&a, &v).await), 0, "{v}");
    }
    // обычный секрет — только `secrets_detected`
    let n = a
        .note(
            "knowledge",
            "ops/plain",
            &format!("# K\n\ntoken: {}\n", secret()),
        )
        .await;
    assert_eq!(n["warnings"], json!(["secrets_detected"]), "{n}");
}

/// Журнал сервера в буфер (подписчик `tracing` на время `f`).
fn captured_log(f: impl FnOnce()) -> String {
    use std::sync::{Arc, Mutex};
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buf {
        type Writer = Buf;
        fn make_writer(&'a self) -> Buf {
            self.clone()
        }
    }
    let buf = Buf::default();
    let sub = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(sub, f);
    String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
}

/// Задача 3 M7.1: на входе упора правил M6 сервер больше не пишет WARN `secrets_scan_depth` (скан
/// сходится), ответ — только `secrets_detected`.
#[test]
fn scan_depth_fixture_not_logged() {
    let t = depth_capped_text();
    let mut v = json!({});
    let log = captured_log(|| zam_server::api::secrets::attach(&mut v, &t));
    assert!(!log.contains("secrets_scan_depth"), "{log}");
    assert_eq!(v["warnings"], json!(["secrets_detected"]), "{v}");
}

/// Упор сканера — WARN в журнал сервера без текста находок и самого текста (только счётчики), и
/// `warnings` получает `secrets_scan_depth`. Входа, который упирается в предел, с задачи 3 M7.1
/// нет: итог скана с упором подаётся в `attach_scan` (находки — настоящие, со входа фикстуры).
#[test]
fn scan_depth_warn_without_text() {
    let t = depth_capped_text();
    let hits = zam_markdown::scan_secrets(&t);
    assert!(!hits.is_empty());
    let scan = zam_markdown::ScanOutcome {
        hits: hits.clone(),
        passes: 7,
        depth_capped: true,
    };
    let mut v = json!({});
    let log = captured_log(|| zam_server::api::secrets::attach_scan(&mut v, t.len(), scan));
    assert!(
        log.contains("WARN") && log.contains("secrets_scan_depth"),
        "{log}"
    );
    assert!(log.contains("passes=7"), "{log}");
    assert!(log.contains(&format!("text_bytes={}", t.len())), "{log}");
    // ни значений находок, ни фрагментов текста (в том числе случайных строк фикстуры)
    for h in &hits {
        let s = &t[h.range.clone()];
        assert!(!log.contains(s), "{s}: {log}");
    }
    for frag in [
        &scan_common::rnd(40, 101)[..12],
        &scan_common::rnd(20, 301)[..12],
        "x://",
        "pwd=",
    ] {
        assert!(!log.contains(frag), "{frag}: {log}");
    }
    assert_eq!(
        v["warnings"],
        json!(["secrets_detected", "secrets_scan_depth"]),
        "{v}"
    );
}

// --- Восстановление значений при записи агентом (задача 6 M7.1, спецификация M7 §4.3.2) ---

/// Второй «ключ» из частей.
fn secret2() -> String {
    format!("gh{}_{}", "p", "Zz9yX8wV7uT6sR5qP4oN3mL2kJ1iH0gF9eD8")
}

/// Текст заметки для человека (сессия WebUI) — исходник в базе.
async fn human_text(a: &Api, id: &str) -> String {
    let r = a
        .adm(Method::GET, &format!("/api/v1/nodes/{id}"), None, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["content"].as_str().unwrap().to_string()
}

/// L2 заметки по токену (`POST /read`) — вид агента.
async fn agent_text(a: &Api, id: &str) -> String {
    let r = a
        .t(
            Method::POST,
            "/api/v1/read",
            Some(json!({"items": [{"id": format!("n:{id}")}]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["items"][0]["text"].as_str().unwrap().to_string()
}

/// Предупреждение `secret_mark_kept` ответа записи: число в поле и строка в `warnings`.
fn kept_of(j: &Value) -> Option<u64> {
    let n = j["secret_mark_kept"].as_u64();
    let w = j["warnings"].as_array().and_then(|w| {
        w.iter()
            .filter_map(Value::as_str)
            .find(|s| s.starts_with("secret_mark_kept: "))
    });
    assert_eq!(n.is_some(), w.is_some(), "поле и строка — вместе: {j}");
    if let (Some(n), Some(w)) = (n, w) {
        assert!(w.starts_with(&format!("secret_mark_kept: {n} ")), "{w}");
    }
    n
}

/// `PUT` агента по маскированному L2: правка другой строки и правка вокруг метки сохраняют
/// значения в базе (чтение сессией); новая строка с `‹secret›` — литерал с предупреждением;
/// эхо ответа — с маской.
#[tokio::test]
async fn agent_put_restores_secret_values() {
    let a = api("secrets-restore-put", |_| {}).await;
    let s = secret();
    let pw = ["pw", "Qz9", "x77"].concat();
    let content = format!(
        "# Стенд\n\nВход описан ниже.\n\n## Доступ\n\ntoken: {s}\nбаза: \
         https://user:{pw}@db.example.com/\n\n## Прочее\n\nтекст\n"
    );
    let n = a.note("knowledge", "ops/restore-put", &content).await;
    let id = n["id"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/nodes/{id}");
    let seen = agent_text(&a, &id).await;
    assert!(
        !seen.contains(&s) && !seen.contains(&pw) && seen.contains(MARK),
        "{seen}"
    );
    let edited = seen.replace("текст\n", "текст, правка агента\n");
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &uri,
            Some(json!({"content": edited})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), None, "{}", r.text());
    assert!(!r.text().contains(&s), "эхо — с маской: {}", r.text());
    let v2 = content.replace("текст\n", "текст, правка агента\n");
    assert_eq!(human_text(&a, &id).await, v2);
    // правка вокруг метки и новая строка с меткой
    let seen = agent_text(&a, &id).await;
    let edited = seen.replace(
        &format!("token: {MARK}\n"),
        &format!("token: {MARK} (prod)\nновый ключ: {MARK}\n"),
    );
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &uri,
            Some(json!({"content": edited})),
            &[("if-match", "2")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), Some(1), "{}", r.text());
    assert_eq!(
        human_text(&a, &id).await,
        v2.replace(
            &format!("token: {s}\n"),
            &format!("token: {s} (prod)\nновый ключ: {MARK}\n")
        )
    );
}

/// `PATCH` агента: секция с ключом в заголовке адресуется маскированным заголовком; замена
/// секции с меткой из прежнего текста и дописка в конец — значения на месте, новая метка —
/// литерал с предупреждением; старая `base_rev` с неизменённой (по виду) секцией проходит.
#[tokio::test]
async fn agent_patch_masked_heading() {
    let a = api("secrets-restore-patch", |_| {}).await;
    let (s, s2) = (secret(), secret2());
    let content = format!("# Ключи\n\n## Стенд {s}\n\nстарое\n\n## Доступ\n\ntoken: {s2}\n");
    let n = a.note("knowledge", "ops/restore-patch", &content).await;
    let id = n["id"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/nodes/{id}");
    assert!(
        agent_text(&a, &id)
            .await
            .contains(&format!("## Стенд {MARK}\n")),
        "заголовок — с маской"
    );
    let heading = format!("Ключи > Стенд {MARK}");
    let r = a
        .t(
            Method::PATCH,
            &uri,
            Some(json!({"base_rev": 1, "ops": [
                {"op": "replace_section", "section": &heading, "content": "\nновое\n\n"}]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), None, "{}", r.text());
    assert!(!r.text().contains(&s), "эхо — с маской: {}", r.text());
    assert_eq!(
        human_text(&a, &id).await,
        format!("# Ключи\n\n## Стенд {s}\n\nновое\n\n## Доступ\n\ntoken: {s2}\n")
    );
    let r = a
        .t(
            Method::PATCH,
            &uri,
            Some(json!({"base_rev": 2, "ops": [
                {"op": "replace_section", "section": "Ключи > Доступ",
                 "content": format!("token: {MARK}\nещё строка\n")},
                {"op": "append", "content": format!("хвост {MARK}\n")}]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), Some(1), "{}", r.text());
    let t = human_text(&a, &id).await;
    assert!(t.contains(&format!("## Стенд {s}\n")), "{t}");
    assert!(t.contains(&format!("token: {s2}\nещё строка\n")), "{t}");
    assert!(t.contains(&format!("хвост {MARK}\n")), "{t}");
    // base_rev 2 при текущей 3: секция «Стенд ‹secret›» с тех пор не менялась
    let r = a
        .t(
            Method::PATCH,
            &uri,
            Some(json!({"base_rev": 2, "ops": [
                {"op": "append_to_section", "section": &heading, "content": "дописано\n"}]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let t = human_text(&a, &id).await;
    assert!(
        t.contains(&format!("## Стенд {s}\n\nновое\n")) && t.contains("дописано\n"),
        "{t}"
    );
    assert!(t.contains(&format!("token: {s2}\n")), "{t}");
}

/// Правка записи журнала по токену по маскированному виду: заголовок, тело, строка `data` и
/// следующий шаг задачи — значения на месте; дописка с меткой — литерал с предупреждением.
#[tokio::test]
async fn agent_entry_edit_restores_secret() {
    let a = api("secrets-restore-entry", |_| {}).await;
    let (s, s2) = (secret(), secret2());
    let e = post_entry(
        &a,
        "knowledge",
        json!({"kind": "insight", "title": format!("Ключ {s} в логах"),
               "body": format!("В логе:\nAPI_TOKEN={s}\nисправлено.\n"),
               "data": {"kind": "gotcha", "evidence": {"checked": format!("token: {s2}"),
                                                       "method": "grep", "result": "найден"}}}),
    )
    .await;
    let uri = format!("/api/v1/entries/{}", e["id"].as_str().unwrap());
    let v = a.t(Method::GET, &uri, None).await.json();
    assert!(
        !v.to_string().contains(&s) && !v.to_string().contains(&s2),
        "{v}"
    );
    let checked = v["data"]["evidence"]["checked"].as_str().unwrap();
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &uri,
            Some(json!({
                "title": format!("{} (CI)", v["title"].as_str().unwrap()),
                "body": v["body"].as_str().unwrap().replace("исправлено.", "исправлено в CI."),
                "data": {"kind": "gotcha", "evidence": {"checked": format!("{checked} (повтор)"),
                                                        "method": "grep", "result": "найден"}},
            })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), None, "{}", r.text());
    assert!(!r.text().contains(&s), "эхо — с маской: {}", r.text());
    let h = a.adm(Method::GET, &uri, None, None).await.json();
    assert_eq!(h["title"], format!("Ключ {s} в логах (CI)"), "{h}");
    assert_eq!(
        h["body"],
        format!("В логе:\nAPI_TOKEN={s}\nисправлено в CI.\n")
    );
    assert_eq!(
        h["data"]["evidence"]["checked"],
        format!("token: {s2} (повтор)")
    );
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &uri,
            Some(json!({"append_body": format!("ещё {MARK}")})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), Some(1), "{}", r.text());
    let h = a.adm(Method::GET, &uri, None, None).await.json();
    let body = h["body"].as_str().unwrap();
    assert!(
        body.contains(&format!("API_TOKEN={s}")) && body.ends_with(&format!("ещё {MARK}")),
        "{body}"
    );
    // задача: следующий шаг правлен по виду
    let t = post_entry(
        &a,
        "knowledge",
        json!({"kind": "task", "title": "Ротация ключа",
               "data": {"next_step": format!("проверить token: {s}")}}),
    )
    .await;
    let turi = format!("/api/v1/entries/{}", t["id"].as_str().unwrap());
    let v = a.t(Method::GET, &turi, None).await.json();
    let step = v["data"]["next_step"].as_str().unwrap();
    assert!(step.contains(MARK), "{v}");
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &turi,
            Some(json!({"status": "in_progress", "next_step": format!("{step} завтра")})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let h = a.adm(Method::GET, &turi, None, None).await.json();
    assert_eq!(
        h["data"]["next_step"],
        format!("проверить token: {s} завтра"),
        "{h}"
    );
}

/// Человек в сессии WebUI видит исходник и пишет без восстановления: литерал `‹secret›` в его
/// правке заметки и записи сохраняется как есть, без предупреждения.
#[tokio::test]
async fn session_writes_literal_mark_as_is() {
    let a = api("secrets-restore-session", |_| {}).await;
    let s = secret();
    let n = a
        .note(
            "knowledge",
            "ops/restore-human",
            &format!("# K\n\ntoken: {s}\n"),
        )
        .await;
    let id = n["id"].as_str().unwrap().to_string();
    let human = format!("# K\n\ntoken: {MARK}\nправка человека\n");
    let r = a
        .adm_h(
            Method::PUT,
            &format!("/api/v1/nodes/{id}"),
            Some(json!({"content": &human})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), None, "{}", r.text());
    assert_eq!(human_text(&a, &id).await, human);
    let e = post_entry(
        &a,
        "knowledge",
        json!({"kind": "insight", "title": "Ключ в логах", "body": format!("token: {s}"),
               "data": {"kind": "gotcha"}}),
    )
    .await;
    let uri = format!("/api/v1/entries/{}", e["id"].as_str().unwrap());
    let r = a
        .adm_h(
            Method::PATCH,
            &uri,
            Some(json!({"body": format!("token: {MARK} (человек)")})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), None, "{}", r.text());
    let h = a.adm(Method::GET, &uri, None, None).await.json();
    assert_eq!(h["body"], format!("token: {MARK} (человек)"));
}

/// Создание по токену с литералом `‹secret›` (базы нет — значение не сохранено; решение
/// координатора по задаче 6): заметка, запись журнала каждого вида (insight, changelog, задача,
/// идея, вопрос) — предупреждение `secret_mark_kept` с числом меток; повтор по ключу и создание
/// без меток — без него; сессия WebUI — без предупреждения.
#[tokio::test]
async fn agent_create_with_mark_warns() {
    let a = with_solution("secrets-create-mark").await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "knowledge", "path": "ops/new-mark", "kind": "note",
                        "content": format!("# K\n\ntoken: {MARK}\nещё {MARK}\n")}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(kept_of(&r.json()), Some(2), "{}", r.text());
    let n = a
        .note("knowledge", "ops/new-clean", "# K\n\nбез меток\n")
        .await;
    assert_eq!(kept_of(&n), None, "{n}");
    for (space, body, want) in [
        (
            "knowledge",
            json!({"kind": "insight", "title": format!("Ключ {MARK}"), "body": format!("token: {MARK}"),
                   "data": {"kind": "gotcha"}, "force": true}),
            2,
        ),
        (
            "knowledge",
            json!({"kind": "task", "title": "Ротация", "body": format!("ключ {MARK}"),
                   "data": {"next_step": format!("проверить {MARK}")}}),
            2,
        ),
        (
            "knowledge",
            json!({"kind": "idea", "title": "Идея", "body": format!("x {MARK}"),
                   "tags": [MARK], "data": {"origin": "agent"}}),
            2,
        ),
        (
            "knowledge",
            json!({"kind": "question", "title": "Какой ключ?", "body": format!("был {MARK}"),
                   "force": true}),
            1,
        ),
        (
            SOL,
            json!({"kind": "changelog", "title": "Сборка", "body": format!("token: {MARK}"),
                   "data": {"kind": "ops"}}),
            1,
        ),
    ] {
        let uri = format!("/api/v1/spaces/{space}/entries");
        let r = a.t(Method::POST, &uri, Some(body.clone())).await;
        assert_eq!(r.status, StatusCode::CREATED, "{body}: {}", r.text());
        assert_eq!(kept_of(&r.json()), Some(want), "{body}: {}", r.text());
        // сессия WebUI: человек видит исходник — литерал его, без предупреждения
        let mut human = body.clone();
        human["title"] = json!(format!("{} (человек)", body["title"].as_str().unwrap()));
        if human["data"].get("origin").is_some() {
            human["data"].as_object_mut().unwrap().remove("origin");
        }
        let r = a.adm(Method::POST, &uri, Some(human.clone()), None).await;
        assert_eq!(r.status, StatusCode::CREATED, "{human}: {}", r.text());
        assert_eq!(kept_of(&r.json()), None, "{human}: {}", r.text());
    }
    // повтор по ключу — запись уже создана, предупреждения нет
    let body = json!({"kind": "task", "title": "Повтор", "body": format!("ключ {MARK}")});
    let key = [("idempotency-key", "k-mark")];
    let first = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(body.clone()),
            &key,
        )
        .await;
    assert_eq!(kept_of(&first.json()), Some(1), "{}", first.text());
    let again = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(body),
            &key,
        )
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text());
    assert_eq!(kept_of(&again.json()), None, "{}", again.text());
    // человек создаёт заметку с литералом — без предупреждения
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "knowledge", "path": "ops/human-mark", "kind": "note",
                        "content": format!("# K\n\ntoken: {MARK}\n")}),
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(kept_of(&r.json()), None, "{}", r.text());
}

/// Замещение записи агентом по маскированному виду старой (`/supersede` и `supersedes_id`) —
/// значения в новой записи на месте.
#[tokio::test]
async fn agent_supersede_restores_secret() {
    let a = api("secrets-restore-supersede", |_| {}).await;
    let s = secret();
    let mut ids = Vec::new();
    for i in 0..2 {
        let e = post_entry(
            &a,
            "knowledge",
            json!({"kind": "insight", "title": format!("Ключ {s} в сборке {i}"),
                   "body": format!("token: {s}\nпервая версия {i}\n"), "data": {"kind": "gotcha"},
                   "force": true}),
        )
        .await;
        ids.push(e["id"].as_str().unwrap().to_string());
    }
    let shown = |v: &Value| {
        (
            v["title"].as_str().unwrap().to_string(),
            v["body"].as_str().unwrap().replace("первая", "вторая"),
        )
    };
    let v = a
        .t(Method::GET, &format!("/api/v1/entries/{}", ids[0]), None)
        .await
        .json();
    let (title, body) = shown(&v);
    assert!(body.contains(MARK) && !body.contains(&s), "{body}");
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{}/supersede", ids[0]),
            Some(json!({"title": title, "body": body, "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(kept_of(&r.json()["entry"]), None, "{}", r.text());
    let new1 = r.json()["entry"]["id"].as_str().unwrap().to_string();
    let v = a
        .t(Method::GET, &format!("/api/v1/entries/{}", ids[1]), None)
        .await
        .json();
    let (title, body) = shown(&v);
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(
                json!({"kind": "insight", "supersedes_id": &ids[1], "title": title,
                        "body": format!("{body}новая {MARK}\n"), "data": {"kind": "gotcha"}}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(kept_of(&r.json()), Some(1), "{}", r.text());
    let new2 = r.json()["id"].as_str().unwrap().to_string();
    for (id, i, tail) in [
        (new1, 0, String::new()),
        (new2, 1, format!("новая {MARK}\n")),
    ] {
        let h = a
            .adm(Method::GET, &format!("/api/v1/entries/{id}"), None, None)
            .await
            .json();
        assert_eq!(h["title"], format!("Ключ {s} в сборке {i}"), "{h}");
        assert_eq!(h["body"], format!("token: {s}\nвторая версия {i}\n{tail}"));
    }
}

/// 422 `secret_context_changed` (круг правок 1 задачи 6 M7.1, C1): ответ — код и строки
/// маскированного вида, без значения `s`; база не тронута.
fn context_changed(r: &Resp, s: &str, lines: &str) {
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    let j = r.json();
    assert_eq!(j["code"], "secret_context_changed", "{j}");
    assert_eq!(j["type"], "/problems/secret-context-changed", "{j}");
    let d = j["detail"].as_str().unwrap();
    assert!(d.contains(lines) && d.contains("WebUI"), "{d}");
    assert!(!r.text().contains(s), "{}", r.text());
}

/// Смена окружения метки (C1): агент снял забор кода вокруг `mysql -p‹secret›` (`PUT` и `PATCH`),
/// у ключа записи снят разделитель — запись отклонена 422, значение в базе прежнее; правка с
/// сохранённым контекстом проходит.
#[tokio::test]
async fn agent_context_changed_rejected() {
    let a = api("secrets-restore-context", |_| {}).await;
    let p = ["hunter", "7Qz9x"].concat();
    let content = format!("# Стенд\n\n## Доступ\n\n```bash\nmysql -p{p} db\n```\n");
    let n = a.note("knowledge", "ops/restore-context", &content).await;
    let id = n["id"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/nodes/{id}");
    let seen = agent_text(&a, &id).await;
    assert!(!seen.contains(&p), "{seen}");
    let unfenced = seen.replace("```bash\n", "").replace("```\n", "");
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &uri,
            Some(json!({"content": unfenced})),
            &[("if-match", "1")],
        )
        .await;
    context_changed(&r, &p, "строка 5");
    let r = a
        .t(
            Method::PATCH,
            &uri,
            Some(json!({"base_rev": 1, "ops": [{"op": "replace_section",
                "section": "Стенд > Доступ", "content": format!("\nmysql -p{MARK} db\n")}]})),
        )
        .await;
    context_changed(&r, &p, "строка 5");
    assert_eq!(human_text(&a, &id).await, content);
    // забор сохранён — проходит, значение на месте
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &uri,
            Some(json!({"content": seen.replace("db\n", "db --verbose\n")})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        human_text(&a, &id)
            .await
            .contains(&format!("mysql -p{p} db --verbose"))
    );
    // запись журнала: у ключа снят разделитель
    let e = post_entry(
        &a,
        "knowledge",
        json!({"kind": "insight", "title": "Пароль стенда", "body": format!("стенд\npassword = {p}\n"),
               "data": {"kind": "gotcha"}}),
    )
    .await;
    let euri = format!("/api/v1/entries/{}", e["id"].as_str().unwrap());
    let body = a.t(Method::GET, &euri, None).await.json()["body"]
        .as_str()
        .unwrap()
        .to_string();
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &euri,
            Some(json!({"body": body.replace("password = ", "password ")})),
        )
        .await;
    context_changed(&r, &p, "body: строка 2");
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &euri,
            Some(json!({"body": body.replace("стенд", "стенд prod")})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let h = a.adm(Method::GET, &euri, None, None).await.json();
    assert_eq!(h["body"], format!("стенд prod\npassword = {p}\n"));
}

/// Теги и `blocked_by` задачи по маске (M11): переставленный тег с ключом — прежний тег,
/// блокер правлен вокруг метки — значение на месте; повтор замещения по `Idempotency-Key` — без
/// `secret_mark_kept` (M9).
#[tokio::test]
async fn agent_tags_blocked_by_and_replay() {
    let a = api("secrets-restore-tags", |_| {}).await;
    let s = secret();
    let e = post_entry(
        &a,
        "knowledge",
        json!({"kind": "task", "title": "Ротация", "tags": [format!("t-{s}"), "ops"],
               "data": {"blocked_by": format!("ждём token: {s}")}}),
    )
    .await;
    let uri = format!("/api/v1/entries/{}", e["id"].as_str().unwrap());
    let v = a.t(Method::GET, &uri, None).await.json();
    let tags: Vec<Value> = v["tags"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .cloned()
        .collect();
    let blocked = v["data"]["blocked_by"].as_str().unwrap().to_string();
    assert!(!v.to_string().contains(&s), "{v}");
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &uri,
            Some(json!({"tags": tags, "status": "blocked",
                        "blocked_by": format!("{blocked} (до пятницы)")})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), None, "{}", r.text());
    let h = a.adm(Method::GET, &uri, None, None).await.json();
    assert_eq!(h["tags"], json!(["ops", format!("t-{s}")]), "{h}");
    assert_eq!(
        h["data"]["blocked_by"],
        format!("ждём token: {s} (до пятницы)")
    );
    // замещение с ключом повтора: первый ответ — предупреждение, повтор — без него
    let i = post_entry(
        &a,
        "knowledge",
        json!({"kind": "insight", "title": "Ключ стенда", "body": format!("token: {s}"),
               "data": {"kind": "gotcha"}}),
    )
    .await;
    let sup = format!("/api/v1/entries/{}/supersede", i["id"].as_str().unwrap());
    let body = json!({"title": "Ключ стенда", "body": format!("token: {MARK}\nновый {MARK}"),
                      "data": {"kind": "gotcha"}});
    let h = [("idempotency-key", "sup-1")];
    let r = a
        .tok_h(&a.token, Method::POST, &sup, Some(body.clone()), &h)
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(kept_of(&r.json()["entry"]), Some(1), "{}", r.text());
    let r = a.tok_h(&a.token, Method::POST, &sup, Some(body), &h).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()["entry"]), None, "{}", r.text());
}

/// Создание по токену с меткой (M7): выжимка кэша, её превращение в заметку и ответ на вопрос —
/// `secret_mark_kept`; без меток — без него.
#[tokio::test]
async fn agent_cache_and_answer_mark_kept() {
    let a = api("secrets-restore-cache", |_| {}).await;
    let n = a.note("knowledge", "ops/cache-src", "# K\n\nтекст\n").await;
    let src = format!("n:{}", n["id"].as_str().unwrap());
    let r = a
        .t(
            Method::POST,
            "/api/v1/cache",
            Some(
                json!({"query": "ключ стенда", "summary": format!("ключ {MARK}"),
                        "sources": [{"id": &src, "rev": 1}]}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(kept_of(&r.json()), Some(1), "{}", r.text());
    let cid = r.json()["id"].as_str().unwrap().to_string();
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/cache/{cid}/promote"),
            Some(json!({"as": "note", "path": "ops/from-cache"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(kept_of(&r.json()), Some(1), "{}", r.text());
    let r = a
        .t(
            Method::POST,
            "/api/v1/cache",
            Some(json!({"query": "без меток", "summary": "чисто",
                        "sources": [{"id": &src, "rev": 1}]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(kept_of(&r.json()), None, "{}", r.text());
    let q = post_entry(
        &a,
        "knowledge",
        json!({"kind": "question", "title": "Какой ключ у стенда?", "force": true}),
    )
    .await;
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{}/answer", q["id"].as_str().unwrap()),
            Some(json!({"text": format!("ключ {MARK}"), "by": "agent", "channel": "chat"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), Some(1), "{}", r.text());
}
