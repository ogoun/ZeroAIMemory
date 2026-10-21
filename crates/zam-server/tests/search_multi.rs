//! `POST /search` по личной базе и базам владельцев доступных решений одним запросом
//! (задача 5 M3, §9.1–9.2, §11.6): FTS по каждой базе со слиянием каналов, один векторный
//! запрос по областям. Граница изоляции: участник видит в базе владельца только
//! пространство решения — Knowledge, Rules и Human владельца не попадают ни в FTS, ни
//! в векторный канал, ни в `expand`, ни в `pending_count`.
//!
//! Пользователи: A — владелец решения, B — участник, C — запрос ждёт (или отклонён).
//! Тесты с векторным каналом — MiniLM на CPU-сборке ORT и настоящий Qdrant; без них
//! пропускаются (в CI — падают).
mod common;

use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api, api_with};
use common::ml::{MINILM, external, have, ml, until, vector_ready};
use common::qdrant::TestQdrant;
use serde_json::{Value, json};
use zam_core::Id;
use zam_server::config::ServerConfig;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e11";
/// Слово, которое есть во всех пространствах A и в Knowledge B.
const WORD: &str = "квокказавр";

struct W {
    a: Api,
    a_id: String,
    a_tok: String,
    b_id: String,
    b_tok: String,
    c_id: String,
    c_tok: String,
}

async fn world_with(name: &str, tweak: impl FnOnce(&mut ServerConfig)) -> W {
    let a = api(&format!("multi-{name}"), tweak).await;
    let (a_id, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let (c_id, c_tok) = a.new_user("Вера").await;
    W {
        a,
        a_id,
        a_tok,
        b_id,
        b_tok,
        c_id,
        c_tok,
    }
}

async fn world(name: &str) -> W {
    world_with(name, |_| {}).await
}

fn sid(r: &Resp) -> String {
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

fn ids(j: &Value) -> Vec<String> {
    j["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{j}"))
        .iter()
        .map(|i| i["node_id"].as_str().unwrap().to_string())
        .collect()
}

fn item<'a>(j: &'a Value, node: &str) -> &'a Value {
    j["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["node_id"] == node)
        .unwrap_or_else(|| panic!("нет {node}: {j}"))
}

