//! Импорт пространства (M6 §12б.5, задача 18b2): круговой путь экспорт → импорт (свой и чужой
//! архив, `own=true` на новом сервере, коллизии id), переписывание ссылок, аксиомы, повторы и
//! конфликты, защита архива и приёма тела, содержимое (захваты, предложения, отчёты, связи вне
//! архива, секреты), права.
//!
//! Мир — `common::reports`: владелец решения `G` (Анна), участники `rw` (Борис) и `ro` (Вера) —
//! у каждого токен и сессия WebUI. Чужой импорт — Борис в свою базу.
mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Write};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::api::{Api, Resp};
use common::reports::{G, User, W, ok, png, ready_user, web, with_ip};
use serde_json::{Value, json};
use zam_core::Id;
use zip::write::SimpleFileOptions;

/// Решение Бориса (для отчётов: они живут только в пространстве решения).
const G2: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e99";

// ---------- запросы ----------

async fn note(a: &Api, tok: &str, space: &str, path: &str, content: &str) -> String {
    let r = a
        .tok(
            tok,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": space, "path": path, "kind": "note", "content": content})),
        )
        .await;
    ok(&r, StatusCode::CREATED, path);
    r.json()["id"].as_str().unwrap().to_string()
}

async fn entry(a: &Api, tok: &str, space: &str, body: Value) -> String {
    let r = a
        .tok(
            tok,
            Method::POST,
            &format!("/api/v1/spaces/{space}/entries"),
            Some(body),
        )
        .await;
    ok(&r, StatusCode::CREATED, "запись");
    r.json()["id"].as_str().unwrap().to_string()
}

async fn get_entry(a: &Api, tok: &str, id: &str) -> Value {
    let r = a
        .tok(tok, Method::GET, &format!("/api/v1/entries/{id}"), None)
        .await;
    ok(&r, StatusCode::OK, "запись");
    r.json()
}

async fn get_node(a: &Api, tok: &str, id: &str) -> Value {
    let r = a
        .tok(tok, Method::GET, &format!("/api/v1/nodes/{id}"), None)
        .await;
    ok(&r, StatusCode::OK, "узел");
    r.json()["node"].clone()
}

async fn revisions(a: &Api, tok: &str, id: &str) -> Vec<Value> {
    let r = a
        .tok(
            tok,
            Method::GET,
            &format!("/api/v1/entries/{id}/revisions"),
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "ревизии");
    r.json()["items"].as_array().unwrap().clone()
}

