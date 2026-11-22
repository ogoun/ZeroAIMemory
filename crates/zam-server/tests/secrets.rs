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

/// Метка значения секрета в выдаче агенту (нередактируемая выдача, метка без номера).
const MARK: &str = "‹secret›";

/// Метка объекта и ревизии T — как у сервера (M7 §4.3.2 п. 1, круг правок 5 задачи 6 M7.1).
fn tg(id: &str, rev: i64) -> String {
    zam_server::api::agent_view::object_tag(&id, rev)
}

/// Метка `‹secret-T-N›` — маска N объекта с меткой T в редактируемой выдаче агенту (L2, поля
/// записи).
fn mk(t: &str, n: u32) -> String {
    format!("‹secret-{t}-{n}›")
}

/// Есть ли в тексте метка (с номером или без).
trait HasMark {
    fn has_mark(&self) -> bool;
}

impl HasMark for str {
    fn has_mark(&self) -> bool {
        zam_secrets::has_any_mark(self)
    }
}

/// Вид с метками объекта T — редактируемая выдача агенту.
fn numbered(t: &str, s: &str) -> String {
    zam_secrets::restore::numbered_view(t, s).into_owned()
}

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
    let t = tg(id, 1);
    assert_eq!(
        r.json()["content"],
        format!("# Ключи\n\ntoken: {}\n", mk(&t, 1))
    );
    let md = a.t(Method::GET, &format!("{uri}?format=md"), None).await;
    assert_eq!(md.text(), format!("# Ключи\n\ntoken: {}\n", mk(&t, 1)));
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
    let t = tg(n["id"].as_str().unwrap(), 1);
    assert_eq!(read(items[0].clone()).await, numbered(&t, &content));
    assert_eq!(
        read(items[3].clone()).await,
        format!("## Доступ\n\ntoken: {}\n\n", mk(&t, 1))
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
        assert!(clean(&t) && t.has_mark(), "{fmt}: {t}");
    }
    for uri in [
        format!("/api/v1/nodes/{id}/diff?from=1&to=2"),
        format!("/api/v1/nodes/{id}/revisions/2"),
        format!("/api/v1/nodes/{id}/revisions/2?format=md"),
    ] {
        let r = a.t(Method::GET, &uri, None).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        assert!(
            clean(&r.text()) && r.text().has_mark(),
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
    assert!(j["body"].as_str().unwrap().has_mark(), "{j}");
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
    assert!(j["title"].as_str().unwrap().has_mark(), "{j}");
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
        assert!(r.text().has_mark(), "{uri}: {}", r.text());
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
        assert!(r.text().has_mark(), "{fmt}: {}", r.text());
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
        !t.text().contains(s) && t.text().has_mark(),
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
    assert!(!mark || r.text().has_mark(), "{what}: {}", r.text());
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
        p["conflicts"][0]["title"].as_str().unwrap().has_mark(),
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
        j["commits"][0]["subject"].as_str().unwrap().has_mark(),
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

/// Подзадачи `part_of` (M7 §5.4 п. 2, задача 5 M7.2): заголовки общей в `parent.title` и шагов в
/// `subtasks.items[].title` агенту по токену — с маской `‹secret›` (карточка, лента, эхо правки),
/// сессии WebUI — исходник.
#[tokio::test]
async fn subtask_titles_masked_for_token() {
    let a = api("secrets-subtasks", |_| {}).await;
    let s = secret();
    let task = |title: String| json!({"kind": "task", "title": title, "body": "План."});
    let p = post_entry(&a, "knowledge", task(format!("Этап {s}"))).await;
    let st = post_entry(&a, "knowledge", task(format!("Шаг {s}"))).await;
    let (p, st) = (
        p["id"].as_str().unwrap().to_string(),
        st["id"].as_str().unwrap().to_string(),
    );
    let r = a
        .t(
            Method::POST,
            "/api/v1/edges",
            Some(json!({"src": format!("e:{st}"), "type": "part_of", "dst": format!("e:{p}")})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    // шаг: заголовок общей в `parent`
    let (t, h) = get_pair(&a, &format!("/api/v1/entries/{st}"), &s).await;
    assert_eq!(
        t.json()["parent"]["title"],
        format!("Этап {MARK}"),
        "{}",
        t.text()
    );
    assert_eq!(h.json()["parent"]["title"], format!("Этап {s}"));
    // общая: заголовки шагов в `subtasks.items`
    let (t, h) = get_pair(&a, &format!("/api/v1/entries/{p}"), &s).await;
    assert_eq!(
        t.json()["subtasks"]["items"][0]["title"],
        format!("Шаг {MARK}"),
        "{}",
        t.text()
    );
    assert_eq!(
        h.json()["subtasks"]["items"][0]["title"],
        format!("Шаг {s}")
    );
    // лента и эхо правки — без значения
    get_pair(&a, "/api/v1/spaces/knowledge/entries?kind=task", &s).await;
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{st}"),
            Some(json!({"status": "in_progress"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    clean(&r, &s, true, "эхо правки шага");
    assert_eq!(r.json()["parent"]["title"], format!("Этап {MARK}"));
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
            assert!(r.text().has_mark(), "{uri}: {}", r.text());
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

/// `range` из сессии WebUI режет исходник (смещения — по исходному тексту), по токену —
/// нумерованный вид всей ревизии (смещения — по нему, M7 §4.3.2 п. 6).
#[tokio::test]
async fn range_session_original_token_masked() {
    let a = api("secrets-range-pair", |_| {}).await;
    let s = secret();
    let content = format!("# Ключи\n\ntoken: {s}\n\nхвост\n");
    let n = a.note("knowledge", "ops/range-pair", &content).await;
    let nid = format!("n:{}", n["id"].as_str().unwrap());
    let masked = numbered(&tg(n["id"].as_str().unwrap(), 1), &content);
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

/// Предупреждение `secret_mark_from_rev` ответа записи (финальное ревью M7.1, N-3): список
/// ревизий в поле и строка в `warnings` с теми же номерами.
fn from_rev_of(j: &Value) -> Option<Vec<i64>> {
    let n: Option<Vec<i64>> = j["secret_mark_from_rev"]
        .as_array()
        .map(|a| a.iter().map(|x| x.as_i64().unwrap()).collect());
    let w = j["warnings"].as_array().and_then(|w| {
        w.iter()
            .filter_map(Value::as_str)
            .find(|s| s.starts_with("secret_mark_from_rev: "))
    });
    assert_eq!(n.is_some(), w.is_some(), "поле и строка — вместе: {j}");
    if let (Some(n), Some(w)) = (&n, w) {
        let list: Vec<String> = n.iter().map(i64::to_string).collect();
        assert!(
            w.starts_with(&format!("secret_mark_from_rev: {} ", list.join(", "))),
            "{w}"
        );
    }
    n
}

/// `PUT` агента по нумерованному L2 (`‹secret-N›`): правка другой строки и правка вокруг метки
/// сохраняют значения в базе (чтение сессией); новая строка с `‹secret›` — литерал с
/// предупреждением; эхо ответа — с маской.
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
        !seen.contains(&s) && !seen.contains(&pw) && seen.has_mark(),
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
        &format!("token: {}\n", mk(&tg(&id, 2), 1)),
        &format!("token: {} (prod)\nновый ключ: {MARK}\n", mk(&tg(&id, 2), 1)),
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

/// `PATCH` агента: секция с ключом в заголовке адресуется заголовком нумерованного вида; замена
/// секции с меткой из прежнего текста и дописка в конец — значения на месте по номерам, метка без
/// номера — литерал с предупреждением; старая `base_rev` с неизменённой секцией проходит.
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
            .contains(&format!("## Стенд {}\n", mk(&tg(&id, 1), 1))),
        "заголовок — с маской"
    );
    let heading = format!("Ключи > Стенд {}", mk(&tg(&id, 1), 1));
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
                 "content": format!("token: {}\nещё строка\n", mk(&tg(&id, 2), 2))},
                {"op": "append", "content": format!("хвост {MARK}\n")}]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), Some(1), "{}", r.text());
    let t = human_text(&a, &id).await;
    assert!(t.contains(&format!("## Стенд {s}\n")), "{t}");
    assert!(t.contains(&format!("token: {s2}\nещё строка\n")), "{t}");
    assert!(t.contains(&format!("хвост {MARK}\n")), "{t}");
    // base_rev 2 при текущей 3: секция «Стенд ‹secret-1›» с тех пор не менялась
    let r = a
        .t(
            Method::PATCH,
            &uri,
            Some(json!({"base_rev": 2, "ops": [
                {"op": "append_to_section", "content": "дописано\n",
                 "section": format!("Ключи > Стенд {}", mk(&tg(&id, 2), 1))}]})),
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
    // дописка со строкой вида тела (метка объекта, круг правок 5) — значение; отказ дописки —
    // модульный тест `restore_append_body`
    let v = a.t(Method::GET, &uri, None).await.json();
    let line = v["body"]
        .as_str()
        .unwrap()
        .lines()
        .find(|l| l.starts_with("API_TOKEN="))
        .unwrap()
        .to_string();
    assert!(!line.contains(&s), "{line}");
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &uri,
            Some(json!({"append_body": format!("прод: {line}")})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), None, "{}", r.text());
    let h = a.adm(Method::GET, &uri, None, None).await.json();
    assert!(
        h["body"]
            .as_str()
            .unwrap()
            .ends_with(&format!("прод: API_TOKEN={s}")),
        "{h}"
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
    assert!(step.has_mark(), "{v}");
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
    assert!(body.has_mark() && !body.contains(&s), "{body}");
    let sup = json!({"title": title, "body": body, "data": {"kind": "gotcha"}});
    let uri = format!("/api/v1/entries/{}/supersede", ids[0]);
    // метки с номером без ревизии замещаемой записи — 428 (§4.3.2 п. 3, круг правок 4, M6)
    let r = a.t(Method::POST, &uri, Some(sup.clone())).await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED, "{}", r.text());
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &uri,
            Some(sup),
            &[("if-match", "1")],
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
    let mut req = json!({"kind": "insight", "supersedes_id": &ids[1], "title": title,
                         "body": format!("{body}новая {MARK}\n"), "data": {"kind": "gotcha"}});
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(req.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED, "{}", r.text());
    req["supersedes_rev"] = json!(1);
    let r = a
        .t(Method::POST, "/api/v1/spaces/knowledge/entries", Some(req))
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

/// Смена окружения метки (C1): агент снял забор кода вокруг `mysql -p‹secret-1›` (`PUT` и `PATCH`),
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
                "section": "Стенд > Доступ", "content": format!("\nmysql -p{} db\n", mk(&tg(&id, 1), 1))}]})),
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
    // пробы ревью 3 и 4: URL переписан прозой, «password = ‹secret-2›» — «пароль от стенда —
    // ‹secret-2›»: значение восстановлено по номеру, но сканер прозу не закрывает — оба 422,
    // значение в базе прежнее
    let base = format!("# Доступ\n\nбаза: https://user:{p}@db.example.com/\npassword = {p}\n");
    let n2 = a.note("knowledge", "ops/restore-context-2", &base).await;
    let id2 = n2["id"].as_str().unwrap().to_string();
    let seen2 = agent_text(&a, &id2).await;
    for (edited, lines) in [
        (
            seen2
                .replace("https://user:", "логин user, пароль ")
                .replace("@db.example.com/", " (db.example.com)"),
            "строка 3",
        ),
        (
            seen2.replace("password = ", "пароль от стенда — "),
            "строка 4",
        ),
    ] {
        let r = a
            .tok_h(
                &a.token,
                Method::PUT,
                &format!("/api/v1/nodes/{id2}"),
                Some(json!({"content": edited})),
                &[("if-match", "1")],
            )
            .await;
        context_changed(&r, &p, lines);
    }
    assert_eq!(human_text(&a, &id2).await, base);
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