impl W {
    async fn req(&self, tok: &str, m: Method, uri: &str, body: Option<Value>) -> Resp {
        self.a.tok(tok, m, uri, body).await
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
    async fn decide(&self, user: &str, action: &str) -> Resp {
        let uri = format!("/api/v1/solutions/{G}/access/{user}/{action}");
        self.req(&self.a_tok, Method::POST, &uri, None).await
    }
    /// A регистрирует решение; B получает доступ, запрос C ждёт.
    async fn shared(&self) {
        assert_eq!(self.register(&self.a_tok).await.status, StatusCode::CREATED);
        for t in [&self.b_tok, &self.c_tok] {
            assert_eq!(self.register(t).await.status, StatusCode::ACCEPTED);
        }
        let r = self.decide(&self.b_id, "approve").await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    async fn note(&self, tok: &str, space: &str, path: &str, content: &str) -> String {
        sid(&self
            .req(
                tok,
                Method::POST,
                "/api/v1/nodes",
                Some(json!({"space": space, "path": path, "kind": "note", "content": content})),
            )
            .await)
    }
    /// Заметка администратора в базе пользователя `user` (Rules и Human токену недоступны).
    async fn admin_note(&self, user: &str, space: &str, path: &str, content: &str) -> String {
        sid(&self
            .a
            .adm(
                Method::POST,
                "/api/v1/nodes",
                Some(json!({"space": space, "path": path, "kind": "note", "content": content})),
                Some(user),
            )
            .await)
    }
    async fn search_h(&self, tok: &str, body: Value, extra: &[(&str, &str)]) -> Value {
        let r = self
            .a
            .tok_h(tok, Method::POST, "/api/v1/search", Some(body), extra)
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        r.json()
    }
    async fn search(&self, tok: &str, body: Value) -> Value {
        self.search_h(tok, body, &[]).await
    }
}

/// Узлы мира: одно и то же слово во всех пространствах A и в Knowledge B.
struct Nodes {
    a_knowledge: String,
    a_rules: String,
    a_human: String,
    a_solution: String,
    b_knowledge: String,
}

async fn corpus(w: &W) -> Nodes {
    let body = |t: &str| format!("# {t}\n\nЗаметка про {WORD} и его повадки.\n");
    Nodes {
        a_knowledge: w
            .note(&w.a_tok, "knowledge", "proj/знания", &body("Знания A"))
            .await,
        a_rules: w
            .admin_note(&w.a_id, "rules", "proj/правила", &body("Правила A"))
            .await,
        a_human: w
            .admin_note(&w.a_id, "human", "proj/дневник", &body("Дневник A"))
            .await,
        a_solution: w.note(&w.a_tok, G, "proj/решение", &body("Решение")).await,
        b_knowledge: w
            .note(&w.b_tok, "knowledge", "proj/свои", &body("Знания B"))
            .await,
    }
}

/// Узлы владельца вне решения не встречаются в выдаче.
#[track_caller]
fn no_owner_private(j: &Value, n: &Nodes) {
    let got = ids(j);
    for x in [&n.a_knowledge, &n.a_rules, &n.a_human] {
        assert!(!got.contains(x), "узел владельца вне решения в выдаче: {j}");
    }
}

#[tokio::test]
async fn member_search_finds_owner_solution_and_own_knowledge() {
    let w = world("finds").await;
    w.shared().await;
    let n = corpus(&w).await;
    // решение из scope.solution и из заголовка агента — одинаково
    let by_scope = w
        .search(&w.b_tok, json!({"query": WORD, "scope": {"solution": G}}))
        .await;
    let by_header = w
        .search_h(&w.b_tok, json!({"query": WORD}), &[("x-zam-solution", G)])
        .await;
    for j in [&by_scope, &by_header] {
        let mut got = ids(j);
        got.sort();
        let mut want = vec![n.a_solution.clone(), n.b_knowledge.clone()];
        want.sort();
        assert_eq!(got, want, "{j}");
        assert_eq!(j["warnings"], json!([]), "{j}");
        let it = item(j, &n.a_solution);
        assert_eq!(it["agent_solution_id"], G, "{it}");
        assert_eq!(it["space"]["kind"], "solution", "{it}");
        assert_eq!(it["space"]["agent_solution_id"], G, "{it}");
        // элемент личной базы — без agent_solution_id
        let own = item(j, &n.b_knowledge);
        assert!(own.get("agent_solution_id").is_none(), "{own}");
        assert_eq!(own["space"]["kind"], "knowledge");
    }
    // all_solutions — все доступные, в том числе чужое решение
    let j = w
        .search(
            &w.b_tok,
            json!({"query": WORD, "scope": {"all_solutions": true, "include_knowledge": false}}),
        )
        .await;
    assert_eq!(ids(&j), vec![n.a_solution.clone()], "{j}");
    // явный список пространств: GUID чужого решения
    let j = w
        .search(&w.b_tok, json!({"query": WORD, "scope": {"spaces": [G]}}))
        .await;
    assert_eq!(ids(&j), vec![n.a_solution.clone()], "{j}");
    // формат md: карточка чужого решения с именем его пространства
    let r =
        w.a.tok(
            &w.b_tok,
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": WORD, "scope": {"solution": G}, "format": "md"})),
        )
        .await;
    let t = r.text();
    assert!(
        t.contains(&format!("n:{} Общее/proj/решение", n.a_solution)),
        "{t}"
    );
    // pending_count: Knowledge и Rules B и решение A, но не Knowledge владельца
    assert_eq!(by_scope["pending_count"], 2, "{by_scope}");
    // владелец по умолчанию (без решения) решение не ищет, а с ним — видит своё
    let j = w.search(&w.a_tok, json!({"query": WORD})).await;
    assert!(ids(&j).contains(&n.a_knowledge), "{j}");
    assert!(!ids(&j).contains(&n.a_solution), "{j}");
    assert!(!ids(&j).contains(&n.b_knowledge), "{j}");
}

#[tokio::test]
async fn member_search_never_returns_owner_knowledge_fts() {
    let w = world("never-fts").await;
    w.shared().await;
    let n = corpus(&w).await;
    let a_spaces = {
        let a: Id = w.a_id.parse().unwrap();
        let db = w.a.st.users.get(a).await.unwrap();
        zam_store::list_spaces(&db).await.unwrap()
    };
    let id_of = |k: zam_store::SpaceKind| {
        a_spaces
            .iter()
            .find(|s| s.kind == k)
            .unwrap()
            .id
            .to_string()
    };
    for scope in [
        json!({}),
        json!({"solution": G}),
        json!({"all_solutions": true, "include_human": true}),
        json!({"solution": G, "path_prefix": "proj"}),
        // явные пространства владельца по id и формы фиксированных пространств
        json!({"spaces": [id_of(zam_store::SpaceKind::Knowledge),
                          id_of(zam_store::SpaceKind::Rules),
                          id_of(zam_store::SpaceKind::Human), G]}),
        json!({"spaces": ["knowledge", "rules", "human", G], "include_human": true}),
    ] {
        for mode in ["brief", "chunks", "notes"] {
            let body = json!({"query": WORD, "scope": scope, "mode": mode, "expand": "none"});
            let j = w.search(&w.b_tok, body).await;
            no_owner_private(&j, &n);
        }
    }
    // явные пространства владельца без решения — пустая выдача и pending_count 0
    let j = w
        .search(
            &w.b_tok,
            json!({"query": WORD, "scope": {"spaces": [id_of(zam_store::SpaceKind::Knowledge)]}}),
        )
        .await;
    assert!(ids(&j).is_empty(), "{j}");
    assert_eq!(j["pending_count"], 0, "{j}");
    // посторонний (запрос ждёт) не видит ничего из базы A
    let j = w
        .search(&w.c_tok, json!({"query": WORD, "scope": {"solution": G}}))
        .await;
    assert!(ids(&j).is_empty(), "{j}");
}

#[tokio::test]
async fn revoked_solution_excluded_with_warning() {
    let w = world("revoked").await;
    w.shared().await;
    let n = corpus(&w).await;
    let q = json!({"query": WORD, "scope": {"solution": G}});
    let j = w.search(&w.b_tok, q.clone()).await;
    assert!(ids(&j).contains(&n.a_solution), "{j}");
    // отзыв действует на следующий запрос того же токена
    let r = w.decide(&w.b_id, "revoke").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    for j in [
        w.search(&w.b_tok, q.clone()).await,
        w.search_h(&w.b_tok, json!({"query": WORD}), &[("x-zam-solution", G)])
            .await,
    ] {
        assert_eq!(ids(&j), vec![n.b_knowledge.clone()], "{j}");
        assert_eq!(j["warnings"], json!(["solution access revoked"]), "{j}");
        // pending_count — без решения
        assert_eq!(j["pending_count"], 1, "{j}");
    }
    // явный список из одного решения — пустая выдача с предупреждением
    let j = w
        .search(&w.b_tok, json!({"query": WORD, "scope": {"spaces": [G]}}))
        .await;
    assert!(ids(&j).is_empty(), "{j}");
    assert_eq!(j["warnings"], json!(["solution access revoked"]), "{j}");
    assert_eq!(j["pending_count"], 0, "{j}");
    // all_solutions молча не включает отозванное
    let j = w
        .search(
            &w.b_tok,
            json!({"query": WORD, "scope": {"all_solutions": true}}),
        )
        .await;
    assert_eq!(ids(&j), vec![n.b_knowledge.clone()], "{j}");
    // ожидание и отклонение
    let j = w.search(&w.c_tok, q.clone()).await;
    assert!(ids(&j).is_empty(), "{j}");
    assert_eq!(j["warnings"], json!(["solution access pending"]), "{j}");
    let r = w.decide(&w.c_id, "reject").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = w.search(&w.c_tok, q.clone()).await;
    assert_eq!(j["warnings"], json!(["solution access rejected"]), "{j}");
    // md: предупреждение строкой
    let r =
        w.a.tok(
            &w.c_tok,
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": WORD, "scope": {"solution": G}, "format": "md"})),
        )
        .await;
    assert!(
        r.text().contains("> warning: solution access rejected"),
        "{}",
        r.text()
    );
    // повторное одобрение — снова в выдаче, без предупреждения
    let r = w.decide(&w.b_id, "approve").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = w.search(&w.b_tok, q).await;
    assert!(ids(&j).contains(&n.a_solution), "{j}");
    assert_eq!(j["warnings"], json!([]), "{j}");
}

#[tokio::test]
async fn path_prefix_within_solution_only() {
    let w = world("prefix").await;
    w.shared().await;
    let n = corpus(&w).await;
    let deep = w
        .note(
            &w.a_tok,
            G,
            "proj/sub/глубже",
            &format!("# Глубже\n\n{WORD} в подпапке.\n"),
        )
        .await;
    let other = w
        .note(
            &w.a_tok,
            G,
            "другое/рядом",
            &format!("# Рядом\n\n{WORD} вне префикса.\n"),
        )
        .await;
    let j = w
        .search(
            &w.b_tok,
            json!({"query": WORD, "scope": {"solution": G, "path_prefix": "proj/sub"}}),
        )
        .await;
    assert_eq!(ids(&j), vec![deep.clone()], "{j}");
    let j = w
        .search(
            &w.b_tok,
            json!({"query": WORD, "scope": {"solution": G, "path_prefix": "proj"}}),
        )
        .await;
    let got = ids(&j);
    assert!(
        got.contains(&n.a_solution) && got.contains(&deep) && got.contains(&n.b_knowledge),
        "{j}"
    );
    assert!(!got.contains(&other), "{j}");
    no_owner_private(&j, &n);
    // pending_count под префиксом: решение (2) и B (1)
    assert_eq!(j["pending_count"], 3, "{j}");
}

#[tokio::test]
async fn degraded_fts_only_multi_db() {
    let w = world("degraded").await;
    w.shared().await;
    let n = corpus(&w).await;
    // trigram-канал: идентификатор в обеих базах
    let ident = "build_cfg.v42";
    let a_code = w
        .note(
            &w.a_tok,
            G,
            "код/сборка",
            &format!("# Сборка\n\nКлюч {ident} задаёт сборку.\n"),
        )
        .await;
    let b_code = w
        .note(
            &w.b_tok,
            "knowledge",
            "код/свой",
            &format!("# Свой\n\nСвой {ident} для проверки.\n"),
        )
        .await;
    let j = w
        .search(&w.b_tok, json!({"query": WORD, "scope": {"solution": G}}))
        .await;
    assert_eq!(j["degraded"], true, "{j}");
    let reason = j["degraded_reason"].as_str().unwrap();
    assert!(["no_runtime", "no_model"].contains(&reason), "{j}");
    assert!(j["timings"]["vector_ms"].is_null(), "{j}");
    for x in [&n.a_solution, &n.b_knowledge] {
        assert_eq!(item(&j, x)["sources"], json!(["fts"]), "{j}");
    }
    // баллы: первое место канала — 1,0, второе ниже
    let scores: Vec<f64> = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["score"].as_f64().unwrap())
        .collect();
    assert_eq!(scores.len(), 2, "{j}");
    assert!(
        (scores[0] - 1.0).abs() < 1e-9 && scores[1] < scores[0],
        "{j}"
    );
    let j = w
        .search(&w.b_tok, json!({"query": ident, "scope": {"solution": G}}))
        .await;
    for x in [&a_code, &b_code] {
        let src = item(&j, x)["sources"].as_array().unwrap().clone();
        assert!(src.contains(&json!("trigram")), "{j}");
    }
    no_owner_private(&j, &n);
}

#[tokio::test]
async fn expand_section_and_neighbors_in_owner_db() {
    let w = world("expand").await;
    w.shared().await;
    let filler = |m: &str| {
        (0..120)
            .map(|i| format!("{m} слово{i}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let content = format!(
        "# Решение\n\nВступление.\n\n## Детали\n\nАЛЬФАМЕТКА начало. {}\n\n\
         БЕТАМЕТКА середина. {}\n\nГАММАМЕТКА конец. {}\n\n## Другое\n\nПро другое.\n",
        filler("первый"),
        filler("второй"),
        filler("третий")
    );
    let node = w.note(&w.a_tok, G, "арх/детали", &content).await;
    let q = |extra: Value| {
        let mut b = json!({"query": "БЕТАМЕТКА", "scope": {"solution": G}, "mode": "chunks",
                           "top_k": 1, "budget_tokens": 5000});
        for (k, v) in extra.as_object().unwrap() {
            b[k] = v.clone();
        }
        b
    };
    let plain = w.search(&w.b_tok, q(json!({}))).await;
    assert_eq!(ids(&plain), vec![node.clone()], "{plain}");
    let leaf = plain["items"][0]["text"].as_str().unwrap().to_string();
    assert!(leaf.contains("БЕТАМЕТКА"), "{plain}");
    let j = w.search(&w.b_tok, q(json!({"expand": "section"}))).await;
    let sec = j["items"][0]["text"].as_str().unwrap();
    assert!(sec.starts_with("## Детали"), "{sec}");
    for m in ["АЛЬФАМЕТКА", "БЕТАМЕТКА", "ГАММАМЕТКА"] {
        assert!(sec.contains(m), "{m}: {sec}");
    }
    assert!(!sec.contains("Про другое"), "{sec}");
    let j = w.search(&w.b_tok, q(json!({"expand": "neighbors"}))).await;
    let nb = j["items"][0]["text"].as_str().unwrap();
    assert!(nb.contains("БЕТАМЕТКА") && nb.len() > leaf.len(), "{nb}");
    // notes: узел и L0 читаются из базы владельца (в личной базе B узла нет — было бы 500)
    let j = w
        .search(
            &w.b_tok,
            json!({"query": "БЕТАМЕТКА", "scope": {"solution": G}, "mode": "notes"}),
        )
        .await;
    assert_eq!(j["items"][0]["node_id"], node.as_str(), "{j}");
    assert!(j["items"][0].get("l0").is_some(), "{j}");
}

/// База владельца испорчена до первого обращения (перезапуск на том же каталоге данных):
/// участник получает 200 по личной базе и `solution unavailable`, без 500.
#[tokio::test]
async fn broken_owner_db_excluded_with_warning() {
    let dir = common::tmp("multi-broken");
    let c = common::cfg_in(dir.clone());
    let w = {
        let a = api_with(c.clone()).await;
        let (a_id, a_tok) = a.new_user("Анна").await;
        let (b_id, b_tok) = a.new_user("Борис").await;
        let (c_id, c_tok) = a.new_user("Вера").await;
        W {
            a,
            a_id,
            a_tok,
            b_id,
            b_tok,
            c_id,
            c_tok,
        }
    };
    w.shared().await;
    let n = corpus(&w).await;
    let (a_id, b_tok) = (w.a_id.clone(), w.b_tok.clone());
    w.a.st.shutdown().await;
    drop(w);
    // заголовок файла базы A перезаписан мусором, WAL и SHM убраны
    let users = dir.join("users");
    for ext in ["db-wal", "db-shm"] {
        let _ = std::fs::remove_file(users.join(format!("{a_id}.{ext}")));
    }
    std::fs::write(
        users.join(format!("{a_id}.db")),
        b"not a sqlite database at all",
    )
    .unwrap();

    let a = api_with(c).await;
    for (scope, mode) in [
        (json!({"solution": G}), "brief"),
        (json!({"solution": G}), "chunks"),
        (json!({"all_solutions": true}), "notes"),
        (json!({"spaces": [G, "knowledge"]}), "brief"),
    ] {
        let r = a
            .tok(
                &b_tok,
                Method::POST,
                "/api/v1/search",
                Some(json!({"query": WORD, "scope": scope, "mode": mode, "expand": "section"})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{scope} {mode}: {}", r.text());
        let j = r.json();
        assert_eq!(ids(&j), vec![n.b_knowledge.clone()], "{scope} {mode}: {j}");
        assert_eq!(j["warnings"], json!(["solution unavailable"]), "{j}");
        assert_eq!(j["pending_count"], 1, "{j}");
    }
    a.st.shutdown().await;
}

/// Поздний сбой базы владельца: FTS читается, а ревизии — нет (таблица `revisions`
/// переименована), `expand=section` падает уже после FTS. Участник получает 200 без
/// элементов решения и `solution unavailable`; владельцу (личная база) — ошибка, как в M2.
#[tokio::test]
async fn late_owner_db_failure_excluded_with_warning() {
    let w = world("late").await;
    w.shared().await;
    let n = corpus(&w).await;
    let body = json!({"query": WORD, "scope": {"solution": G}, "mode": "chunks",
                      "expand": "section", "budget_tokens": 5000});
    // контроль: до порчи решение в выдаче
    let j = w.search(&w.b_tok, body.clone()).await;
    assert!(ids(&j).contains(&n.a_solution), "{j}");
    let a: Id = w.a_id.parse().unwrap();
    let db = w.a.st.users.get(a).await.unwrap();
    db.write(|tx| {
        tx.execute_batch("ALTER TABLE revisions RENAME TO revisions_broken")?;
        Ok(())
    })
    .await
    .unwrap();
    // FTS по базе A по-прежнему работает (без expand решение находится)
    let j = w
        .search(&w.b_tok, json!({"query": WORD, "scope": {"solution": G}}))
        .await;
    assert!(ids(&j).contains(&n.a_solution), "{j}");
    assert_eq!(j["warnings"], json!([]), "{j}");
    let j = w.search(&w.b_tok, body.clone()).await;
    assert_eq!(ids(&j), vec![n.b_knowledge.clone()], "{j}");
    assert_eq!(j["warnings"], json!(["solution unavailable"]), "{j}");
    // pending_count исключённой цели не прибавляется
    assert_eq!(j["pending_count"], 1, "{j}");
    // владельцу сбой его личной базы — ошибка запроса
    let r =
        w.a.tok(&w.a_tok, Method::POST, "/api/v1/search", Some(body))
            .await;
    assert!(r.status.is_server_error(), "{} {}", r.status, r.text());
}

// ---- векторный канал ----

/// Состояние индексации узла в базе пользователя `user`.
async fn state(a: &Api, user: &str, node: &str) -> String {
    let db = a.st.users.get(user.parse().unwrap()).await.unwrap();
    zam_store::get_node(&db, node.parse().unwrap())
        .await
        .unwrap()
        .index_state
}

/// Мир с MiniLM и Qdrant; заметки — английские (модель англоязычная).
struct VecWorld {
    w: W,
    _q: TestQdrant,
    a_telescope: String,
    a_rules: String,
    a_human: String,
    a_bread: String,
    b_coffee: String,
}

const STAR_Q: &str = "stargazing instrument for astronomy";
const BREAD_Q: &str = "naturally fermented loaf";

async fn vec_world(name: &str) -> Option<VecWorld> {
    if !have(MINILM) {
        return None;
    }
    let q = TestQdrant::start(name);
    let w = world_with(name, |c| {
        ml(c, MINILM);
        external(c, &q);
    })
    .await;
    vector_ready(&w.a).await;
    w.shared().await;
    let tele = "# Telescope mirrors\n\nGrind the parabolic mirror of a reflecting telescope and \
                collimate the optics before observing distant galaxies and nebulae.\n";
    let a_telescope = w.note(&w.a_tok, "knowledge", "astro/telescope", tele).await;
    let a_rules = w.admin_note(&w.a_id, "rules", "astro/rules", tele).await;
    let a_human = w.admin_note(&w.a_id, "human", "astro/diary", tele).await;
    let a_bread = w
        .note(
            &w.a_tok,
            G,
            "kitchen/sourdough",
            "# Sourdough bread\n\nMix flour, water and a starter culture, knead the dough, let it \
             rise overnight and bake it in a very hot oven until the crust turns brown.\n",
        )
        .await;
    let b_coffee = w
        .note(
            &w.b_tok,
            "knowledge",
            "kitchen/coffee",
            "# Coffee brewing\n\nGrind beans right before brewing, use water just below boiling \
             and steep for four minutes in a French press.\n",
        )
        .await;
    let indexed = [
        (&w.a_id, &a_telescope),
        (&w.a_id, &a_rules),
        (&w.a_id, &a_bread),
        (&w.b_id, &b_coffee),
    ];
    until(
        "заметки проиндексированы",
        Duration::from_secs(240),
        async || {
            for (u, n) in indexed {
                if state(&w.a, u, n).await != "indexed" {
                    return false;
                }
            }
            true
        },
    )
    .await;
    Some(VecWorld {
        w,
        _q: q,
        a_telescope,
        a_rules,
        a_human,
        a_bread,
        b_coffee,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn member_search_never_returns_owner_knowledge() {
    let Some(v) = vec_world("multi-vec-never").await else {
        return;
    };
    let w = &v.w;
    // контроль: владельцу перефраз находит телескоп векторным каналом
    let j = w
        .search(&w.a_tok, json!({"query": STAR_Q, "top_k": 5}))
        .await;
    assert_eq!(j["degraded"], false, "{j}");
    let src = item(&j, &v.a_telescope)["sources"]
        .as_array()
        .unwrap()
        .clone();
    assert!(src.contains(&json!("vector")), "{j}");
    // участник: ни перефраз (вектор), ни точное слово (FTS) не находят Knowledge, Rules и
    // Human владельца ни в одной форме области
    let private = [&v.a_telescope, &v.a_rules, &v.a_human];
    for query in [STAR_Q, "telescope mirrors"] {
        for scope in [
            json!({"solution": G}),
            json!({"all_solutions": true, "include_human": true}),
            json!({"spaces": ["knowledge", "rules", G]}),
            json!({"solution": G, "path_prefix": "astro"}),
        ] {
            for mode in ["brief", "chunks", "notes"] {
                let j = w
                    .search(
                        &w.b_tok,
                        json!({"query": query, "scope": scope, "mode": mode, "top_k": 20}),
                    )
                    .await;
                assert_eq!(j["degraded"], false, "{j}");
                let got = ids(&j);
                for p in private {
                    assert!(!got.contains(p), "{query} {scope}: {j}");
                }
            }
        }
    }
    // решение владельца и своя Knowledge — векторным каналом одним запросом
    let j = w
        .search(
            &w.b_tok,
            json!({"query": BREAD_Q, "scope": {"solution": G}, "top_k": 5}),
        )
        .await;
    let it = item(&j, &v.a_bread);
    assert_eq!(it["sources"], json!(["vector"]), "{j}");
    assert_eq!(it["agent_solution_id"], G, "{it}");
    assert_eq!(
        it["level"], "chunk",
        "сводка заменена листом базы владельца: {it}"
    );
    let j = w
        .search(
            &w.b_tok,
            json!({"query": "brewing beverage with a press", "scope": {"solution": G}, "top_k": 5}),
        )
        .await;
    assert!(ids(&j).contains(&v.b_coffee), "{j}");
    // без решения в области — решение A не находится и вектором
    let j = w
        .search(&w.b_tok, json!({"query": BREAD_Q, "top_k": 20}))
        .await;
    assert!(!ids(&j).contains(&v.a_bread), "{j}");
    // отзыв: векторный канал тоже не отдаёт решение
    let r = w.decide(&w.b_id, "revoke").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = w
        .search(
            &w.b_tok,
            json!({"query": BREAD_Q, "scope": {"solution": G}, "top_k": 20}),
        )
        .await;
    assert!(!ids(&j).contains(&v.a_bread), "{j}");
    assert_eq!(j["warnings"], json!(["solution access revoked"]), "{j}");
}

/// Замер (задача 4, ревью): время гибридного запроса Qdrant с одной и с тремя областями —
/// при нескольких областях условие `user_id` уходит внутрь `should` и Qdrant не применяет
/// оптимизацию `is_tenant`. Только печать времени: `cargo test --test search_multi --
/// --ignored --nocapture bench_vector_scopes`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn bench_vector_scopes() {
    let Some(v) = vec_world("multi-vec-bench").await else {
        return;
    };
    let w = &v.w;
    // наполнение: по 60 заметок в базах A (решение и Knowledge) и B
    let mut extra = Vec::new();
    for i in 0..60 {
        let body = format!(
            "# Topic {i}\n\nNote number {i} about gardening, cooking, travel and code review \
             practice, variant {i}.\n"
        );
        extra.push((
            w.a_id.clone(),
            w.note(&w.a_tok, G, &format!("s/{i}"), &body).await,
        ));
        extra.push((
            w.a_id.clone(),
            w.note(&w.a_tok, "knowledge", &format!("k/{i}"), &body)
                .await,
        ));
        extra.push((
            w.b_id.clone(),
            w.note(&w.b_tok, "knowledge", &format!("k/{i}"), &body)
                .await,
        ));
    }
    until(
        "наполнение проиндексировано",
        Duration::from_secs(600),
        async || {
            for (u, n) in &extra {
                if state(&w.a, u, n).await != "indexed" {
                    return false;
                }
            }
            true
        },
    )
    .await;
    let st = &w.a.st;
    let emb = st
        .index
        .embedder
        .clone()
        .unwrap()
        .embed_query("travel and cooking notes")
        .unwrap();
    let vector = st.index.vector.clone().unwrap();
    let spaces = async |u: &str| {
        let db = st.users.get(u.parse().unwrap()).await.unwrap();
        zam_store::list_spaces(&db)
            .await
            .unwrap()
            .into_iter()
            .filter(|s| s.kind != zam_store::SpaceKind::Human)
            .collect::<Vec<_>>()
    };
    let a_sp = spaces(&w.a_id).await;
    let b_sp = spaces(&w.b_id).await;
    let c_sp = spaces(&w.c_id).await;
    let sol = a_sp
        .iter()
        .find(|s| s.kind == zam_store::SpaceKind::Solution)
        .unwrap()
        .id;
    let scope = |u: &str, s: Vec<Id>| zam_index::VectorScope {
        user_id: u.parse().unwrap(),
        space_ids: s,
    };
    let one = zam_index::VectorFilter {
        scopes: vec![scope(&w.b_id, b_sp.iter().map(|s| s.id).collect())],
        ..Default::default()
    };
    let three = zam_index::VectorFilter {
        scopes: vec![
            scope(&w.b_id, b_sp.iter().map(|s| s.id).collect()),
            scope(&w.a_id, vec![sol]),
            scope(&w.c_id, c_sp.iter().map(|s| s.id).collect()),
        ],
        ..Default::default()
    };
    for (name, f) in [("1 область", &one), ("3 области", &three)] {
        // прогрев
        for _ in 0..10 {
            vector.search(&emb.dense, &emb.sparse, f).await.unwrap();
        }
        let mut t: Vec<f64> = Vec::new();
        for _ in 0..100 {
            let s = Instant::now();
            let h = vector.search(&emb.dense, &emb.sparse, f).await.unwrap();
            t.push(s.elapsed().as_secs_f64() * 1000.0);
            assert!(!h.is_empty());
        }
        t.sort_by(f64::total_cmp);
        println!(
            "bench_vector_scopes: {name}: p50 {:.2} мс, p95 {:.2} мс",
            t[50], t[95]
        );
    }
}