async fn link(a: &Api, tok: &str, src: &str, kind: &str, dst: &str) {
    let r = a
        .tok(
            tok,
            Method::POST,
            "/api/v1/edges",
            Some(json!({"src": src, "type": kind, "dst": dst})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "связь");
}

async fn export_tok(a: &Api, tok: &str, space: &str, q: &str) -> Vec<u8> {
    let r = a
        .tok(
            tok,
            Method::GET,
            &format!("/api/v1/spaces/{space}/export{q}"),
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "экспорт");
    r.body.to_vec()
}

async fn export_web(a: &Api, u: &User, space: &str, q: &str) -> Vec<u8> {
    let r = web(
        a,
        &u.web,
        Method::GET,
        &format!("/api/v1/spaces/{space}/export{q}"),
        None,
    )
    .await;
    ok(&r, StatusCode::OK, "экспорт");
    r.body.to_vec()
}

/// Свободное место тестов импорта — подменённое, а не диска раннера (резерв места не мешает
/// тестам; тесты места ставят своё).
const TEST_FREE: u64 = 1 << 40;

fn free(a: &Api, n: Option<u64>) {
    zam_server::backup::test_hooks::set_free_space(&a.st.cfg.data_dir, n);
}

async fn api(name: &str, tweak: impl FnOnce(&mut zam_server::config::ServerConfig)) -> Api {
    let a = common::api::api(name, tweak).await;
    free(&a, Some(TEST_FREE));
    a
}

async fn world(name: &str) -> W {
    world_cfg(name, |_| {}).await
}

async fn world_cfg(name: &str, tweak: impl FnOnce(&mut zam_server::config::ServerConfig)) -> W {
    let w = common::reports::world_cfg(name, tweak).await;
    free(&w.a, Some(TEST_FREE));
    w
}

fn import_req(space: &str, q: &str, body: Body) -> axum::http::request::Builder {
    let _ = &body;
    Request::post(format!("/api/v1/spaces/{space}/import{q}"))
        .header(header::CONTENT_TYPE, "application/zip")
}

/// Кто опрашивает задание.
#[derive(Clone, Copy)]
enum Who<'a> {
    Tok(&'a str),
    Web(&'a User),
    Admin(&'a str),
}

async fn get_as(a: &Api, who: Who<'_>, uri: &str) -> Resp {
    match who {
        Who::Tok(t) => a.tok(t, Method::GET, uri, None).await,
        Who::Web(u) => web(a, &u.web, Method::GET, uri, None).await,
        Who::Admin(u) => a.adm(Method::GET, uri, None, Some(u)).await,
    }
}

/// Опрос задания до конца: итог как ответ — 200 и отчёт или статус и problem+json ошибки.
async fn wait_job(a: &Api, who: Who<'_>, accepted: Resp) -> Resp {
    if accepted.status != StatusCode::ACCEPTED {
        return accepted;
    }
    let status = format!("/api/v1{}", accepted.json()["status"].as_str().unwrap());
    let t0 = std::time::Instant::now();
    loop {
        let r = get_as(a, who, &status).await;
        ok(&r, StatusCode::OK, "опрос задания");
        let j = r.json();
        match j["state"].as_str().unwrap() {
            "running" => {
                assert!(t0.elapsed() < std::time::Duration::from_secs(300), "{j}");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            "done" => {
                return Resp {
                    status: StatusCode::OK,
                    headers: Default::default(),
                    body: j["report"].to_string().into(),
                };
            }
            _ => {
                let e = &j["error"];
                return Resp {
                    status: StatusCode::from_u16(e["status"].as_u64().unwrap() as u16).unwrap(),
                    headers: Default::default(),
                    body: e.to_string().into(),
                };
            }
        }
    }
}

async fn post_tok(a: &Api, tok: &str, space: &str, q: &str, zip: Vec<u8>) -> Resp {
    let r = import_req(space, q, Body::empty())
        .header(header::AUTHORIZATION, format!("Bearer {tok}"))
        .body(Body::from(zip))
        .unwrap();
    a.send(r).await
}

async fn import_tok(a: &Api, tok: &str, space: &str, q: &str, zip: Vec<u8>) -> Resp {
    let r = post_tok(a, tok, space, q, zip).await;
    wait_job(a, Who::Tok(tok), r).await
}

async fn import_web(a: &Api, u: &User, space: &str, q: &str, zip: Vec<u8>) -> Resp {
    let r = import_req(space, q, Body::empty())
        .header(header::COOKIE, &u.web.cookie)
        .header("x-csrf-token", &u.web.csrf)
        .body(Body::from(zip))
        .unwrap();
    let r = a.send(with_ip(r, &u.web.ip)).await;
    wait_job(a, Who::Web(u), r).await
}

fn report(r: &Resp) -> Value {
    ok(r, StatusCode::OK, "импорт");
    r.json()
}

fn n(v: &Value, k: &str) -> u64 {
    v[k].as_u64()
        .unwrap_or_else(|| panic!("нет числа {k} в {v}"))
}

/// Решение `guid` пользователя `u` (его токеном).
async fn register(a: &Api, u: &User, guid: &str) {
    let r = a
        .tok(
            &u.tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": guid, "name": "Своё"})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "решение");
}

// ---------- база ----------

async fn db(a: &Api, user: &str) -> zam_store::Db {
    a.st.users.get(user.parse().unwrap()).await.unwrap()
}

async fn q1(db: &zam_store::Db, sql: &str, p: Vec<String>) -> i64 {
    let sql = sql.to_string();
    db.read(move |c| Ok(c.query_row(&sql, rusqlite::params_from_iter(p.iter()), |r| r.get(0))?))
        .await
        .unwrap()
}

async fn rows(db: &zam_store::Db, sql: &str, p: Vec<String>) -> Vec<Vec<Option<String>>> {
    let sql = sql.to_string();
    db.read(move |c| {
        let mut st = c.prepare(&sql)?;
        let w = st.column_count();
        let out = st
            .query_map(rusqlite::params_from_iter(p.iter()), |r| {
                (0..w).map(|i| r.get::<_, Option<String>>(i)).collect()
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(out)
    })
    .await
    .unwrap()
}

/// Id записи по заголовку в базе `db` (единственная живая).
async fn entry_by_title(db: &zam_store::Db, title: &str) -> String {
    let r = rows(
        db,
        "SELECT id FROM entries WHERE title = ?1 AND deleted_at IS NULL",
        vec![title.into()],
    )
    .await;
    assert_eq!(r.len(), 1, "записей «{title}»: {}", r.len());
    r[0][0].clone().unwrap()
}

async fn node_by_path(db: &zam_store::Db, path: &str) -> Option<String> {
    let r = rows(
        db,
        "SELECT id FROM nodes WHERE path = ?1 AND deleted_at IS NULL",
        vec![path.into()],
    )
    .await;
    r.first().map(|x| x[0].clone().unwrap())
}

async fn live_counts(db: &zam_store::Db) -> (i64, i64, i64) {
    (
        q1(
            db,
            "SELECT count(*) FROM nodes WHERE deleted_at IS NULL",
            vec![],
        )
        .await,
        q1(
            db,
            "SELECT count(*) FROM entries WHERE deleted_at IS NULL",
            vec![],
        )
        .await,
        q1(
            db,
            "SELECT count(*) FROM edges WHERE deleted_at IS NULL",
            vec![],
        )
        .await,
    )
}

// ---------- архивы ----------

fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in files {
        z.start_file(*name, SimpleFileOptions::default()).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap().into_inner()
}

fn incoming_parts(a: &Api) -> Vec<String> {
    let dir = a.st.cfg.data_dir.join(".incoming");
    std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".part"))
                .collect()
        })
        .unwrap_or_default()
}

// ---------- исходное пространство ----------

/// Пространство `G` Анны со всеми видами журналов, вложением, историей `certainty` и связями.
struct Src {
    plan: String,
    res: String,
    ins: String,
    ins2: String,
    task: String,
    idea: String,
    q: String,
    accepted: String,
    from_idea: String,
    term: String,
    rel: String,
    chg: String,
}

async fn src(w: &W) -> Src {
    let (a, o) = (&w.a, &w.o);
    let res = note(a, &o.tok, G, "Итоги", "# Итоги\n\nитог").await;
    let plan = note(
        a,
        &o.tok,
        G,
        "docs/план",
        "---\nrefs: [src/plan.rs]\n---\n# План\n\nсм. [[Итоги]]\n",
    )
    .await;
    let up = common::attach::upload(a, &o.tok, &plan, "схема 1.png", &png(7, 300)).await;
    let link_md = up["link"].as_str().unwrap().to_string();
    let r = a
        .tok_h(
            &o.tok,
            Method::PUT,
            &format!("/api/v1/nodes/{plan}"),
            Some(json!({"content": format!(
                "---\nrefs: [src/plan.rs]\n---\n# План\n\nсм. [[Итоги]]\n\n![схема]({link_md})\n")})),
            &[("if-match", "1")],
        )
        .await;
    ok(&r, StatusCode::OK, "правка плана");
    let ins2 = entry(
        a,
        &o.tok,
        G,
        json!({"kind": "insight", "title": "Второй вывод", "body": "без ссылок",
               "data": {"kind": "pattern"}}),
    )
    .await;
    let idea = entry(
        a,
        &o.tok,
        G,
        json!({"kind": "idea", "title": "Идея предложения", "body": "замысел",
               "data": {"origin": "human"}}),
    )
    .await;
    let r = a
        .tok(
            &o.tok,
            Method::POST,
            &format!("/api/v1/entries/{idea}/propose"),
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "propose");
    let q = r.json()["question"]["id"].as_str().unwrap().to_string();
    let accepted = entry(
        a,
        &o.tok,
        G,
        json!({"kind": "idea", "title": "Идея принятая", "body": "начать",
               "data": {"origin": "human"}}),
    )
    .await;
    let r = web(
        a,
        &o.web,
        Method::POST,
        &format!("/api/v1/entries/{accepted}/decide"),
        Some(json!({"decision": "accept"})),
    )
    .await;
    ok(&r, StatusCode::OK, "decide");
    let from_idea = r.json()["task"]["id"].as_str().unwrap().to_string();
    let task = entry(
        a,
        &o.tok,
        G,
        json!({"kind": "task", "title": "Задача со ссылками",
               "body": format!("сначала i:{ins2}, потом w:{idea}"),
               "data": {"links": [format!("e:{ins2}")],
                        "blocked_by": format!("ждёт k:{from_idea}")}}),
    )
    .await;
    let ins = entry(
        a,
        &o.tok,
        G,
        json!({"kind": "insight", "title": "Кэш дешевле",
               "body": format!("см. k:{task}, w:{idea} и n:{plan}"),
               "tags": ["perf"], "data": {"kind": "pattern"},
               "triggers": [{"type": "keyword", "pattern": "кэш"}],
               "refs": {"files": [{"path": "src/cache.rs"},
                                  {"path": "src/cache.rs", "symbol": "Cache::get"}]}}),
    )
    .await;
    // история certainty: hypothesis → theorem с причиной
    let r = a
        .tok_rev(
            &o.tok,
            Method::PATCH,
            &format!("/api/v1/entries/{ins}"),
            Some(json!({"certainty": "axiom", "reason": "пользователь подтвердил"})),
        )
        .await;
    ok(&r, StatusCode::OK, "certainty");
    let term = entry(
        a,
        &o.tok,
        G,
        json!({"kind": "term", "title": "Выжимка", "body": "подробно",
               "data": {"definition": "сжатый ответ", "aliases": ["summary"]}}),
    )
    .await;
    let rel = entry(
        a,
        &o.tok,
        G,
        json!({"kind": "release", "title": "Релиз 1", "data": {"version": "1.0.0"}}),
    )
    .await;
    let chg = entry(
        a,
        &o.tok,
        G,
        json!({"kind": "changelog", "title": "Правка кэша",
               "data": {"kind": "fix", "refs": {"files": ["src/cache.rs"]}}}),
    )
    .await;
    link(
        a,
        &o.tok,
        &format!("n:{plan}"),
        "relates_to",
        &format!("e:{ins}"),
    )
    .await;
    link(
        a,
        &o.tok,
        &format!("e:{task}"),
        "depends_on",
        &format!("e:{ins2}"),
    )
    .await;
    Src {
        plan,
        res,
        ins,
        ins2,
        task,
        idea,
        q,
        accepted,
        from_idea,
        term,
        rel,
        chg,
    }
}

impl Src {
    fn entries(&self) -> Vec<&str> {
        vec![
            &self.ins,
            &self.ins2,
            &self.task,
            &self.idea,
            &self.q,
            &self.accepted,
            &self.from_idea,
            &self.term,
            &self.rel,
            &self.chg,
        ]
    }
}

/// (вид, заголовок, статус, certainty) живых записей базы — по заголовку.
async fn entry_shapes(db: &zam_store::Db) -> BTreeSet<Vec<Option<String>>> {
    rows(
        db,
        "SELECT kind, title, status, certainty FROM entries WHERE deleted_at IS NULL",
        vec![],
    )
    .await
    .into_iter()
    .collect()
}

// ---------- круговой путь ----------

/// Свой архив на новом сервере (`own=true`, сессия WebUI — с отчётом): id записей и узлов
/// сохраняются, все виды журналов, вложение, история `certainty`, связи.
#[tokio::test]
async fn roundtrip_own_archive_keeps_ids() {
    let w = world("imp-rt").await;
    let s = src(&w).await;
    let rep_id = w.report("Отчёт обучения").await;
    let zip = export_web(&w.a, &w.o, G, "").await;

    let b = api("imp-rt-b", |_| {}).await;
    let u = ready_user(&b, "Анна", "10.0.1.1").await;
    register(&b, &u, G).await;
    let r = report(&import_web(&b, &u, G, "?own=true", zip).await);
    assert_eq!(n(&r, "id_remapped"), 0, "{r}");
    assert_eq!(n(&r, "demoted_axioms"), 0, "{r}");
    assert!(r["conflicts"].as_array().unwrap().is_empty(), "{r}");
    // все записи и узлы — с прежними id
    for id in s.entries().into_iter().chain([rep_id.as_str()]) {
        let e = web(
            &b,
            &u.web,
            Method::GET,
            &format!("/api/v1/entries/{id}"),
            None,
        )
        .await;
        ok(&e, StatusCode::OK, id);
    }
    let plan = get_node(&b, &u.tok, &s.plan).await;
    assert_eq!(plan["path"], "docs/план");
    assert_eq!(get_node(&b, &u.tok, &s.res).await["path"], "Итоги");
    // форма записей совпадает с исходной
    let src_db = db(&w.a, &w.o.id).await;
    let dst_db = db(&b, &u.id).await;
    assert_eq!(entry_shapes(&src_db).await, entry_shapes(&dst_db).await);
    // certainty с историей: исходные ревизии с атрибуцией + ревизия импорта system
    let revs = revisions(&b, &u.tok, &s.ins).await;
    let got: Vec<(Value, Value, Value)> = revs
        .iter()
        .map(|r| {
            (
                r["certainty"].clone(),
                r["reason"].clone(),
                r["author"]["kind"].clone(),
            )
        })
        .collect();
    assert_eq!(got[0], (json!("hypothesis"), Value::Null, json!("agent")));
    assert_eq!(
        got[1],
        (
            json!("axiom"),
            json!("пользователь подтвердил"),
            json!("agent")
        )
    );
    assert_eq!(got.len(), 3, "{revs:?}");
    assert_eq!(got[2].0, json!("axiom"));
    assert_eq!(got[2].2, json!("system"));
    assert!(
        got[2].1.as_str().unwrap().starts_with("импорт "),
        "{:?}",
        got[2]
    );
    // вложение: файл блоба на месте, ссылка тела открывается
    let att = b
        .tok(
            &u.tok,
            Method::GET,
            &format!("/api/v1/nodes/{}/attachments", s.plan),
            None,
        )
        .await;
    ok(&att, StatusCode::OK, "вложения");
    let a0 = &att.json()[0];
    let sha = a0["sha256"].as_str().unwrap();
    let blob = b
        .tok(
            &u.tok,
            Method::GET,
            &format!("/api/v1/blobs/{sha}/{}", common::api::enc("схема 1.png")),
            None,
        )
        .await;
    ok(&blob, StatusCode::OK, "блоб");
    assert_eq!(blob.body.to_vec(), png(7, 300));
    // связи: явные с теми же концами, derived_from задачи из идеи, relates_to вопроса
    let l = b
        .tok(
            &u.tok,
            Method::GET,
            &format!("/api/v1/nodes/{}/links", s.plan),
            None,
        )
        .await;
    assert!(l.text().contains(&s.ins), "{}", l.text());
    let l = b
        .tok(
            &u.tok,
            Method::GET,
            &format!("/api/v1/entries/{}/links", s.from_idea),
            None,
        )
        .await;
    assert!(
        l.text().contains(&s.accepted) && l.text().contains("derived_from"),
        "{}",
        l.text()
    );
    let l = b
        .tok(
            &u.tok,
            Method::GET,
            &format!("/api/v1/entries/{}/links", s.q),
            None,
        )
        .await;
    assert!(l.text().contains(&s.idea), "{}", l.text());
    // отчёт с исходным created_at
    let e1 = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/entries/{rep_id}"),
        None,
    )
    .await;
    let e2 = web(
        &b,
        &u.web,
        Method::GET,
        &format!("/api/v1/entries/{rep_id}"),
        None,
    )
    .await;
    assert_eq!(e1.json()["created_at"], e2.json()["created_at"]);
}

/// Чужой архив: все id новые, ссылки `k:`/`i:`/`w:`/`n:` в телах и `data` переписаны,
/// `derived_from` задачи из идеи и связи идея ↔ задача ↔ вопрос — на новые id.
#[tokio::test]
async fn foreign_archive_new_ids_and_rewritten_refs() {
    let w = world("imp-foreign").await;
    let s = src(&w).await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let r = report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await);
    assert!(n(&r, "created") >= 12, "{r}");
    let bdb = db(&w.a, &w.rw.id).await;
    let old: BTreeSet<String> = s.entries().iter().map(|x| x.to_string()).collect();
    for id in &old {
        assert_eq!(
            q1(
                &bdb,
                "SELECT count(*) FROM entries WHERE id = ?1",
                vec![id.clone()]
            )
            .await,
            0,
            "старый id {id} в базе Бориса"
        );
    }
    let ins = entry_by_title(&bdb, "Кэш дешевле").await;
    let task = entry_by_title(&bdb, "Задача со ссылками").await;
    let idea = entry_by_title(&bdb, "Идея предложения").await;
    let ins2 = entry_by_title(&bdb, "Второй вывод").await;
    let accepted = rows(
        &bdb,
        "SELECT id FROM entries WHERE kind = 'idea' AND title = 'Идея принятая'",
        vec![],
    )
    .await[0][0]
        .clone()
        .unwrap();
    let plan = node_by_path(&bdb, "docs/план").await.unwrap();
    assert_ne!(plan, s.plan);
    let e = get_entry(&w.a, &w.rw.tok, &ins).await;
    assert_eq!(
        e["body"],
        json!(format!("см. k:{task}, w:{idea} и n:{plan}")),
        "{e}"
    );
    let t = get_entry(&w.a, &w.rw.tok, &task).await;
    assert_eq!(
        t["body"],
        json!(format!("сначала i:{ins2}, потом w:{idea}"))
    );
    assert_eq!(t["data"]["links"], json!([format!("e:{ins2}")]));
    let from_idea = rows(
        &bdb,
        "SELECT id FROM entries WHERE kind = 'task' AND title = 'Идея принятая'",
        vec![],
    )
    .await[0][0]
        .clone()
        .unwrap();
    assert_eq!(
        t["data"]["blocked_by"],
        json!(format!("ждёт k:{from_idea}"))
    );
    // идея ↔ задача ↔ вопрос
    let acc = get_entry(&w.a, &w.rw.tok, &accepted).await;
    assert_eq!(acc["data"]["task_id"], json!(from_idea));
    let id = get_entry(&w.a, &w.rw.tok, &idea).await;
    let q = id["data"]["question_id"].as_str().unwrap().to_string();
    assert_ne!(q, s.q);
    assert_eq!(
        get_entry(&w.a, &w.rw.tok, &q).await["data"]["idea"],
        json!(idea)
    );
    let l =
        w.a.tok(
            &w.rw.tok,
            Method::GET,
            &format!("/api/v1/entries/{from_idea}/links"),
            None,
        )
        .await;
    assert!(
        l.text().contains(&accepted) && l.text().contains("derived_from"),
        "{}",
        l.text()
    );
    // ни одной ссылки на старые id в текстах базы
    for id in old.iter().chain([&s.plan, &s.res]) {
        let hits = q1(
            &bdb,
            "SELECT count(*) FROM entries WHERE instr(body, ?1) > 0 OR instr(data, ?1) > 0",
            vec![id.clone()],
        )
        .await;
        assert_eq!(hits, 0, "ссылка на старый {id}");
    }
}

/// Архив A импортирует B на том же сервере: поиск A (топ-5) и точки Qdrant A не меняются.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreign_import_same_server_keeps_owner_search() {
    use common::ml::{MINILM, external, have, ml};
    if !have(MINILM) {
        return;
    }
    let qd = common::qdrant::TestQdrant::start("imp-search");
    let w = world_cfg("imp-search", |c| {
        ml(c, MINILM);
        external(c, &qd);
    })
    .await;
    common::ml::vector_ready(&w.a).await;
    let texts = [
        (
            "kb/rust",
            "Rust ownership and borrowing rules prevent data races.",
        ),
        (
            "kb/sqlite",
            "SQLite WAL mode allows concurrent readers with one writer.",
        ),
        (
            "kb/qdrant",
            "Qdrant stores dense and sparse vectors for hybrid search.",
        ),
        (
            "kb/zip",
            "Zip archives use deflate compression and a central directory.",
        ),
        (
            "kb/cache",
            "Caching answers saves tokens on repeated agent questions.",
        ),
        (
            "kb/http",
            "HTTP chunked transfer encoding streams large bodies.",
        ),
    ];
    for (p, t) in texts {
        note(&w.a, &w.o.tok, G, p, t).await;
    }
    let adb = db(&w.a, &w.o.id).await;
    let bdb = db(&w.a, &w.rw.id).await;
    let all_indexed = async |d: &zam_store::Db| {
        q1(
            d,
            "SELECT count(*) FROM nodes WHERE kind = 'note' AND deleted_at IS NULL
               AND index_state <> 'indexed' AND index_state <> 'excluded'",
            vec![],
        )
        .await
            == 0
    };
    common::ml::until(
        "A проиндексирован",
        common::ml::READY,
        async || all_indexed(&adb).await,
    )
    .await;
    let search = async || {
        let r = w
            .a
            .tok(
                &w.o.tok,
                Method::POST,
                "/api/v1/search",
                Some(json!({"query": "vectors for hybrid search", "top_k": 5, "scope": {"solution": G}})),
            )
            .await;
        ok(&r, StatusCode::OK, "поиск");
        r.json()["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| format!("{}|{}", h["path"], h["id"]))
            .collect::<Vec<_>>()
    };
    let points_of = |user: &str| {
        let mut v: Vec<String> = qd
            .points()
            .into_iter()
            .filter(|p| p["payload"]["user_id"] == json!(user))
            .map(|p| format!("{}|{}", p["id"], p["payload"]["text_hash"]))
            .collect();
        v.sort();
        v
    };
    // воркер затих (пачки и свёртки L0 папок закончены), точки A больше не меняются
    let settle = async || {
        let t = std::time::Instant::now();
        let mut last = Vec::new();
        loop {
            let st = w.a.st.index.worker.status();
            let now = points_of(&w.o.id);
            if !st.batch_running && st.rollups_pending == 0 && now == last && !now.is_empty() {
                return now;
            }
            assert!(t.elapsed() < common::ml::READY, "воркер не затих: {st:?}");
            last = now;
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        }
    };
    let before = search().await;
    assert!(!before.is_empty());
    let a_points = settle().await;
    assert!(!a_points.is_empty());
    // коллизия своего архива видна по точкам Qdrant другого тенанта
    let a_node: Id = node_by_path(&adb, "kb/rust")
        .await
        .unwrap()
        .parse()
        .unwrap();
    let vec = w.a.st.index.vector.as_ref().unwrap();
    let t = [zam_core::TargetRef {
        kind: zam_core::TargetKind::Node,
        id: a_node,
    }];
    let other: Id = w.rw.id.parse().unwrap();
    let me: Id = w.o.id.parse().unwrap();
    assert_eq!(vec.foreign_targets(other, &t).await.unwrap(), t.to_vec());
    assert!(vec.foreign_targets(me, &t).await.unwrap().is_empty());
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await);
    common::ml::until(
        "B проиндексирован",
        common::ml::READY,
        async || all_indexed(&bdb).await && !points_of(&w.rw.id).is_empty(),
    )
    .await;
    assert_eq!(settle().await, a_points, "точки A изменились");
    assert_eq!(search().await, before, "поиск A изменился");
    let b_ids: BTreeSet<String> = points_of(&w.rw.id)
        .into_iter()
        .map(|p| p.split('|').next().unwrap().to_string())
        .collect();
    let a_ids: BTreeSet<String> = a_points
        .iter()
        .map(|p| p.split('|').next().unwrap().to_string())
        .collect();
    assert!(b_ids.is_disjoint(&a_ids), "общие точки A и B");
}

