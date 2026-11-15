//! REST API заметок: пространства, решения, дерево, узлы, ревизии, политика доступа.
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, api, enc};
use serde_json::{Value, json};

const NOTE: &str = "---\nl0: Кратко о скриптах\n---\n# Установка\r\nПервое предложение. Второе.\r\n\r\n## Linux\r\napt install x.\r\n\r\n# Использование\r\nЗапуск.\r\n";

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

async fn get(a: &Api, uri: &str) -> common::api::Resp {
    a.t(Method::GET, uri, None).await
}

#[tokio::test]
async fn token_creates_note_and_reads_all_formats() {
    let a = api("formats", |_| {}).await;
    let n = a.note("knowledge", "Инструменты/Bash скрипты", NOTE).await;
    assert_eq!(n["path"], "Инструменты/Bash скрипты");
    assert_eq!(n["name"], "Bash скрипты");
    assert_eq!(n["kind"], "note");
    assert_eq!(n["current_rev"], 1);
    assert!(n["index_state"].is_string(), "{n}");
    assert!(n["created_at"].as_str().unwrap().ends_with('Z'));
    let nid = id(&n);
    let base = format!("/api/v1/nodes/{nid}");

    // md — байты как сохранены (CRLF), ETag с ревизией
    let r = get(&a, &format!("{base}?format=md")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("content-type"), "text/markdown; charset=utf-8");
    assert_eq!(r.header("etag"), "\"1\"");
    assert_eq!(&r.body[..], NOTE.as_bytes());

    // json по умолчанию
    let j = get(&a, &base).await.json();
    assert_eq!(j["rev"], 1);
    assert_eq!(j["content"], NOTE);
    assert_eq!(j["node"]["id"], nid.as_str());

    // html
    let h = get(&a, &format!("{base}?format=html")).await;
    assert_eq!(h.status, StatusCode::OK, "{}", h.text());
    let h = h.json();
    assert!(h["html"].as_str().unwrap().contains("<h1"), "{h}");
    assert_eq!(h["toc"][0]["title"], "Установка");
    assert_eq!(h["rev"], 1);
    assert!(h["warnings"].is_array());

    // L0 и L1
    let l0 = get(&a, &format!("{base}?tier=L0")).await.json();
    assert_eq!(l0["l0"], "Кратко о скриптах");
    assert_eq!(l0["rev"], 1);
    let l1 = get(&a, &format!("{base}?tier=L1")).await.json();
    let l1s = l1["l1"].as_str().unwrap();
    assert!(l1s.contains("Установка") && l1s.contains("Первое предложение."));
    assert!(!l1s.contains("Второе"), "{l1s}");
    assert!(l1s.contains("Linux") && l1s.contains("Использование"));
    assert!(l1s.chars().count() <= 1200);
    let r = get(&a, &format!("{base}?tier=L9")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // секция
    let sec = enc("Установка > Linux");
    let r = get(&a, &format!("{base}?format=md&section={sec}")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.text(), "## Linux\r\napt install x.\r\n\r\n");
    let j = get(&a, &format!("{base}?section={sec}")).await.json();
    assert_eq!(j["content"], "## Linux\r\napt install x.\r\n\r\n");
    let r = get(&a, &format!("{base}?section={}", enc("Нет такой"))).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // outline
    let o = get(&a, &format!("{base}/outline")).await.json();
    let titles: Vec<&str> = o
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Установка", "Linux", "Использование"]);

    // неизвестный формат и ревизия
    assert_eq!(
        get(&a, &format!("{base}?format=pdf")).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(&a, &format!("{base}?rev=9")).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&a, "/api/v1/nodes/zzz").await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn ambiguous_section_is_422() {
    let a = api("ambig", |_| {}).await;
    let n = a.note("knowledge", "dup", "# A\nx\n# A\ny\n").await;
    let r = get(&a, &format!("/api/v1/nodes/{}?section=A", id(&n))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
}

#[tokio::test]
async fn by_path_with_cyrillic_percent_encoding() {
    let a = api("bypath", |_| {}).await;
    let n = a.note("knowledge", "Инструменты/Bash скрипты", NOTE).await;
    let uri = format!(
        "/api/v1/nodes/by-path?space=knowledge&path={}",
        enc("Инструменты/Bash скрипты")
    );
    let r = get(&a, &uri).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["node"]["id"], id(&n).as_str());
    let r = get(&a, &format!("{uri}&format=md")).await;
    assert_eq!(&r.body[..], NOTE.as_bytes());
    let r = get(
        &a,
        &format!(
            "/api/v1/nodes/by-path?space=knowledge&path={}",
            enc("Нет/такой")
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = get(&a, "/api/v1/nodes/by-path?space=bogus&path=x").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tree_depth_and_path() {
    let a = api("tree", |_| {}).await;
    a.note("knowledge", "Инструменты/Bash скрипты", NOTE).await;
    a.note("knowledge", "Инструменты/Глубже/x", "x").await;
    a.note("knowledge", "корень", "y").await;
    let r = get(&a, "/api/v1/spaces/knowledge/tree").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["space"]["kind"], "knowledge");
    let paths = |j: &Value| -> Vec<String> {
        j["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["path"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(paths(&j), ["Инструменты", "корень"]);
    assert_eq!(j["items"][0]["has_children"], true);
    let j = get(&a, "/api/v1/spaces/knowledge/tree?depth=2")
        .await
        .json();
    assert_eq!(
        paths(&j),
        [
            "Инструменты",
            "Инструменты/Глубже",
            "Инструменты/Bash скрипты",
            "корень"
        ]
    );
    let j = get(
        &a,
        &format!("/api/v1/spaces/knowledge/tree?path={}", enc("Инструменты")),
    )
    .await
    .json();
    assert_eq!(
        paths(&j),
        ["Инструменты/Глубже", "Инструменты/Bash скрипты"]
    );
    assert_eq!(j["items"][1]["l0"], "Кратко о скриптах");
    for bad in ["depth=0", "depth=6", "depth=x"] {
        let r = get(&a, &format!("/api/v1/spaces/knowledge/tree?{bad}")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let r = get(&a, "/api/v1/spaces/knowledge/tree?path=nope").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = get(&a, "/api/v1/spaces/nope/tree").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn solutions_idempotent_and_addressable() {
    let a = api("solutions", |_| {}).await;
    let guid = "0B3C2A8E-6F1D-4C5E-9A7B-1D2E3F405162";
    let body = json!({"id": guid, "name": "Проект"});
    let r = a.t(Method::POST, "/api/v1/solutions", Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let s1 = r.json();
    assert_eq!(s1["kind"], "solution");
    assert_eq!(s1["agent_solution_id"], guid.to_lowercase());
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"id": guid.to_lowercase(), "name": "Другое"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let s2 = r.json();
    assert_eq!(s1["id"], s2["id"]);
    assert_eq!(s2["name"], "Проект");
    let r = get(&a, &format!("/api/v1/solutions/{guid}")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["id"], s1["id"]);
    let r = get(&a, "/api/v1/solutions/11111111-2222-3333-4444-555555555555").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    for bad in [
        json!({"id": "nope", "name": "x"}),
        json!({"id": guid, "name": ""}),
    ] {
        let r = a.t(Method::POST, "/api/v1/solutions", Some(bad)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
    }
    // адресация пространства по GUID и по Id
    let n = a.note(guid, "a", "x").await;
    assert_eq!(n["space_id"], s1["id"]);
    let r = get(
        &a,
        &format!("/api/v1/spaces/{}/tree", s1["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(r.json()["items"][0]["path"], "a");
    // ro-токен не регистрирует решения
    let ro = a.issue(&a.admin_id(), "ro").await;
    let r = a
        .tok(
            &ro,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"id": "a3c2a8e0-6f1d-4c5e-9a7b-1d2e3f405162", "name": "y"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn human_hidden_from_token() {
    let a = api("human", |_| {}).await;
    let r = get(&a, "/api/v1/spaces/human/tree").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = a
        .t(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space":"human","path":"x","kind":"note","content":"секрет"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // администратор пишет в Human; токен не видит узел ни по id, ни по пути
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space":"human","path":"дневник","kind":"note","content":"секрет"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let hid = id(&r.json());
    for uri in [
        format!("/api/v1/nodes/{hid}"),
        format!("/api/v1/nodes/{hid}?format=md"),
        format!("/api/v1/nodes/{hid}/revisions"),
        format!("/api/v1/nodes/{hid}/outline"),
        format!("/api/v1/nodes/by-path?space=human&path={}", enc("дневник")),
    ] {
        let r = get(&a, &uri).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
        assert!(!r.text().contains("секрет"));
    }
    let r = a
        .t(Method::DELETE, &format!("/api/v1/nodes/{hid}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = a
        .t(
            Method::POST,
            "/api/v1/render",
            Some(json!({"md": "x", "base_node": hid})),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // корзина по токену не показывает Human
    let r = a
        .adm(Method::DELETE, &format!("/api/v1/nodes/{hid}"), None, None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let t = get(&a, "/api/v1/trash").await.json();
    assert_eq!(t["items"].as_array().unwrap().len(), 0, "{t}");
    assert_eq!(
        get(&a, "/api/v1/trash?space=human").await.status,
        StatusCode::NOT_FOUND
    );
    let t = a.adm(Method::GET, "/api/v1/trash", None, None).await.json();
    assert_eq!(t["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn ro_token_reads_but_never_writes() {
    let a = api("ro", |_| {}).await;
    let n = a.note("knowledge", "x", "# A\nтекст\n").await;
    let ro = a.issue(&a.admin_id(), "ro").await;
    let r = a
        .tok(&ro, Method::GET, &format!("/api/v1/nodes/{}", id(&n)), None)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let nid = id(&n);
    let writes = [
        (
            Method::POST,
            "/api/v1/nodes".to_string(),
            json!({"space":"knowledge","path":"y","kind":"note","content":"z"}),
        ),
        (
            Method::PATCH,
            format!("/api/v1/nodes/{nid}"),
            json!({"base_rev":1,"ops":[{"op":"append","content":"q"}]}),
        ),
        (
            Method::POST,
            format!("/api/v1/nodes/{nid}/move"),
            json!({"path":"z"}),
        ),
        (
            Method::POST,
            format!("/api/v1/nodes/{nid}/revert"),
            json!({"rev":1}),
        ),
    ];
    for (m, uri, body) in writes {
        let r = a.tok(&ro, m.clone(), &uri, Some(body)).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{m} {uri}");
    }
    let r = a
        .tok_h(
            &ro,
            Method::PUT,
            &format!("/api/v1/nodes/{nid}"),
            Some(json!({"content":"new"})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = a
        .tok(&ro, Method::DELETE, &format!("/api/v1/nodes/{nid}"), None)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(
        get(&a, &format!("/api/v1/nodes/{nid}")).await.json()["rev"],
        1
    );
}

#[tokio::test]
async fn rules_write_gated_by_setting() {
    let a = api("rules", |_| {}).await;
    let body = json!({"space":"rules","path":"style","kind":"note","content":"# Стиль\n"});
    let r = a.t(Method::POST, "/api/v1/nodes", Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // администратор пишет всегда; токен читает всегда
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space":"rules","path":"base","kind":"note","content":"x"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let rid = id(&r.json());
    assert_eq!(
        get(&a, &format!("/api/v1/nodes/{rid}")).await.status,
        StatusCode::OK
    );
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{rid}"),
            Some(json!({"content":"y"})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // настройка читается на каждом запросе
    a.st.system
        .set_setting("rules.agent_write", &true)
        .await
        .unwrap();
    let r = a.t(Method::POST, "/api/v1/nodes", Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    a.st.system
        .set_setting("rules.agent_write", &false)
        .await
        .unwrap();
    let r = a
        .t(Method::DELETE, &format!("/api/v1/nodes/{rid}"), None)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_reads_user_note_with_x_zam_user() {
    let a = api("xuser", |_| {}).await;
    let (uid, tok) = a.new_user("ivan").await;
    let r = a
        .tok(
            &tok,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space":"knowledge","path":"ivan","kind":"note","content":"# Ivan\n"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let nid = id(&r.json());
    let r = a
        .adm(
            Method::GET,
            &format!("/api/v1/nodes/{nid}?format=md"),
            None,
            Some(&uid),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.text(), "# Ivan\n");
    // без заголовка — база администратора, где узла нет
    let r = a
        .adm(Method::GET, &format!("/api/v1/nodes/{nid}"), None, None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // чужой токен не видит
    assert_eq!(
        get(&a, &format!("/api/v1/nodes/{nid}")).await.status,
        StatusCode::NOT_FOUND
    );
    // запись администратора — автор human
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space":"knowledge","path":"from-admin","kind":"note","content":"a"})),
            Some(&uid),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let aid = id(&r.json());
    let revs = a
        .tok(
            &tok,
            Method::GET,
            &format!("/api/v1/nodes/{aid}/revisions"),
            None,
        )
        .await
        .json();
    assert_eq!(revs[0]["author_kind"], "human");
}

#[tokio::test]
async fn put_requires_if_match_and_reports_conflict_diff() {
    let a = api("put", |c| c.limits.note_max_bytes = 1000).await;
    let n = a.note("knowledge", "p", "строка 1\nстрока 2\n").await;
    let uri = format!("/api/v1/nodes/{}", id(&n));
    let r = a.t(Method::PUT, &uri, Some(json!({"content":"x"}))).await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED);
    assert_eq!(r.header("content-type"), "application/problem+json");
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &uri,
            Some(json!({"content":"строка 1\nстрока 2 изменена\n","message":"правка"})),
            &[
                ("if-match", "\"1\""),
                ("x-zam-machine", "box-A"),
                ("x-zam-session", "sess-1"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["current_rev"], 2);
    // старая ревизия → 409 с текущей и диффом base→current
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &uri,
            Some(json!({"content":"другое"})),
            &[("if-match", "W/\"1\"")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let p = r.json();
    assert_eq!(p["current_rev"], 2);
    let diff = p["diff"].as_str().unwrap();
    assert!(
        diff.contains("--- rev 1") && diff.contains("+++ rev 2"),
        "{diff}"
    );
    assert!(diff.contains("+строка 2 изменена"), "{diff}");
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &uri,
            Some(json!({"content":"x"})),
            &[("if-match", "abc")],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // предел размера
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &uri,
            Some(json!({"content":"я".repeat(501)})),
            &[("if-match", "2")],
        )
        .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    let r = a
        .t(
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space":"knowledge","path":"big","kind":"note","content":"x".repeat(1001)}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    // атрибуция
    let revs = get(&a, &format!("{uri}/revisions")).await.json();
    assert_eq!(revs[0]["rev"], 2);
    assert_eq!(revs[0]["machine"], "box-A");
    assert_eq!(revs[0]["session_id"], "sess-1");
    assert_eq!(revs[0]["message"], "правка");
    assert_eq!(revs[0]["author_kind"], "agent");
    assert!(revs[0]["token_id"].is_string());
}

#[tokio::test]
async fn patch_from_two_machines() {
    let a = api("patch2", |c| c.limits.note_max_bytes = 200).await;
    let n = a.note("knowledge", "shared", "# A\nа\n# B\nб\n").await;
    let uri = format!("/api/v1/nodes/{}", id(&n));
    let rep = |s: &str, c: &str| json!({"op":"replace_section","section":s,"content":c});
    let patch = |base: i64, op: Value, m: &'static str| {
        let uri = uri.clone();
        let a = &a;
        async move {
            a.tok_h(
                &a.token,
                Method::PATCH,
                &uri,
                Some(json!({"base_rev":base,"ops":[op]})),
                &[("x-zam-machine", m)],
            )
            .await
        }
    };
    // машина 1 правит A
    let r = patch(1, rep("A", "а1\n"), "m1").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["current_rev"], 2);
    // машина 2 с тем же base_rev правит чужую секцию B — проходит
    let r = patch(1, rep("B", "б2\n"), "m2").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["current_rev"], 3);
    // машина 2 с тем же base_rev правит уже изменённую A — 409
    let r = patch(1, rep("A", "а2\n"), "m2").await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let p = r.json();
    assert_eq!(p["section"], "A");
    assert_eq!(p["current_rev"], 3);
    let md = get(&a, &format!("{uri}?format=md")).await.text();
    assert_eq!(md, "# A\nа1\n# B\nб2\n");
    // секции нет — 422; base_rev новее — 409; пустые ops — 400
    let r = patch(3, rep("Нет", "x\n"), "m1").await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = patch(9, rep("A", "x\n"), "m1").await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json()["current_rev"], 3);
    let r = a
        .t(Method::PATCH, &uri, Some(json!({"base_rev":3,"ops":[]})))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // итог больше предела — 413, ничего не записано
    let r = patch(3, json!({"op":"append","content":"я".repeat(100)}), "m1").await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
    // машины в атрибуции; неверный X-Zam-Machine — 400
    let revs = get(&a, &format!("{uri}/revisions")).await.json();
    assert_eq!(revs[0]["rev"], 3);
    assert_eq!(revs[0]["machine"], "m2");
    assert_eq!(revs[1]["machine"], "m1");
    let long = "x".repeat(129);
    let r = a
        .tok_h(
            &a.token,
            Method::PATCH,
            &uri,
            Some(json!({"base_rev":3,"ops":[{"op":"append","content":"q"}]})),
            &[("x-zam-machine", long.as_str())],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = a
        .tok_h(
            &a.token,
            Method::PATCH,
            &uri,
            Some(json!({"base_rev":3,"ops":[{"op":"append","content":"q"}]})),
            &[("x-zam-session", "a\tb")],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn delete_trash_restore_move_revert_and_audit() {
    let a = api("lifecycle", |_| {}).await;
    let n = a.note("knowledge", "Папка/заметка", "v1\n").await;
    let nid = id(&n);
    let uri = format!("/api/v1/nodes/{nid}");
    let by_path = format!(
        "/api/v1/nodes/by-path?space=knowledge&path={}",
        enc("Папка/заметка")
    );
    let r = a.t(Method::DELETE, &uri, None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(get(&a, &by_path).await.status, StatusCode::NOT_FOUND);
    assert_eq!(get(&a, &uri).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        a.t(Method::DELETE, &uri, None).await.status,
        StatusCode::NOT_FOUND
    );
    let t = get(&a, "/api/v1/trash?space=knowledge").await.json();
    assert_eq!(t["items"][0]["id"], nid.as_str());
    assert!(t["items"][0]["deleted_at"].is_string());
    let r = a.t(Method::POST, &format!("{uri}/restore"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["deleted_at"].is_null());
    assert_eq!(get(&a, &by_path).await.status, StatusCode::OK);
    let r = a.t(Method::POST, &format!("{uri}/restore"), None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // move
    let r = a
        .t(
            Method::POST,
            &format!("{uri}/move"),
            Some(json!({"path":"Другая/имя"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["path"], "Другая/имя");
    let r = a
        .t(
            Method::POST,
            &format!("{uri}/move"),
            Some(json!({"path":"/bad/"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // ревизии, дифф, откат
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &uri,
            Some(json!({"content":"v2\n"})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = get(&a, &format!("{uri}/diff?from=1&to=2")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.header("content-type").starts_with("text/plain"));
    assert!(
        r.text().contains("-v1") && r.text().contains("+v2"),
        "{}",
        r.text()
    );
    let r = get(&a, &format!("{uri}/revisions/1?format=md")).await;
    assert_eq!(r.text(), "v1\n");
    assert_eq!(r.header("etag"), "\"1\"");
    let r = get(&a, &format!("{uri}/revisions/1")).await.json();
    assert_eq!(r["content"], "v1\n");
    assert_eq!(r["rev"], 1);
    assert_eq!(
        get(&a, &format!("{uri}/revisions/7")).await.status,
        StatusCode::NOT_FOUND
    );
    let r = a
        .t(
            Method::POST,
            &format!("{uri}/revert"),
            Some(json!({"rev":1,"message":"назад"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["current_rev"], 3);
    assert_eq!(get(&a, &format!("{uri}?format=md")).await.text(), "v1\n");
    let revs = get(&a, &format!("{uri}/revisions")).await.json();
    assert_eq!(revs[0]["message"], "назад");

    // аудит: удаление, восстановление, откат
    let audit = a
        .adm(Method::GET, "/api/v1/admin/audit?limit=50", None, None)
        .await
        .json();
    let items = audit["items"].as_array().unwrap();
    for action in ["node.delete", "node.restore", "node.revert"] {
        let row = items
            .iter()
            .find(|r| r["action"] == action)
            .unwrap_or_else(|| panic!("нет {action}: {audit}"));
        assert_eq!(row["target"], nid.as_str());
        assert_eq!(row["user_id"], a.admin_id().as_str());
        assert_ne!(row["actor"], "admin", "по токену актор — id токена");
    }
}

#[tokio::test]
async fn admin_cookie_write_without_csrf_is_403() {
    use axum::body::Body;
    use axum::http::header;
    let a = api("csrf", |_| {}).await;
    let n = a.note("knowledge", "x", "x").await;
    let body = json!({"space":"knowledge","path":"y","kind":"note","content":"z"});
    for (m, uri, b) in [
        (Method::POST, "/api/v1/nodes".to_string(), Some(body)),
        (Method::DELETE, format!("/api/v1/nodes/{}", id(&n)), None),
    ] {
        let r = common::api::req(m.clone(), &uri, b.as_ref())
            .header(header::COOKIE, &a.cookie)
            .body(b.map_or_else(Body::empty, |v| Body::from(v.to_string())))
            .unwrap();
        let r = a.send(r).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{m} {uri}");
    }
    // ничего не изменилось
    assert_eq!(
        get(&a, &format!("/api/v1/nodes/{}", id(&n))).await.status,
        StatusCode::OK
    );
    let t = get(&a, "/api/v1/spaces/knowledge/tree").await.json();
    assert_eq!(t["items"].as_array().unwrap().len(), 1);
}

// ---------- Матрица доступа HTTP ----------

/// Запрос матрицы: метод, URI, тело, дополнительные заголовки.
type Case = (
    Method,
    String,
    Option<Value>,
    &'static [(&'static str, &'static str)],
);

/// Загрузка вложения токеном: минимальный multipart `node` + `file`.
async fn upload_by(a: &Api, token: &str, node: &str) -> common::api::Resp {
    use axum::body::Body;
    use axum::http::{Request, header};
    let b = "zam-matrix-boundary";
    let body = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"node\"\r\n\r\n{node}\r\n\
         --{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\n\r\nx\r\n--{b}--\r\n"
    );
    let r = Request::post("/api/v1/blobs")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={b}"),
        )
        .body(Body::from(body))
        .unwrap();
    a.send(r).await
}

#[tokio::test]
async fn access_matrix_ro_rules_human() {
    let a = api("matrix", |_| {}).await;
    let ro = a.issue(&a.admin_id(), "ro").await;

    // ro: restore удалённого узла и загрузка вложения — 403
    let gone = a.note("knowledge", "удалённая", "# У\n").await;
    let gid = id(&gone);
    let r = a
        .t(Method::DELETE, &format!("/api/v1/nodes/{gid}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = a
        .tok(
            &ro,
            Method::POST,
            &format!("/api/v1/nodes/{gid}/restore"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let n = a.note("knowledge", "живая", "# Ж\n").await;
    assert_eq!(
        upload_by(&a, &ro, &id(&n)).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        upload_by(&a, &a.token, &id(&n)).await.status,
        StatusCode::CREATED
    );

    // Rules: rw-токен пишет только при rules.agent_write = true, ro — никогда
    let body = json!({"space":"rules","path":"правило","kind":"note","content":"# П\n"});
    let r = a.t(Method::POST, "/api/v1/nodes", Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    a.st.system
        .set_setting("rules.agent_write", &true)
        .await
        .unwrap();
    let r = a
        .tok(&ro, Method::POST, "/api/v1/nodes", Some(body.clone()))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = a.t(Method::POST, "/api/v1/nodes", Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    // Human: любые операции с узлом по токену — 404, текст не утекает
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space":"human","path":"тайна","kind":"note","content":"секрет\n"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let hid = id(&r.json());
    let r = a
        .adm(
            Method::PATCH,
            &format!("/api/v1/nodes/{hid}"),
            Some(json!({"base_rev":1,"ops":[{"op":"append","content":"ещё"}]})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let cases: Vec<Case> = vec![
        (
            Method::GET,
            format!("/api/v1/nodes/{hid}/diff?from=1&to=2"),
            None,
            &[],
        ),
        (
            Method::GET,
            format!("/api/v1/nodes/{hid}/revisions/1"),
            None,
            &[],
        ),
        (
            Method::POST,
            format!("/api/v1/nodes/{hid}/restore"),
            None,
            &[],
        ),
        (
            Method::POST,
            format!("/api/v1/nodes/{hid}/move"),
            Some(json!({"path":"наружу"})),
            &[],
        ),
        (
            Method::PUT,
            format!("/api/v1/nodes/{hid}"),
            Some(json!({"content":"подмена"})),
            &[("if-match", "2")],
        ),
        (
            Method::PATCH,
            format!("/api/v1/nodes/{hid}"),
            Some(json!({"base_rev":2,"ops":[{"op":"append","content":"x"}]})),
            &[],
        ),
        (
            Method::POST,
            format!("/api/v1/nodes/{hid}/revert"),
            Some(json!({"rev":1})),
            &[],
        ),
    ];
    for token in [&a.token, &ro] {
        for (m, uri, body, extra) in &cases {
            let r = a.tok_h(token, m.clone(), uri, body.clone(), extra).await;
            assert_eq!(r.status, StatusCode::NOT_FOUND, "{m} {uri}: {}", r.text());
            assert!(!r.text().contains("секрет"), "{m} {uri}");
        }
    }
    assert_eq!(
        upload_by(&a, &a.token, &hid).await.status,
        StatusCode::NOT_FOUND
    );
    // узел не изменился
    let r = a
        .adm(Method::GET, &format!("/api/v1/nodes/{hid}"), None, None)
        .await;
    assert_eq!(r.json()["rev"], 2);
}

/// Стенд Debian 13 (доработка): PATCH секции — только полный путь; если задан однозначный
/// хвост, 422 подсказывает полный путь, а текст не меняется.
#[tokio::test]
async fn patch_section_tail_hints_full_path() {
    let a = api("patch-tail", |_| {}).await;
    let n = a
        .note(
            "knowledge",
            "Сборка",
            "# Сборка\nmake\n## Тесты\ncargo test\n",
        )
        .await;
    let uri = format!("/api/v1/nodes/{}", id(&n));
    let patch = |section: &str| json!({"base_rev": 1, "ops": [{"op": "replace_section", "section": section, "content": "x\n"}]});
    let r = a.t(Method::PATCH, &uri, Some(patch("Тесты"))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert!(r.text().contains("«Сборка > Тесты»"), "{}", r.text());
    let md = get(&a, &format!("{uri}?format=md")).await.text();
    assert_eq!(md, "# Сборка\nmake\n## Тесты\ncargo test\n");
    let r = a
        .t(Method::PATCH, &uri, Some(patch("Сборка > Тесты")))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

// ---- M6 §16 п. 19 (CR-20): `Idempotency-Key` заметок ----

/// Запрос первым токеном с `Idempotency-Key: key` (и `If-Match`, если задан).
async fn keyed(
    a: &Api,
    m: Method,
    uri: &str,
    body: Value,
    key: &str,
    if_match: Option<&str>,
) -> common::api::Resp {
    let mut h = vec![("idempotency-key", key)];
    if let Some(r) = if_match {
        h.push(("if-match", r));
    }
    a.tok_h(&a.token, m, uri, Some(body), &h).await
}

/// Ревизии узла (новые первыми) и текст текущей.
async fn revs_and_text(a: &Api, nid: &str) -> (usize, String) {
    let revs = get(a, &format!("/api/v1/nodes/{nid}/revisions"))
        .await
        .json();
    let md = get(a, &format!("/api/v1/nodes/{nid}?format=md"))
        .await
        .text();
    (revs.as_array().unwrap().len(), md)
}

/// CR-20 (§17 п. 25): два одинаковых `PATCH append` с одним ключом — одна вставка, оба 200
/// (повтор — узел в текущем виде, как у записей журнала); другой ключ — новая правка. Так же
/// `PUT`: повтор с тем же `If-Match` не даёт 409.
#[tokio::test]
async fn patch_append_idempotent() {
    let a = api("idem-patch", |_| {}).await;
    let n = a.note("knowledge", "Идем/Патч", "# A\nначало\n").await;
    let nid = id(&n);
    let uri = format!("/api/v1/nodes/{nid}");
    let body = json!({"base_rev": 1, "ops": [{"op": "append", "content": "хвост\n"}]});
    let r1 = keyed(&a, Method::PATCH, &uri, body.clone(), "p-1", None).await;
    assert_eq!(r1.status, StatusCode::OK, "{}", r1.text());
    let r2 = keyed(&a, Method::PATCH, &uri, body.clone(), "p-1", None).await;
    assert_eq!(r2.status, StatusCode::OK, "{}", r2.text());
    assert_eq!(r2.json()["current_rev"], 2, "{}", r2.text());
    assert_eq!(r2.json()["id"], nid.as_str());
    let (revs, md) = revs_and_text(&a, &nid).await;
    assert_eq!((revs, md.matches("хвост").count()), (2, 1), "{md}");
    // другой ключ — та же правка ещё раз (append применяется к текущему тексту)
    let r3 = keyed(&a, Method::PATCH, &uri, body, "p-2", None).await;
    assert_eq!(r3.status, StatusCode::OK, "{}", r3.text());
    let (revs, md) = revs_and_text(&a, &nid).await;
    assert_eq!((revs, md.matches("хвост").count()), (3, 2), "{md}");
    // PUT: повтор с прежним If-Match — 200 без новой ревизии, а не 409
    let put = json!({"content": "# A\nзамена\n"});
    let r = keyed(&a, Method::PUT, &uri, put.clone(), "u-1", Some("3")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = keyed(&a, Method::PUT, &uri, put.clone(), "u-1", Some("3")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["current_rev"], 4, "{}", r.text());
    assert_eq!(revs_and_text(&a, &nid).await.0, 4);
    // без ключа — прежнее поведение: устаревший If-Match — 409
    let r = a
        .tok_h(&a.token, Method::PUT, &uri, Some(put), &[("if-match", "3")])
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
}

/// CR-20: повтор `POST /nodes` с тем же ключом — 200 и тот же узел (не 409 «путь занят»);
/// другой ключ — новый узел; ключ действует, пока жив узел (как у записей журнала).
#[tokio::test]
async fn post_node_idempotent() {
    let a = api("idem-post", |_| {}).await;
    let body = |path: &str| json!({"space": "knowledge", "path": path, "kind": "note", "content": "# Идем\nтекст\n"});
    let r1 = keyed(
        &a,
        Method::POST,
        "/api/v1/nodes",
        body("Идем/Один"),
        "c-1",
        None,
    )
    .await;
    assert_eq!(r1.status, StatusCode::CREATED, "{}", r1.text());
    let r2 = keyed(
        &a,
        Method::POST,
        "/api/v1/nodes",
        body("Идем/Один"),
        "c-1",
        None,
    )
    .await;
    assert_eq!(r2.status, StatusCode::OK, "{}", r2.text());
    assert_eq!(r2.json()["id"], r1.json()["id"]);
    assert_eq!(r2.json()["current_rev"], 1);
    let nid = id(&r1.json());
    assert_eq!(revs_and_text(&a, &nid).await.0, 1);
    // другой ключ, другой путь — новый узел; другой ключ, тот же путь — 409, как без ключа
    let r3 = keyed(
        &a,
        Method::POST,
        "/api/v1/nodes",
        body("Идем/Два"),
        "c-2",
        None,
    )
    .await;
    assert_eq!(r3.status, StatusCode::CREATED, "{}", r3.text());
    assert_ne!(r3.json()["id"], r1.json()["id"]);
    let r4 = keyed(
        &a,
        Method::POST,
        "/api/v1/nodes",
        body("Идем/Один"),
        "c-3",
        None,
    )
    .await;
    assert_eq!(r4.status, StatusCode::CONFLICT, "{}", r4.text());
    // правка с ключом до удаления — для повтора после
    let uri = format!("/api/v1/nodes/{nid}");
    let append = json!({"base_rev": 1, "ops": [{"op": "append", "content": "x\n"}]});
    let r = keyed(&a, Method::PATCH, &uri, append.clone(), "c-p", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // узел удалён в корзину — повтор POST по ключу 409 (как у удалённой записи журнала),
    // повтор PATCH — 404 (узел в корзине для правки не виден, ревью задачи 19 M-1)
    let d = a
        .t(Method::DELETE, &format!("/api/v1/nodes/{nid}"), None)
        .await;
    assert_eq!(d.status, StatusCode::NO_CONTENT, "{}", d.text());
    let r5 = keyed(
        &a,
        Method::POST,
        "/api/v1/nodes",
        body("Идем/Один"),
        "c-1",
        None,
    )
    .await;
    assert_eq!(r5.status, StatusCode::CONFLICT, "{}", r5.text());
    let r = keyed(&a, Method::PATCH, &uri, append, "c-p", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    // пустой ключ равен отсутствию, слишком длинный — 400
    let r = keyed(
        &a,
        Method::POST,
        "/api/v1/nodes",
        body("Идем/Три"),
        " ",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let long = "k".repeat(201);
    let r = keyed(
        &a,
        Method::POST,
        "/api/v1/nodes",
        body("Идем/Четыре"),
        &long,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

/// CR-20: ключ, повторённый с другим телом или для другой цели, — 422
/// `/problems/idempotency-mismatch`, ничего не записано.
#[tokio::test]
async fn idempotency_key_body_mismatch_422() {
    let a = api("idem-mismatch", |_| {}).await;
    let n = a.note("knowledge", "Идем/Сверка", "# A\nначало\n").await;
    let nid = id(&n);
    let other = id(&a.note("knowledge", "Идем/Другая", "# B\nб\n").await);
    let uri = format!("/api/v1/nodes/{nid}");
    let append = |c: &str| json!({"base_rev": 1, "ops": [{"op": "append", "content": c}]});
    let r = keyed(&a, Method::PATCH, &uri, append("один\n"), "m-1", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let mismatch = |r: &common::api::Resp| {
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
        assert_eq!(
            r.json()["type"],
            "/problems/idempotency-mismatch",
            "{}",
            r.text()
        );
    };
    // другое тело
    mismatch(&keyed(&a, Method::PATCH, &uri, append("два\n"), "m-1", None).await);
    // другая заметка
    let ouri = format!("/api/v1/nodes/{other}");
    mismatch(&keyed(&a, Method::PATCH, &ouri, append("один\n"), "m-1", None).await);
    // другой метод: PUT с ключом PATCH
    let put = json!({"content": "# A\nзамена\n"});
    mismatch(&keyed(&a, Method::PUT, &uri, put, "m-1", Some("2")).await);
    // POST с ключом PATCH
    let create = json!({"space": "knowledge", "path": "Идем/Новая", "kind": "note",
                        "content": "# Н\n"});
    mismatch(&keyed(&a, Method::POST, "/api/v1/nodes", create, "m-1", None).await);
    let (revs, md) = revs_and_text(&a, &nid).await;
    assert_eq!(revs, 2);
    assert!(!md.contains("два") && !md.contains("замена"), "{md}");
    assert_eq!(revs_and_text(&a, &other).await.0, 1);
    let uri = format!(
        "/api/v1/nodes/by-path?space=knowledge&path={}",
        enc("Идем/Новая")
    );
    let r = get(&a, &uri).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
}

/// CR-20: параллельные запросы с одним ключом — одна вставка, все ответы 200 (писатель базы
/// один, сверка ключа и правка — в одной транзакции).
#[tokio::test]
async fn idempotency_key_parallel_single_write() {
    let a = api("idem-parallel", |_| {}).await;
    let n = a.note("knowledge", "Идем/Гонка", "# A\nначало\n").await;
    let nid = id(&n);
    let uri = format!("/api/v1/nodes/{nid}");
    let body = json!({"base_rev": 1, "ops": [{"op": "append", "content": "гонка\n"}]});
    let calls = (0..8).map(|_| keyed(&a, Method::PATCH, &uri, body.clone(), "race-1", None));
    let rs = futures_util::future::join_all(calls).await;
    for r in &rs {
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(r.json()["current_rev"], 2, "{}", r.text());
    }
    let (revs, md) = revs_and_text(&a, &nid).await;
    assert_eq!((revs, md.matches("гонка").count()), (2, 1), "{md}");
    // то же для создания: один узел
    let create = json!({"space": "knowledge", "path": "Идем/Гонка2", "kind": "note",
                        "content": "# Г\n"});
    let calls = (0..8).map(|_| {
        keyed(
            &a,
            Method::POST,
            "/api/v1/nodes",
            create.clone(),
            "race-2",
            None,
        )
    });
    let rs = futures_util::future::join_all(calls).await;
    let created = rs
        .iter()
        .filter(|r| r.status == StatusCode::CREATED)
        .count();
    let replayed = rs.iter().filter(|r| r.status == StatusCode::OK).count();
    assert_eq!(
        (created, replayed),
        (1, 7),
        "{:?}",
        rs.iter().map(|r| r.status).collect::<Vec<_>>()
    );
    let ids: std::collections::HashSet<String> = rs.iter().map(|r| id(&r.json())).collect();
    assert_eq!(ids.len(), 1);
}
