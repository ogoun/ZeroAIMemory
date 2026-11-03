//! Права на карту кода (M5.6 задача 4, спецификация §5 «Права», §13, §17 п. 10): синхронизация —
//! владелец и участник `rw` (атрибуция `user_id`), чтение — любой с правом чтения решения,
//! `ro` — 403 на запись, отзыв — как у прочих маршрутов решения, Knowledge/Rules/Human — 422,
//! очистка карты — только владелец решения в своей сессии WebUI (аудит `code.clear`).
//!
//! Мир — `common::reports`: A — владелец решения `G`, B — участник `rw`, C — участник `ro`; у
//! каждого — токен `rw` и сессия WebUI.
mod common;

use axum::http::{Method, StatusCode};
use common::api::Resp;
use common::reports::{G, User, W, ok, problem, web, web_h};
use serde_json::{Value, json};

async fn world(name: &str) -> W {
    common::reports::world(&format!("code-access-{name}")).await
}

fn h(n: u64) -> String {
    format!("{n:064x}")
}

fn file(path: &str, n: u64, name: &str) -> Value {
    json!({"path": path, "lang": "rust", "hash": h(n), "size": 10, "parse_state": "ok",
           "symbols": [{"name": name, "qual": name, "kind": "function", "line": 1,
                        "end_line": 2, "sig": format!("fn {name}()"),
                        "body_hash": format!("{n:016x}")}],
           "refs": []})
}

fn put_body(guid: &str, files: Vec<Value>) -> Value {
    json!({"solution": guid, "machine": "box", "files": files})
}

fn sync_body(guid: &str) -> Value {
    json!({"solution": guid, "manifest": [{"path": "a.rs", "hash": h(1)}]})
}