/// Все буквы и столбцы-ссылки: `e:`, `d:`, `n:`, `supersedes_id`/`superseded_by_id`,
/// `data.blocked_by`; `a:<чанк>` — в `unresolved`.
#[tokio::test]
async fn remap_all_letters_and_columns() {
    let w = world("imp-remap").await;
    let (a, o) = (&w.a, &w.o);
    let nid = note(a, &o.tok, G, "doc", "# Док\n\nтекст").await;
    common::attach::upload(a, &o.tok, &nid, "a.txt", b"plain text attachment").await;
    let odb = db(a, &o.id).await;
    let doc = rows(
        &odb,
        "SELECT id FROM attachment_docs WHERE node_id = ?1",
        vec![nid.clone()],
    )
    .await[0][0]
        .clone()
        .unwrap();
    let old = entry(
        a,
        &o.tok,
        G,
        json!({"kind": "insight", "title": "Старый вывод", "data": {"kind": "pattern"}}),
    )
    .await;
    let new = entry(
        a,
        &o.tok,
        G,
        json!({"kind": "insight", "title": "Новый вывод", "data": {"kind": "pattern"},
               "supersedes_id": old}),
    )
    .await;
    let chunk = format!("a:{}", "0123456789abcdef".repeat(2));
    let t = entry(
        a,
        &o.tok,
        G,
        json!({"kind": "task", "title": "Ссылки всех букв",
               "body": format!("e:{new} d:{doc} n:{nid} {chunk}"),
               "data": {"blocked_by": format!("ждёт e:{old}")}}),
    )
    .await;
    let zip = export_tok(a, &o.tok, G, "").await;
    let r = report(&import_tok(a, &w.rw.tok, "knowledge", "", zip).await);
    let un: Vec<String> = r["unresolved"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    assert!(un.contains(&chunk), "{r}");
    let bdb = db(a, &w.rw.id).await;
    let new2 = entry_by_title(&bdb, "Новый вывод").await;
    let old2 = entry_by_title(&bdb, "Старый вывод").await;
    let t2 = entry_by_title(&bdb, "Ссылки всех букв").await;
    let nid2 = node_by_path(&bdb, "doc").await.unwrap();
    let doc2 = rows(
        &bdb,
        "SELECT id FROM attachment_docs WHERE node_id = ?1",
        vec![nid2.clone()],
    )
    .await[0][0]
        .clone()
        .unwrap();
    assert_ne!(doc2, doc);
    let e = get_entry(a, &w.rw.tok, &t2).await;
    assert_eq!(
        e["body"],
        json!(format!("e:{new2} d:{doc2} n:{nid2} {chunk}")),
        "{e}"
    );
    assert_eq!(e["data"]["blocked_by"], json!(format!("ждёт e:{old2}")));
    let n2 = get_entry(a, &w.rw.tok, &new2).await;
    assert_eq!(n2["supersedes_id"], json!(old2));
    let o2 = get_entry(a, &w.rw.tok, &old2).await;
    assert_eq!(o2["superseded_by_id"], json!(new2));
    assert_eq!(o2["status"], "superseded");
    let _ = t;
}

/// Производные таблицы строит запись: `file_refs` и `triggers` после круга совпадают (с
/// поправкой на новые id), рёбра wikilink построены.
#[tokio::test]
async fn roundtrip_derived_tables_match() {
    let w = world("imp-derived").await;
    let s = src(&w).await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await);
    let (odb, bdb) = (db(&w.a, &w.o.id).await, db(&w.a, &w.rw.id).await);
    // соответствие id: по заголовкам записей и путям узлов
    let titles = rows(
        &odb,
        "SELECT id, title, kind FROM entries WHERE deleted_at IS NULL",
        vec![],
    )
    .await;
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for t in &titles {
        let new = rows(
            &bdb,
            "SELECT id FROM entries WHERE title = ?1 AND kind = ?2",
            vec![t[1].clone().unwrap(), t[2].clone().unwrap()],
        )
        .await;
        map.insert(t[0].clone().unwrap(), new[0][0].clone().unwrap());
    }
    for p in ["docs/план", "Итоги"] {
        let o = node_by_path(&odb, p).await.unwrap();
        map.insert(o, node_by_path(&bdb, p).await.unwrap());
    }
    let refs = async |d: &zam_store::Db, m: Option<&BTreeMap<String, String>>| {
        rows(
            d,
            "SELECT owner_kind, owner_id, path, symbol, content_hash FROM file_refs",
            vec![],
        )
        .await
        .into_iter()
        .map(|mut r| {
            if let Some(m) = m {
                r[1] = Some(m[r[1].as_ref().unwrap()].clone());
            }
            r
        })
        .collect::<BTreeSet<_>>()
    };
    let src_refs = refs(&odb, Some(&map)).await;
    assert!(!src_refs.is_empty());
    assert_eq!(src_refs, refs(&bdb, None).await);
    let trig = async |d: &zam_store::Db, m: Option<&BTreeMap<String, String>>| {
        rows(
            d,
            "SELECT entry_id, type, pattern, tools FROM triggers",
            vec![],
        )
        .await
        .into_iter()
        .map(|mut r| {
            if let Some(m) = m {
                r[0] = Some(m[r[0].as_ref().unwrap()].clone());
            }
            r
        })
        .collect::<BTreeSet<_>>()
    };
    let src_trig = trig(&odb, Some(&map)).await;
    assert_eq!(src_trig.len(), 1);
    assert_eq!(src_trig, trig(&bdb, None).await);
    // ребро wikilink [[Итоги]] построено записью и разрешено в новую заметку
    let plan = map[&s.plan].clone();
    let res = map[&s.res].clone();
    let e = q1(
        &bdb,
        "SELECT count(*) FROM edges WHERE src_id = ?1 AND dst_id = ?2 AND origin = 'wikilink'",
        vec![plan, res],
    )
    .await;
    assert_eq!(e, 1);
    // словарь: термин импортирован и действует
    let g =
        w.a.tok(
            &w.rw.tok,
            Method::GET,
            "/api/v1/spaces/knowledge/glossary",
            None,
        )
        .await;
    assert!(g.text().contains("summary"), "{}", g.text());
}

/// Свой архив на новом сервере: другой `user_id`, `own=true` — id и аксиомы как есть; без флага
/// — чужой: новые id, аксиома понижена.
#[tokio::test]
async fn own_flag_keeps_ids_and_axioms_on_new_server() {
    let w = world("imp-own").await;
    let ax = entry(
        &w.a,
        &w.o.tok,
        G,
        json!({"kind": "insight", "title": "Аксиома", "data": {"kind": "decision"},
               "certainty": "axiom"}),
    )
    .await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let b = api("imp-own-b", |_| {}).await;
    let u = ready_user(&b, "Анна", "10.0.1.1").await;
    assert_ne!(u.id, w.o.id);
    register(&b, &u, G).await;
    let r = report(&import_tok(&b, &u.tok, G, "?own=true", zip.clone()).await);
    assert_eq!(n(&r, "demoted_axioms"), 0);
    let e = get_entry(&b, &u.tok, &ax).await;
    assert_eq!(e["certainty"], "axiom");
    // без флага в knowledge — чужой
    let r = report(&import_tok(&b, &u.tok, "knowledge", "", zip).await);
    assert_eq!(n(&r, "demoted_axioms"), 1, "{r}");
    let udb = db(&b, &u.id).await;
    let ids = rows(
        &udb,
        "SELECT e.id, e.certainty FROM entries e JOIN spaces s ON s.id = e.space_id
         WHERE s.kind = 'knowledge' AND e.title = 'Аксиома'",
        vec![],
    )
    .await;
    assert_eq!(ids.len(), 1);
    assert_ne!(ids[0][0].as_deref(), Some(ax.as_str()));
    assert_eq!(ids[0][1].as_deref(), Some("theorem"));
}