/// Устаревший `If-Match` и правка, которая вывела бы секрет из-под маски (M6 круга 1, B4 и B6
/// круга правок 2): `PUT` заметки, `PATCH` и замещение записи журнала — 409 с текущей ревизией,
/// а не 422; без значения в ответе, база не тронута.
#[tokio::test]
async fn agent_stale_rev_conflict_before_restore() {
    let a = api("secrets-restore-stale", |_| {}).await;
    let p = ["hunter", "8Qz9x"].concat();
    let content = format!("# Стенд\n\npassword = {p}\nтекст\n");
    let n = a.note("knowledge", "ops/restore-stale", &content).await;
    let id = n["id"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/nodes/{id}");
    let seen = agent_text(&a, &id).await;
    let v2 = content.replace("текст", "текст человека");
    let r = a
        .adm_h(
            Method::PUT,
            &uri,
            Some(json!({"content": &v2})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &uri,
            Some(json!({"content": seen.replace("password = ", "password ")})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.json()["current_rev"], 2, "{}", r.text());
    assert!(!r.text().contains(&p), "{}", r.text());
    assert_eq!(human_text(&a, &id).await, v2);
    // запись журнала: правка и замещение по виду ревизии 1 при текущей 2
    for kind in ["patch", "supersede"] {
        let e = post_entry(
            &a,
            "knowledge",
            json!({"kind": "insight", "title": format!("Пароль стенда {kind}"),
                   "body": format!("стенд\npassword = {p}\n"), "data": {"kind": "gotcha"}}),
        )
        .await;
        let euri = format!("/api/v1/entries/{}", e["id"].as_str().unwrap());
        let body = a.t(Method::GET, &euri, None).await.json()["body"]
            .as_str()
            .unwrap()
            .to_string();
        let r = a
            .adm_h(
                Method::PATCH,
                &euri,
                Some(json!({"title": format!("Пароль стенда {kind} (человек)")})),
                &[("if-match", "1")],
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let exposing = body.replace("password = ", "password ");
        let r = if kind == "patch" {
            a.tok_h(
                &a.token,
                Method::PATCH,
                &euri,
                Some(json!({"body": exposing})),
                &[("if-match", "1")],
            )
            .await
        } else {
            a.tok_h(
                &a.token,
                Method::POST,
                &format!("{euri}/supersede"),
                Some(json!({"title": "Пароль стенда", "body": exposing,
                            "data": {"kind": "gotcha"}})),
                &[("if-match", "1")],
            )
            .await
        };
        assert_eq!(r.status, StatusCode::CONFLICT, "{kind}: {}", r.text());
        assert_eq!(r.json()["rev"], 2, "{kind}: {}", r.text());
        assert!(!r.text().contains(&p), "{}", r.text());
        let h = a.adm(Method::GET, &euri, None, None).await.json();
        assert_eq!(h["body"], format!("стенд\npassword = {p}\n"), "{kind}");
    }
}

/// Гонка `PATCH` агента по маске (B5 и B6 круга правок 2): между подсчётом и записью заметку
/// правят — подсчёт заново по новой ревизии, значение на месте, чужая правка сохранена; правят
/// быстрее всех попыток — 409 с `current_rev` и `diff` (как у устаревшей ревизии), без значения.
#[tokio::test]
async fn agent_patch_race_recounts() {
    use zam_server::api::nodes::write::TEST_PATCH_RACES;
    let a = api("secrets-restore-race", |_| {}).await;
    let s = secret();
    let content = format!("# K\n\n## A\n\ntoken: {s}\n\n## B\n\nхвост\n");
    let n = a.note("knowledge", "ops/restore-race", &content).await;
    let id = n["id"].as_str().unwrap().to_string();
    let nid: Id = id.parse().unwrap();
    let uri = format!("/api/v1/nodes/{id}");
    let races = |c: usize| {
        let mut g = TEST_PATCH_RACES.lock().unwrap();
        g.retain(|(x, _)| *x != nid);
        g.push((nid, c));
    };
    races(1);
    let r = a
        .t(
            Method::PATCH,
            &uri,
            Some(
                json!({"base_rev": 1, "ops": [{"op": "replace_section", "section": "K > A",
                "content": format!("\ntoken: {} (prod)\n\n", mk(&tg(&id, 1), 1))}]}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["current_rev"], 3, "{}", r.text());
    let t = human_text(&a, &id).await;
    assert!(
        t.contains(&format!("token: {s} (prod)\n")) && t.ends_with("хвост\nгонка 1\n"),
        "{t}"
    );
    races(3);
    let r = a
        .t(
            Method::PATCH,
            &uri,
            Some(
                json!({"base_rev": 3, "ops": [{"op": "replace_section", "section": "K > A",
                "content": format!("\ntoken: {} (dev)\n\n", mk(&tg(&id, 3), 1))}]}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let j = r.json();
    assert_eq!(j["current_rev"], 6, "{j}");
    assert!(
        j["diff"].as_str().is_some_and(|d| d.contains("гонка 5")),
        "{j}"
    );
    assert!(!r.text().contains(&s), "{}", r.text());
    let t = human_text(&a, &id).await;
    assert!(t.contains(&format!("token: {s} (prod)\n")), "{t}");
    TEST_PATCH_RACES.lock().unwrap().retain(|(x, _)| *x != nid);
}

/// Литерал `‹secret›` в базе (импорт архива с маской, прежний `secret_mark_kept`) и правка
/// элемента массива с секретом (круг правок 3): литерал в виде — метка без номера, правка строк с
/// ним проходит, литералы — как есть с предупреждением; элемент `data` правлен вокруг метки — значение
/// по номеру; метка без номера на месте элемента — литерал, значение удалено правкой.
#[tokio::test]
async fn agent_literal_and_item_edits() {
    let a = api("secrets-restore-literal", |_| {}).await;
    let s = secret();
    let lit: String = (0..5).map(|i| format!("сервис {i}: {MARK}\n")).collect();
    let n = a.note("knowledge", "ops/restore-literal", &lit).await;
    let id = n["id"].as_str().unwrap().to_string();
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{id}"),
            Some(json!({"content": lit.replace("сервис 3: ", "сервис 3 (выведен): ")})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // литералы напротив себя — не предупреждение (сверх литералов вида базы, §4.3.2 п. 2)
    assert_eq!(kept_of(&r.json()), None, "{}", r.text());
    assert_eq!(
        human_text(&a, &id).await,
        lit.replace("сервис 3: ", "сервис 3 (выведен): ")
    );
    let e = post_entry(
        &a,
        "knowledge",
        json!({"kind": "insight", "title": "Варианты ключа", "body": "",
               "data": {"kind": "decision", "decision": {
                   "options": [format!("CI: {s}"), "без ключа"], "chosen": "CI",
                   "rationale": "проверено"}}}),
    )
    .await;
    let euri = format!("/api/v1/entries/{}", e["id"].as_str().unwrap());
    let v = a.t(Method::GET, &euri, None).await.json();
    let opt = v["data"]["decision"]["options"][0]
        .as_str()
        .unwrap()
        .to_string();
    assert!(opt.has_mark(), "{v}");
    let data = |o: String| {
        json!({"kind": "decision", "decision": {"options": [o, "без ключа"], "chosen": "CI",
               "rationale": "проверено"}})
    };
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &euri,
            Some(json!({"data": data(opt.replace("CI:", "CI (GitHub Actions):"))})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let h = a.adm(Method::GET, &euri, None, None).await.json();
    assert_eq!(
        h["data"]["decision"]["options"][0],
        format!("CI (GitHub Actions): {s}"),
        "{h}"
    );
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &euri,
            Some(json!({"data": data(format!("OPENAI_KEY: {MARK}"))})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), Some(1), "{}", r.text());
    let h = a.adm(Method::GET, &euri, None, None).await.json();
    assert_eq!(
        h["data"]["decision"]["options"][0],
        format!("OPENAI_KEY: {MARK}")
    );
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
    let body = json!({"title": "Ключ стенда", "body": format!("token: {}\nновый {MARK}", mk(&tg(i["id"].as_str().unwrap(), 1), 1)),
                      "data": {"kind": "gotcha"}});
    let h = [("idempotency-key", "sup-1"), ("if-match", "1")];
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

/// Покрытие всего текста (круг правок 4, C1, §4.3.2 п. 5): незакрытый забор над блоком с
/// `mysql -p…` — и `PATCH` (`prepend`, `insert_after_heading`), и `PUT` — 422, значение в базе
/// прежнее; значение, уже открытое в прозе человека, при правке рядом — не отказ.
#[tokio::test]
async fn agent_unclosed_fence_rejected() {
    let a = api("secrets-restore-fence", |_| {}).await;
    let p = ["hunter", "9Qz9x"].concat();
    let content =
        format!("# Стенд\n\n## Доступ\n\n```\nmysql -uroot -p{p} db\n```\n\n## Прочее\n\nтекст\n");
    let n = a.note("knowledge", "ops/restore-fence", &content).await;
    let id = n["id"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/nodes/{id}");
    for op in [
        json!({"op": "prepend", "content": "```\n"}),
        json!({"op": "insert_after_heading", "section": "Стенд > Доступ", "content": "```\nпример\n"}),
    ] {
        let r = a
            .t(
                Method::PATCH,
                &uri,
                Some(json!({"base_rev": 1, "ops": [op]})),
            )
            .await;
        context_changed(&r, &p, "строка");
    }
    let seen = agent_text(&a, &id).await;
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &uri,
            Some(json!({"content": seen.replace("## Доступ\n\n", "## Доступ\n\n```\nпример\n")})),
            &[("if-match", "1")],
        )
        .await;
    context_changed(&r, &p, "строка 8");
    assert_eq!(human_text(&a, &id).await, content);
    // значение открыто в прозе человека — правка рядом проходит
    let open = format!("password = {p}\nв прозе: {p}\nтекст\n");
    let n = a.note("knowledge", "ops/restore-open", &open).await;
    let id = n["id"].as_str().unwrap().to_string();
    let seen = agent_text(&a, &id).await;
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{id}"),
            Some(json!({"content": seen.replace("текст", "текст 2")})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(human_text(&a, &id).await, open.replace("текст", "текст 2"));
}

/// Номера — только у текущей ревизии (круг правок 4, I2, §4.3.2 п. 1): вид `rev=1`, кусок
/// `range` старой ревизии и ревизии записи — метки без номеров; запись вида rev1 поверх текущей —
/// литерал с `secret_mark_kept`, без подмены значением другой маски.
#[tokio::test]
async fn agent_old_revision_unnumbered() {
    let a = api("secrets-restore-oldrev", |_| {}).await;
    let (s, p) = (secret(), ["hunter", "5Qz9x"].concat());
    let v1 = format!("## Prod\npassword = {p}\n");
    let n = a.note("knowledge", "ops/restore-oldrev", &v1).await;
    let id = n["id"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/nodes/{id}");
    let v2 = format!("## CI\ntoken: {s}\n{v1}");
    let r = a
        .adm_h(
            Method::PUT,
            &uri,
            Some(json!({"content": &v2})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let old = a.t(Method::GET, &format!("{uri}?rev=1"), None).await.json();
    let old = old["content"].as_str().unwrap().to_string();
    assert_eq!(old, format!("## Prod\npassword = {MARK}\n"));
    let cur = a.t(Method::GET, &uri, None).await.json();
    assert!(
        cur["content"]
            .as_str()
            .unwrap()
            .contains(&mk(&tg(&id, 2), 2)),
        "{cur}"
    );
    let r = a
        .t(
            Method::POST,
            "/api/v1/read",
            Some(json!({"items": [{"id": format!("n:{id}"), "range": format!("r1:0-{}", numbered(&tg(&id, 1), &v1).len())}]})),
        )
        .await;
    assert_eq!(r.json()["items"][0]["text"], old.as_str(), "{}", r.text());
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &uri,
            Some(json!({"content": &old})),
            &[("if-match", "2")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), Some(1), "{}", r.text());
    assert_eq!(human_text(&a, &id).await, old);
    // ревизии записи — без номеров
    let e = post_entry(
        &a,
        "knowledge",
        json!({"kind": "insight", "title": "Ключ", "body": format!("token: {s}"), "data": {"kind": "gotcha"}}),
    )
    .await;
    let r = a
        .t(
            Method::GET,
            &format!("/api/v1/entries/{}/revisions", e["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert!(
        r.text().contains(MARK) && !r.text().contains("‹secret-") && !r.text().contains(&s),
        "{}",
        r.text()
    );
}

/// Секции (круг правок 4, M5, M2, §4.3.2 п. 3): `/read section` второй секции, где секрет не
/// первый в заметке, — `‹secret-2›`; адрес с меткой без номера (из карточки поиска) находит
/// секцию у `/read section` и `PATCH`, если она одна, и даёт 422, если таких несколько.
#[tokio::test]
async fn agent_section_numbers_and_plain_address() {
    let a = api("secrets-restore-sections", |_| {}).await;
    let (s, s2) = (secret(), secret2());
    let content = format!("# K\n\n## A\n\ntoken: {s}\n\n## B\n\ntoken: {s2}\n");
    let n = a.note("knowledge", "ops/restore-sections", &content).await;
    let nid = n["id"].as_str().unwrap().to_string();
    let read = |item: Value| {
        let a = &a;
        async move {
            let r = a
                .t(Method::POST, "/api/v1/read", Some(json!({"items": [item]})))
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()["items"][0].clone()
        }
    };
    let it = read(json!({"id": format!("n:{nid}"), "section": "B"})).await;
    assert_eq!(
        it["text"],
        format!("## B\n\ntoken: {}\n", mk(&tg(&nid, 1), 2)),
        "{it}"
    );
    // заголовки с ключом: адрес с `‹secret›` без номера
    let heads = format!("# K\n\n## Стенд {s}\n\nстарое\n\n## Склад {s2}\n\nx\n");
    let n = a.note("knowledge", "ops/restore-heads", &heads).await;
    let hid = n["id"].as_str().unwrap().to_string();
    let it = read(json!({"id": format!("n:{hid}"), "section": format!("Стенд {MARK}")})).await;
    assert_eq!(
        it["text"],
        format!("## Стенд {}\n\nстарое\n\n", mk(&tg(&hid, 1), 1)),
        "{it}"
    );
    let r = a
        .t(
            Method::PATCH,
            &format!("/api/v1/nodes/{hid}"),
            Some(json!({"base_rev": 1, "ops": [{"op": "replace_section",
                "section": format!("K > Стенд {MARK}"), "content": "\nновое\n\n"}]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(human_text(&a, &hid).await, heads.replace("старое", "новое"));
    let twin = format!("# K\n\n## Стенд {s}\n\nx\n\n## Стенд {s2}\n\ny\n");
    let n = a.note("knowledge", "ops/restore-twin", &twin).await;
    let tid = n["id"].as_str().unwrap().to_string();
    let r = a
        .t(
            Method::PATCH,
            &format!("/api/v1/nodes/{tid}"),
            Some(json!({"base_rev": 1, "ops": [{"op": "append_to_section",
                "section": format!("K > Стенд {MARK}"), "content": "z\n"}]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert!(
        !r.text().contains(&s) && !r.text().contains(&s2),
        "{}",
        r.text()
    );
    let r = a
        .t(
            Method::POST,
            "/api/v1/read",
            Some(
                json!({"items": [{"id": format!("n:{tid}"), "section": format!("Стенд {MARK}")}]}),
            ),
        )
        .await;
    assert!(r.text().contains("неоднозначен"), "{}", r.text());
}

/// Предупреждение `invalid_ref` агенту (круг правок 4): путь маскируется до усечения до 200
/// знаков — кусок ключа на границе усечения не выходит в ответ.
#[tokio::test]
async fn agent_invalid_ref_masked_before_cut() {
    let a = api("secrets-invalid-ref", |_| {}).await;
    let s = secret();
    // граница усечения (200 знаков) — внутри ключа: без маски до усечения вышли бы его 11 знаков
    let path = format!("/{}/{s}", "x".repeat(187));
    let r = a
        .t(
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "knowledge", "path": "ops/bad-ref", "kind": "note",
                        "content": format!("---\nrefs: [\"{path}\"]\n---\n# R\n\nтекст\n")}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let w = r.json()["warnings"].to_string();
    assert!(w.contains("invalid_ref"), "{w}");
    assert!(!w.contains(&s[..8]), "кусок ключа в предупреждении: {w}");
}

/// Метки с личностью (круг правок 5, I-D, §4.3.2 пп. 1–2): копия строки из заметки A в B — литерал
/// и `secret_mark_kept`, без чужого значения; метка вида круга 3 без T — литерал; перенос метки из
/// `body` в `title` записи — значение; дописка `‹secret›` к заметке из литералов — предупреждение
/// (M-c).
#[tokio::test]
async fn agent_mark_identity() {
    let a = api("secrets-restore-identity", |_| {}).await;
    let (s, p) = (secret(), ["hunter", "6Qz9x"].concat());
    let na = a
        .note("knowledge", "ops/id-a", &format!("token: {s}\n"))
        .await;
    let nb = a
        .note("knowledge", "ops/id-b", &format!("# B\n\npassword = {p}\n"))
        .await;
    let (ida, idb) = (
        na["id"].as_str().unwrap().to_string(),
        nb["id"].as_str().unwrap().to_string(),
    );
    let (va, vb) = (agent_text(&a, &ida).await, agent_text(&a, &idb).await);
    assert!(va.contains(&mk(&tg(&ida, 1), 1)), "{va}");
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{idb}"),
            Some(json!({"content": format!("{vb}{va}‹secret-1›\n")})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), Some(2), "{}", r.text());
    let t = human_text(&a, &idb).await;
    assert!(!t.contains(&s), "чужое значение: {t}");
    assert_eq!(t, format!("# B\n\npassword = {p}\n{}‹secret-1›\n", va));
    // запись: строка пароля из тела — в заголовок
    let e = post_entry(
        &a,
        "knowledge",
        json!({"kind": "insight", "title": format!("Ключ CI {s}"),
               "body": format!("Стенд\npassword = {p}\n"), "data": {"kind": "gotcha"}}),
    )
    .await;
    let eid = e["id"].as_str().unwrap().to_string();
    let euri = format!("/api/v1/entries/{eid}");
    let v = a.t(Method::GET, &euri, None).await.json();
    let t2 = tg(&eid, 1);
    assert_eq!(v["title"], format!("Ключ CI {}", mk(&t2, 1)), "{v}");
    assert_eq!(
        v["body"],
        format!("Стенд\npassword = {}\n", mk(&t2, 2)),
        "{v}"
    );
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &euri,
            Some(json!({"title": format!("Ключ CI {}; password = {}", mk(&t2, 1), mk(&t2, 2))})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), None, "{}", r.text());
    let h = a.adm(Method::GET, &euri, None, None).await.json();
    assert_eq!(h["title"], format!("Ключ CI {s}; password = {p}"), "{h}");
    // заметка из литералов: дописанный `‹secret›` — предупреждение
    let lit: String = (0..3).map(|i| format!("сервис {i}: {MARK}\n")).collect();
    let n = a.note("knowledge", "ops/id-lit", &lit).await;
    let lid = n["id"].as_str().unwrap().to_string();
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{lid}"),
            Some(json!({"content": format!("{lit}новый {MARK}\n")})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), Some(1), "{}", r.text());
}

/// Слабые значения (круг правок 5, I-A, I-B, I-C): пароль URL, совпадающий с логином и схемой, и
/// пароль из одного знака, переписанные прозой, — 422; обычные слова («Перезапустить postgres»,
/// «Сменить password»), дубль строки URL и дописанные догадки без меток — 200.
#[tokio::test]
async fn agent_weak_values() {
    let a = api("secrets-restore-weak", |_| {}).await;
    let mut k = 0;
    let mut note = |content: String| {
        k += 1;
        let path = format!("ops/weak-{k}");
        let a = &a;
        async move {
            let n = a.note("knowledge", &path, &content).await;
            let id = n["id"].as_str().unwrap().to_string();
            let seen = agent_text(a, &id).await;
            (id, seen)
        }
    };
    // URL с паролем — из частей: в исходнике нет пары `логин:пароль@`
    let url = |head: &str, pass: &str, tail: &str| format!("{head}:{pass}@{tail}");
    for (content, prose) in [
        (
            format!(
                "БД: {}\n",
                url("postgres://postgres", "postgres", "localhost:5432/app")
            ),
            "БД: пароль {m}\n",
        ),
        (
            format!(
                "Очередь: {}\n",
                url("amqp://guest", "guest", "localhost:5672/")
            ),
            "Очередь: пароль {m}\n",
        ),
        (
            format!("Строка {}\n", url("postgres://u", "p", "h/db")),
            "Строка: пароль {m}\n",
        ),
    ] {
        let (id, seen) = note(content.clone()).await;
        let m1 = mk(&tg(&id, 1), 1);
        assert!(seen.contains(&m1), "{seen}");
        let r = a
            .tok_h(
                &a.token,
                Method::PUT,
                &format!("/api/v1/nodes/{id}"),
                Some(json!({"content": prose.replace("{m}", &m1)})),
                &[("if-match", "1")],
            )
            .await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{content}: {}",
            r.text()
        );
        for extra in [
            "Перезапустить postgres после обновления\n",
            "Сменить password раз в квартал\npassword policy: 90 дней\ndeploy\n",
            "",
            "hunter20Qz9x\nhunter21Qz9x\npostgres\nguest\np\n",
        ] {
            let (id, seen) = note(content.clone()).await;
            // пустая дописка — дубль строки URL с меткой
            let extra = if extra.is_empty() {
                seen.clone()
            } else {
                extra.to_string()
            };
            let r = a
                .tok_h(
                    &a.token,
                    Method::PUT,
                    &format!("/api/v1/nodes/{id}"),
                    Some(json!({"content": format!("{seen}{extra}")})),
                    &[("if-match", "1")],
                )
                .await;
            assert_eq!(
                r.status,
                StatusCode::OK,
                "{content} + {extra:?}: {}",
                r.text()
            );
            assert!(!r.text().contains("secret_context_changed"), "{}", r.text());
        }
    }
}

/// Вторая правка подряд из того же контекста (финальное ревью M7.1, I-2, §4.3.2 п. 2): агент
/// прочитал ревизию r, записал r+1 и снова пишет из того же текста с `rev` r+1 из ответа — метки
/// ревизии r действуют, значения на месте, `secret_mark_kept` нет. Заметка (`PUT`, `PATCH`) и запись
/// журнала (правка, дописка, замещение).
#[tokio::test]
async fn agent_second_write_from_same_view() {
    let a = api("secrets-restore-second", |_| {}).await;
    let (s, s2) = (secret(), secret2());
    let content = format!("# Стенд\n\n## Доступ\n\ntoken: {s}\n\n## Прочее\n\nтекст\n");
    let n = a.note("knowledge", "ops/restore-second", &content).await;
    let id = n["id"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/nodes/{id}");
    let seen = agent_text(&a, &id).await;
    assert!(seen.contains(&mk(&tg(&id, 1), 1)), "{seen}");
    let put = |text: String, rev: &'static str| {
        let (a, uri) = (&a, &uri);
        async move {
            let r = a
                .tok_h(
                    &a.token,
                    Method::PUT,
                    uri,
                    Some(json!({"content": text})),
                    &[("if-match", rev)],
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            assert_eq!(kept_of(&r.json()), None, "{}", r.text());
            r
        }
    };
    let r = put(seen.replace("текст\n", "текст 1\n"), "1").await;
    assert_eq!(r.json()["current_rev"], 2, "{}", r.text());
    // метки базовой ревизии — без `secret_mark_from_rev` (N-3)
    assert_eq!(from_rev_of(&r.json()), None, "{}", r.text());
    // тот же вид ревизии 1, `rev` 2 из ответа: значения — из ревизии 1, предупреждение
    let r = put(seen.replace("текст\n", "текст 2\n"), "2").await;
    assert_eq!(from_rev_of(&r.json()), Some(vec![1]), "{}", r.text());
    assert_eq!(
        human_text(&a, &id).await,
        content.replace("текст\n", "текст 2\n")
    );
    // PATCH по base_rev 3 с меткой вида ревизии 1
    let r = a
        .t(
            Method::PATCH,
            &uri,
            Some(json!({"base_rev": 3, "ops": [{"op": "replace_section",
                "section": "Стенд > Доступ",
                "content": format!("\ntoken: {} (prod)\n\n", mk(&tg(&id, 1), 1))}]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), None, "{}", r.text());
    assert_eq!(from_rev_of(&r.json()), Some(vec![1]), "{}", r.text());
    assert!(
        human_text(&a, &id)
            .await
            .contains(&format!("token: {s} (prod)\n")),
        "PATCH: значение ревизии 1"
    );
    // запись журнала: правка, вторая правка тем же видом, дописка и замещение
    let e = post_entry(
        &a,
        "knowledge",
        json!({"kind": "insight", "title": format!("Ключ {s2}"),
               "body": format!("token: {s}\nверсия 1\n"), "data": {"kind": "gotcha"}}),
    )
    .await;
    let eid = e["id"].as_str().unwrap().to_string();
    let euri = format!("/api/v1/entries/{eid}");
    let v = a.t(Method::GET, &euri, None).await.json();
    let (title, body) = (
        v["title"].as_str().unwrap().to_string(),
        v["body"].as_str().unwrap().to_string(),
    );
    assert!(body.contains(&mk(&tg(&eid, 1), 2)), "{v}");
    for (rev, tail) in [("1", "версия 2"), ("2", "версия 3")] {
        let r = a
            .tok_h(
                &a.token,
                Method::PATCH,
                &euri,
                Some(json!({"title": format!("{title} ({tail})"),
                            "body": body.replace("версия 1", tail)})),
                &[("if-match", rev)],
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{rev}: {}", r.text());
        assert_eq!(kept_of(&r.json()), None, "{rev}: {}", r.text());
        let want = (rev != "1").then(|| vec![1]);
        assert_eq!(from_rev_of(&r.json()), want, "{rev}: {}", r.text());
    }
    let r = a
        .tok_h(
            &a.token,
            Method::PATCH,
            &euri,
            Some(json!({"append_body": format!("ещё: {}", body.lines().next().unwrap())})),
            &[("if-match", "3")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), None, "{}", r.text());
    let h = a.adm(Method::GET, &euri, None, None).await.json();
    assert_eq!(h["title"], format!("Ключ {s2} (версия 3)"), "{h}");
    assert_eq!(
        h["body"],
        format!("token: {s}\nверсия 3\n\nещё: token: {s}"),
        "{h}"
    );
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &format!("{euri}/supersede"),
            Some(
                json!({"title": title, "body": body.replace("версия 1", "версия 4"),
                        "data": {"kind": "gotcha"}}),
            ),
            &[("if-match", "4")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(kept_of(&r.json()["entry"]), None, "{}", r.text());
    assert_eq!(
        from_rev_of(&r.json()["entry"]),
        Some(vec![1]),
        "{}",
        r.text()
    );
    let nid = r.json()["entry"]["id"].as_str().unwrap().to_string();
    let h = a
        .adm(Method::GET, &format!("/api/v1/entries/{nid}"), None, None)
        .await
        .json();
    assert_eq!(h["title"], format!("Ключ {s2}"), "{h}");
    assert_eq!(h["body"], format!("token: {s}\nверсия 4\n"), "{h}");
}

/// Заметка `id`: `n` правок человека подряд (ревизии 2..=n+1) — текст `body(k)` ревизии `k`.
async fn human_revisions(a: &Api, id: &str, n: i64, body: impl Fn(i64) -> String) {
    for k in 2..=n + 1 {
        let prev = (k - 1).to_string();
        let r = a
            .adm_h(
                Method::PUT,
                &format!("/api/v1/nodes/{id}"),
                Some(json!({"content": body(k)})),
                &[("if-match", prev.as_str())],
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{k}: {}", r.text());
    }
}

/// Предел ревизий по меткам (финальное ревью M7.1, N-2): метки 21 ревизии заметки в одной записи —
/// значения у текущей и 8 ближайших к ней прежних, прочие — литералы с `secret_mark_kept`; ответ
/// 200 за ограниченное время; `secret_mark_from_rev` — ревизии значений.
#[tokio::test]
async fn agent_marks_of_many_revisions_limited() {
    let a = api("secrets-restore-many-revs", |_| {}).await;
    let s = secret();
    let body = |k: i64| format!("# K\n\ntoken: {s}\nверсия {k:02}\n");
    let n = a.note("knowledge", "ops/restore-many", &body(1)).await;
    let id = n["id"].as_str().unwrap().to_string();
    human_revisions(&a, &id, 20, body).await;
    let text: String = (1..=21)
        .map(|k| format!("k{k:02}: token: {}\n", mk(&tg(&id, k), 1)))
        .collect();
    let t0 = std::time::Instant::now();
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{id}"),
            Some(json!({"content": text})),
            &[("if-match", "21")],
        )
        .await;
    let took = t0.elapsed();
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(took < std::time::Duration::from_secs(10), "{took:?}");
    assert_eq!(kept_of(&r.json()), Some(12), "{}", r.text());
    assert_eq!(
        from_rev_of(&r.json()),
        Some((13..=20).collect()),
        "{}",
        r.text()
    );
    let h = human_text(&a, &id).await;
    let restored: Vec<&str> = h.lines().filter(|l| l.contains(&s)).collect();
    assert_eq!(restored.len(), 9, "{h}");
    assert!(
        restored[0].starts_with("k13: ") && restored[8].starts_with("k21: "),
        "{h}"
    );
}

/// Предел байт прежних ревизий (N-2): `limits.note_max_bytes` × 4 — у ревизий по 2 970 байт при
/// пределе 4 096 загружаются 5 ближайших к текущей, старше — литералы.
#[tokio::test]
async fn agent_marks_revisions_byte_limit() {
    let a = api("secrets-restore-rev-bytes", |c| {
        c.limits.note_max_bytes = 4096
    })
    .await;
    let s = secret();
    let pad = "x".repeat(2900);
    let body = |k: i64| format!("# K\n\ntoken: {s}\nверсия {k:02}\n{pad}\n");
    assert_eq!(body(1).len(), 2970);
    let n = a.note("knowledge", "ops/restore-bytes", &body(1)).await;
    let id = n["id"].as_str().unwrap().to_string();
    human_revisions(&a, &id, 7, body).await;
    let text: String = (1..=8)
        .map(|k| format!("k{k}: token: {}\n", mk(&tg(&id, k), 1)))
        .collect();
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{id}"),
            Some(json!({"content": text})),
            &[("if-match", "8")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(kept_of(&r.json()), Some(2), "{}", r.text());
    assert_eq!(
        from_rev_of(&r.json()),
        Some(vec![3, 4, 5, 6, 7]),
        "{}",
        r.text()
    );
    let h = human_text(&a, &id).await;
    assert_eq!(h.lines().filter(|l| l.contains(&s)).count(), 6, "{h}");
}

/// Метки, прочитанные до обновления сканера (I-2, §4.3.2 п. 1): T другой версии правил или крейта
/// у того же объекта и ревизии — литерал с `secret_mark_kept`, значение не подставлено.
#[tokio::test]
async fn agent_marks_of_older_scanner_literal() {
    let a = api("secrets-restore-rules", |_| {}).await;
    let s = secret();
    let content = format!("# K\n\ntoken: {s}\n");
    for (i, (rules, version)) in [
        (zam_secrets::RULES_VERSION - 1, zam_secrets::VERSION),
        (zam_secrets::RULES_VERSION, "0.0.13-m6.6"),
    ]
    .into_iter()
    .enumerate()
    {
        let n = a
            .note("knowledge", &format!("ops/restore-rules-{i}"), &content)
            .await;
        let id = n["id"].as_str().unwrap().to_string();
        let old = zam_server::api::agent_view::object_tag_with(&id, 1, rules, version);
        assert_ne!(old, tg(&id, 1));
        let r = a
            .tok_h(
                &a.token,
                Method::PUT,
                &format!("/api/v1/nodes/{id}"),
                Some(json!({"content": format!("# K\n\ntoken: {} (prod)\n", mk(&old, 1))})),
                &[("if-match", "1")],
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(kept_of(&r.json()), Some(1), "{}", r.text());
        let t = human_text(&a, &id).await;
        assert!(!t.contains(&s), "значение по прежней метке: {t}");
        assert_eq!(t, format!("# K\n\ntoken: {} (prod)\n", mk(&old, 1)));
    }
}

/// Доступ раньше восстановления (финальное ревью M7.1, I-1, §4.3.2 п. 2): участник общего решения
/// замещает через `supersedes_id` запись из личного Knowledge владельца — 404, как у чтения: без 428
/// (метки без ревизии), 409 (устаревшая ревизия) и 422 (значение вне маски).
#[tokio::test]
async fn participant_supersedes_foreign_entry_not_found() {
    const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e66";
    let a = api("secrets-restore-access", |_| {}).await;
    let (_, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let reg = json!({ "agent_solution_id": G, "name": "Общее" });
    let r = a
        .tok(&a_tok, Method::POST, "/api/v1/solutions", Some(reg.clone()))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = a
        .tok(&b_tok, Method::POST, "/api/v1/solutions", Some(reg))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    a.set_access(&a_tok, G, &b_id, "rw").await;
    let p = ["hunter", "7Qz9x"].concat();
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(json!({"kind": "insight", "title": "Пароль стенда",
                        "body": format!("password = {p}\n"), "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let old = r.json()["id"].as_str().unwrap().to_string();
    let m1 = mk(&tg(&old, 1), 1);
    let uri = format!("/api/v1/spaces/{G}/entries");
    for (what, rev, body) in [
        ("без ревизии", None, format!("password = {m1}\n")),
        ("устаревшая ревизия", Some(5), format!("password = {m1}\n")),
        ("значение вне маски", Some(1), format!("пароль {m1}\n")),
        ("без меток", None, "текст\n".to_string()),
    ] {
        let mut req = json!({"kind": "insight", "supersedes_id": &old, "title": "Пароль",
                             "body": body, "data": {"kind": "gotcha"}});
        if let Some(rev) = rev {
            req["supersedes_rev"] = json!(rev);
        }
        let r = a.tok(&b_tok, Method::POST, &uri, Some(req)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{what}: {}", r.text());
        assert!(!r.text().contains(&p), "{what}: {}", r.text());
    }
    // запись владельца не тронута
    let r = a
        .tok(&a_tok, Method::GET, &format!("/api/v1/entries/{old}"), None)
        .await;
    assert_eq!(r.json()["rev"], 1, "{}", r.text());
}