/// Кто действует: токен или сессия WebUI пользователя.
enum As<'a> {
    Tok(&'a str),
    Web(&'a User),
}

async fn call(w: &W, who: &As<'_>, m: Method, uri: &str, body: Option<Value>) -> Resp {
    match who {
        As::Tok(t) => w.a.tok(t, m, uri, body).await,
        As::Web(u) => web(&w.a, &u.web, m, uri, body).await,
    }
}

async fn put(w: &W, who: &As<'_>, guid: &str, files: Vec<Value>) -> Resp {
    call(
        w,
        who,
        Method::PUT,
        "/api/v1/code/files",
        Some(put_body(guid, files)),
    )
    .await
}

async fn map(w: &W, who: &As<'_>, guid: &str) -> Resp {
    call(
        w,
        who,
        Method::GET,
        &format!("/api/v1/code/map?solution={guid}&format=json"),
        None,
    )
    .await
}

fn paths(r: &Resp) -> Vec<String> {
    r.json()["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect()
}

/// `user_id` строки файла карты в базе владельца.
async fn uploader(w: &W, path: &str) -> String {
    let path = path.to_string();
    w.db()
        .await
        .read(move |c| {
            Ok(c.query_row(
                "SELECT user_id FROM code_files WHERE path = ?1",
                [&path],
                |r| r.get::<_, String>(0),
            )?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn owner_and_rw_member_sync_attributed() {
    let w = world("attrib").await;
    let owner = As::Tok(&w.o.tok);
    let member = As::Tok(&w.rw.tok);
    let member_web = As::Web(&w.rw);
    ok(
        &put(&w, &owner, G, vec![file("a.rs", 1, "alpha")]).await,
        StatusCode::OK,
        "владелец",
    );
    let r = call(
        &w,
        &member,
        Method::POST,
        "/api/v1/code/sync",
        Some(sync_body(G)),
    )
    .await;
    ok(&r, StatusCode::OK, "участник rw: синхронизация");
    ok(
        &put(&w, &member, G, vec![file("b.rs", 2, "beta")]).await,
        StatusCode::OK,
        "участник rw",
    );
    ok(
        &put(&w, &member_web, G, vec![file("c.rs", 3, "gamma")]).await,
        StatusCode::OK,
        "участник rw в WebUI",
    );
    assert_eq!(uploader(&w, "a.rs").await, w.o.id);
    assert_eq!(uploader(&w, "b.rs").await, w.rw.id);
    assert_eq!(uploader(&w, "c.rs").await, w.rw.id);
    // карта одна — в базе владельца, видна обоим
    let r = map(&w, &owner, G).await;
    ok(&r, StatusCode::OK, "карта владельца");
    let mut p = paths(&r);
    p.sort();
    assert_eq!(p, ["a.rs", "b.rs", "c.rs"]);
    let r = map(&w, &member, G).await;
    assert_eq!(paths(&r).len(), 3);
}

#[tokio::test]
async fn ro_token_and_member_read_map_403_sync() {
    let w = world("ro").await;
    ok(
        &put(&w, &As::Tok(&w.o.tok), G, vec![file("a.rs", 1, "alpha")]).await,
        StatusCode::OK,
        "карта",
    );
    let owner_ro = w.a.issue(&w.o.id, "ro").await;
    for (who, ctx) in [
        (As::Tok(&w.ro.tok), "участник ro (токен)"),
        (As::Web(&w.ro), "участник ro (WebUI)"),
        (As::Tok(&owner_ro), "токен ro владельца"),
    ] {
        let r = map(&w, &who, G).await;
        ok(&r, StatusCode::OK, &format!("{ctx}: карта"));
        assert_eq!(paths(&r), ["a.rs"]);
        let r = call(
            &w,
            &who,
            Method::GET,
            &format!("/api/v1/code/status?solution={G}"),
            None,
        )
        .await;
        ok(&r, StatusCode::OK, &format!("{ctx}: статус"));
        let r = call(
            &w,
            &who,
            Method::GET,
            &format!("/api/v1/code/symbols?solution={G}&q=alpha"),
            None,
        )
        .await;
        ok(&r, StatusCode::OK, &format!("{ctx}: символы"));
        let r = call(
            &w,
            &who,
            Method::POST,
            "/api/v1/code/impact",
            Some(json!({"solution": G, "files": [{"path": "a.rs"}]})),
        )
        .await;
        ok(&r, StatusCode::OK, &format!("{ctx}: влияние — чтение"));
        let uri = format!("/api/v1/code/files?solution={G}");
        let r = call(&w, &who, Method::GET, &uri, None).await;
        ok(&r, StatusCode::OK, &format!("{ctx}: таблица файлов"));
        assert_eq!(r.json()["total"], 1);
        let r = call(
            &w,
            &who,
            Method::POST,
            "/api/v1/code/sync",
            Some(sync_body(G)),
        )
        .await;
        ok(&r, StatusCode::FORBIDDEN, &format!("{ctx}: синхронизация"));
        let r = put(&w, &who, G, vec![file("x.rs", 9, "x")]).await;
        ok(&r, StatusCode::FORBIDDEN, &format!("{ctx}: загрузка"));
    }
    let r = map(&w, &As::Tok(&w.o.tok), G).await;
    assert_eq!(paths(&r), ["a.rs"], "ro ничего не записал");
}

#[tokio::test]
async fn revoked_member_403_like_search() {
    let w = world("revoked").await;
    ok(
        &put(&w, &As::Tok(&w.o.tok), G, vec![file("a.rs", 1, "alpha")]).await,
        StatusCode::OK,
        "карта",
    );
    ok(
        &map(&w, &As::Tok(&w.rw.tok), G).await,
        StatusCode::OK,
        "до отзыва",
    );
    let r =
        w.a.tok(
            &w.o.tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{}/revoke", w.rw.id),
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "отзыв");
    let member = As::Tok(&w.rw.tok);
    // тот же ответ, что у прочих маршрутов решения (дерево пространства)
    let tree = call(
        &w,
        &member,
        Method::GET,
        &format!("/api/v1/spaces/{G}/tree"),
        None,
    )
    .await;
    ok(&tree, StatusCode::FORBIDDEN, "дерево после отзыва");
    for (m, uri, body) in [
        (Method::GET, format!("/api/v1/code/map?solution={G}"), None),
        (
            Method::GET,
            format!("/api/v1/code/status?solution={G}"),
            None,
        ),
        (
            Method::POST,
            "/api/v1/code/sync".to_string(),
            Some(sync_body(G)),
        ),
    ] {
        let r = call(&w, &member, m, &uri, body).await;
        ok(&r, StatusCode::FORBIDDEN, &uri);
        assert_eq!(problem(&r), problem(&tree), "{uri}");
        assert_eq!(problem(&r), "/problems/solution-access");
    }
}

#[tokio::test]
async fn knowledge_rules_human_422() {
    let w = world("spaces").await;
    let owner = As::Tok(&w.o.tok);
    let spaces =
        w.a.tok(&w.o.tok, Method::GET, "/api/v1/spaces", None)
            .await
            .json();
    let knowledge_id = spaces["spaces"]
        .as_array()
        .unwrap_or_else(|| panic!("{spaces}"))
        .iter()
        .find(|s| s["kind"] == "knowledge")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    for space in ["knowledge", "rules", "human", knowledge_id.as_str()] {
        let r = map(&w, &owner, space).await;
        ok(
            &r,
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("карта {space}"),
        );
        assert!(
            r.json()["detail"].as_str().unwrap().contains("решени"),
            "{}",
            r.text()
        );
        let r = call(
            &w,
            &owner,
            Method::POST,
            "/api/v1/code/sync",
            Some(sync_body(space)),
        )
        .await;
        ok(
            &r,
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("синхронизация {space}"),
        );
        let r = put(&w, &owner, space, vec![]).await;
        ok(
            &r,
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("загрузка {space}"),
        );
        let uri = format!("/api/v1/code/files?solution={space}");
        let r = call(&w, &owner, Method::GET, &uri, None).await;
        ok(
            &r,
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("таблица файлов {space}"),
        );
    }
    // неизвестное решение — 404
    let r = map(&w, &owner, "0192f1c4-7a1e-7c3b-9a55-000000000000").await;
    ok(&r, StatusCode::NOT_FOUND, "нет решения");
}

#[tokio::test]
async fn clear_owner_cookie_only_audited() {
    let w = world("clear").await;
    ok(
        &put(&w, &As::Tok(&w.o.tok), G, vec![file("a.rs", 1, "alpha")]).await,
        StatusCode::OK,
        "карта",
    );
    let uri = format!("/api/v1/code?solution={G}");
    let not_owners: Vec<(&str, Resp)> = vec![
        (
            "rw web",
            web(&w.a, &w.rw.web, Method::DELETE, &uri, None).await,
        ),
        (
            "ro web",
            web(&w.a, &w.ro.web, Method::DELETE, &uri, None).await,
        ),
        (
            "owner token",
            w.a.tok(&w.o.tok, Method::DELETE, &uri, None).await,
        ),
        (
            "rw token",
            w.a.tok(&w.rw.tok, Method::DELETE, &uri, None).await,
        ),
        (
            "ro token",
            w.a.tok(&w.ro.tok, Method::DELETE, &uri, None).await,
        ),
        (
            "admin for owner",
            w.a.adm_h(Method::DELETE, &uri, None, &[("x-zam-user", &w.o.id)])
                .await,
        ),
    ];
    for (who, r) in not_owners {
        ok(&r, StatusCode::FORBIDDEN, who);
    }
    let s =
        w.a.tok(
            &w.o.tok,
            Method::GET,
            &format!("/api/v1/code/status?solution={G}"),
            None,
        )
        .await
        .json();
    assert_eq!(s["files"], 1, "карта на месте: {s}");
    let r = web_h(&w.a, &w.o.web, Method::DELETE, &uri, None, &[]).await;
    ok(&r, StatusCode::OK, "владелец в WebUI");
    assert_eq!(r.json()["removed"], 1);
    let s =
        w.a.tok(
            &w.o.tok,
            Method::GET,
            &format!("/api/v1/code/status?solution={G}"),
            None,
        )
        .await
        .json();
    assert_eq!(s["exists"], false, "{s}");
    // администратор в своей базе — владелец своего решения: очистка по cookie — 200
    let g_admin = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e55";
    let reg = json!({"agent_solution_id": g_admin, "name": "Своё"});
    let r = w.a.t(Method::POST, "/api/v1/solutions", Some(reg)).await;
    ok(&r, StatusCode::CREATED, "решение администратора");
    let body = put_body(g_admin, vec![file("x.rs", 7, "x")]);
    let r = w.a.t(Method::PUT, "/api/v1/code/files", Some(body)).await;
    ok(&r, StatusCode::OK, "карта администратора");
    let uri = format!("/api/v1/code?solution={g_admin}");
    let r = w.a.adm(Method::DELETE, &uri, None, None).await;
    ok(&r, StatusCode::OK, "администратор очищает свою");
    assert_eq!(r.json()["removed"], 1);
    let audit = w.a.st.system.list_audit(100, None).await.unwrap();
    let rows: Vec<_> = audit.iter().filter(|a| a.action == "code.clear").collect();
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|a| a.target.as_deref() == Some(G))
        .collect();
    assert_eq!(rows.len(), 1, "одна запись аудита");
    assert_eq!(rows[0].target.as_deref(), Some(G));
    assert_eq!(rows[0].user_id.map(|u| u.to_string()), Some(w.o.id.clone()));
    assert_eq!(rows[0].details["files"], 1);
}

#[tokio::test]
async fn member_sees_only_solution_map() {
    let w = world("isolation").await;
    // второе решение владельца — без участников
    let g3 = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e77";
    let r =
        w.a.tok(
            &w.o.tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": g3, "name": "Личное"})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "второе решение");
    let owner = As::Tok(&w.o.tok);
    ok(
        &put(&w, &owner, G, vec![file("a.rs", 1, "alpha")]).await,
        StatusCode::OK,
        "карта G",
    );
    ok(
        &put(&w, &owner, g3, vec![file("secret.rs", 2, "hidden_fn")]).await,
        StatusCode::OK,
        "карта g3",
    );
    let member = As::Tok(&w.rw.tok);
    let r = map(&w, &member, G).await;
    assert_eq!(paths(&r), ["a.rs"], "только карта общего решения");
    let r = map(&w, &member, g3).await;
    ok(
        &r,
        StatusCode::NOT_FOUND,
        "чужое решение без запроса доступа",
    );
    // символ другого решения по id — не найден даже владельцу
    let s =
        w.a.tok(
            &w.o.tok,
            Method::GET,
            &format!("/api/v1/code/symbols?solution={g3}&q=hidden_fn"),
            None,
        )
        .await
        .json();
    let id = s["symbols"][0]["id"].as_i64().unwrap();
    for who in [&member, &owner] {
        let r = call(
            &w,
            who,
            Method::GET,
            &format!("/api/v1/code/symbols/{id}?solution={G}"),
            None,
        )
        .await;
        ok(&r, StatusCode::NOT_FOUND, "символ другого решения");
    }
    let r = call(
        &w,
        &member,
        Method::GET,
        &format!("/api/v1/code/symbols?solution={G}&q=hidden_fn"),
        None,
    )
    .await;
    ok(&r, StatusCode::OK, "поиск в G");
    assert!(
        r.json()["symbols"].as_array().unwrap().is_empty(),
        "{}",
        r.text()
    );
}

/// Состояния ссылок пути `path` в индексе хуков участника: `(заголовок, символ, состояние)`.
async fn ref_states(w: &W, tok: &str, path: &str) -> Vec<(String, Option<String>, String)> {
    let idx =
        w.a.tok(
            tok,
            Method::GET,
            &format!("/api/v1/file-refs?solution={G}"),
            None,
        )
        .await
        .json();
    idx["refs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["path"] == path)
        .flat_map(|r| r["items"].as_array().unwrap().clone())
        .map(|it| {
            (
                it["title"].as_str().unwrap().to_string(),
                it["symbol"].as_str().map(str::to_string),
                it["state"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Knowledge участника (другая база) по правилу карты (ревью задачи 3, I2/I3): CRLF-близнец
/// файла с другой машины не трогает файловых ссылок; удаление файла синхронизацией
/// (`git rm`) — `symbol_removed` у ссылок на его символы, в ответе `sync` — `stale_refs`.
#[tokio::test]
async fn member_knowledge_crlf_twin_and_removed_file() {
    let w = world("member-knowledge").await;
    let sol = [("x-zam-solution", G)];
    // у участника в Knowledge: запись о файле (с хэшем) и заметка о символе
    let r =
        w.a.tok_h(
            &w.rw.tok,
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(
                json!({"kind": "insight", "title": "О файле a.rs", "body": "Подробности.",
                        "data": {"kind": "gotcha"}, "force": true,
                        "refs": {"files": [{"path": "a.rs", "hash": h(1)}]}}),
            ),
            &sol,
        )
        .await;
    ok(&r, StatusCode::CREATED, "запись участника");
    let r =
        w.a.tok_h(
            &w.rw.tok,
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "knowledge", "path": "О alpha", "kind": "note",
                        "content": "---\nrefs: [\"a.rs#alpha\"]\n---\nтекст\n"}),
            ),
            &sol,
        )
        .await;
    ok(&r, StatusCode::CREATED, "заметка участника");
    // у владельца в решении — заметка о том же символе
    let r =
        w.a.tok(
            &w.o.tok,
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": G, "path": "О alpha (решение)", "kind": "note",
                        "content": "---\nrefs: [\"a.rs#alpha\"]\n---\nтекст\n"}),
            ),
        )
        .await;
    ok(&r, StatusCode::CREATED, "заметка владельца");
    let r = put(&w, &As::Tok(&w.rw.tok), G, vec![file("a.rs", 1, "alpha")]).await;
    ok(&r, StatusCode::OK, "первая загрузка");
    assert_eq!(r.json()["stale_refs"], 0);
    // та же версия с другой машины: другой хэш файла (CRLF), то же тело символа
    let mut twin = file("a.rs", 2, "alpha");
    twin["symbols"][0]["body_hash"] = json!(format!("{:016x}", 1));
    let r = put(&w, &As::Tok(&w.rw.tok), G, vec![twin]).await;
    ok(&r, StatusCode::OK, "близнец с тем же телом");
    assert_eq!(
        r.json()["stale_refs"],
        0,
        "файловые ссылки не судятся картой"
    );
    for (title, symbol, state) in ref_states(&w, &w.rw.tok, "a.rs").await {
        assert_eq!(state, "ok", "{title} {symbol:?}");
    }
    // `git rm a.rs`: манифест без файла
    let r = call(
        &w,
        &As::Tok(&w.rw.tok),
        Method::POST,
        "/api/v1/code/sync",
        Some(json!({"solution": G, "manifest": [{"path": "b.rs", "hash": h(5)}]})),
    )
    .await;
    ok(&r, StatusCode::OK, "синхронизация без a.rs");
    let j = r.json();
    assert_eq!(j["removed"], 1, "{j}");
    assert_eq!(
        j["stale_refs"], 2,
        "символьные ссылки владельца и участника: {j}"
    );
    let states = ref_states(&w, &w.rw.tok, "a.rs").await;
    assert_eq!(states.len(), 3, "{states:?}");
    for (title, symbol, state) in states {
        let want = if symbol.is_some() { "stale" } else { "ok" };
        assert_eq!(state, want, "{title} {symbol:?}");
    }
}