/// Свой архив (`own=true`) у другого пользователя того же сервера: id заняты в базе A —
/// столкнувшиеся переназначаются, отчёт `id_remapped` со списком.
#[tokio::test]
async fn own_flag_collision_remaps_with_report() {
    let w = world("imp-coll").await;
    let n1 = note(&w.a, &w.o.tok, G, "x", "текст").await;
    let e1 = entry(
        &w.a,
        &w.o.tok,
        G,
        json!({"kind": "insight", "title": "Вывод", "data": {"kind": "pattern"}}),
    )
    .await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let r = report(&import_tok(&w.a, &w.rw.tok, "knowledge", "?own=true", zip).await);
    assert_eq!(n(&r, "id_remapped"), 2, "{r}");
    let list: BTreeSet<String> = r["remapped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["from"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(list, BTreeSet::from([n1.clone(), e1.clone()]));
    let bdb = db(&w.a, &w.rw.id).await;
    assert_ne!(node_by_path(&bdb, "x").await.unwrap(), n1);
    assert_ne!(entry_by_title(&bdb, "Вывод").await, e1);
    // A не тронут
    assert_eq!(get_node(&w.a, &w.o.tok, &n1).await["path"], "x");
}

// ---------- аксиомы ----------

async fn axioms_src(w: &W) -> (String, String) {
    let ax = entry(
        &w.a,
        &w.o.tok,
        G,
        json!({"kind": "insight", "title": "Аксиома", "data": {"kind": "decision"},
               "certainty": "axiom"}),
    )
    .await;
    let nid = note(
        &w.a,
        &w.o.tok,
        G,
        "rule",
        "---\ncertainty: axiom\n---\n# Правило\n\nвсегда LF\n",
    )
    .await;
    (ax, nid)
}

/// Чужой архив: аксиомы записи и заметки → `theorem`, ревизия импорта с причиной.
#[tokio::test]
async fn import_foreign_axioms_to_theorem_with_revision() {
    let w = world("imp-ax-foreign").await;
    axioms_src(&w).await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let r = report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await);
    assert_eq!(n(&r, "demoted_axioms"), 2, "{r}");
    let bdb = db(&w.a, &w.rw.id).await;
    let ax = entry_by_title(&bdb, "Аксиома").await;
    let e = get_entry(&w.a, &w.rw.tok, &ax).await;
    assert_eq!(e["certainty"], "theorem");
    let revs = revisions(&w.a, &w.rw.tok, &ax).await;
    let last = revs.last().unwrap();
    assert_eq!(last["author"]["kind"], "system");
    assert_eq!(last["certainty"], "theorem");
    assert!(
        last["reason"]
            .as_str()
            .unwrap()
            .contains("аксиома другого пользователя — понижена до theorem"),
        "{last}"
    );
    assert_eq!(revs[revs.len() - 2]["certainty"], "axiom");
    let nid = node_by_path(&bdb, "rule").await.unwrap();
    let node = get_node(&w.a, &w.rw.tok, &nid).await;
    assert_eq!(node["certainty"], "theorem", "{node}");
    let r =
        w.a.tok(
            &w.rw.tok,
            Method::GET,
            &format!("/api/v1/nodes/{nid}/revisions"),
            None,
        )
        .await;
    // ревизии узла — новые сверху
    let revs = r.json().as_array().unwrap().clone();
    let last = &revs[0];
    assert_eq!(last["author_kind"], "system", "{last}");
    assert!(
        last["message"]
            .as_str()
            .unwrap()
            .contains("понижена до theorem"),
        "{last}"
    );
}

/// Свой архив (пользователь базы = `source_user_id`): аксиомы как есть.
#[tokio::test]
async fn import_own_axioms_kept() {
    let w = world("imp-ax-own").await;
    axioms_src(&w).await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let r = report(&import_tok(&w.a, &w.o.tok, "knowledge", "", zip).await);
    assert_eq!(n(&r, "demoted_axioms"), 0, "{r}");
    let odb = db(&w.a, &w.o.id).await;
    let k = rows(
        &odb,
        "SELECT e.certainty FROM entries e JOIN spaces s ON s.id = e.space_id
         WHERE s.kind = 'knowledge' AND e.title = 'Аксиома'",
        vec![],
    )
    .await;
    assert_eq!(k, vec![vec![Some("axiom".to_string())]]);
}

/// `axioms=keep` — прямое указание: аксиомы чужого архива как есть.
#[tokio::test]
async fn import_axioms_keep_explicit() {
    let w = world("imp-ax-keep").await;
    axioms_src(&w).await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let r = report(&import_tok(&w.a, &w.rw.tok, "knowledge", "?axioms=keep", zip).await);
    assert_eq!(n(&r, "demoted_axioms"), 0, "{r}");
    let bdb = db(&w.a, &w.rw.id).await;
    let ax = entry_by_title(&bdb, "Аксиома").await;
    assert_eq!(get_entry(&w.a, &w.rw.tok, &ax).await["certainty"], "axiom");
    let nid = node_by_path(&bdb, "rule").await.unwrap();
    assert_eq!(get_node(&w.a, &w.rw.tok, &nid).await["certainty"], "axiom");
}

// ---------- повторы и конфликты ----------

/// Повтор того же архива — только `skipped`, без новых строк.
#[tokio::test]
async fn reimport_same_archive_only_skipped() {
    let w = world("imp-again").await;
    src(&w).await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let r1 = report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip.clone()).await);
    let bdb = db(&w.a, &w.rw.id).await;
    let before = live_counts(&bdb).await;
    let r2 = report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await);
    assert_eq!(n(&r2, "created"), 0, "{r2}");
    assert_eq!(n(&r2, "revised"), 0, "{r2}");
    assert_eq!(n(&r2, "skipped"), n(&r1, "created"), "{r1} / {r2}");
    assert_eq!(n(&r2, "id_remapped"), 0, "{r2}");
    assert!(r2["conflicts"].as_array().unwrap().is_empty(), "{r2}");
    assert_eq!(live_counts(&bdb).await, before);
}

/// Изменившиеся записи и заметки: `skip` — в `conflicts`, `revision` — новая ревизия.
#[tokio::test]
async fn import_on_conflict_skip_and_revision() {
    let w = world("imp-conflict").await;
    let (a, o) = (&w.a, &w.o);
    let nid = note(a, &o.tok, G, "n", "версия 1").await;
    let eid = entry(
        a,
        &o.tok,
        G,
        json!({"kind": "insight", "title": "Вывод", "body": "версия 1",
               "data": {"kind": "pattern"}}),
    )
    .await;
    let v1 = export_tok(a, &o.tok, G, "").await;
    report(&import_tok(a, &w.rw.tok, "knowledge", "", v1).await);
    let r = a
        .tok_h(
            &o.tok,
            Method::PUT,
            &format!("/api/v1/nodes/{nid}"),
            Some(json!({"content": "версия 2"})),
            &[("if-match", "1")],
        )
        .await;
    ok(&r, StatusCode::OK, "правка заметки");
    let r = a
        .tok_rev(
            &o.tok,
            Method::PATCH,
            &format!("/api/v1/entries/{eid}"),
            Some(json!({"body": "версия 2"})),
        )
        .await;
    ok(&r, StatusCode::OK, "правка записи");
    let v2 = export_tok(a, &o.tok, G, "").await;
    let r = report(&import_tok(a, &w.rw.tok, "knowledge", "", v2.clone()).await);
    assert_eq!(r["conflicts"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(n(&r, "revised"), 0);
    let bdb = db(a, &w.rw.id).await;
    let bn = node_by_path(&bdb, "n").await.unwrap();
    let be = entry_by_title(&bdb, "Вывод").await;
    let tok = w.rw.tok.clone();
    let content = async |id: String| {
        let r = a
            .tok(&tok, Method::GET, &format!("/api/v1/nodes/{id}"), None)
            .await;
        r.json()["content"].as_str().unwrap_or_default().to_string()
    };
    assert!(content(bn.clone()).await.contains("версия 1"));
    let r = report(&import_tok(a, &w.rw.tok, "knowledge", "?on_conflict=revision", v2).await);
    assert_eq!(n(&r, "revised"), 2, "{r}");
    assert!(content(bn.clone()).await.contains("версия 2"));
    assert_eq!(get_entry(a, &w.rw.tok, &be).await["body"], "версия 2");
    // та же заметка и запись, а не новые
    assert_eq!(node_by_path(&bdb, "n").await.unwrap(), bn);
}

/// Заметка с `imported_from.source_id` — та же заметка, даже по другому пути; путь меняется
/// только при `revision`.
#[tokio::test]
async fn note_id_wins_over_path() {
    let w = world("imp-idpath").await;
    let (a, o) = (&w.a, &w.o);
    let nid = note(a, &o.tok, G, "a/n", "версия 1").await;
    let v1 = export_tok(a, &o.tok, G, "").await;
    report(&import_tok(a, &w.rw.tok, "knowledge", "", v1.clone()).await);
    let bdb = db(a, &w.rw.id).await;
    let bn = node_by_path(&bdb, "a/n").await.unwrap();
    let r = a
        .tok(
            &w.rw.tok,
            Method::POST,
            &format!("/api/v1/nodes/{bn}/move"),
            Some(json!({"path": "b/moved"})),
        )
        .await;
    ok(&r, StatusCode::OK, "перенос");
    let r = report(&import_tok(a, &w.rw.tok, "knowledge", "", v1).await);
    assert_eq!(n(&r, "skipped"), 1, "{r}");
    assert_eq!(n(&r, "created"), 0, "{r}");
    assert!(
        node_by_path(&bdb, "a/n").await.is_none(),
        "дубль по старому пути"
    );
    let r = a
        .tok_h(
            &o.tok,
            Method::PUT,
            &format!("/api/v1/nodes/{nid}"),
            Some(json!({"content": "версия 2"})),
            &[("if-match", "1")],
        )
        .await;
    ok(&r, StatusCode::OK, "правка");
    let v2 = export_tok(a, &o.tok, G, "").await;
    let r = report(&import_tok(a, &w.rw.tok, "knowledge", "", v2.clone()).await);
    assert_eq!(r["conflicts"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(
        node_by_path(&bdb, "b/moved").await.as_deref(),
        Some(bn.as_str())
    );
    let r = report(&import_tok(a, &w.rw.tok, "knowledge", "?on_conflict=revision", v2).await);
    assert_eq!(n(&r, "revised"), 1, "{r}");
    assert_eq!(
        node_by_path(&bdb, "a/n").await.as_deref(),
        Some(bn.as_str())
    );
}

/// Путь занят другой заметкой: `skip` — конфликт `path-taken`; `revision` — заметка рядом.
#[tokio::test]
async fn note_path_taken_conflict() {
    let w = world("imp-taken").await;
    note(&w.a, &w.o.tok, G, "docs/план", "чужой план").await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let mine = note(&w.a, &w.rw.tok, "knowledge", "docs/план", "мой план").await;
    let r = report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip.clone()).await);
    let c = r["conflicts"].as_array().unwrap();
    assert_eq!(c.len(), 1, "{r}");
    assert_eq!(c[0]["reason"], "path-taken");
    assert_eq!(c[0]["path"], "docs/план");
    let r = report(&import_tok(&w.a, &w.rw.tok, "knowledge", "?on_conflict=revision", zip).await);
    assert_eq!(n(&r, "created"), 1, "{r}");
    let bdb = db(&w.a, &w.rw.id).await;
    assert_eq!(
        node_by_path(&bdb, "docs/план").await.as_deref(),
        Some(mine.as_str())
    );
    let today = jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .to_string();
    let beside = node_by_path(&bdb, &format!("docs/план (импорт {today})")).await;
    assert!(beside.is_some(), "заметка рядом");
}

/// `dry_run=true` — тот же отчёт, база и блобы не меняются.
#[tokio::test]
async fn import_dry_run_writes_nothing() {
    let w = world("imp-dry").await;
    src(&w).await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let bdb = db(&w.a, &w.rw.id).await;
    let before = live_counts(&bdb).await;
    let jobs = q1(&bdb, "SELECT count(*) FROM index_jobs", vec![]).await;
    let r = report(&import_tok(&w.a, &w.rw.tok, "knowledge", "?dry_run=true", zip.clone()).await);
    assert!(n(&r, "created") >= 12, "{r}");
    assert_eq!(live_counts(&bdb).await, before);
    assert_eq!(
        q1(&bdb, "SELECT count(*) FROM index_jobs", vec![]).await,
        jobs
    );
    let blobs = w.a.st.data_dir.blobs_dir().join(&w.rw.id);
    let files = walk(&blobs);
    assert!(files.is_empty(), "блобы записаны: {files:?}");
    let real = report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await);
    assert_eq!(n(&real, "created"), n(&r, "created"));
}

fn walk(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p.display().to_string());
        }
    }
    out
}

