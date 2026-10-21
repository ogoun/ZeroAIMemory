//! Совместный доступ к решению (§11.6): регистрация, запрос, одобрение, отклонение, отзыв,
//! изоляция участника в пространстве решения базы владельца, `/admin/settings`, регистрация
//! решений M2 при старте.
//!
//! Пользователи: A — владелец решения, B — участник, C — посторонний; администратор — cookie.
mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::api::{Api, Resp, api, api_with};
use serde_json::{Value, json};
use zam_core::{Id, SolutionGuid};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e11";
const BOUNDARY: &str = "zam-access-boundary-3c9a";

struct W {
    a: Api,
    a_id: String,
    a_tok: String,
    b_id: String,
    b_tok: String,
    c_tok: String,
}

async fn world(name: &str) -> W {
    let a = api(&format!("access-{name}"), |_| {}).await;
    let (a_id, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let (_, c_tok) = a.new_user("Вера").await;
    W {
        a,
        a_id,
        a_tok,
        b_id,
        b_tok,
        c_tok,
    }
}

fn guid() -> SolutionGuid {
    G.parse().unwrap()
}

impl W {
    async fn req(&self, tok: &str, m: Method, uri: &str, body: Option<Value>) -> Resp {
        self.a.tok(tok, m, uri, body).await
    }
    async fn get(&self, tok: &str, uri: &str) -> Resp {
        self.req(tok, Method::GET, uri, None).await
    }
    async fn register(&self, tok: &str) -> Resp {
        self.req(
            tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({ "agent_solution_id": G, "name": "Общее" })),
        )
        .await
    }
    async fn decide(&self, tok: &str, user: &str, action: &str, body: Option<Value>) -> Resp {
        let uri = format!("/api/v1/solutions/{G}/access/{user}/{action}");
        self.req(tok, Method::POST, &uri, body).await
    }
    /// A регистрирует решение, B запрашивает доступ, A одобряет с `access`.
    async fn shared(&self, access: Option<&str>) {
        assert_eq!(self.register(&self.a_tok).await.status, StatusCode::CREATED);
        assert_eq!(
            self.register(&self.b_tok).await.status,
            StatusCode::ACCEPTED
        );
        let body = access.map(|a| json!({ "access": a }));
        let r = self.decide(&self.a_tok, &self.b_id, "approve", body).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    async fn note(&self, tok: &str, space: &str, path: &str, content: &str) -> Resp {
        self.req(
            tok,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": space, "path": path, "kind": "note", "content": content})),
        )
        .await
    }
    /// Заметка администратора в базе пользователя `user` (Rules и Human недоступны токену).
    async fn admin_note(&self, user: &str, space: &str, path: &str) -> Value {
        let r = self
            .a
            .adm(
                Method::POST,
                "/api/v1/nodes",
                Some(json!({"space": space, "path": path, "kind": "note", "content": "# x\n"})),
                Some(user),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        r.json()
    }
    async fn put(&self, tok: &str, id: &str, rev: i64, content: &str) -> Resp {
        let r = Request::put(format!("/api/v1/nodes/{id}"))
            .header(header::AUTHORIZATION, format!("Bearer {tok}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::IF_MATCH, format!("\"{rev}\""))
            .body(Body::from(json!({ "content": content }).to_string()))
            .unwrap();
        self.a.send(r).await
    }
    async fn upload(&self, tok: &str, node: &str, file: &str, data: &[u8]) -> Resp {
        let mut b = Vec::new();
        b.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"node\"\r\n\r\n{node}\r\n"
            )
            .as_bytes(),
        );
        b.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file}\"\r\n\
                 Content-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        b.extend_from_slice(data);
        b.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
        let r = Request::post("/api/v1/blobs")
            .header(header::AUTHORIZATION, format!("Bearer {tok}"))
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .body(Body::from(b))
            .unwrap();
        self.a.send(r).await
    }
    /// Id первого токена пользователя.
    async fn token_id(&self, user: &str) -> String {
        let r = self
            .a
            .adm(
                Method::GET,
                &format!("/api/v1/admin/users/{user}/tokens"),
                None,
                None,
            )
            .await;
        r.json()[0]["id"].as_str().unwrap().to_string()
    }
    async fn audit_actions(&self) -> Vec<Value> {
        let r = self
            .a
            .adm(Method::GET, "/api/v1/admin/audit?limit=500", None, None)
            .await;
        r.json()["items"].as_array().unwrap().clone()
    }
}

