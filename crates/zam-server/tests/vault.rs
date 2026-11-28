//! Vault (§7.6, §12; задача 12 M3): шифрование значений, мастер-ключ и блокировка,
//! раскрытие с аудитом, изоляция Vault участника общего решения, значения вне FTS, Qdrant,
//! логов и байтов базы. Без моделей (FTS-only).
mod common;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api, api_with};
use common::process::ServerProc;
use serde_json::json;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e51";
const SESSION: &str = "sess-vault-1";
const MACHINE: &str = "box-vault";

/// Значение секрета: в исходниках теста нет строки целиком, поиск по байтам однозначен.
fn value(tag: &str) -> String {
    format!("zv-{tag}-{}-{}", "Qm9zcy1zZWNyZXQ", "x7Lp2Rt9Wd")
}

fn attrib_h() -> [(&'static str, &'static str); 2] {
    [("x-zam-session", SESSION), ("x-zam-machine", MACHINE)]
}

async fn set(a: &Api, tok: &str, space: &str, title: &str, secret: &str) -> Resp {
    a.tok_h(
        tok,
        Method::POST,
        &format!("/api/v1/spaces/{space}/vault"),
        Some(json!({
            "title": title,
            "data": {"type": "token", "url": "https://git.example.com", "description": "деплой"},
            "secret": secret,
        })),
        &attrib_h(),
    )
    .await
}

async fn reveal(a: &Api, tok: &str, id: &str) -> Resp {
    a.tok_h(
        tok,
        Method::GET,
        &format!("/api/v1/vault/{id}?reveal=1"),
        None,
        &attrib_h(),
    )
    .await
}