// ---------- защита архива ----------

/// Пути с `..`, абсолютные, с `\`, ссылки — отказ всего архива (422), ничего не записано.
#[tokio::test]
async fn import_rejects_traversal_abs_symlink() {
    let w = world("imp-evil").await;
    let ok_md: &[u8] = b"# ok\n";
    let bdb = db(&w.a, &w.rw.id).await;
    let before = live_counts(&bdb).await;
    for bad in ["../x.md", "/abs.md", "a\\b.md", "C:/x.md", "a/../../b.md"] {
        let zip = zip_of(&[("good.md", ok_md), (bad, ok_md)]);
        let r = import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await;
        ok(&r, StatusCode::UNPROCESSABLE_ENTITY, bad);
    }
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    z.start_file("good.md", SimpleFileOptions::default())
        .unwrap();
    z.write_all(ok_md).unwrap();
    z.add_symlink("link.md", "../../etc/passwd", SimpleFileOptions::default())
        .unwrap();
    let zip = z.finish().unwrap().into_inner();
    let r = import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "ссылка");
    assert_eq!(live_counts(&bdb).await, before);
}

/// Коэффициент сжатия больше 100 и файлов больше `import.max_files` — 422.
#[tokio::test]
async fn import_rejects_zip_bomb() {
    let w = world_cfg("imp-bomb", |c| c.import.max_files = 3).await;
    let zeros = vec![0u8; 20 << 20];
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    z.start_file(
        "bomb.md",
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
    )
    .unwrap();
    z.write_all(&zeros).unwrap();
    let zip = z.finish().unwrap().into_inner();
    assert!(zip.len() < 1 << 20);
    let r = import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "бомба");
    assert!(r.text().contains("сжат"), "{}", r.text());
    let many: Vec<(String, Vec<u8>)> = (0..5)
        .map(|i| (format!("n{i}.md"), b"x".to_vec()))
        .collect();
    let refs: Vec<(&str, &[u8])> = many
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_slice()))
        .collect();
    let r = import_tok(&w.a, &w.rw.tok, "knowledge", "", zip_of(&refs)).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "файлов");
}

/// Тело больше `import.max_bytes` — 413 (по `Content-Length` и потоком), `.part` удалён.
#[tokio::test]
async fn import_body_over_limit_413_and_part_removed() {
    let w = world_cfg("imp-413", |c| c.import.max_bytes = 1 << 20).await;
    let big = vec![7u8; 3 << 20];
    let r = import_tok(&w.a, &w.rw.tok, "knowledge", "", big.clone()).await;
    ok(&r, StatusCode::PAYLOAD_TOO_LARGE, "Content-Length");
    // без длины: поток кусками
    let chunks: Vec<Result<axum::body::Bytes, std::io::Error>> = big
        .chunks(256 * 1024)
        .map(|c| Ok(axum::body::Bytes::copy_from_slice(c)))
        .collect();
    let body = Body::from_stream(futures_util::stream::iter(chunks));
    let req = Request::post("/api/v1/spaces/knowledge/import")
        .header(header::AUTHORIZATION, format!("Bearer {}", w.rw.tok))
        .header(header::CONTENT_TYPE, "application/zip")
        .body(body)
        .unwrap();
    let r = w.a.send(req).await;
    ok(&r, StatusCode::PAYLOAD_TOO_LARGE, "поток");
    assert!(
        incoming_parts(&w.a).is_empty(),
        "{:?}",
        incoming_parts(&w.a)
    );
}

/// Обрыв загрузки посреди тела — `.part` удалён, ничего не записано.
#[tokio::test]
async fn import_aborted_upload_part_removed() {
    let w = world("imp-abort").await;
    src(&w).await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let bdb = db(&w.a, &w.rw.id).await;
    let before = live_counts(&bdb).await;
    let half = zip[..zip.len() / 2].to_vec();
    let chunks: Vec<Result<axum::body::Bytes, std::io::Error>> = vec![
        Ok(axum::body::Bytes::from(half)),
        Err(std::io::Error::other("клиент ушёл")),
    ];
    let body = Body::from_stream(futures_util::stream::iter(chunks));
    let req = Request::post("/api/v1/spaces/knowledge/import")
        .header(header::AUTHORIZATION, format!("Bearer {}", w.rw.tok))
        .header(header::CONTENT_TYPE, "application/zip")
        .body(body)
        .unwrap();
    let r = w.a.send(req).await;
    assert!(r.status.is_client_error(), "{} {}", r.status, r.text());
    assert!(
        incoming_parts(&w.a).is_empty(),
        "{:?}",
        incoming_parts(&w.a)
    );
    assert_eq!(live_counts(&bdb).await, before);
    // аудит: начало и незавершённый конец
    let rows = w.a.st.system.list_audit(100, None).await.unwrap();
    let done: Vec<_> = rows
        .iter()
        .filter(|r| r.action == "space.import.done")
        .collect();
    assert_eq!(done.len(), 1);
    assert_eq!(
        done[0].details["complete"],
        json!(false),
        "{:?}",
        done[0].details
    );
    assert!(rows.iter().any(|r| r.action == "space.import"));
}

// ---------- содержимое ----------

/// Захваты задач сбрасываются: сессий старой машины нет.
#[tokio::test]
async fn import_drops_claims() {
    let w = world("imp-claims").await;
    let t = entry(
        &w.a,
        &w.o.tok,
        G,
        json!({"kind": "task", "title": "Захваченная"}),
    )
    .await;
    let r =
        w.a.tok_h(
            &w.o.tok,
            Method::POST,
            &format!("/api/v1/entries/{t}/claim"),
            None,
            &[("x-zam-session", "s-claim-1")],
        )
        .await;
    ok(&r, StatusCode::OK, "захват");
    assert!(get_entry(&w.a, &w.o.tok, &t).await["data"]["claimed_by"].is_object());
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await);
    let bdb = db(&w.a, &w.rw.id).await;
    let t2 = entry_by_title(&bdb, "Захваченная").await;
    let e = get_entry(&w.a, &w.rw.tok, &t2).await;
    assert!(e["data"].get("claimed_by").is_none(), "{e}");
}

/// Вопрос-предложение без своей идеи (её нет в архиве) становится `obsolete`.
#[tokio::test]
async fn import_orphan_proposal_obsolete() {
    let w = world("imp-orphan").await;
    let s = src(&w).await;
    let zip = export_tok(&w.a, &w.o.tok, G, "?kinds=question").await;
    report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await);
    let bdb = db(&w.a, &w.rw.id).await;
    let q = entry_by_title(
        &bdb,
        get_entry(&w.a, &w.o.tok, &s.q).await["title"]
            .as_str()
            .unwrap(),
    )
    .await;
    assert_eq!(get_entry(&w.a, &w.rw.tok, &q).await["status"], "obsolete");
}

/// Отчёт с файлами: исходные `created_at` и файлы, одной транзакцией; неизменность цела.
/// Токену импорт отчётов недоступен.
#[tokio::test]
async fn import_report_with_images_keeps_immutability() {
    let w = world("imp-report").await;
    let rid = w.report("Отчёт с картинкой").await;
    let src_e = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/entries/{rid}"),
        None,
    )
    .await;
    let src_e = src_e.json();
    let zip = export_web(&w.a, &w.o, G, "").await;
    register(&w.a, &w.rw, G2).await;
    // токен: отчёт пропущен с предупреждением
    let r = report(&import_tok(&w.a, &w.rw.tok, G2, "", zip.clone()).await);
    assert_eq!(n(&r, "created"), 0, "{r}");
    assert!(r["warnings"].to_string().contains("отчёт"), "{r}");
    let r = report(&import_web(&w.a, &w.rw, G2, "", zip).await);
    assert_eq!(n(&r, "created"), 1, "{r}");
    let bdb = db(&w.a, &w.rw.id).await;
    let nid = entry_by_title(&bdb, "Отчёт с картинкой").await;
    let e = web(
        &w.a,
        &w.rw.web,
        Method::GET,
        &format!("/api/v1/entries/{nid}"),
        None,
    )
    .await;
    let e = e.json();
    assert_eq!(e["created_at"], src_e["created_at"]);
    assert_eq!(e["rev"], 1);
    let files = w.files_of_in(&bdb, &nid).await;
    assert_eq!(files.len(), 3);
    // неизменность: правка — 409
    let r = common::reports::web_h(
        &w.a,
        &w.rw.web,
        Method::PATCH,
        &format!("/api/v1/entries/{nid}"),
        Some(json!({"body": "x"})),
        &[("if-match", "1")],
    )
    .await;
    ok(&r, StatusCode::CONFLICT, "правка отчёта");
    // повтор — пропуск
    let zip = export_web(&w.a, &w.o, G, "").await;
    let r = report(&import_web(&w.a, &w.rw, G2, "", zip).await);
    assert_eq!(n(&r, "skipped"), 1, "{r}");
}

impl W {
    async fn files_of_in(&self, db: &zam_store::Db, id: &str) -> Vec<zam_store::EntryAttachment> {
        zam_store::entry_attachments(db, id.parse().unwrap())
            .await
            .unwrap()
    }
}

/// Связь на цель вне архива и базы — пропуск с отчётом `links_skipped`.
#[tokio::test]
async fn import_links_outside_skipped_reported() {
    let w = world("imp-links").await;
    let k = note(&w.a, &w.o.tok, "knowledge", "kb/x", "знание").await;
    let g = note(&w.a, &w.o.tok, G, "g", "решение").await;
    let e = entry(
        &w.a,
        &w.o.tok,
        G,
        json!({"kind": "insight", "title": "Вывод", "data": {"kind": "pattern"}}),
    )
    .await;
    link(
        &w.a,
        &w.o.tok,
        &format!("n:{g}"),
        "relates_to",
        &format!("n:{k}"),
    )
    .await;
    link(
        &w.a,
        &w.o.tok,
        &format!("n:{g}"),
        "relates_to",
        &format!("e:{e}"),
    )
    .await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let r = report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await);
    assert_eq!(n(&r, "links_skipped"), 1, "{r}");
    let bdb = db(&w.a, &w.rw.id).await;
    assert_eq!(
        q1(
            &bdb,
            "SELECT count(*) FROM edges WHERE origin = 'explicit' AND deleted_at IS NULL",
            vec![]
        )
        .await,
        1
    );
}

/// Сканер секретов работает, как при обычной записи: предупреждение `secrets_detected`.
#[tokio::test]
async fn import_secrets_detected_warning() {
    let w = world("imp-secret").await;
    // фиктивный ключ собирается конкатенацией: в исходнике его нет целиком (push protection)
    let key = format!("{}{}", "AK", "IAQWERTYUIOPASDFGH");
    note(&w.a, &w.o.tok, G, "s", &format!("ключ {key} тут")).await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let r = report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await);
    let ws = r["warnings"].to_string();
    assert!(ws.contains("secrets_detected"), "{r}");
    assert!(!ws.contains(&key), "значение секрета в отчёте: {r}");
}