/// 403 `/problems/solution-access` с состоянием `state`.
#[track_caller]
fn denied(r: &Resp, state: &str) {
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let j = r.json();
    assert_eq!(j["type"], "/problems/solution-access", "{j}");
    assert_eq!(j["state"], state, "{j}");
    assert_eq!(j["agent_solution_id"], G, "{j}");
}

fn sid(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn register_new_guid_registers_and_creates_space() {
    let w = world("register").await;
    let r = w.register(&w.a_tok).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = r.json();
    assert_eq!(j["agent_solution_id"], G);
    assert_eq!(j["kind"], "solution");
    assert_eq!(j["role"], "owner");
    assert_eq!(j["owner"]["id"], w.a_id.as_str());
    assert!(j.get("solution_guid").is_none(), "{j}");
    let info = w.a.st.system.get_solution(&guid()).await.unwrap().unwrap();
    assert_eq!(info.owner_id.to_string(), w.a_id);
    assert_eq!(info.space_id.to_string(), sid(&j));
    // пространство в базе владельца — с тем же id, что в реестре
    let db = w.a.st.users.get(info.owner_id).await.unwrap();
    let sp = zam_store::get_solution(&db, G).await.unwrap().unwrap();
    assert_eq!(sp.id, info.space_id);
    let audit = w.audit_actions().await;
    assert!(
        audit
            .iter()
            .any(|e| e["action"] == "solution.register" && e["target"] == G),
        "{audit:?}"
    );
    // повтор владельцем — 200, старое имя поля `id` принимается
    let r = w
        .req(
            &w.a_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({ "id": G.to_uppercase(), "name": "Другое" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["id"], j["id"]);
    assert_eq!(r.json()["name"], "Общее");
    // GET /solutions/{guid}
    let r = w.get(&w.a_tok, &format!("/api/v1/solutions/{G}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let g = r.json();
    assert_eq!(g["id"], j["id"]);
    assert_eq!(g["role"], "owner");
    assert_eq!(g["owner"]["name"], "Анна");
    // незарегистрированный GUID — 404
    let r = w
        .get(
            &w.a_tok,
            "/api/v1/solutions/11111111-2222-3333-4444-555555555555",
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // ro-токен решение не регистрирует
    let ro = w.a.issue(&w.a_id, "ro").await;
    let r = w
        .req(
            &ro,
            Method::POST,
            "/api/v1/solutions",
            Some(
                json!({ "agent_solution_id": "21111111-2222-3333-4444-555555555555", "name": "x" }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn second_user_gets_pending_no_space_created() {
    let w = world("pending").await;
    assert_eq!(w.register(&w.a_tok).await.status, StatusCode::CREATED);
    for _ in 0..2 {
        let r = w.register(&w.b_tok).await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
        assert_eq!(
            r.json(),
            json!({ "agent_solution_id": G, "state": "pending" })
        );
    }
    let b: Id = w.b_id.parse().unwrap();
    let db = w.a.st.users.get(b).await.unwrap();
    assert!(zam_store::get_solution(&db, G).await.unwrap().is_none());
    denied(
        &w.get(&w.b_tok, &format!("/api/v1/spaces/{G}/tree")).await,
        "pending",
    );
    denied(
        &w.get(&w.b_tok, &format!("/api/v1/solutions/{G}")).await,
        "pending",
    );
    denied(&w.note(&w.b_tok, G, "x", "# x\n").await, "pending");
    // посторонний без запроса не видит решения
    let r = w.get(&w.c_tok, &format!("/api/v1/spaces/{G}/tree")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // владелец видит число ожидающих запросов
    let r = w.get(&w.a_tok, "/api/v1/spaces").await;
    assert_eq!(r.json()["access_requests_pending"], 1, "{}", r.text());
    let r = w.get(&w.b_tok, "/api/v1/spaces").await;
    assert_eq!(r.json()["access_requests_pending"], 0);
    assert!(
        !r.json()["spaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["agent_solution_id"] == G)
    );
}

#[tokio::test]
async fn owner_approves_member_reads_writes_with_attribution() {
    let w = world("approve").await;
    assert_eq!(w.register(&w.a_tok).await.status, StatusCode::CREATED);
    let n = w.note(&w.a_tok, G, "Архитектура", "# Арх\nтекст\n").await;
    assert_eq!(n.status, StatusCode::CREATED);
    let nid = sid(&n.json());
    assert_eq!(w.register(&w.b_tok).await.status, StatusCode::ACCEPTED);
    // без тела — режим по умолчанию rw
    let r = w.decide(&w.a_tok, &w.b_id, "approve", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["state"], "approved");
    assert_eq!(r.json()["access"], "rw");
    // регистрация участником — 200 с ролью
    let r = w.register(&w.b_tok).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["role"], "member");
    // дерево, узел по id и по пути
    let r = w.get(&w.b_tok, &format!("/api/v1/spaces/{G}/tree")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["items"][0]["name"], "Архитектура");
    let r = w.get(&w.b_tok, &format!("/api/v1/nodes/{nid}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = w
        .get(
            &w.b_tok,
            &format!("/api/v1/nodes/by-path?space={G}&path=Архитектура"),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // запись участника: ревизия в базе A с user_id и token_id B
    let r = w.put(&w.b_tok, &nid, 1, "# Арх\nправка B\n").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = w.note(&w.b_tok, G, "От Бориса", "# B\n").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let b_token = w.token_id(&w.b_id).await;
    let revs = w
        .get(&w.a_tok, &format!("/api/v1/nodes/{nid}/revisions"))
        .await
        .json();
    assert_eq!(revs[0]["rev"], 2);
    assert_eq!(revs[0]["user_id"], w.b_id.as_str(), "{revs}");
    assert_eq!(revs[0]["token_id"], b_token.as_str());
    assert_eq!(revs[1]["user_id"], w.a_id.as_str());
    let a: Id = w.a_id.parse().unwrap();
    let db = w.a.st.users.get(a).await.unwrap();
    let list = zam_store::list_revisions(&db, nid.parse().unwrap())
        .await
        .unwrap();
    assert_eq!(list[0].user_id.map(|u| u.to_string()), Some(w.b_id.clone()));
    // в личной базе B заметок решения нет
    let b: Id = w.b_id.parse().unwrap();
    let bdb = w.a.st.users.get(b).await.unwrap();
    assert!(zam_store::get_solution(&bdb, G).await.unwrap().is_none());
    // GET /spaces и GET /solutions участника
    let s = w.get(&w.b_tok, "/api/v1/spaces").await.json();
    let sp = s["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["agent_solution_id"] == G)
        .cloned()
        .expect("решение в /spaces участника");
    assert_eq!(sp["role"], "member");
    assert_eq!(sp["access"], "rw");
    assert_eq!(sp["owner"]["id"], w.a_id.as_str());
    let s = w.get(&w.a_tok, "/api/v1/spaces").await.json();
    let own = s["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["agent_solution_id"] == G)
        .cloned()
        .unwrap();
    assert_eq!(own["role"], "owner");
    assert_eq!(own["shared"], true);
    let l = w.get(&w.b_tok, "/api/v1/solutions").await.json();
    assert_eq!(l.as_array().unwrap().len(), 1, "{l}");
    assert_eq!(l[0]["agent_solution_id"], G);
    assert_eq!(l[0]["role"], "member");
    // аудит одобрения
    let audit = w.audit_actions().await;
    assert!(
        audit
            .iter()
            .any(|e| e["action"] == "solution.access.approve"
                && e["target"] == G
                && e["user_id"] == w.b_id.as_str()),
        "{audit:?}"
    );
}

#[tokio::test]
async fn ro_member_and_ro_token_cannot_write() {
    let w = world("ro").await;
    w.shared(Some("ro")).await;
    let n = w.note(&w.a_tok, G, "n", "# n\n").await.json();
    let nid = sid(&n);
    assert_eq!(
        w.get(&w.b_tok, &format!("/api/v1/nodes/{nid}"))
            .await
            .status,
        StatusCode::OK
    );
    let r = w.put(&w.b_tok, &nid, 1, "# правка\n").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert!(
        r.json()["detail"].as_str().unwrap().contains("ro"),
        "{}",
        r.text()
    );
    assert_eq!(
        w.note(&w.b_tok, G, "x", "# x\n").await.status,
        StatusCode::FORBIDDEN
    );
    // смена режима на rw — rw-токен пишет, ro-токен — нет
    let r = w
        .decide(&w.a_tok, &w.b_id, "approve", Some(json!({"access": "rw"})))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        w.put(&w.b_tok, &nid, 1, "# правка\n").await.status,
        StatusCode::OK
    );
    let ro = w.a.issue(&w.b_id, "ro").await;
    assert_eq!(
        w.get(&ro, &format!("/api/v1/nodes/{nid}")).await.status,
        StatusCode::OK
    );
    assert_eq!(
        w.put(&ro, &nid, 2, "# ro\n").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.note(&ro, G, "y", "# y\n").await.status,
        StatusCode::FORBIDDEN
    );
    let audit = w.audit_actions().await;
    assert!(audit.iter().any(|e| e["action"] == "solution.access.mode"));
}

#[tokio::test]
async fn revoke_takes_effect_on_next_request_same_token() {
    let w = world("revoke").await;
    w.shared(None).await;
    let n = w.note(&w.a_tok, G, "n", "# n\n").await.json();
    let nid = sid(&n);
    let up = w.upload(&w.a_tok, &nid, "a.bin", b"payload").await;
    assert_eq!(up.status, StatusCode::CREATED, "{}", up.text());
    let sha = up.json()["sha256"].as_str().unwrap().to_string();
    let blob = format!("/api/v1/blobs/{sha}/a.bin");
    assert_eq!(
        w.get(&w.b_tok, &format!("/api/v1/nodes/{nid}"))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(w.get(&w.b_tok, &blob).await.status, StatusCode::OK);
    let r = w.decide(&w.a_tok, &w.b_id, "revoke", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["state"], "revoked");
    // тот же токен, следующий запрос
    denied(
        &w.get(&w.b_tok, &format!("/api/v1/nodes/{nid}")).await,
        "revoked",
    );
    denied(
        &w.get(&w.b_tok, &format!("/api/v1/nodes/{nid}/revisions"))
            .await,
        "revoked",
    );
    denied(
        &w.get(&w.b_tok, &format!("/api/v1/spaces/{G}/tree")).await,
        "revoked",
    );
    denied(
        &w.get(&w.b_tok, &format!("/api/v1/nodes/by-path?space={G}&path=n"))
            .await,
        "revoked",
    );
    denied(&w.put(&w.b_tok, &nid, 1, "# x\n").await, "revoked");
    denied(&w.note(&w.b_tok, G, "x", "# x\n").await, "revoked");
    denied(&w.get(&w.b_tok, &blob).await, "revoked");
    denied(&w.upload(&w.b_tok, &nid, "b.bin", b"x").await, "revoked");
    denied(&w.register(&w.b_tok).await, "revoked");
    let s = w.get(&w.b_tok, "/api/v1/spaces").await.json();
    assert!(
        !s["spaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["agent_solution_id"] == G)
    );
    // повторный отзыв — недопустимый переход
    assert_eq!(
        w.decide(&w.a_tok, &w.b_id, "revoke", None).await.status,
        StatusCode::CONFLICT
    );
    // revoked → approved
    let r = w.decide(&w.a_tok, &w.b_id, "approve", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        w.get(&w.b_tok, &format!("/api/v1/nodes/{nid}"))
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn reject_then_repeat_request_stays_rejected() {
    let w = world("reject").await;
    assert_eq!(w.register(&w.a_tok).await.status, StatusCode::CREATED);
    assert_eq!(w.register(&w.b_tok).await.status, StatusCode::ACCEPTED);
    let r = w
        .decide(
            &w.a_tok,
            &w.b_id,
            "reject",
            Some(json!({"reason": "не сейчас"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["state"], "rejected");
    denied(&w.register(&w.b_tok).await, "rejected");
    denied(
        &w.get(&w.b_tok, &format!("/api/v1/spaces/{G}/tree")).await,
        "rejected",
    );
    // отклонение не-ожидающего — 409; некорректная причина — 400
    assert_eq!(
        w.decide(&w.a_tok, &w.b_id, "reject", None).await.status,
        StatusCode::CONFLICT
    );
    let r = w
        .decide(
            &w.a_tok,
            &w.b_id,
            "reject",
            Some(json!({"reason": "x".repeat(2000)})),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // rejected → approved
    let r = w.decide(&w.a_tok, &w.b_id, "approve", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        w.get(&w.b_tok, &format!("/api/v1/spaces/{G}/tree"))
            .await
            .status,
        StatusCode::OK
    );
    // отклонить одобренный нельзя
    assert_eq!(
        w.decide(&w.a_tok, &w.b_id, "reject", None).await.status,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn only_owner_rw_or_admin_manage_access() {
    let w = world("manage").await;
    assert_eq!(w.register(&w.a_tok).await.status, StatusCode::CREATED);
    assert_eq!(w.register(&w.b_tok).await.status, StatusCode::ACCEPTED);
    let access = format!("/api/v1/solutions/{G}/access");
    // посторонний и сам участник управлять не могут
    assert_eq!(w.get(&w.c_tok, &access).await.status, StatusCode::FORBIDDEN);
    assert_eq!(w.get(&w.b_tok, &access).await.status, StatusCode::FORBIDDEN);
    assert_eq!(
        w.decide(&w.c_tok, &w.b_id, "approve", None).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.decide(&w.b_tok, &w.b_id, "approve", None).await.status,
        StatusCode::FORBIDDEN
    );
    // ro-токен владельца читает список, но не решает
    let ro = w.a.issue(&w.a_id, "ro").await;
    let r = w.get(&ro, &access).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["owner"]["id"], w.a_id.as_str());
    assert_eq!(j["pending"], 1);
    assert_eq!(j["members"][0]["user_id"], w.b_id.as_str());
    assert_eq!(j["members"][0]["user_name"], "Борис");
    assert_eq!(j["members"][0]["state"], "pending");
    assert_eq!(
        w.decide(&ro, &w.b_id, "approve", None).await.status,
        StatusCode::FORBIDDEN
    );
    // недопустимые переходы и неизвестные — 409/404/400
    assert_eq!(
        w.decide(&w.a_tok, &w.b_id, "revoke", None).await.status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        w.decide(&w.a_tok, &Id::new().to_string(), "approve", None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        w.decide(
            &w.a_tok,
            &w.b_id,
            "approve",
            Some(json!({"access": "admin"}))
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    let r = w
        .req(
            &w.a_tok,
            Method::GET,
            "/api/v1/solutions/21111111-2222-3333-4444-555555555555/access",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // владелец своим rw-токеном
    let r = w
        .decide(&w.a_tok, &w.b_id, "approve", Some(json!({"access": "ro"})))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["access"], "ro");
    let j = w.get(&w.a_tok, &access).await.json();
    assert_eq!(j["pending"], 0);
    assert_eq!(j["members"][0]["decided_by"], w.a_id.as_str());
}

#[tokio::test]
async fn member_confined_to_solution_space() {
    let w = world("confined").await;
    w.shared(None).await;
    let sol = w.note(&w.a_tok, G, "Решение", "# Р\n").await.json();
    let k = w
        .note(&w.a_tok, "knowledge", "Личное", "# секрет\n")
        .await
        .json();
    let rules = w.admin_note(&w.a_id, "rules", "Правило").await;
    let human = w.admin_note(&w.a_id, "human", "Дневник").await;
    for n in [&k, &rules, &human] {
        let id = sid(n);
        for uri in [
            format!("/api/v1/nodes/{id}"),
            format!("/api/v1/nodes/{id}/revisions"),
            format!("/api/v1/nodes/{id}/outline"),
            format!("/api/v1/nodes/{id}/diff"),
        ] {
            let r = w.get(&w.b_tok, &uri).await;
            assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}: {}", r.text());
        }
        assert_eq!(
            w.put(&w.b_tok, &id, 1, "# взлом\n").await.status,
            StatusCode::NOT_FOUND
        );
        let r = w
            .req(
                &w.b_tok,
                Method::DELETE,
                &format!("/api/v1/nodes/{id}"),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
        let r = w
            .req(
                &w.b_tok,
                Method::POST,
                "/api/v1/render",
                Some(json!({ "md": "x", "base_node": id })),
            )
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
        assert_eq!(
            w.upload(&w.b_tok, &id, "x.bin", b"x").await.status,
            StatusCode::NOT_FOUND
        );
    }
    // пространства владельца по id: Knowledge — 404, решение — доступно
    let a: Id = w.a_id.parse().unwrap();
    let db = w.a.st.users.get(a).await.unwrap();
    let a_spaces = zam_store::list_spaces(&db).await.unwrap();
    for s in &a_spaces {
        let uri = format!("/api/v1/spaces/{}/tree", s.id);
        let r = w.get(&w.b_tok, &uri).await;
        let want = if s.kind == zam_store::SpaceKind::Solution {
            StatusCode::OK
        } else {
            StatusCode::NOT_FOUND
        };
        assert_eq!(r.status, want, "{:?}: {}", s.kind, r.text());
        let r = w
            .note(&w.b_tok, &s.id.to_string(), "через-id", "# x\n")
            .await;
        let want = if s.kind == zam_store::SpaceKind::Solution {
            StatusCode::CREATED
        } else {
            StatusCode::NOT_FOUND
        };
        assert_eq!(r.status, want, "{:?}: {}", s.kind, r.text());
    }
    // вложения: Knowledge владельца — 404, решения — 200
    let kb = w
        .upload(&w.a_tok, &sid(&k), "k.bin", b"knowledge-secret")
        .await;
    assert_eq!(kb.status, StatusCode::CREATED);
    let ksha = kb.json()["sha256"].as_str().unwrap().to_string();
    let r = w
        .get(&w.b_tok, &format!("/api/v1/blobs/{ksha}/k.bin"))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let sb = w
        .upload(&w.b_tok, &sid(&sol), "s.bin", b"solution-data")
        .await;
    assert_eq!(sb.status, StatusCode::CREATED, "{}", sb.text());
    let ssha = sb.json()["sha256"].as_str().unwrap().to_string();
    let r = w
        .get(&w.b_tok, &format!("/api/v1/blobs/{ssha}/s.bin"))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(&r.body[..], b"solution-data");
    // файл — в каталоге блобов владельца, владелец его тоже читает
    let r = w
        .get(&w.a_tok, &format!("/api/v1/blobs/{ssha}/s.bin"))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    // ссылка из заметки решения на вложение Knowledge владельца не разрешается участнику
    let link = format!("![k](_attachments/{}/k.bin)\n", &ksha[..16]);
    let n = w.note(&w.a_tok, G, "Ссылка", &link).await.json();
    let r = w
        .get(&w.b_tok, &format!("/api/v1/nodes/{}?format=html", sid(&n)))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!r.text().contains(&ksha), "{}", r.text());
    // а вики-ссылка на заметку Knowledge владельца — битая
    let n = w.note(&w.a_tok, G, "Вики", "[[Личное]]\n").await.json();
    let r = w
        .get(&w.b_tok, &format!("/api/v1/nodes/{}?format=html", sid(&n)))
        .await;
    assert!(!r.text().contains(&sid(&k)), "{}", r.text());
    // корзина без space — только своё
    let r = w.get(&w.b_tok, "/api/v1/trash").await;
    assert_eq!(r.status, StatusCode::OK);
    // own knowledge участника — его личная база
    let r = w.note(&w.b_tok, "knowledge", "Своё", "# своё\n").await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = w
        .get(&w.a_tok, &format!("/api/v1/nodes/{}", sid(&r.json())))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn node_lookup_in_owner_db_without_header() {
    let w = world("lookup").await;
    w.shared(None).await;
    let n = w
        .note(&w.a_tok, G, "n", "# Заголовок\nтекст\n")
        .await
        .json();
    let nid = sid(&n);
    for uri in [
        format!("/api/v1/nodes/{nid}"),
        format!("/api/v1/nodes/{nid}?format=md"),
        format!("/api/v1/nodes/{nid}?format=html"),
        format!("/api/v1/nodes/{nid}/outline"),
        format!("/api/v1/nodes/{nid}/revisions"),
        format!("/api/v1/nodes/{nid}/revisions/1"),
        format!("/api/v1/nodes/{nid}/diff"),
        format!("/api/v1/trash?space={G}"),
    ] {
        let r = w.get(&w.b_tok, &uri).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
    }
    // перенос, удаление, восстановление и откат — в базе владельца
    let r = w
        .req(
            &w.b_tok,
            Method::POST,
            &format!("/api/v1/nodes/{nid}/move"),
            Some(json!({"path": "n2"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = w
        .req(
            &w.b_tok,
            Method::DELETE,
            &format!("/api/v1/nodes/{nid}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = w
        .req(
            &w.b_tok,
            Method::POST,
            &format!("/api/v1/nodes/{nid}/restore"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        w.put(&w.b_tok, &nid, 1, "# v2\n").await.status,
        StatusCode::OK
    );
    let r = w
        .req(
            &w.b_tok,
            Method::POST,
            &format!("/api/v1/nodes/{nid}/revert"),
            Some(json!({"rev": 1})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = w.get(&w.a_tok, &format!("/api/v1/nodes/{nid}")).await;
    assert_eq!(r.json()["node"]["path"], "n2");
    assert_eq!(r.json()["rev"], 3);
}

#[tokio::test]
async fn admin_sees_and_manages_all() {
    let w = world("admin").await;
    assert_eq!(w.register(&w.a_tok).await.status, StatusCode::CREATED);
    assert_eq!(w.register(&w.b_tok).await.status, StatusCode::ACCEPTED);
    let r = w.a.adm(Method::GET, "/api/v1/solutions", None, None).await;
    assert_eq!(r.status, StatusCode::OK);
    let l = r.json();
    assert!(
        l.as_array()
            .unwrap()
            .iter()
            .any(|s| s["agent_solution_id"] == G && s["owner"]["id"] == w.a_id.as_str()),
        "{l}"
    );
    let r =
        w.a.adm(
            Method::GET,
            &format!("/api/v1/solutions/{G}/access"),
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["pending"], 1);
    // дерево и запись в чужое решение без X-Zam-User
    let r =
        w.a.adm(Method::GET, &format!("/api/v1/spaces/{G}/tree"), None, None)
            .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r =
        w.a.adm(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": G, "path": "От админа", "kind": "note", "content": "# a\n"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r =
        w.a.adm(
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{}/approve", w.b_id),
            Some(json!({"access": "ro"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r =
        w.a.adm(
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{}/revoke", w.b_id),
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = w
        .get(&w.a_tok, &format!("/api/v1/solutions/{G}/access"))
        .await
        .json();
    assert_eq!(j["members"][0]["state"], "revoked");
    assert_eq!(j["members"][0]["decided_by"], "admin");
}

#[tokio::test]
async fn settings_rules_agent_write_toggle() {
    let w = world("settings").await;
    let r =
        w.a.adm(Method::GET, "/api/v1/admin/settings", None, None)
            .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["rules"]["agent_write"], false);
    assert_eq!(
        w.get(&w.a_tok, "/api/v1/admin/settings").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.note(&w.a_tok, "rules", "R", "# r\n").await.status,
        StatusCode::FORBIDDEN
    );
    for bad in [
        json!({"rules": {"agent_write": "yes"}}),
        json!({}),
        json!({"x": 1}),
    ] {
        let r =
            w.a.adm(
                Method::PATCH,
                "/api/v1/admin/settings",
                Some(bad.clone()),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text());
    }
    let r =
        w.a.adm(
            Method::PATCH,
            "/api/v1/admin/settings",
            Some(json!({"rules": {"agent_write": true}})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["rules"]["agent_write"], true);
    assert_eq!(
        w.note(&w.a_tok, "rules", "R", "# r\n").await.status,
        StatusCode::CREATED
    );
    let audit = w.audit_actions().await;
    let e = audit
        .iter()
        .find(|e| e["action"] == "settings.update")
        .expect("аудит settings.update");
    assert_eq!(e["details"]["key"], "rules.agent_write");
    assert_eq!(e["details"]["to"], true);
    // выключение — запись агента снова запрещена
    let r =
        w.a.adm(
            Method::PATCH,
            "/api/v1/admin/settings",
            Some(json!({"rules": {"agent_write": false}})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        w.note(&w.a_tok, "rules", "R2", "# r\n").await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn startup_registers_m2_solution() {
    let dir = common::tmp("access-startup");
    let c = common::cfg_in(dir.clone());
    let a = api_with(c.clone()).await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let b: Id = b_id.parse().unwrap();
    // решение M2: только пространство в базе, реестра нет
    let space = {
        let db = a.st.users.get(b).await.unwrap();
        let (s, created) = zam_store::create_solution(&db, G, "Старое").await.unwrap();
        assert!(created);
        s
    };
    assert!(a.st.system.get_solution(&guid()).await.unwrap().is_none());
    a.st.shutdown().await;
    drop(a);

    let a = api_with(c).await;
    let info = a.st.system.get_solution(&guid()).await.unwrap().unwrap();
    assert_eq!(info.owner_id, b);
    assert_eq!(info.space_id, space.id);
    let r = a.tok(&b_tok, Method::GET, "/api/v1/spaces", None).await;
    let s = r.json()["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["agent_solution_id"] == G)
        .cloned()
        .expect("решение M2 в /spaces");
    assert_eq!(s["role"], "owner");
    assert_eq!(s["shared"], false);
    a.st.shutdown().await;
}
