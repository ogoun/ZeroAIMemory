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

/// Байтовое вхождение `needle` в файлы каталога (рекурсивно).
fn files_containing(dir: &Path, needle: &[u8]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files_containing(&p, needle));
        } else if let Ok(b) = std::fs::read(&p)
            && b.windows(needle.len()).any(|w| w == needle)
        {
            out.push(p);
        }
    }
    out
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
    for (m, uri) in [
        (Method::GET, "/api/v1/spaces/knowledge/vault".to_string()),
        (Method::GET, format!("/api/v1/vault/{id}?reveal=1")),
        (Method::GET, format!("/api/v1/vault/{id}")),
    ] {
        let r = b.tok(&token, m, &uri, None).await;
        assert_eq!(
            r.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{uri}: {}",
            r.text()
        );
        let j = r.json();
        assert_eq!(j["type"], "/problems/vault-locked", "{j}");
        assert!(j["detail"].as_str().unwrap().contains("мастер-ключ"), "{j}");
    }
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
    let hits = files_containing(&dst, v.as_bytes());
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
    srv.kill();
    let logs = data.join("logs");
    assert!(
        std::fs::read_dir(&logs).unwrap().flatten().any(|e| e
            .file_name()
            .to_string_lossy()
            .starts_with("zam-server.log")),
        "журнал сервера в каталоге данных"
    );
    let hits = files_containing(&data, v.as_bytes());
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