/// Участник решения (`rw` и `ro`) не импортирует — 403; владелец и администратор — да.
#[tokio::test]
async fn member_cannot_import() {
    let w = world("imp-member").await;
    note(&w.a, &w.o.tok, G, "x", "текст").await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    for (who, tok) in [("rw", &w.rw.tok), ("ro", &w.ro.tok)] {
        let r = import_tok(&w.a, tok, G, "", zip.clone()).await;
        ok(&r, StatusCode::FORBIDDEN, who);
    }
    let r = import_web(&w.a, &w.rw, G, "", zip.clone()).await;
    ok(&r, StatusCode::FORBIDDEN, "сессия участника");
    let r = report(&import_tok(&w.a, &w.o.tok, G, "", zip.clone()).await);
    assert_eq!(n(&r, "skipped"), 1, "{r}");
    let req = Request::post(format!("/api/v1/spaces/{G}/import"))
        .header(header::COOKIE, &w.a.cookie)
        .header("x-csrf-token", &w.a.csrf)
        .header("x-zam-user", &w.o.id)
        .header(header::CONTENT_TYPE, "application/zip")
        .body(Body::from(zip))
        .unwrap();
    let r = w.a.send(req).await;
    let r = wait_job(&w.a, Who::Admin(&w.o.id), r).await;
    assert_eq!(n(&report(&r), "skipped"), 1);
}

// ---------- слот, остановка, аудит ----------

/// Слоты приёма заняты — 503 `/problems/busy` с `Retry-After`: на сервере (два) и у
/// пользователя (один, пока идёт его приём); освободились — импорт идёт. Запись — очередь: при
/// занятом слоте записи задание ждёт на этапе `queued`.
#[tokio::test]
async fn import_busy_when_slot_taken() {
    let w = world("imp-busy").await;
    note(&w.a, &w.o.tok, G, "x", "текст").await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let rt = &w.a.st.imports;
    let (p1, p2) = (rt.hold_recv(), rt.hold_recv());
    let r = post_tok(&w.a, &w.rw.tok, "knowledge", "", zip.clone()).await;
    ok(&r, StatusCode::SERVICE_UNAVAILABLE, "занято на сервере");
    assert_eq!(r.header("retry-after"), "30");
    // права проверяются раньше слота
    let r = post_tok(&w.a, &w.ro.tok, G, "", zip.clone()).await;
    ok(&r, StatusCode::FORBIDDEN, "участник при занятом слоте");
    drop((p1, p2));
    // свой приём у пользователя идёт (тело не кончается) — второй его приём занят
    let app = w.a.app.clone();
    let tok = w.rw.tok.clone();
    let hang = tokio::spawn(async move {
        let body = Body::from_stream(futures_util::stream::pending::<
            Result<axum::body::Bytes, std::io::Error>,
        >());
        let req = Request::post("/api/v1/spaces/knowledge/import")
            .header(header::AUTHORIZATION, format!("Bearer {tok}"))
            .body(body)
            .unwrap();
        let _ = tower::ServiceExt::oneshot(app, req).await;
    });
    let t0 = std::time::Instant::now();
    while rt.recv_available() == zam_server::api::transfer::import::RECV_SLOTS {
        assert!(t0.elapsed() < std::time::Duration::from_secs(10));
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let r = post_tok(&w.a, &w.rw.tok, "knowledge", "", zip.clone()).await;
    ok(
        &r,
        StatusCode::SERVICE_UNAVAILABLE,
        "второй приём пользователя",
    );
    // другой пользователь в это время принимается
    let r = import_tok(&w.a, &w.o.tok, "knowledge", "", zip.clone()).await;
    report(&r);
    hang.abort();
    let _ = hang.await;
    // очередь записи: задание ждёт на этапе queued
    let hold = rt.hold_write();
    let r = post_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await;
    ok(&r, StatusCode::ACCEPTED, "принято");
    let status = format!("/api/v1{}", r.json()["status"].as_str().unwrap());
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let s = w.a.tok(&w.rw.tok, Method::GET, &status, None).await.json();
    assert_eq!(
        (s["state"].as_str(), s["stage"].as_str()),
        (Some("running"), Some("queued")),
        "{s}"
    );
    // место в очереди: впереди никого (слот держит тест, а не задание)
    assert_eq!(s["queue"], json!(0), "{s}");
    drop(hold);
    let r = wait_job(&w.a, Who::Tok(&w.rw.tok), r).await;
    assert_eq!(n(&report(&r), "created"), 1);
    assert_eq!(
        rt.recv_available(),
        zam_server::api::transfer::import::RECV_SLOTS
    );
    assert_eq!(
        rt.write_available(),
        zam_server::api::transfer::import::WRITE_SLOTS
    );
}

/// Сервер останавливается — 503 до аудита и приёма тела.
#[tokio::test]
async fn import_refused_while_stopping() {
    let w = world("imp-stop").await;
    note(&w.a, &w.o.tok, G, "x", "текст").await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    w.a.st.begin_stop();
    let r = import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await;
    ok(&r, StatusCode::SERVICE_UNAVAILABLE, "остановка");
    let rows = w.a.st.system.list_audit(100, None).await.unwrap();
    assert!(!rows.iter().any(|r| r.action.starts_with("space.import")));
    assert!(incoming_parts(&w.a).is_empty());
}

/// Аудит до тела не записался — 500, тело не читается, ничего не записано.
#[tokio::test]
async fn import_audit_failure_no_body() {
    let w = world("imp-auditfail").await;
    note(&w.a, &w.o.tok, G, "x", "текст").await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let bdb = db(&w.a, &w.rw.id).await;
    let before = live_counts(&bdb).await;
    zam_server::backup::test_hooks::set_fail_point(&w.a.st.cfg.data_dir, Some("import_audit"));
    let r = import_tok(&w.a, &w.rw.tok, "knowledge", "", zip.clone()).await;
    zam_server::backup::test_hooks::set_fail_point(&w.a.st.cfg.data_dir, None);
    ok(&r, StatusCode::INTERNAL_SERVER_ERROR, "аудит");
    assert_eq!(live_counts(&bdb).await, before);
    let rows = w.a.st.system.list_audit(100, None).await.unwrap();
    assert!(!rows.iter().any(|r| r.action.starts_with("space.import")));
    assert_eq!(
        w.a.st.imports.recv_available(),
        zam_server::api::transfer::import::RECV_SLOTS
    );
    report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await);
}

// ---------- задания, повтор после обрыва, мелкие файлы, ответ на вопрос ----------

/// Опрос: этап и итог видит тот же пользователь и администратор; другому — 404.
#[tokio::test]
async fn import_job_status_owner_only() {
    let w = world("imp-poll").await;
    note(&w.a, &w.o.tok, G, "x", "текст").await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let r = post_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await;
    ok(&r, StatusCode::ACCEPTED, "принято");
    let j = r.json();
    let status = format!("/api/v1{}", j["status"].as_str().unwrap());
    assert!(r.header("location").ends_with(j["job"].as_str().unwrap()));
    let done = wait_job(&w.a, Who::Tok(&w.rw.tok), r).await;
    assert_eq!(n(&report(&done), "created"), 1);
    let s = w.a.tok(&w.rw.tok, Method::GET, &status, None).await.json();
    assert_eq!(s["state"], "done");
    assert_eq!(s["stage"], "done");
    assert_eq!(s["progress"]["done"], s["progress"]["total"]);
    // сессия того же пользователя и администратор — видят
    ok(
        &get_as(&w.a, Who::Web(&w.rw), &status).await,
        StatusCode::OK,
        "сессия",
    );
    ok(
        &get_as(&w.a, Who::Admin(&w.rw.id), &status).await,
        StatusCode::OK,
        "админ",
    );
    // другой пользователь — как несуществующее
    for t in [&w.o.tok, &w.ro.tok] {
        ok(
            &w.a.tok(t, Method::GET, &status, None).await,
            StatusCode::NOT_FOUND,
            "чужой",
        );
    }
    let other = status.replace("/knowledge/", &format!("/{G}/"));
    ok(
        &w.a.tok(&w.rw.tok, Method::GET, &other, None).await,
        StatusCode::NOT_FOUND,
        "другое пространство",
    );
    let nojob = format!("/api/v1/spaces/knowledge/imports/{}", Id::new());
    ok(
        &w.a.tok(&w.rw.tok, Method::GET, &nojob, None).await,
        StatusCode::NOT_FOUND,
        "нет задания",
    );
}

/// Настоящий обрыв соединения: клиент ушёл, получив 202, пока задание ждёт записи, — задание
/// доходит до конца, итог виден опросом.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_job_survives_client_disconnect() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let w = world("imp-disc").await;
    src(&w).await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = w.a.app.clone();
    let srv = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let hold = w.a.st.imports.hold_write();
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST /api/v1/spaces/knowledge/import HTTP/1.1\r\nHost: {addr}\r\n\
         Authorization: Bearer {}\r\nContent-Type: application/zip\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        w.rw.tok,
        zip.len()
    );
    sock.write_all(head.as_bytes()).await.unwrap();
    sock.write_all(&zip).await.unwrap();
    let mut buf = vec![0u8; 4096];
    // ответ 202 приходит до записи (запись ждёт слот): клиент его не ждёт вечно
    let got = tokio::time::timeout(std::time::Duration::from_secs(30), sock.read(&mut buf))
        .await
        .expect("202 не пришёл: задание держит запрос")
        .unwrap();
    let resp = String::from_utf8_lossy(&buf[..got]).to_string();
    assert!(resp.starts_with("HTTP/1.1 202"), "{resp}");
    let body = resp.split("\r\n\r\n").nth(1).unwrap();
    let job: Value = serde_json::from_str(body.trim()).unwrap();
    // клиент уходит, не дожидаясь записи
    drop(sock);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    drop(hold);
    let fake = Resp {
        status: StatusCode::ACCEPTED,
        headers: Default::default(),
        body: job.to_string().into(),
    };
    let r = wait_job(&w.a, Who::Tok(&w.rw.tok), fake).await;
    assert!(n(&report(&r), "created") >= 12, "{}", r.text());
    srv.abort();
}

/// Обрыв после первой пачки (остановка) → повтор: id назначения детерминированы — ни одной
/// ссылки на несуществующий id и ни одного дубля.
#[tokio::test]
async fn import_resume_after_stop_no_dangling_refs() {
    let w = world("imp-resume").await;
    let s = src(&w).await;
    // первая пачка (папка docs и эта заметка) ссылается на записи следующих пачек
    note(
        &w.a,
        &w.o.tok,
        G,
        "aaa",
        &format!("см. i:{} и k:{}", s.ins, s.task),
    )
    .await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let bdb = db(&w.a, &w.rw.id).await;
    let kspace: Id = rows(
        &bdb,
        "SELECT id FROM spaces WHERE kind = 'knowledge'",
        vec![],
    )
    .await[0][0]
        .clone()
        .unwrap()
        .parse()
        .unwrap();
    let uid: Id = w.rw.id.parse().unwrap();
    let gen0 = w.a.st.render_gen.get(uid, kspace);
    zam_server::backup::test_hooks::set_fail_point(&w.a.st.cfg.data_dir, Some("import_one_batch"));
    let r = import_tok(&w.a, &w.rw.tok, "knowledge", "", zip.clone()).await;
    zam_server::backup::test_hooks::set_fail_point(&w.a.st.cfg.data_dir, None);
    ok(
        &r,
        StatusCode::SERVICE_UNAVAILABLE,
        "остановка после первой пачки",
    );
    // записанная пачка оповестила рендер и индексацию и на пути ошибки
    assert!(w.a.st.render_gen.get(uid, kspace) > gen0);
    assert!(
        node_by_path(&bdb, "aaa").await.is_some(),
        "первая пачка записана"
    );
    assert_eq!(q1(&bdb, "SELECT count(*) FROM entries", vec![]).await, 0);
    report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await);
    // дублей нет
    let dup = q1(
        &bdb,
        "SELECT count(*) FROM (SELECT kind, title FROM entries WHERE deleted_at IS NULL
                               GROUP BY kind, title HAVING count(*) > 1)",
        vec![],
    )
    .await;
    assert_eq!(dup, 0);
    let dupn = q1(
        &bdb,
        "SELECT count(*) FROM (SELECT path FROM nodes WHERE deleted_at IS NULL
                               GROUP BY path HAVING count(*) > 1)",
        vec![],
    )
    .await;
    assert_eq!(dupn, 0);
    // каждая ссылка текстов ведёт на существующий id
    let texts = rows(
        &bdb,
        "SELECT body || ' ' || data FROM entries
         UNION ALL SELECT content FROM revisions r JOIN nodes n ON n.id = r.node_id
                   AND r.rev = n.current_rev",
        vec![],
    )
    .await;
    let mut checked = 0;
    for t in texts.into_iter().flatten().flatten() {
        for sp in zam_store::import::scan_refs(&t) {
            let Some(id) = sp.id else { continue };
            let found = q1(
                &bdb,
                "SELECT (SELECT count(*) FROM nodes WHERE id = ?1)
                      + (SELECT count(*) FROM entries WHERE id = ?1)
                      + (SELECT count(*) FROM attachment_docs WHERE id = ?1)",
                vec![id.to_string()],
            )
            .await;
            assert_eq!(found, 1, "висячая ссылка {}:{id} в «{t}»", sp.letter);
            checked += 1;
        }
    }
    assert!(checked >= 5, "проверено ссылок: {checked}");
}