fn id_of(r: &Resp) -> String {
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn agent_sets_and_reveals_secret_audited() {
    let a = api("vault-reveal", |_| {}).await;
    let v = value("reveal");
    let id = id_of(&set(&a, &a.token, "knowledge", "github", &v).await);
    let created = set(&a, &a.token, "knowledge", "github-2", &v).await;
    assert!(!created.text().contains(&v), "ответ создания без значения");
    // метаданные — без значения
    let r = a.t(Method::GET, &format!("/api/v1/vault/{id}"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let m = r.json();
    assert_eq!(m["title"], "github");
    assert_eq!(m["data"]["type"], "token");
    assert_eq!(m["space"], "knowledge");
    assert_eq!(m["rev"], 1);
    assert_eq!(m["author"]["kind"], "agent");
    assert!(m.get("secret").is_none() && !r.text().contains(&v), "{m}");
    // аудита без раскрытия нет
    let audit = a.st.system.list_audit(100, None).await.unwrap();
    assert!(!audit.iter().any(|x| x.action == "vault.reveal"));
    // раскрытие: значение, no-store, аудит с сессией и машиной, actor — id токена
    let r = reveal(&a, &a.token, &id).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["secret"], v);
    assert_eq!(r.header("cache-control"), "no-store");
    let tok = a.st.system.list_tokens(a.st.admin_id).await.unwrap();
    let audit = a.st.system.list_audit(100, None).await.unwrap();
    let row = audit
        .iter()
        .find(|x| x.action == "vault.reveal")
        .expect("аудит vault.reveal");
    assert_eq!(row.actor, tok[0].id.to_string());
    assert_eq!(row.target.as_deref(), Some(id.as_str()));
    assert_eq!(row.user_id, Some(a.st.admin_id));
    assert_eq!(row.details["item"], id.as_str());
    assert_eq!(row.details["session"], SESSION);
    assert_eq!(row.details["machine"], MACHINE);
    for x in &audit {
        assert!(!x.details.to_string().contains(&v), "значение в аудите");
    }
    // Vault решения: своё зарегистрированное решение
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Своё"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let sid = id_of(&set(&a, &a.token, G, "github", &value("sol")).await);
    let r = reveal(&a, &a.token, &sid).await;
    assert_eq!(r.json()["secret"], value("sol"));
    assert_eq!(r.json()["space"], G);
    assert_eq!(r.json()["agent_solution_id"], G);
    // незарегистрированное решение, rules, human — 404
    for s in ["0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e99", "rules", "human"] {
        let r = set(&a, &a.token, s, "x", "y").await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{s}: {}", r.text());
        let r = a
            .t(Method::GET, &format!("/api/v1/spaces/{s}/vault"), None)
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{s}");
    }
    // неверные данные: тип, лишнее поле, пустое значение
    for body in [
        json!({"title": "a", "data": {"type": "magic"}, "secret": "s"}),
        json!({"title": "a", "data": {"type": "token", "extra": 1}, "secret": "s"}),
        json!({"title": "a", "data": {"type": "token"}, "secret": ""}),
        json!({"title": " ", "data": {"type": "token"}, "secret": "s"}),
    ] {
        let r = a
            .t(Method::POST, "/api/v1/spaces/knowledge/vault", Some(body))
            .await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    }
    // удаление: мягкое, с аудитом; потом 404
    let r = a
        .t(Method::DELETE, &format!("/api/v1/vault/{id}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    assert_eq!(
        a.t(Method::GET, &format!("/api/v1/vault/{id}"), None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let audit = a.st.system.list_audit(100, None).await.unwrap();
    assert!(
        audit
            .iter()
            .any(|x| x.action == "vault.delete" && x.target.as_deref() == Some(id.as_str()))
    );
    // история удалённой записи: токену — 404, администратору — видна и раскрывается
    let hist = format!("/api/v1/vault/{id}/revisions");
    assert_eq!(
        a.t(Method::GET, &hist, None).await.status,
        StatusCode::NOT_FOUND
    );
    let r = a.adm(Method::GET, &hist, None, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["items"].as_array().unwrap().len(), 1);
    let r = a
        .adm(Method::GET, &format!("{hist}?reveal=1&rev=1"), None, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["secret"], v);
}

#[tokio::test]
async fn vault_list_never_returns_values() {
    let a = api("vault-list", |_| {}).await;
    let (v1, v2) = (value("one"), value("two"));
    let id = id_of(&set(&a, &a.token, "knowledge", "github", &v1).await);
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/vault",
            Some(
                json!({"title": "postgres", "data": {"type": "connection_string",
                "username": "app", "description": "основная база"}, "secret": v2}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = a
        .t(Method::GET, "/api/v1/spaces/knowledge/vault", None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let items = r.json()["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|i| i.get("secret").is_none()));
    assert!(!r.text().contains(&v1) && !r.text().contains(&v2));
    // поиск по открытым полям: имя, url, description
    for (q, title) in [
        ("GIT", "github"),
        ("example.com", "github"),
        ("основная", "postgres"),
    ] {
        let r = a
            .t(
                Method::GET,
                &format!("/api/v1/spaces/knowledge/vault?q={}", common::api::enc(q)),
                None,
            )
            .await;
        let j = r.json();
        assert_eq!(j["items"].as_array().unwrap().len(), 1, "{q}: {j}");
        assert_eq!(j["items"][0]["title"], title, "{q}");
    }
    // поиск по значению ничего не находит — ни в Vault, ни в общем поиске
    let r = a
        .t(
            Method::GET,
            &format!("/api/v1/spaces/knowledge/vault?q={v1}"),
            None,
        )
        .await;
    assert!(r.json()["items"].as_array().unwrap().is_empty());
    let r = a
        .t(Method::POST, "/api/v1/search", Some(json!({"query": v1})))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!r.text().contains(&v1));
    // новое значение — новая ревизия; ревизии без значений; старое раскрывает только
    // администратор
    let v3 = value("three");
    let r = a
        .t(
            Method::PATCH,
            &format!("/api/v1/vault/{id}"),
            Some(json!({"secret": v3})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["rev"], 2);
    assert!(!r.text().contains(&v3));
    // правка имени без значения — ещё ревизия, значение то же
    let r = a
        .tok_h(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/vault/{id}"),
            Some(json!({"title": "github-main"})),
            &[("if-match", "\"2\"")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["rev"], 3);
    let r = a
        .tok_h(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/vault/{id}"),
            Some(json!({"title": "stale"})),
            &[("if-match", "\"2\"")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(reveal(&a, &a.token, &id).await.json()["secret"], v3);
    let r = a
        .t(Method::GET, &format!("/api/v1/vault/{id}/revisions"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let revs = r.json()["items"].as_array().unwrap().clone();
    assert_eq!(revs.len(), 3);
    assert!(!r.text().contains(&v1) && !r.text().contains(&v3));
    let old = format!("/api/v1/vault/{id}/revisions?reveal=1&rev=1");
    assert_eq!(
        a.t(Method::GET, &old, None).await.status,
        StatusCode::FORBIDDEN
    );
    let r = a.adm(Method::GET, &old, None, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["secret"], v1);
    assert_eq!(r.json()["rev"], 1);
    let audit = a.st.system.list_audit(100, None).await.unwrap();
    assert!(
        audit
            .iter()
            .any(|x| x.action == "vault.reveal" && x.actor == "admin" && x.details["rev"] == 1)
    );
}

#[tokio::test]
async fn ro_token_cannot_set_403() {
    let a = api("vault-ro", |_| {}).await;
    let ro = a.issue(&a.admin_id(), "ro").await;
    let r = set(&a, &ro, "knowledge", "github", "x").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let id = id_of(&set(&a, &a.token, "knowledge", "github", &value("ro")).await);
    let r = a
        .tok(
            &ro,
            Method::PATCH,
            &format!("/api/v1/vault/{id}"),
            Some(json!({"secret": "y"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = a
        .tok(&ro, Method::DELETE, &format!("/api/v1/vault/{id}"), None)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // чтение и раскрытие `ro`-токеном — можно
    assert_eq!(reveal(&a, &ro, &id).await.json()["secret"], value("ro"));
}

#[tokio::test]
async fn title_unique_per_scope_409() {
    let a = api("vault-unique", |_| {}).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Своё"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = id_of(&set(&a, &a.token, "knowledge", "github", "a").await);
    let r = set(&a, &a.token, "knowledge", "github", "b").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    // то же имя в Vault решения — другая область
    id_of(&set(&a, &a.token, G, "github", "c").await);
    // переименование в занятое — 409; после удаления имя свободно
    let other = id_of(&set(&a, &a.token, "knowledge", "gitlab", "d").await);
    let r = a
        .t(
            Method::PATCH,
            &format!("/api/v1/vault/{other}"),
            Some(json!({"title": "github"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    a.t(Method::DELETE, &format!("/api/v1/vault/{id}"), None)
        .await;
    id_of(&set(&a, &a.token, "knowledge", "github", "e").await);
}

#[tokio::test]
async fn member_vault_for_shared_solution_in_member_db() {
    let a = api("vault-member", |_| {}).await;
    let (a_id, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let (_, c_tok) = a.new_user("Вера").await;
    let reg = json!({"agent_solution_id": G, "name": "Общее"});
    let r = a
        .tok(&a_tok, Method::POST, "/api/v1/solutions", Some(reg.clone()))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = a
        .tok(&b_tok, Method::POST, "/api/v1/solutions", Some(reg))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    // до одобрения — 403 solution-access
    let r = set(&a, &b_tok, G, "db", "x").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(r.json()["type"], "/problems/solution-access");
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let (va, vb) = (value("owner"), value("member"));
    let id_a = id_of(&set(&a, &a_tok, G, "db", &va).await);
    // у участника своё имя «db» в своей базе — не конфликт с владельцем
    let id_b = id_of(&set(&a, &b_tok, G, "db", &vb).await);
    // запись B — в users/B.db с agent_solution_id решения, в базе A её нет
    let b_db = a.st.users.get(b_id.parse().unwrap()).await.unwrap();
    let a_db = a.st.users.get(a_id.parse().unwrap()).await.unwrap();
    let q = |db: zam_store::Db, id: String| async move {
        db.read(move |c| {
            Ok(c.query_row(
                "SELECT count(*), max(agent_solution_id), max(scope) FROM vault_items WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )?)
        })
        .await
        .unwrap()
    };
    assert_eq!(
        q(b_db.clone(), id_b.clone()).await,
        (1, Some(G.to_string()), Some("solution".into()))
    );
    assert_eq!(q(a_db.clone(), id_b.clone()).await.0, 0);
    assert_eq!(q(b_db, id_a.clone()).await.0, 0);
    // каждый видит только свои записи
    let list = |tok: String| {
        let a = &a;
        async move {
            let r = a
                .tok(
                    &tok,
                    Method::GET,
                    &format!("/api/v1/spaces/{G}/vault"),
                    None,
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["id"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(list(a_tok.clone()).await, vec![id_a.clone()]);
    assert_eq!(list(b_tok.clone()).await, vec![id_b.clone()]);
    // чужая запись по id — 404 в обе стороны, и для чтения, и для раскрытия, и для записи
    for (tok, other) in [(&b_tok, &id_a), (&a_tok, &id_b), (&c_tok, &id_a)] {
        for uri in [
            format!("/api/v1/vault/{other}"),
            format!("/api/v1/vault/{other}?reveal=1"),
            format!("/api/v1/vault/{other}/revisions"),
        ] {
            let r = a.tok(tok, Method::GET, &uri, None).await;
            assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}: {}", r.text());
        }
        let r = a
            .tok(
                tok,
                Method::PATCH,
                &format!("/api/v1/vault/{other}"),
                Some(json!({"secret": "z"})),
            )
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
    }
    // постороннему список Vault решения — 404 (нет запроса доступа)
    let r = a
        .tok(
            &c_tok,
            Method::GET,
            &format!("/api/v1/spaces/{G}/vault"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    // свой Vault участник раскрывает; администратор — по X-Zam-User
    assert_eq!(reveal(&a, &b_tok, &id_b).await.json()["secret"], vb);
    let r = a
        .adm(
            Method::GET,
            &format!("/api/v1/vault/{id_b}?reveal=1"),
            None,
            Some(&b_id),
        )
        .await;
    assert_eq!(r.json()["secret"], vb);
    // отзыв доступа действует со следующего запроса
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/revoke"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = reveal(&a, &b_tok, &id_b).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(r.json()["state"], "revoked");
}

/// Сервер с другим мастер-ключом в файле: Vault заблокирован (503 vault-locked с причиной),
/// остальной сервер работает, ключ не перезаписан.
#[tokio::test]
async fn wrong_master_key_locks_vault_server_works() {
    let dir = common::tmp("vault-wrong-key");
    let c = common::cfg_in(dir.clone());
    let a = api_with(c.clone()).await;
    let id = id_of(&set(&a, &a.token, "knowledge", "github", &value("wk")).await);
    let token = a.token.clone();
    a.st.shutdown().await;
    drop(a);
    let key_file = dir.join("master.key");
    let original = std::fs::read(&key_file).expect("master.key создан при первом старте");
    // чужой ключ: 32 байта в base64
    std::fs::write(&key_file, "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE=\n").unwrap();
    let b = api_with(c.clone()).await;
    // значение — 503 с причиной; метаданные — 200 (см. vault_list_when_locked_returns_metadata)
    let r = reveal(&b, &token, &id).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    let j = r.json();
    assert_eq!(j["type"], "/problems/vault-locked", "{j}");
    assert!(j["detail"].as_str().unwrap().contains("мастер-ключ"), "{j}");
    let r = set(&b, &token, "knowledge", "new", "x").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    // остальной сервер работает
    let r = b.tok(&token, Method::GET, "/api/v1/spaces", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = b
        .tok(
            &token,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "knowledge", "path": "n", "kind": "note", "content": "# N\n"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    // подменённый файл не перезаписан
    assert_ne!(std::fs::read(&key_file).unwrap(), original);
    b.st.shutdown().await;
    drop(b);
    // прежний ключ — Vault снова работает
    std::fs::write(&key_file, &original).unwrap();
    let c2 = api_with(c).await;
    assert_eq!(reveal(&c2, &token, &id).await.json()["secret"], value("wk"));
}

/// Сервер с подменённым мастер-ключом после записи секрета: `(Api, токен, id записи,
/// каталог данных)`; прежний ключ — `master.key.orig` рядом.
async fn locked_world(name: &str, secret: &str) -> (Api, String, String, PathBuf) {
    let dir = common::tmp(name);
    let c = common::cfg_in(dir.clone());
    let a = api_with(c.clone()).await;
    let id = id_of(&set(&a, &a.token, "knowledge", "github", secret).await);
    let token = a.token.clone();
    a.st.shutdown().await;
    drop(a);
    let key_file = dir.join("master.key");
    std::fs::copy(&key_file, dir.join("master.key.orig")).unwrap();
    std::fs::write(
        &key_file,
        "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE=
",
    )
    .unwrap();
    (api_with(c).await, token, id, dir)
}

/// Заблокированный Vault: список, метаданные и ревизии — 200 с `locked: true` и `reason`,
/// только открытые поля (ни значения, ни шифротекста); при работающем Vault `locked: false`.
#[tokio::test]
async fn vault_list_when_locked_returns_metadata() {
    // работающий Vault: locked=false, reason нет
    let ok = api("vault-meta-ok", |_| {}).await;
    let sec = value("lk");
    let id = id_of(&set(&ok, &ok.token, "knowledge", "github", &sec).await);
    let tok = ok.token.clone();
    let r = ok
        .tok(&tok, Method::GET, "/api/v1/spaces/knowledge/vault", None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["locked"], false);
    assert!(r.json().get("reason").is_none());
    let r = ok
        .tok(&tok, Method::GET, &format!("/api/v1/vault/{id}"), None)
        .await;
    assert_eq!(r.json()["locked"], false, "{}", r.text());

    let (b, token, id, _dir) = locked_world("vault-meta-locked", &sec).await;
    let uris = [
        "/api/v1/spaces/knowledge/vault".to_string(),
        "/api/v1/spaces/knowledge/vault?q=git".to_string(),
        format!("/api/v1/vault/{id}"),
        format!("/api/v1/vault/{id}/revisions"),
    ];
    for uri in &uris {
        let r = b.tok(&token, Method::GET, uri, None).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        let j = r.json();
        assert_eq!(j["locked"], true, "{uri}: {j}");
        assert!(
            j["reason"].as_str().unwrap().contains("мастер-ключ"),
            "{uri}: {j}"
        );
        let text = r.text();
        assert!(!text.contains(&sec), "{uri}: значение в ответе");
        assert!(!text.contains("\"secret\""), "{uri}: {text}");
    }
    // метаданные те же, что при работающем Vault
    let j = b
        .tok(&token, Method::GET, "/api/v1/spaces/knowledge/vault", None)
        .await
        .json();
    assert_eq!(j["items"][0]["title"], "github", "{j}");
    assert_eq!(j["items"][0]["data"]["type"], "token", "{j}");
    assert_eq!(j["items"][0]["id"], id.as_str(), "{j}");
    let j = b
        .tok(&token, Method::GET, &format!("/api/v1/vault/{id}"), None)
        .await
        .json();
    assert_eq!(j["title"], "github", "{j}");
    assert_eq!(j["rev"], 1, "{j}");
    let j = b
        .tok(
            &token,
            Method::GET,
            &format!("/api/v1/vault/{id}/revisions"),
            None,
        )
        .await
        .json();
    assert_eq!(j["items"].as_array().unwrap().len(), 1, "{j}");
    // чужая область и несуществующая запись — прежние ошибки
    let r = b
        .tok(&token, Method::GET, "/api/v1/spaces/rules/vault", None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let r = b
        .tok(
            &token,
            Method::GET,
            &format!("/api/v1/vault/{}", zam_core::Id::new()),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
}

/// Заблокированный Vault: значения, создание, правка, удаление и раскрытие ревизий — 503.
#[tokio::test]
async fn vault_reveal_and_write_when_locked_503() {
    let (b, token, id, _dir) = locked_world("vault-write-locked", &value("w")).await;
    let cases = [
        (Method::GET, format!("/api/v1/vault/{id}?reveal=1"), None),
        (
            Method::GET,
            format!("/api/v1/vault/{id}/revisions?reveal=1&rev=1"),
            None,
        ),
        (
            Method::POST,
            "/api/v1/spaces/knowledge/vault".to_string(),
            Some(json!({"title": "n", "data": {"type": "note"}, "secret": "x"})),
        ),
        (
            Method::PATCH,
            format!("/api/v1/vault/{id}"),
            Some(json!({"title": "z"})),
        ),
        (Method::DELETE, format!("/api/v1/vault/{id}"), None),
    ];
    for (m, uri, body) in cases {
        let r = b.tok(&token, m.clone(), &uri, body).await;
        assert_eq!(
            r.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{m} {uri}: {}",
            r.text()
        );
        assert_eq!(r.json()["type"], "/problems/vault-locked", "{uri}");
    }
    // запись не удалена и не изменена
    let j = b
        .tok(&token, Method::GET, &format!("/api/v1/vault/{id}"), None)
        .await
        .json();
    assert_eq!(j["title"], "github", "{j}");
    assert_eq!(j["rev"], 1, "{j}");
}

#[tokio::test]
async fn missing_key_file_with_existing_keys_no_regen() {
    let dir = common::tmp("vault-missing-key");
    let c = common::cfg_in(dir.clone());
    let a = api_with(c.clone()).await;
    let id = id_of(&set(&a, &a.token, "knowledge", "github", &value("mk")).await);
    let token = a.token.clone();
    a.st.shutdown().await;
    drop(a);
    let key_file = dir.join("master.key");
    let original = std::fs::read(&key_file).unwrap();
    std::fs::remove_file(&key_file).unwrap();
    let b = api_with(c.clone()).await;
    let r = reveal(&b, &token, &id).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert_eq!(r.json()["type"], "/problems/vault-locked");
    assert!(!key_file.exists(), "новый мастер-ключ молча не создаётся");
    let r = b.tok(&token, Method::GET, "/api/v1/spaces", None).await;
    assert_eq!(r.status, StatusCode::OK);
    b.st.shutdown().await;
    drop(b);
    std::fs::write(&key_file, &original).unwrap();
    let c2 = api_with(c).await;
    assert_eq!(reveal(&c2, &token, &id).await.json()["secret"], value("mk"));
}

/// Копия каталога данных без мастер-ключа: значений нет в байтах баз (в том числе в
/// ревизиях), сервер на копии держит Vault заблокированным.
#[tokio::test]
async fn copy_without_master_key_reveals_nothing() {
    let dir = common::tmp("vault-copy-src");
    let a = api_with(common::cfg_in(dir.clone())).await;
    let v = value("copy");
    let id = id_of(&set(&a, &a.token, "knowledge", "github", &v).await);
    // ещё ревизия: значение в vault_revisions тоже только шифротекстом
    let r = a
        .t(
            Method::PATCH,
            &format!("/api/v1/vault/{id}"),
            Some(json!({"title": "github-2"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let token = a.token.clone();
    a.st.shutdown().await;
    drop(a);
    // копия каталога без master.key (на Windows открытые базы SQLite заблокированы для
    // копирования — копия после остановки; WAL «на ходу» проверяет тест журналов)
    let dst = common::tmp("vault-copy-dst");
    copy_dir(&dir, &dst, &["master.key", ".zam.lock"]);
    let hits = common::files_containing(&dst, v.as_bytes());
    assert!(hits.is_empty(), "значение в байтах копии: {hits:?}");
    assert!(
        std::fs::read_dir(dst.join("users"))
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(".db")),
        "в копии есть базы пользователей"
    );
    let b = api_with(common::cfg_in(dst.clone())).await;
    let r = reveal(&b, &token, &id).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert!(!dst.join("master.key").exists());
}

fn copy_dir(src: &Path, dst: &Path, skip: &[&str]) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap().flatten() {
        let name = e.file_name();
        if skip.iter().any(|s| name == *s) {
            continue;
        }
        let (from, to) = (e.path(), dst.join(&name));
        if from.is_dir() {
            copy_dir(&from, &to, skip);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

/// Настоящий процесс `zam-server` с журналом `trace` в файл: значение секрета не попадает
/// ни в журнал сервера, ни в байты баз (включая WAL), ни в выдачу поиска, ни в очередь
/// векторной индексации (Qdrant).
#[test]
fn vault_values_not_in_fts_qdrant_logs() {
    let root = common::tmp("vault-logs");
    let data = root.join("data");
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, common::qdrant::free_port()));
    let mut c = common::cfg_in(data.clone());
    c.server.bind = addr;
    c.log.level = "trace".into();
    c.log.format = zam_server::config::LogFormat::Text;
    let conf = root.join("zam.toml");
    std::fs::write(&conf, toml::to_string(&c).unwrap()).unwrap();
    let mut srv = ServerProc::start(&conf, addr);
    let token = srv.initial_token(Duration::from_secs(120));
    let t = Instant::now();
    while srv.try_call("GET", "/api/v1/spaces", &token, None).is_err() {
        srv.assert_running("приём соединений");
        assert!(
            t.elapsed() < Duration::from_secs(120),
            "сервер не слушает порт"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let v = value("logs");
    // заметка: очередь индексации и чанки не пусты — проверки ниже содержательны
    let (code, n) = srv.call(
        "POST",
        "/api/v1/nodes",
        &token,
        Some(
            &json!({"space": "knowledge", "path": "ops/deploy", "kind": "note",
            "content": "# Деплой\n\nКлюч GitHub лежит в Vault под именем github.\n"}),
        ),
    );
    assert_eq!(code, 201, "{n}");
    let note_id = n["id"].as_str().unwrap().to_string();
    let (code, j) = srv.call(
        "POST",
        "/api/v1/spaces/knowledge/vault",
        &token,
        Some(&json!({"title": "github", "data": {"type": "token"}, "secret": v})),
    );
    assert_eq!(code, 201, "{j}");
    let id = j["id"].as_str().unwrap().to_string();
    let (code, j) = srv.call("GET", &format!("/api/v1/vault/{id}?reveal=1"), &token, None);
    assert_eq!(code, 200, "{j}");
    assert_eq!(j["secret"], v);
    let (code, j) = srv.call(
        "POST",
        "/api/v1/search",
        &token,
        Some(&json!({"query": v, "mode": "chunks"})),
    );
    assert_eq!(code, 200, "{j}");
    assert!(!j.to_string().contains(&v), "{j}");
    // неудачная запись с значением в теле тоже не попадает в журнал
    let (code, _) = srv.call(
        "POST",
        "/api/v1/spaces/knowledge/vault",
        &token,
        Some(&json!({"title": "github", "data": {"type": "token"}, "secret": v})),
    );
    assert_eq!(code, 409);
    common::process::wait_server_log(&data, "", Duration::from_secs(5));
    srv.kill();
    let logs = data.join("logs");
    assert!(
        std::fs::read_dir(&logs).unwrap().flatten().any(|e| e
            .file_name()
            .to_string_lossy()
            .starts_with("zam-server.log")),
        "журнал сервера в каталоге данных"
    );
    let hits = common::files_containing(&data, v.as_bytes());
    assert!(
        hits.is_empty(),
        "значение в файлах каталога данных: {hits:?}"
    );
    let tail = srv.tail();
    assert!(!tail.contains(&v), "значение в stderr сервера");
    // очередь индексации (FTS и Qdrant) и чанки: заметка есть, записи Vault и значения — нет
    let users: Vec<PathBuf> = std::fs::read_dir(data.join("users"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "db"))
        .collect();
    assert!(!users.is_empty());
    let (mut note_jobs, mut note_chunks) = (0, 0);
    for p in users {
        let db = zam_store::Db::open(&p, zam_store::USER_MIGRATIONS).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (id, note, v) = (id.clone(), note_id.clone(), v.clone());
        let (nj, nc, vault_jobs, vault_chunks, value_chunks): (i64, i64, i64, i64, i64) = rt
            .block_on(db.read(move |c| {
                let q = |sql: &str, arg: &str| -> Result<i64, zam_store::StoreError> {
                    c.query_row(sql, [arg], |r| r.get(0))
                        .map_err(zam_store::StoreError::from)
                };
                Ok((
                    q(
                        "SELECT count(*) FROM index_jobs WHERE target_id = ?1",
                        &note,
                    )?,
                    q("SELECT count(*) FROM chunks WHERE node_id = ?1", &note)?,
                    q("SELECT count(*) FROM index_jobs WHERE target_id = ?1", &id)?,
                    q(
                        "SELECT count(*) FROM chunks WHERE node_id = ?1 OR entry_id = ?1",
                        &id,
                    )?,
                    q("SELECT count(*) FROM chunks WHERE instr(text, ?1) > 0", &v)?,
                ))
            }))
            .unwrap();
        assert_eq!(
            (vault_jobs, vault_chunks, value_chunks),
            (0, 0, 0),
            "{}",
            p.display()
        );
        note_jobs += nj;
        note_chunks += nc;
    }
    assert!(
        note_jobs >= 1 && note_chunks >= 1,
        "заметка в очереди и чанках"
    );
}

// ---- окружение (M4.5 §8, задача 11): host/environment, vault_fts, дайджест ----

async fn post_env(
    a: &Api,
    title: &str,
    ty: &str,
    extra: serde_json::Value,
    secret: Option<&str>,
) -> Resp {
    let mut data = json!({"type": ty});
    for (k, v) in extra.as_object().unwrap() {
        data[k] = v.clone();
    }
    let mut body = json!({"title": title, "data": data});
    if let Some(s) = secret {
        body["secret"] = json!(s);
    }
    a.t(Method::POST, "/api/v1/spaces/knowledge/vault", Some(body))
        .await
}

fn titles(r: &Resp) -> Vec<String> {
    r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap().to_string())
        .collect()
}

async fn list_q(a: &Api, q: &str) -> Resp {
    let r = a
        .t(
            Method::GET,
            &format!("/api/v1/spaces/knowledge/vault?q={}", common::api::enc(q)),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r
}

#[tokio::test]
async fn vault_host_without_secret() {
    let a = api("vault-host-nosecret", |_| {}).await;
    let extra = json!({"purpose": "стенд интеграционных тестов", "address": "10.0.0.5:22",
        "limits": "только ночью", "notes": "ключи ротируются раз в месяц"});
    for ty in ["host", "environment"] {
        let r = post_env(&a, &format!("{ty}-1"), ty, extra.clone(), None).await;
        let id = id_of(&r);
        assert_eq!(r.json()["data"]["address"], "10.0.0.5:22");
        // раскрытие записи без значения: secret = null, не ошибка
        let r = reveal(&a, &a.token, &id).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert!(r.json()["secret"].is_null(), "{}", r.text());
        // правка открытых полей без значения — новая ревизия, значения по-прежнему нет
        let r = a
            .t(
                Method::PATCH,
                &format!("/api/v1/vault/{id}"),
                Some(json!({"data": {"type": ty, "purpose": "новое"}})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(r.json()["rev"], 2);
        assert!(reveal(&a, &a.token, &id).await.json()["secret"].is_null());
        // значение можно добавить позже
        let sec = value(ty);
        let r = a
            .t(
                Method::PATCH,
                &format!("/api/v1/vault/{id}"),
                Some(json!({"secret": sec})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(reveal(&a, &a.token, &id).await.json()["secret"], sec);
    }
    // пределы открытых полей
    for (k, n) in [
        ("purpose", 501),
        ("address", 501),
        ("limits", 1001),
        ("notes", 2001),
    ] {
        let r = post_env(&a, "big", "host", json!({ k: "x".repeat(n) }), None).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{k}");
    }
    let max = json!({"notes": "x".repeat(2000), "limits": "y".repeat(1000)});
    let r = post_env(&a, "ok", "host", max, None).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

#[tokio::test]
async fn vault_password_still_requires_secret() {
    let a = api("vault-pw-secret", |_| {}).await;
    for ty in [
        "password",
        "token",
        "ssh_key",
        "connection_string",
        "url",
        "repo",
        "note",
    ] {
        let r = post_env(&a, ty, ty, json!({}), None).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{ty}: {}",
            r.text()
        );
    }
    // host без значения нельзя превратить в password без значения
    let id = id_of(&post_env(&a, "h", "host", json!({}), None).await);
    let r = a
        .t(
            Method::PATCH,
            &format!("/api/v1/vault/{id}"),
            Some(json!({"data": {"type": "password"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    let r = a
        .t(
            Method::PATCH,
            &format!("/api/v1/vault/{id}"),
            Some(json!({"data": {"type": "password"}, "secret": value("pw")})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // пустое значение по-прежнему 422, даже у host
    let r = post_env(&a, "h2", "host", json!({}), Some("")).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn vault_fts_open_fields() {
    let a = api("vault-fts-open", |_| {}).await;
    let h = id_of(
        &post_env(
            &a,
            "стенд-приёмки",
            "environment",
            json!({"purpose": "интеграционные тесты биллинга", "address": "stage.example.com:8443",
                "limits": "только ночью", "notes": "ротация ключей ежемесячно",
                "username": "deployer"}),
            None,
        )
        .await,
    );
    id_of(&set(&a, &a.token, "knowledge", "github", &value("g")).await);
    // title, purpose (префикс), address, limits, notes, username, url, description
    for (q, want) in [
        ("приёмки", "стенд-приёмки"),
        ("интегр", "стенд-приёмки"),
        ("stage.example", "стенд-приёмки"),
        ("ночью", "стенд-приёмки"),
        ("ротация", "стенд-приёмки"),
        ("deployer", "стенд-приёмки"),
        ("git.example.com", "github"),
        ("деплой", "github"),
    ] {
        assert_eq!(titles(&list_q(&a, q).await), [want], "q={q}");
    }
    // все слова запроса (AND)
    assert_eq!(
        titles(&list_q(&a, "интеграционные биллинга").await),
        ["стенд-приёмки"]
    );
    assert!(titles(&list_q(&a, "интеграционные деплой").await).is_empty());
    // общее слово: оба
    assert_eq!(titles(&list_q(&a, "example").await).len(), 2);
    // спецсинтаксис FTS во входе не действует и не ломает запрос
    assert_eq!(list_q(&a, "\"интегр\" OR *").await.status, StatusCode::OK);
    // правка меняет индекс, удаление убирает запись
    let r = a
        .t(
            Method::PATCH,
            &format!("/api/v1/vault/{h}"),
            Some(json!({"data": {"type": "environment", "purpose": "нагрузочные испытания"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(titles(&list_q(&a, "интегр").await).is_empty());
    assert_eq!(titles(&list_q(&a, "нагрузочные").await), ["стенд-приёмки"]);
    // короткий запрос (2 символа): запасной путь — подстрока имени
    assert_eq!(titles(&list_q(&a, "gi").await), ["github"]);
    let r = a
        .t(Method::DELETE, &format!("/api/v1/vault/{h}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(titles(&list_q(&a, "нагрузочные").await).is_empty());
}

/// Значение и шифротекст не попадают в `vault_fts`: ни поиском, ни прямым чтением таблицы.
#[tokio::test]
async fn vault_fts_never_indexes_secret() {
    use base64::Engine;
    let a = api("vault-fts-secret", |_| {}).await;
    let sec = "zebra-correct-horse-battery";
    id_of(&set(&a, &a.token, "knowledge", "github", sec).await);
    let h = id_of(&post_env(&a, "стенд", "host", json!({"purpose": "тесты"}), Some(sec)).await);
    // правка значения тоже не индексируется
    let r = a
        .t(
            Method::PATCH,
            &format!("/api/v1/vault/{h}"),
            Some(json!({"secret": "okapi-secret"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    for q in [
        "zebra",
        "correct",
        "horse",
        "okapi",
        "zebra-correct-horse-battery",
    ] {
        assert!(titles(&list_q(&a, q).await).is_empty(), "q={q}");
    }
    assert_eq!(titles(&list_q(&a, "тесты").await), ["стенд"]);
    // прямой SQL по vault_fts: только title и открытый текст
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let rows: Vec<(String, String, String)> = db
        .read(|c| {
            let mut st = c.prepare("SELECT item_id, title, open_text FROM vault_fts")?;
            Ok(st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<_, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let dump = format!("{rows:?}");
    for bad in ["zebra", "okapi", "horse"] {
        assert!(!dump.contains(bad), "{bad} в vault_fts: {dump}");
    }
    let blobs: Vec<Vec<u8>> = db
        .read(|c| {
            let mut st = c.prepare("SELECT secret FROM vault_items")?;
            Ok(st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(blobs.len(), 2);
    for b in &blobs {
        let b64 = base64::engine::general_purpose::STANDARD.encode(b);
        assert!(!dump.contains(&b64), "шифротекст в vault_fts");
    }
    // прямой MATCH по значению ничего не находит
    let n: i64 = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM vault_fts WHERE vault_fts MATCH 'zebra OR okapi'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(n, 0);
}

async fn recall_digest(a: &Api, tok: &str, body: serde_json::Value) -> serde_json::Value {
    let r = a
        .tok_h(
            tok,
            Method::POST,
            "/api/v1/recall",
            Some(body),
            &[("x-zam-session", "sess-env")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

#[tokio::test]
async fn digest_environment_section_no_secret() {
    let a = api("vault-digest-env", |c| c.recall.timeout_ms = 30_000).await;
    let sec = "zebra-digest-secret-99";
    let h = id_of(
        &post_env(
            &a,
            "stage",
            "host",
            json!({"purpose": "приёмочный стенд", "address": "stage.example.com:8443",
                "limits": "только ночью", "notes": "служебная заметка не для дайджеста"}),
            Some(sec),
        )
        .await,
    );
    // запись без открытых полей: только имя и ссылка; без значения
    let bare = id_of(&post_env(&a, "bare", "environment", json!({}), None).await);
    // пароль в дайджест не попадает
    set(&a, &a.token, "knowledge", "pg-password", sec).await;
    let j = recall_digest(&a, &a.token, json!({"event": "session_start"})).await;
    let text = j["text"].as_str().unwrap();
    assert!(text.contains("### Окружение"), "{text}");
    assert!(
        text.contains(&format!(
            "stage — приёмочный стенд; stage.example.com:8443; ограничения: только ночью — v:{h}"
        )),
        "{text}"
    );
    assert!(text.contains(&format!("bare — v:{bare}")), "{text}");
    assert!(
        !text.contains(sec) && !text.contains("служебная заметка"),
        "{text}"
    );
    assert!(!text.contains("pg-password"), "{text}");
    assert!(!j.to_string().contains(sec));
    // повтор в той же сессии: дайджест повторяемый, окружение остаётся
    let j2 = recall_digest(&a, &a.token, json!({"event": "session_start"})).await;
    assert!(j2["text"].as_str().unwrap().contains("### Окружение"));
    // пакет восстановления окружения не содержит (его состав фиксирован, §9.2)
    let j3 = recall_digest(&a, &a.token, json!({"event": "recovery"})).await;
    assert!(!j3["text"].as_str().unwrap().contains("### Окружение"));
    // учёта показа окружения нет: ни session_seen, ни usage
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let n: i64 = db
        .read(move |c| {
            Ok(c.query_row(
                "SELECT (SELECT count(*) FROM usage WHERE target_id IN (?1, ?2))
                      + (SELECT count(*) FROM session_seen WHERE target_id IN (?1, ?2))",
                [&h, &bare],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(n, 0);
    // не более 5 записей
    for i in 0..6 {
        let n = format!("extra-{i}");
        id_of(&post_env(&a, &n, "host", json!({"purpose": "p"}), None).await);
    }
    let j = recall_digest(&a, &a.token, json!({"event": "session_start"})).await;
    let text = j["text"].as_str().unwrap();
    let env = text.split("### Окружение").nth(1).unwrap();
    let env = env.split("###").next().unwrap();
    assert_eq!(env.matches("v:").count(), 5, "{text}");
}

/// M6.6 §4.3 (замечание 16а): у частей строки окружения (`purpose`, `address`, `limits`) хвостовые
/// `.`, `;`, `,` и пробелы срезаются до склейки «; » — не «стенд.; адрес»; многоточие `…`
/// остаётся, часть из одной пунктуации опускается.
#[tokio::test]
async fn digest_environment_trims_trailing_punct() {
    let a = api("vault-digest-env-punct", |c| c.recall.timeout_ms = 30_000).await;
    let h = id_of(
        &post_env(
            &a,
            "stage",
            "host",
            json!({"purpose": "приёмочный стенд.", "address": "192.0.2.10:22;",
                "limits": "только ночью, "}),
            None,
        )
        .await,
    );
    let w = id_of(
        &post_env(
            &a,
            "wait",
            "environment",
            json!({"purpose": "ждём доступа…", "address": "."}),
            None,
        )
        .await,
    );
    let j = recall_digest(&a, &a.token, json!({"event": "session_start"})).await;
    let text = j["text"].as_str().unwrap();
    assert!(
        text.contains(&format!(
            "stage — приёмочный стенд; 192.0.2.10:22; ограничения: только ночью — v:{h}"
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!("wait — ждём доступа… — v:{w}")),
        "{text}"
    );
    assert!(!text.contains(".;") && !text.contains(";;"), "{text}");
}

/// Vault решения: в дайджесте — личное окружение и окружение запрошенного решения.
#[tokio::test]
async fn digest_environment_solution_scope() {
    let a = api("vault-digest-sol", |c| c.recall.timeout_ms = 30_000).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Своё"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let body = json!({"title": "sol-host", "data": {"type": "host", "purpose": "решение"}});
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/spaces/{G}/vault"),
            Some(body),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    id_of(
        &post_env(
            &a,
            "personal-host",
            "host",
            json!({"purpose": "личное"}),
            None,
        )
        .await,
    );
    let with = recall_digest(
        &a,
        &a.token,
        json!({"event": "session_start", "solution": G}),
    )
    .await;
    let t = with["text"].as_str().unwrap();
    assert!(t.contains("sol-host") && t.contains("personal-host"), "{t}");
    let without = recall_digest(&a, &a.token, json!({"event": "session_start"})).await;
    let t = without["text"].as_str().unwrap();
    assert!(
        !t.contains("sol-host") && t.contains("personal-host"),
        "{t}"
    );
}

/// Окружение решения — только при действующем доступе (I-1 финального ревью M4.5): участник
/// видит открытые поля своей записи Vault решения, после отзыва — только личный Vault.
#[tokio::test]
async fn digest_environment_excluded_after_revoke() {
    let a = api("vault-digest-revoked", |c| c.recall.timeout_ms = 30_000).await;
    let (_, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let reg = json!({"agent_solution_id": G, "name": "Общее"});
    let r = a
        .tok(&a_tok, Method::POST, "/api/v1/solutions", Some(reg.clone()))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = a
        .tok(&b_tok, Method::POST, "/api/v1/solutions", Some(reg))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    for (space, title, purpose) in [
        (G, "member-sol-host", "стенд решения"),
        ("knowledge", "member-own-host", "личный стенд"),
    ] {
        let body = json!({"title": title, "data": {"type": "host", "purpose": purpose}});
        let r = a
            .tok(
                &b_tok,
                Method::POST,
                &format!("/api/v1/spaces/{space}/vault"),
                Some(body),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    let body = json!({"event": "session_start", "solution": G});
    let j = recall_digest(&a, &b_tok, body.clone()).await;
    let t = j["text"].as_str().unwrap();
    assert!(
        t.contains("member-sol-host — стенд решения") && t.contains("member-own-host"),
        "{t}"
    );
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/revoke"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = recall_digest(&a, &b_tok, body).await;
    let t = j["text"].as_str().unwrap();
    assert!(
        !t.contains("member-sol-host") && !t.contains("стенд решения"),
        "{t}"
    );
    assert!(t.contains("member-own-host"), "{t}");
    assert!(
        j["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w == "solution access revoked"),
        "{j}"
    );
    // неизвестный GUID — только личное окружение
    let other = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9eff";
    let j = recall_digest(
        &a,
        &b_tok,
        json!({"event": "session_start", "solution": other}),
    )
    .await;
    let t = j["text"].as_str().unwrap();
    assert!(
        !t.contains("member-sol-host") && t.contains("member-own-host"),
        "{t}"
    );
}

/// Заблокированный Vault (нет мастер-ключа): поиск и дайджест отдают открытые поля,
/// значение — 503.
#[tokio::test]
async fn vault_locked_digest_still_lists_open_fields() {
    let dir = common::tmp("vault-locked-digest");
    let mut c = common::cfg_in(dir.clone());
    c.recall.timeout_ms = 30_000;
    let a = api_with(c.clone()).await;
    let sec = "zebra-locked-secret";
    let h = id_of(
        &post_env(
            &a,
            "stage",
            "host",
            json!({"purpose": "приёмочный стенд", "address": "10.1.2.3"}),
            Some(sec),
        )
        .await,
    );
    let token = a.token.clone();
    a.st.shutdown().await;
    drop(a);
    std::fs::write(
        dir.join("master.key"),
        "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE=\n",
    )
    .unwrap();
    let b = api_with(c).await;
    let j = recall_digest(&b, &token, json!({"event": "session_start"})).await;
    let text = j["text"].as_str().unwrap();
    assert!(
        text.contains(&format!("stage — приёмочный стенд; 10.1.2.3 — v:{h}")),
        "{text}"
    );
    assert!(!text.contains(sec));
    // поиск по открытым полям при блокировке работает; значение — 503
    let uri = format!(
        "/api/v1/spaces/knowledge/vault?q={}",
        common::api::enc("приёмочный")
    );
    let r = b.tok(&token, Method::GET, &uri, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["locked"], true);
    assert_eq!(r.json()["items"].as_array().unwrap().len(), 1);
    let r = b
        .tok(
            &token,
            Method::GET,
            &format!("/api/v1/vault/{h}?reveal=1"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
}

// ---- перенос между областями (M6.6 §7, задача 6): POST /vault/{id}/move ----

const G2: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e52";

async fn register(a: &Api, tok: &str, guid: &str, name: &str) {
    let r = a
        .tok(
            tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": guid, "name": name})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

/// `POST /vault/{id}/move {space}` с заголовками атрибуции.
async fn move_item(a: &Api, tok: &str, id: &str, space: &str) -> Resp {
    a.tok_h(
        tok,
        Method::POST,
        &format!("/api/v1/vault/{id}/move"),
        Some(json!({ "space": space })),
        &attrib_h(),
    )
    .await
}

/// Метаданные записи (без значения) по токену `tok`.
async fn meta(a: &Api, tok: &str, id: &str) -> serde_json::Value {
    let r = a
        .tok(tok, Method::GET, &format!("/api/v1/vault/{id}"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Номера ревизий записи.
async fn revs(a: &Api, tok: &str, id: &str) -> Vec<i64> {
    let r = a
        .tok(
            tok,
            Method::GET,
            &format!("/api/v1/vault/{id}/revisions"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["rev"].as_i64().unwrap())
        .collect()
}

/// Строки аудита `vault.move`.
async fn move_audit(a: &Api) -> Vec<zam_store::AuditRow> {
    a.st.system
        .list_audit(200, None)
        .await
        .unwrap()
        .into_iter()
        .filter(|x| x.action == "vault.move")
        .collect()
}

/// Замечание 25: хост, записанный в Vault решения, после переноса в личный Vault виден в разделе
/// «Окружение» дайджеста другого решения и в поиске личного Vault, а в Vault решения его нет.
/// Запись со значением и ревизиями переносится новой ревизией (перешифровка под `rev+1`):
/// значение то же, прежние ревизии раскрываются; перенос обратно — в решение.
#[tokio::test]
async fn vault_move_between_scopes() {
    let a = api("vault-move-scopes", |c| c.recall.timeout_ms = 30_000).await;
    register(&a, &a.token, G, "Первое").await;
    register(&a, &a.token, G2, "Второе").await;
    let body = json!({"title": "lan-stand",
        "data": {"type": "host", "purpose": "общий стенд", "address": "192.0.2.10:22"}});
    let host = id_of(
        &a.t(
            Method::POST,
            &format!("/api/v1/spaces/{G}/vault"),
            Some(body),
        )
        .await,
    );
    let api_ref = &a;
    let digest = |sol: &'static str| async move {
        let j = recall_digest(
            api_ref,
            &api_ref.token,
            json!({"event": "session_start", "solution": sol}),
        )
        .await;
        j["text"].as_str().unwrap().to_string()
    };
    let line = format!("lan-stand — общий стенд; 192.0.2.10:22 — v:{host}");
    let t = digest(G2).await;
    assert!(
        !t.contains("lan-stand"),
        "до переноса — только в своём решении: {t}"
    );
    let r = move_item(&a, &a.token, &host, "knowledge").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json(),
        json!({"id": host, "rev": 2, "scope": "knowledge", "agent_solution_id": null})
    );
    let t = digest(G2).await;
    assert!(t.contains(&line), "{t}");
    assert_eq!(titles(&list_q(&a, "стенд").await), ["lan-stand"]);
    let r = a
        .t(Method::GET, &format!("/api/v1/spaces/{G}/vault"), None)
        .await;
    assert!(titles(&r).is_empty(), "{}", r.text());
    let m = meta(&a, &a.token, &host).await;
    assert_eq!(m["space"], "knowledge", "{m}");
    assert_eq!(m["title"], "lan-stand", "{m}");
    assert_eq!(m["data"]["address"], "192.0.2.10:22", "{m}");
    assert!(reveal(&a, &a.token, &host).await.json()["secret"].is_null());
    // запись со значением и второй ревизией
    let (v1, v2) = (value("mv1"), value("mv2"));
    let tid = id_of(&set(&a, &a.token, G, "deploy-token", &v1).await);
    let r = a
        .t(
            Method::PATCH,
            &format!("/api/v1/vault/{tid}"),
            Some(json!({ "secret": v2 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = move_item(&a, &a.token, &tid, "knowledge").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["rev"], 3, "{}", r.text());
    assert!(!r.text().contains(&v2), "ответ переноса без значения");
    assert_eq!(reveal(&a, &a.token, &tid).await.json()["secret"], v2);
    assert_eq!(revs(&a, &a.token, &tid).await, [1, 2, 3]);
    // AAD связан с ревизией, а не с областью: прежние ревизии раскрываются как были
    for (rev, v) in [(1, &v1), (2, &v2)] {
        let r = a
            .adm(
                Method::GET,
                &format!("/api/v1/vault/{tid}/revisions?reveal=1&rev={rev}"),
                None,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(r.json()["secret"], *v, "rev {rev}");
    }
    // обратно в Vault решения
    let r = move_item(&a, &a.token, &tid, G).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json(),
        json!({"id": tid, "rev": 4, "scope": "solution", "agent_solution_id": G})
    );
    let j = reveal(&a, &a.token, &tid).await.json();
    assert_eq!(
        (j["secret"].clone(), j["space"].clone()),
        (json!(v2), json!(G))
    );
    // цель — не область Vault или незарегистрированное решение — 404; лишнее поле — 400
    for space in ["rules", "human", "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e99"] {
        let r = move_item(&a, &a.token, &tid, space).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{space}: {}", r.text());
    }
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/vault/{tid}/move"),
            Some(json!({"space": "knowledge", "title": "x"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // устаревший If-Match — 409, запись на месте
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &format!("/api/v1/vault/{tid}/move"),
            Some(json!({"space": "knowledge"})),
            &[("if-match", "2")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let m = meta(&a, &a.token, &tid).await;
    assert_eq!((m["space"].clone(), m["rev"].clone()), (json!(G), json!(4)));
    // удалённая запись не переносится
    let r = a
        .t(Method::DELETE, &format!("/api/v1/vault/{tid}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = move_item(&a, &a.token, &tid, "knowledge").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
}

/// Имя занято в целевой области — 409 «имя занято»; исходная запись не тронута (область,
/// ревизия, значение, история), аудита переноса нет. Имя освободилось — перенос проходит.
#[tokio::test]
async fn vault_move_title_taken_409() {
    let a = api("vault-move-taken", |_| {}).await;
    register(&a, &a.token, G, "Своё").await;
    let (vk, vs) = (value("tk"), value("ts"));
    let kid = id_of(&set(&a, &a.token, "knowledge", "github", &vk).await);
    let sid = id_of(&set(&a, &a.token, G, "github", &vs).await);
    let r = move_item(&a, &a.token, &sid, "knowledge").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(
        r.json()["detail"].as_str().unwrap().contains("имя занято"),
        "{}",
        r.text()
    );
    let m = meta(&a, &a.token, &sid).await;
    assert_eq!((m["space"].clone(), m["rev"].clone()), (json!(G), json!(1)));
    assert_eq!(revs(&a, &a.token, &sid).await, [1]);
    assert_eq!(reveal(&a, &a.token, &sid).await.json()["secret"], vs);
    assert_eq!(reveal(&a, &a.token, &kid).await.json()["secret"], vk);
    assert!(move_audit(&a).await.is_empty());
    let r = a
        .t(Method::DELETE, &format!("/api/v1/vault/{kid}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = move_item(&a, &a.token, &sid, "knowledge").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["rev"], 2);
}

/// Переименование (замечание 7; `zam vault rename`, `memory_vault rename`) — `PATCH {title}` с
/// `If-Match`: параллельная правка между чтением и записью — 409, имя прежнее; занятое имя — 409
/// «имя занято».
#[tokio::test]
async fn vault_rename_stale_if_match_409() {
    let a = api("vault-rename-stale", |_| {}).await;
    let v = value("rn");
    let id = id_of(&set(&a, &a.token, "knowledge", "github", &v).await);
    id_of(&set(&a, &a.token, "knowledge", "gitlab", &value("rn2")).await);
    // «другой клиент» успел поменять значение: ревизия 2
    let r = a
        .t(
            Method::PATCH,
            &format!("/api/v1/vault/{id}"),
            Some(json!({ "secret": value("rn3") })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rename = |title: &'static str, rev: &'static str| {
        let (a, id) = (&a, id.clone());
        async move {
            a.tok_h(
                &a.token,
                Method::PATCH,
                &format!("/api/v1/vault/{id}"),
                Some(json!({ "title": title })),
                &[("if-match", rev)],
            )
            .await
        }
    };
    let r = rename("gh-main", "1").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let r = rename("gitlab", "2").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(
        r.json()["detail"].as_str().unwrap().contains("имя занято"),
        "{}",
        r.text()
    );
    let m = meta(&a, &a.token, &id).await;
    assert_eq!(
        (m["title"].clone(), m["rev"].clone()),
        (json!("github"), json!(2))
    );
    let r = rename("gh-main", "2").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        (r.json()["title"].clone(), r.json()["rev"].clone()),
        (json!("gh-main"), json!(3))
    );
    assert_eq!(
        reveal(&a, &a.token, &id).await.json()["secret"],
        value("rn3")
    );
}

/// Портит шифротекст записи `id` в базе пользователя запроса: расшифровка дальше — 500.
async fn corrupt_blob(a: &Api, id: &str) {
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let id = id.to_string();
    db.write(move |tx| {
        tx.execute(
            "UPDATE vault_items SET secret = ?2 WHERE id = ?1",
            rusqlite::params![id, vec![1u8; 64]],
        )?;
        Ok(())
    })
    .await
    .unwrap();
}

/// Ревью задачи 6, M2: устаревший `If-Match` переноса в другую область — 409 до расшифровки и
/// шифрования (испорченный шифротекст не даёт 500); значение, область и история не тронуты,
/// аудита нет. Ревизия «из будущего» (`rev + 1`) — тоже 409.
#[tokio::test]
async fn vault_move_stale_if_match_409() {
    let a = api("vault-move-stale", |_| {}).await;
    register(&a, &a.token, G, "Своё").await;
    let v = value("st");
    let id = id_of(&set(&a, &a.token, "knowledge", "github", &v).await);
    let r = a
        .t(
            Method::PATCH,
            &format!("/api/v1/vault/{id}"),
            Some(json!({ "secret": v })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let stale_move = |id: String, rev: &'static str| {
        let a = &a;
        async move {
            a.tok_h(
                &a.token,
                Method::POST,
                &format!("/api/v1/vault/{id}/move"),
                Some(json!({ "space": G })),
                &[("if-match", rev)],
            )
            .await
        }
    };
    for rev in ["1", "3"] {
        let r = stale_move(id.clone(), rev).await;
        assert_eq!(
            r.status,
            StatusCode::CONFLICT,
            "If-Match {rev}: {}",
            r.text()
        );
    }
    let m = meta(&a, &a.token, &id).await;
    assert_eq!(
        (m["space"].clone(), m["rev"].clone()),
        (json!("knowledge"), json!(2))
    );
    assert_eq!(revs(&a, &a.token, &id).await, [1, 2]);
    assert_eq!(reveal(&a, &a.token, &id).await.json()["secret"], v);
    assert!(move_audit(&a).await.is_empty());
    // проверка ревизии — раньше криптографии: испорченный шифротекст не доходит до расшифровки
    let bad = id_of(&set(&a, &a.token, "knowledge", "broken", &value("br")).await);
    corrupt_blob(&a, &bad).await;
    let r = stale_move(bad.clone(), "7").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(meta(&a, &a.token, &bad).await["space"], "knowledge");
}

/// Ревью задачи 6, M2: у `PATCH` устаревший `If-Match` — тоже 409 до перешифровки (испорченный
/// шифротекст не даёт 500), запись не тронута.
#[tokio::test]
async fn vault_patch_stale_if_match_before_crypto_409() {
    let a = api("vault-patch-stale", |_| {}).await;
    let id = id_of(&set(&a, &a.token, "knowledge", "github", &value("ps")).await);
    corrupt_blob(&a, &id).await;
    let r = a
        .tok_h(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/vault/{id}"),
            Some(json!({"data": {"type": "token", "description": "новое"}})),
            &[("if-match", "5")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let m = meta(&a, &a.token, &id).await;
    assert_eq!(m["rev"], 1, "{m}");
    assert_eq!(m["data"]["description"], "деплой", "{m}");
}

/// Ревью задачи 6, M4: переименование в прежнее имя (в том числе с пробелами по краям) — 200
/// без новой ревизии и перешифровки, как перенос в ту же область; устаревший `If-Match` — 409.
#[tokio::test]
async fn vault_rename_same_name_no_rev() {
    let a = api("vault-rename-same", |_| {}).await;
    let v = value("sn");
    let id = id_of(&set(&a, &a.token, "knowledge", "github", &v).await);
    let before = meta(&a, &a.token, &id).await;
    for title in ["github", "  github "] {
        let r = a
            .tok_h(
                &a.token,
                Method::PATCH,
                &format!("/api/v1/vault/{id}"),
                Some(json!({ "title": title })),
                &[("if-match", "1")],
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{title:?}: {}", r.text());
        assert_eq!(r.json()["rev"], 1, "{title:?}: {}", r.text());
    }
    assert_eq!(revs(&a, &a.token, &id).await, [1]);
    assert_eq!(meta(&a, &a.token, &id).await, before);
    let r = a
        .tok_h(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/vault/{id}"),
            Some(json!({ "title": "github" })),
            &[("if-match", "4")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(reveal(&a, &a.token, &id).await.json()["secret"], v);
}

/// Ревью задачи 6, M5 (`docs/api-compat.md`, «Перенос записи Vault»): перенос в решение, доступ к
/// которому ждёт ответа владельца или отклонён, — 403 `/problems/solution-access` с состоянием;
/// без запроса доступа решение не видно — 404. Запись на месте, аудита нет.
#[tokio::test]
async fn vault_move_to_solution_without_access_403() {
    let a = api("vault-move-no-access", |_| {}).await;
    let (_, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let (c_id, c_tok) = a.new_user("Вера").await;
    register(&a, &a_tok, G, "Общее").await;
    let r = a
        .tok(
            &b_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Общее"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let own = id_of(&set(&a, &b_tok, "knowledge", "member-own", &value("na")).await);
    let r = move_item(&a, &b_tok, &own, G).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(
        r.json()["type"],
        "/problems/solution-access",
        "{}",
        r.text()
    );
    assert_eq!(r.json()["state"], "pending", "{}", r.text());
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/reject"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = move_item(&a, &b_tok, &own, G).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(r.json()["state"], "rejected", "{}", r.text());
    // без запроса доступа — 404, как у списка Vault решения
    let stranger = id_of(&set(&a, &c_tok, "knowledge", "stranger", &value("nc")).await);
    let r = move_item(&a, &c_tok, &stranger, G).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    for (id, user) in [(&own, &b_id), (&stranger, &c_id)] {
        let r = a
            .adm(
                Method::GET,
                &format!("/api/v1/vault/{id}"),
                None,
                Some(user),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(
            (r.json()["space"].clone(), r.json()["rev"].clone()),
            (json!("knowledge"), json!(1))
        );
    }
    assert!(move_audit(&a).await.is_empty());
}

/// Права записи в обеих областях: участник общего решения с `ro` не переносит ни из Vault
/// решения, ни в него (403), `ro`-токен — тоже; записи не тронуты.
#[tokio::test]
async fn vault_move_member_ro_403() {
    let a = api("vault-move-ro", |_| {}).await;
    let (_, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    register(&a, &a_tok, G, "Общее").await;
    let reg = json!({"agent_solution_id": G, "name": "Общее"});
    let r = a
        .tok(&b_tok, Method::POST, "/api/v1/solutions", Some(reg))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    a.set_access(&a_tok, G, &b_id, "rw").await;
    let sol = id_of(&set(&a, &b_tok, G, "member-db", &value("ro-sol")).await);
    let own = id_of(&set(&a, &b_tok, "knowledge", "member-own", &value("ro-own")).await);
    a.set_access(&a_tok, G, &b_id, "ro").await;
    for (id, to) in [(&sol, "knowledge"), (&own, G)] {
        let r = move_item(&a, &b_tok, id, to).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "→ {to}: {}", r.text());
    }
    for (id, space) in [(&sol, G), (&own, "knowledge")] {
        let r = a
            .adm(
                Method::GET,
                &format!("/api/v1/vault/{id}"),
                None,
                Some(&b_id),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(r.json()["space"], space, "{}", r.text());
        assert_eq!(r.json()["rev"], 1, "{}", r.text());
    }
    // `ro`-токен своего Vault — 403
    let ro = a.issue(&a.admin_id(), "ro").await;
    let mine = id_of(&set(&a, &a.token, "knowledge", "github", &value("ro-tok")).await);
    register(&a, &a.token, G2, "Своё").await;
    let r = move_item(&a, &ro, &mine, G2).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert!(move_audit(&a).await.is_empty());
}

/// Заблокированный Vault (другой мастер-ключ) — 503 `vault-locked` до проверки цели; запись на
/// месте.
#[tokio::test]
async fn vault_move_locked_503() {
    let (b, token, id, _dir) = locked_world("vault-move-locked", &value("ml")).await;
    register(&b, &token, G, "Своё").await;
    for to in [G, "knowledge", "rules"] {
        let r = move_item(&b, &token, &id, to).await;
        assert_eq!(
            r.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{to}: {}",
            r.text()
        );
        assert_eq!(r.json()["type"], "/problems/vault-locked", "{to}");
    }
    let m = meta(&b, &token, &id).await;
    assert_eq!(
        (m["space"].clone(), m["rev"].clone()),
        (json!("knowledge"), json!(1))
    );
}

/// Аудит `vault.move {item, from, to}` (с ревизией, сессией и машиной, как у прочих операций
/// Vault; `actor` — id токена), без значения; перенос обратно — вторая строка.
#[tokio::test]
async fn vault_move_audit() {
    let a = api("vault-move-audit", |_| {}).await;
    register(&a, &a.token, G, "Своё").await;
    let v = value("au");
    let id = id_of(&set(&a, &a.token, "knowledge", "github", &v).await);
    let r = move_item(&a, &a.token, &id, G).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rows = move_audit(&a).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let tok = a.st.system.list_tokens(a.st.admin_id).await.unwrap();
    let row = &rows[0];
    assert_eq!(row.actor, tok[0].id.to_string());
    assert_eq!(row.target.as_deref(), Some(id.as_str()));
    assert_eq!(row.user_id, Some(a.st.admin_id));
    let d = &row.details;
    assert_eq!(d["item"], id.as_str(), "{d}");
    assert_eq!(d["from"], "knowledge", "{d}");
    assert_eq!(d["to"], G, "{d}");
    assert_eq!(d["rev"], 2, "{d}");
    assert_eq!(d["session"], SESSION, "{d}");
    assert_eq!(d["machine"], MACHINE, "{d}");
    let r = move_item(&a, &a.token, &id, "knowledge").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rows = move_audit(&a).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows.iter()
            .any(|x| x.details["from"] == G && x.details["to"] == "knowledge"),
        "{rows:?}"
    );
    for x in a.st.system.list_audit(200, None).await.unwrap() {
        assert!(!x.details.to_string().contains(&v), "значение в аудите");
    }
}

/// Та же область — 200 без новой ревизии и без аудита; устаревший `If-Match` — 409, как у
/// пустой правки `PATCH`.
#[tokio::test]
async fn vault_move_same_scope_no_rev() {
    let a = api("vault-move-same", |_| {}).await;
    register(&a, &a.token, G, "Своё").await;
    let id = id_of(&set(&a, &a.token, G, "github", &value("same")).await);
    let before = meta(&a, &a.token, &id).await;
    let r = move_item(&a, &a.token, &id, G).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json(),
        json!({"id": id, "rev": 1, "scope": "solution", "agent_solution_id": G})
    );
    assert_eq!(revs(&a, &a.token, &id).await, [1]);
    assert_eq!(meta(&a, &a.token, &id).await, before);
    assert!(move_audit(&a).await.is_empty());
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &format!("/api/v1/vault/{id}/move"),
            Some(json!({ "space": G })),
            &[("if-match", "7")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(revs(&a, &a.token, &id).await, [1]);
}

// ---- Vault для рантайма агента (M7.6 §9.3 п. 1, CR-21, признак `vault_agent`) ----

/// Подробности аудита `vault.reveal`.
async fn reveal_audit(a: &Api) -> Vec<serde_json::Value> {
    a.st.system
        .list_audit(100, None)
        .await
        .unwrap()
        .into_iter()
        .filter(|x| x.action == "vault.reveal")
        .map(|x| x.details)
        .collect()
}

/// `?reveal=1&purpose=<цель>` — цель в аудите `vault.reveal`; без цели — аудит прежний (поля
/// `purpose` нет); цель без `reveal` раскрытия и аудита не даёт.
#[tokio::test]
async fn vault_reveal_purpose_in_audit() {
    let a = api("vault-purpose", |_| {}).await;
    let v = value("purpose");
    let id = id_of(&set(&a, &a.token, "knowledge", "github", &v).await);
    let r = reveal(&a, &a.token, &id).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rows = reveal_audit(&a).await;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].get("purpose").is_none(), "{}", rows[0]);
    let purpose = "deploy:ci/run-1.2_A";
    let r = a
        .tok_h(
            &a.token,
            Method::GET,
            &format!("/api/v1/vault/{id}?reveal=1&purpose={purpose}"),
            None,
            &attrib_h(),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["secret"], v);
    let rows = reveal_audit(&a).await;
    assert_eq!(rows.len(), 2);
    let row = rows
        .iter()
        .find(|d| d.get("purpose").is_some())
        .expect("цель в аудите");
    assert_eq!(row["purpose"], purpose, "{row}");
    assert_eq!(row["item"], id.as_str());
    assert_eq!(row["session"], SESSION);
    // цель без раскрытия — метаданные, аудита нет
    let r = a
        .t(
            Method::GET,
            &format!("/api/v1/vault/{id}?purpose={purpose}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json().get("secret").is_none(), "{}", r.text());
    assert_eq!(reveal_audit(&a).await.len(), 2);
}

/// Цель вне формата (пустая, длиннее 64, недопустимые символы) — 400 без значения и аудита.
#[tokio::test]
async fn vault_reveal_purpose_invalid_400() {
    let a = api("vault-purpose-bad", |_| {}).await;
    let v = value("purpose-bad");
    let id = id_of(&set(&a, &a.token, "knowledge", "github", &v).await);
    let long = "a".repeat(65);
    let bad = [
        "",
        long.as_str(),
        "deploy%20ci",
        "%D1%86%D0%B5%D0%BB%D1%8C",
        "a%3Fb",
        "a%26b",
    ];
    for b in bad {
        let r = a
            .tok_h(
                &a.token,
                Method::GET,
                &format!("/api/v1/vault/{id}?reveal=1&purpose={b}"),
                None,
                &attrib_h(),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{b}: {}", r.text());
        assert!(!r.text().contains(&v), "{b}: значение в ответе");
        assert!(r.text().contains("purpose"), "{b}: {}", r.text());
    }
    assert!(reveal_audit(&a).await.is_empty());
    // 64 символа — можно
    let ok = "b".repeat(64);
    let r = a
        .t(
            Method::GET,
            &format!("/api/v1/vault/{id}?reveal=1&purpose={ok}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(reveal_audit(&a).await.len(), 1);
}

/// Элемент списка по id.
fn item_of(r: &Resp, id: &str) -> serde_json::Value {
    r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("нет {id}: {}", r.text()))
}

/// `has_value` у элементов списка и метаданных записи: значение есть — `true`; `host` и
/// `environment` без значения — `false`.
#[tokio::test]
async fn vault_list_has_value() {
    let a = api("vault-has-value", |_| {}).await;
    let tok = id_of(&set(&a, &a.token, "knowledge", "github", &value("hv")).await);
    let host = id_of(&post_env(&a, "box", "host", json!({"address": "192.0.2.10"}), None).await);
    let env_with = id_of(
        &post_env(
            &a,
            "stage",
            "environment",
            json!({"description": "стенд"}),
            Some(&value("env")),
        )
        .await,
    );
    let r = a
        .t(Method::GET, "/api/v1/spaces/knowledge/vault", None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    for (id, want) in [(&tok, true), (&host, false), (&env_with, true)] {
        assert_eq!(item_of(&r, id)["has_value"], want, "{}", r.text());
        let m = a.t(Method::GET, &format!("/api/v1/vault/{id}"), None).await;
        assert_eq!(m.status, StatusCode::OK, "{}", m.text());
        assert_eq!(m.json()["has_value"], want, "{}", m.text());
    }
}

/// Заблокированный Vault: `has_value` — по наличию шифротекста, без расшифровки (ключ подменён —
/// расшифровка невозможна, а ответ 200 с верным `has_value`).
#[tokio::test]
async fn vault_has_value_when_locked() {
    let dir = common::tmp("vault-has-value-locked");
    let c = common::cfg_in(dir.clone());
    let a = api_with(c.clone()).await;
    let id = id_of(&set(&a, &a.token, "knowledge", "github", &value("hvl")).await);
    let host = id_of(&post_env(&a, "box", "host", json!({"address": "192.0.2.11"}), None).await);
    let token = a.token.clone();
    a.st.shutdown().await;
    drop(a);
    std::fs::write(
        dir.join("master.key"),
        "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE=\n",
    )
    .unwrap();
    let b = api_with(c).await;
    let r = b
        .tok(&token, Method::GET, "/api/v1/spaces/knowledge/vault", None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["locked"], true, "{}", r.text());
    assert_eq!(item_of(&r, &id)["has_value"], true, "{}", r.text());
    assert_eq!(item_of(&r, &host)["has_value"], false, "{}", r.text());
    let m = b
        .tok(&token, Method::GET, &format!("/api/v1/vault/{id}"), None)
        .await;
    assert_eq!(m.status, StatusCode::OK, "{}", m.text());
    assert_eq!(m.json()["has_value"], true, "{}", m.text());
    assert!(!m.text().contains("\"secret\""), "{}", m.text());
}

async fn list_vault(a: &Api, q: &str) -> Resp {
    a.t(
        Method::GET,
        &format!("/api/v1/spaces/knowledge/vault?{q}"),
        None,
    )
    .await
}

/// `?title=<имя>` — точное имя (без пробелов по краям, регистр важен — как у удаления по имени).
#[tokio::test]
async fn vault_list_title_exact() {
    let a = api("vault-title-exact", |_| {}).await;
    for t in ["github", "github-2", "GitHub", "my github"] {
        id_of(&set(&a, &a.token, "knowledge", t, &value(t)).await);
    }
    let r = list_vault(&a, "title=github").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(titles(&r), ["github"]);
    assert_eq!(r.json()["locked"], false);
    assert_eq!(
        titles(&list_vault(&a, "title=%20github%20").await),
        ["github"]
    );
    assert_eq!(titles(&list_vault(&a, "title=GitHub").await), ["GitHub"]);
    assert_eq!(
        titles(&list_vault(&a, "title=my%20github").await),
        ["my github"]
    );
    assert!(titles(&list_vault(&a, "title=git").await).is_empty());
    // пустое имя — 400
    let r = list_vault(&a, "title=%20").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

/// `title` вместе с `q` — оба условия.
#[tokio::test]
async fn vault_list_title_with_q() {
    let a = api("vault-title-q", |_| {}).await;
    for t in ["github", "github-2"] {
        id_of(&set(&a, &a.token, "knowledge", t, &value(t)).await);
    }
    let both =
        |q: &str, t: &str| format!("q={}&title={}", common::api::enc(q), common::api::enc(t));
    let r = list_vault(&a, &both("деплой", "github")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(titles(&r), ["github"]);
    assert!(titles(&list_vault(&a, &both("нетакого", "github")).await).is_empty());
    assert!(titles(&list_vault(&a, &both("деплой", "gitlab")).await).is_empty());
    assert_eq!(
        titles(&list_vault(&a, &both("git", "github-2")).await),
        ["github-2"]
    );
}