/// Бомба из мелких файлов: каждый меньше 1 МиБ, но сжат в тысячи раз — 422.
#[tokio::test]
async fn import_rejects_small_files_bomb() {
    let w = world("imp-smallbomb").await;
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let o = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let zeros = vec![0u8; 512 << 10];
    for i in 0..100 {
        z.start_file(format!("n{i}.md"), o).unwrap();
        z.write_all(&zeros).unwrap();
    }
    let zip = z.finish().unwrap().into_inner();
    assert!(zip.len() < 1 << 20, "{}", zip.len());
    let r = import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "мелкие файлы");
    assert!(r.text().contains("сжат"), "{}", r.text());
}

/// `data.answer.decision_id` отвеченного вопроса — на решение, импортированное заново.
#[tokio::test]
async fn import_remaps_answer_decision_id() {
    let w = world("imp-answer").await;
    let q = entry(
        &w.a,
        &w.o.tok,
        G,
        json!({"kind": "question", "title": "Брать ли SQLite?", "body": "контекст"}),
    )
    .await;
    let r =
        w.a.tok(
            &w.o.tok,
            Method::POST,
            &format!("/api/v1/entries/{q}/answer"),
            Some(json!({"text": "да, SQLite", "channel": "chat", "by": "human"})),
        )
        .await;
    ok(&r, StatusCode::OK, "ответ");
    let old = get_entry(&w.a, &w.o.tok, &q).await["data"]["answer"]["decision_id"]
        .as_str()
        .unwrap()
        .to_string();
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await);
    let bdb = db(&w.a, &w.rw.id).await;
    let q2 = entry_by_title(&bdb, "Брать ли SQLite?").await;
    let dec = get_entry(&w.a, &w.rw.tok, &q2).await["data"]["answer"]["decision_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(dec, old);
    let d = get_entry(&w.a, &w.rw.tok, &dec).await;
    assert_eq!(d["kind"], "insight", "{d}");
}

/// Размер вложения — фактический из архива, MIME — по имени, как при загрузке: frontmatter
/// архива им не указ.
#[tokio::test]
async fn import_attachment_size_and_mime_from_file() {
    use std::io::Read;
    let w = world("imp-mime").await;
    let nid = note(&w.a, &w.o.tok, G, "pic", "# Картинка").await;
    let pic = png(3, 500);
    common::attach::upload(&w.a, &w.o.tok, &nid, "a.png", &pic).await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    // подделка frontmatter: размер и MIME
    let mut src = zip::ZipArchive::new(Cursor::new(zip)).unwrap();
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..src.len() {
        let mut f = src.by_index(i).unwrap();
        let name = f.name().unwrap().to_string();
        let mut b = Vec::new();
        f.read_to_end(&mut b).unwrap();
        if name == "pic.md" {
            let t = String::from_utf8(b).unwrap();
            let t = t
                .replace("\"mime\":\"image/png\"", "\"mime\":\"text/html\"")
                .replace(&format!("\"size\":{}", pic.len()), "\"size\":1");
            assert!(t.contains("text/html") && t.contains("\"size\":1"), "{t}");
            b = t.into_bytes();
        }
        out.start_file(name.as_str(), SimpleFileOptions::default())
            .unwrap();
        out.write_all(&b).unwrap();
    }
    let zip = out.finish().unwrap().into_inner();
    report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await);
    let bdb = db(&w.a, &w.rw.id).await;
    let n2 = node_by_path(&bdb, "pic").await.unwrap();
    let r = rows(
        &bdb,
        "SELECT mime, CAST(size AS TEXT) FROM attachments WHERE node_id = ?1",
        vec![n2],
    )
    .await;
    assert_eq!(
        r,
        vec![vec![
            Some("image/png".to_string()),
            Some(pic.len().to_string())
        ]]
    );
}

/// Ревизия при повторе с недопустимым `certainty` — конфликт `invalid`, а не сбой задания.
#[tokio::test]
async fn import_revision_invalid_certainty_conflict() {
    let w = world("imp-badcert").await;
    let v1 = zip_of(&[("a.md", b"# A\n\nversion 1\n")]);
    report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", v1).await);
    let v2 = zip_of(&[("a.md", b"---\ncertainty: probably\n---\n# A\n\nversion 2\n")]);
    let r = report(&import_tok(&w.a, &w.rw.tok, "knowledge", "?on_conflict=revision", v2).await);
    let c = r["conflicts"].as_array().unwrap();
    assert_eq!(c.len(), 1, "{r}");
    assert!(
        c[0]["reason"].as_str().unwrap().starts_with("invalid"),
        "{r}"
    );
    assert_eq!(n(&r, "revised"), 0);
}

// ---------- повторное ревью: свой элемент, пределы заданий, стражи ----------

/// Возобновление: поздняя пачка ссылается на документ вложения (`d:`) и папку (`n:`), созданные
/// оборванной первой попыткой, — детерминированный id в своём пространстве того же вида — свой
/// элемент: висячих ссылок нет, переназначений нет.
#[tokio::test]
async fn import_resume_refs_to_doc_and_folder() {
    let w = world("imp-resume-d").await;
    let (a, o) = (&w.a, &w.o);
    let nid = note(a, &o.tok, G, "a0", "# С вложением").await;
    common::attach::upload(a, &o.tok, &nid, "a.txt", b"plain text attachment").await;
    note(a, &o.tok, G, "docs/x", "# В папке").await;
    let odb = db(a, &o.id).await;
    let doc = rows(&odb, "SELECT id FROM attachment_docs", vec![]).await[0][0]
        .clone()
        .unwrap();
    let folder = node_by_path(&odb, "docs").await.unwrap();
    entry(
        a,
        &o.tok,
        G,
        json!({"kind": "insight", "title": "Ссылки на раннее",
               "body": format!("см. d:{doc} и n:{folder}"), "data": {"kind": "pattern"}}),
    )
    .await;
    let zip = export_tok(a, &o.tok, G, "").await;
    zam_server::backup::test_hooks::set_fail_point(&a.st.cfg.data_dir, Some("import_one_batch"));
    let r = import_tok(a, &w.rw.tok, "knowledge", "", zip.clone()).await;
    zam_server::backup::test_hooks::set_fail_point(&a.st.cfg.data_dir, None);
    ok(
        &r,
        StatusCode::SERVICE_UNAVAILABLE,
        "остановка после первой пачки",
    );
    let bdb = db(a, &w.rw.id).await;
    assert_eq!(
        q1(&bdb, "SELECT count(*) FROM attachment_docs", vec![]).await,
        1
    );
    let r = report(&import_tok(a, &w.rw.tok, "knowledge", "", zip).await);
    assert_eq!(n(&r, "id_remapped"), 0, "{r}");
    let e = entry_by_title(&bdb, "Ссылки на раннее").await;
    let body = get_entry(a, &w.rw.tok, &e).await["body"]
        .as_str()
        .unwrap()
        .to_string();
    let d2 = rows(&bdb, "SELECT id FROM attachment_docs", vec![]).await[0][0]
        .clone()
        .unwrap();
    let f2 = node_by_path(&bdb, "docs").await.unwrap();
    assert_eq!(body, format!("см. d:{d2} и n:{f2}"));
    assert_eq!(
        q1(&bdb, "SELECT count(*) FROM attachment_docs", vec![]).await,
        1
    );
}

/// Одно незавершённое задание у пользователя, не больше четырёх на сервере — иначе 503 с
/// `Retry-After`.
#[tokio::test]
async fn import_pending_limits_per_user_and_server() {
    let w = world("imp-pending").await;
    note(&w.a, &w.o.tok, G, "x", "текст").await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let hold = w.a.st.imports.hold_write();
    let r = post_tok(&w.a, &w.rw.tok, "knowledge", "", zip.clone()).await;
    ok(&r, StatusCode::ACCEPTED, "первое задание");
    let r2 = post_tok(&w.a, &w.rw.tok, "knowledge", "", zip.clone()).await;
    ok(
        &r2,
        StatusCode::SERVICE_UNAVAILABLE,
        "второе задание пользователя",
    );
    assert_eq!(r2.header("retry-after"), "30");
    // ещё трое пользователей — до предела сервера, пятый — 503
    let mut toks = vec![w.o.tok.clone(), w.ro.tok.clone()];
    let (_, t4) = w.a.new_user("Галина").await;
    let (_, t5) = w.a.new_user("Дмитрий").await;
    toks.push(t4);
    for t in &toks {
        let rr = post_tok(&w.a, t, "knowledge", "", zip.clone()).await;
        ok(&rr, StatusCode::ACCEPTED, "до предела");
    }
    assert_eq!(
        w.a.st.imports.running(),
        zam_server::api::transfer::import::MAX_PENDING
    );
    let r5 = post_tok(&w.a, &t5, "knowledge", "", zip.clone()).await;
    ok(
        &r5,
        StatusCode::SERVICE_UNAVAILABLE,
        "сверх предела сервера",
    );
    assert_eq!(r5.header("retry-after"), "30");
    drop(hold);
    report(&wait_job(&w.a, Who::Tok(&w.rw.tok), r).await);
    // итог получен — пользователь снова может импортировать
    let t0 = std::time::Instant::now();
    while w.a.st.imports.running() > 0 {
        assert!(t0.elapsed() < std::time::Duration::from_secs(60));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await);
}

/// Приём, тело которого никогда не кончается: заявленный `Content-Length` и пустой поток.
fn hanging_import(a: &Api, tok: &str, declared: u64) -> tokio::task::JoinHandle<Resp> {
    let app = a.app.clone();
    let tok = tok.to_string();
    tokio::spawn(async move {
        let body = Body::from_stream(futures_util::stream::pending::<
            Result<axum::body::Bytes, std::io::Error>,
        >());
        let req = Request::post("/api/v1/spaces/knowledge/import")
            .header(header::AUTHORIZATION, format!("Bearer {tok}"))
            .header(header::CONTENT_LENGTH, declared.to_string())
            .body(body)
            .unwrap();
        let r = tower::ServiceExt::oneshot(app, req).await.unwrap();
        Resp {
            status: r.status(),
            headers: r.headers().clone(),
            body: Default::default(),
        }
    })
}

/// Ждёт, пока резерв места станет `want`.
async fn reserved_is(a: &Api, want: u64, what: &str) {
    let t0 = std::time::Instant::now();
    while a.st.imports.disk_reserved() != want {
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(10),
            "{what}: резерв {} ≠ {want}",
            a.st.imports.disk_reserved()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Место с учётом резерва параллельного приёма (свободное место подменено): запас M, объём E,
/// свободно M + 3E. Первый приём резервирует 2E. Второй (другой пользователь) без резерва
/// поместился бы (M + 2E ≤ M + 3E), с резервом — нет с запасом в целый E (M + E < M + 2E):
/// 507 до чтения тела. Снят первый — второй принят и резервирует 2E.
#[tokio::test]
async fn import_refused_when_disk_low_with_reserve() {
    const M: u64 = 1 << 30;
    const E: u64 = 100 << 20;
    let w = world_cfg("imp-disk", |c| c.import.min_free_bytes = M).await;
    free(&w.a, Some(M + 3 * E));
    let first = hanging_import(&w.a, &w.rw.tok, E);
    reserved_is(&w.a, 2 * E, "первый приём").await;
    let second = hanging_import(&w.a, &w.o.tok, E);
    let r = tokio::time::timeout(std::time::Duration::from_secs(15), second)
        .await
        .expect("второй приём не отвергнут: резерв первого не учтён")
        .unwrap();
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(
        w.a.st.imports.disk_reserved(),
        2 * E,
        "резерв второго не остался"
    );
    first.abort();
    let _ = first.await;
    reserved_is(&w.a, 0, "снятие первого").await;
    let again = hanging_import(&w.a, &w.o.tok, E);
    reserved_is(&w.a, 2 * E, "второй после снятия первого").await;
    again.abort();
    let _ = again.await;
    reserved_is(&w.a, 0, "снятие второго").await;
    assert!(incoming_parts(&w.a).is_empty());
}

/// Запас по умолчанию (2 ГиБ): свободно ровно `2 ГиБ + 2 × тело` — принят, резерв снят после
/// задания; на байт меньше — 507; точка `import_disk_full` — 507.
#[tokio::test]
async fn import_default_disk_reserve_allows() {
    let w = world("imp-disk-default").await;
    assert_eq!(w.a.st.cfg.import.min_free_bytes, 2 << 30);
    note(&w.a, &w.o.tok, G, "x", "текст").await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let len = zip.len() as u64;
    let sized = |zip: Vec<u8>| {
        import_req("knowledge", "", Body::empty())
            .header(header::AUTHORIZATION, format!("Bearer {}", w.rw.tok))
            .header(header::CONTENT_LENGTH, len.to_string())
            .body(Body::from(zip))
            .unwrap()
    };
    free(&w.a, Some((2 << 30) + 2 * len - 1));
    let r = w.a.send(sized(zip.clone())).await;
    ok(&r, StatusCode::INSUFFICIENT_STORAGE, "на байт меньше");
    free(&w.a, Some((2 << 30) + 2 * len));
    let r = w.a.send(sized(zip.clone())).await;
    report(&wait_job(&w.a, Who::Tok(&w.rw.tok), r).await);
    assert_eq!(w.a.st.imports.disk_reserved(), 0);
    zam_server::backup::test_hooks::set_fail_point(&w.a.st.cfg.data_dir, Some("import_disk_full"));
    free(&w.a, Some(TEST_FREE));
    let r = post_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await;
    zam_server::backup::test_hooks::set_fail_point(&w.a.st.cfg.data_dir, None);
    ok(&r, StatusCode::INSUFFICIENT_STORAGE, "точка места");
    assert_eq!(w.a.st.imports.disk_reserved(), 0);
    assert_eq!(w.a.st.imports.running(), 0);
}

/// Тело потоком без `Content-Length` (chunked), с оценкой `X-Zam-Expected-Bytes`.
fn estimated(tok: &str, est: &str, zip: Vec<u8>) -> Request<Body> {
    let chunks: Vec<Result<axum::body::Bytes, std::io::Error>> = zip
        .chunks(64 << 10)
        .map(|c| Ok(axum::body::Bytes::copy_from_slice(c)))
        .collect();
    import_req("knowledge", "", Body::empty())
        .header(header::AUTHORIZATION, format!("Bearer {tok}"))
        .header("x-zam-expected-bytes", est)
        .body(Body::from_stream(futures_util::stream::iter(chunks)))
        .unwrap()
}

/// Оценка объёма каталога (`X-Zam-Expected-Bytes`): маленький поток при малом свободном месте
/// принят (резерв по оценке, а не по `max_bytes`); без оценки — 507; тело больше оценки с
/// запасом — 413; оценка больше `max_bytes` — 413 до тела; не число — 400.
#[tokio::test]
async fn import_estimate_header_reserves_and_limits() {
    let w = world_cfg("imp-estimate", |c| c.import.min_free_bytes = 0).await;
    note(&w.a, &w.o.tok, G, "x", "текст").await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    // места хватает на оценку, но не на 2 × max_bytes (2 ГиБ)
    free(&w.a, Some(64 << 20));
    let est = zip.len().to_string();
    let r = w.a.send(estimated(&w.rw.tok, &est, zip.clone())).await;
    report(&wait_job(&w.a, Who::Tok(&w.rw.tok), r).await);
    assert_eq!(w.a.st.imports.disk_reserved(), 0);
    let r = post_tok(&w.a, &w.rw.tok, "knowledge", "", zip.clone()).await;
    ok(
        &r,
        StatusCode::INSUFFICIENT_STORAGE,
        "без оценки — max_bytes",
    );
    // тело больше оценки с запасом (1/32 + 64 КиБ): 413, резерв снят
    free(&w.a, Some(TEST_FREE));
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let big: Vec<u8> = (0..(1 << 20))
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    let r = w.a.send(estimated(&w.rw.tok, "1000", big)).await;
    ok(&r, StatusCode::PAYLOAD_TOO_LARGE, "тело больше оценки");
    assert_eq!(w.a.st.imports.disk_reserved(), 0);
    assert!(incoming_parts(&w.a).is_empty());
    let rows = w.a.st.system.list_audit(100, None).await.unwrap();
    let d = rows
        .iter()
        .find(|r| r.action == "space.import.done" && r.details["reason"] == json!("too_large"))
        .expect("аудит too_large");
    assert_eq!(d.details["complete"], json!(false));
    let over = ((2u64 << 30) + 1).to_string();
    let r = w.a.send(estimated(&w.rw.tok, &over, zip.clone())).await;
    ok(&r, StatusCode::PAYLOAD_TOO_LARGE, "оценка больше max_bytes");
    let r = w.a.send(estimated(&w.rw.tok, "много", zip)).await;
    ok(&r, StatusCode::BAD_REQUEST, "оценка не число");
    assert_eq!(w.a.st.imports.running(), 0);
}

/// Отказ до приёма: клиенту с `Expect: 100-continue` тело не читается (hyper не шлёт `100`,
/// клиент тело не отправит), без `Expect` — дочитывается (ответ дойдёт до потокового клиента).
#[tokio::test]
async fn import_refusal_skips_drain_on_expect() {
    let w = world("imp-expect").await;
    zam_server::backup::test_hooks::set_fail_point(&w.a.st.cfg.data_dir, Some("import_disk_full"));
    for expect in [true, false] {
        let polled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let p = polled.clone();
        let body = Body::from_stream(futures_util::stream::once(async move {
            p.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"PK"))
        }));
        let mut req = Request::post("/api/v1/spaces/knowledge/import")
            .header(header::AUTHORIZATION, format!("Bearer {}", w.rw.tok));
        if expect {
            req = req.header(header::EXPECT, "100-continue");
        }
        let r = w.a.send(req.body(body).unwrap()).await;
        ok(&r, StatusCode::INSUFFICIENT_STORAGE, "отказ места");
        assert_eq!(
            polled.load(std::sync::atomic::Ordering::SeqCst),
            !expect,
            "expect={expect}"
        );
    }
    zam_server::backup::test_hooks::set_fail_point(&w.a.st.cfg.data_dir, None);
}

/// Паника внутри задания: задание кончается ошибкой 500, аудит `done` записан, слоты и учёт
/// задач свободны.
#[tokio::test]
async fn import_job_panic_reports_error() {
    let w = world("imp-panic").await;
    note(&w.a, &w.o.tok, G, "x", "текст").await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    zam_server::backup::test_hooks::set_fail_point(&w.a.st.cfg.data_dir, Some("import_panic"));
    let r = import_tok(&w.a, &w.rw.tok, "knowledge", "", zip.clone()).await;
    zam_server::backup::test_hooks::set_fail_point(&w.a.st.cfg.data_dir, None);
    ok(&r, StatusCode::INTERNAL_SERVER_ERROR, "паника задания");
    let t0 = std::time::Instant::now();
    loop {
        let rows = w.a.st.system.list_audit(100, None).await.unwrap();
        if let Some(d) = rows.iter().find(|r| r.action == "space.import.done") {
            assert_eq!(d.details["complete"], json!(false), "{:?}", d.details);
            break;
        }
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(10),
            "нет аудита done"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        w.a.st.imports.write_available(),
        zam_server::api::transfer::import::WRITE_SLOTS
    );
    assert_eq!(w.a.st.import_tasks.running(), 0);
    report(&import_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await);
}

/// Задание учитывается в задачах остановки (`import_tasks`), пока не кончилось.
#[tokio::test]
async fn import_job_counted_for_shutdown() {
    let w = world("imp-tasks").await;
    note(&w.a, &w.o.tok, G, "x", "текст").await;
    let zip = export_tok(&w.a, &w.o.tok, G, "").await;
    let hold = w.a.st.imports.hold_write();
    let r = post_tok(&w.a, &w.rw.tok, "knowledge", "", zip).await;
    ok(&r, StatusCode::ACCEPTED, "принято");
    assert_eq!(w.a.st.import_tasks.running(), 1);
    assert!(
        !w.a.st
            .wait_exports(std::time::Duration::from_millis(200))
            .await
    );
    drop(hold);
    report(&wait_job(&w.a, Who::Tok(&w.rw.tok), r).await);
    assert!(w.a.st.wait_exports(std::time::Duration::from_secs(5)).await);
}

/// Обработчик снят посреди приёма тела (клиент оборвал соединение — hyper бросает запрос):
/// страж пишет аудит `done` с `reason: client`, резерв задания снят, `.part` удалён.
#[tokio::test]
async fn import_disconnect_mid_body_audited() {
    let w = world("imp-midbody").await;
    let app = w.a.app.clone();
    let tok = w.rw.tok.clone();
    let hang = tokio::spawn(async move {
        let first: Result<axum::body::Bytes, std::io::Error> =
            Ok(axum::body::Bytes::from(vec![1u8; 100_000]));
        let body = Body::from_stream(futures_util::StreamExt::chain(
            futures_util::stream::iter([first]),
            futures_util::stream::pending(),
        ));
        let req = Request::post("/api/v1/spaces/knowledge/import")
            .header(header::AUTHORIZATION, format!("Bearer {tok}"))
            .body(body)
            .unwrap();
        let _ = tower::ServiceExt::oneshot(app, req).await;
    });
    let t0 = std::time::Instant::now();
    while incoming_parts(&w.a).is_empty() {
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(10),
            "приём не начался"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        w.a.st.imports.running(),
        1,
        "резерв задания на время приёма"
    );
    hang.abort();
    let _ = hang.await;
    let t0 = std::time::Instant::now();
    loop {
        let rows = w.a.st.system.list_audit(100, None).await.unwrap();
        if let Some(d) = rows.iter().find(|r| r.action == "space.import.done") {
            assert_eq!(d.details["reason"], json!("client"), "{:?}", d.details);
            assert_eq!(d.details["complete"], json!(false));
            break;
        }
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(10),
            "нет аудита done"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(w.a.st.imports.running(), 0, "резерв задания не снят");
    assert!(incoming_parts(&w.a).is_empty());
}
