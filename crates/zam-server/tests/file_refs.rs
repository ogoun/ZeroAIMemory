//! Ссылки знаний на файлы решения (M4.5 §6.1–6.3, задача 7): нормализация путей, ссылки
//! записей журналов и заметок, устарелость по хэшу (`POST /files/changed`), подтверждение
//! (`POST /files/verified`), индекс для хуков (`GET /file-refs`, `ETag`/304), пометка
//! `stale_refs` в выдаче `/search` и `/read`, `filters.file` и `GET /graph?file=`, права.
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e78";

fn sol_entries() -> String {
    format!("/api/v1/spaces/{G}/entries")
}

fn h(c: char) -> String {
    c.to_string().repeat(64)
}

fn id(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("нет id: {v}"))
        .to_string()
}

/// Сервер с решением `G` у администратора.
async fn setup(name: &str) -> Api {
    let a = api(name, |_| {}).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Проект"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    a
}

async fn created(r: Resp) -> Value {
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()
}

/// Insight в решении со ссылками `refs`.
async fn insight(a: &Api, title: &str, refs: Value) -> String {
    let v = created(
        a.t(
            Method::POST,
            &sol_entries(),
            Some(
                json!({"kind": "insight", "title": title, "body": format!("{title}: подробности"),
                        "data": {"kind": "gotcha"}, "refs": refs, "force": true}),
            ),
        )
        .await,
    )
    .await;
    id(&v)
}

async fn changed(a: &Api, files: Value) -> Resp {
    a.t(
        Method::POST,
        "/api/v1/files/changed",
        Some(json!({"solution": G, "files": files})),
    )
    .await
}

async fn index(a: &Api) -> Value {
    let r = a
        .t(
            Method::GET,
            &format!("/api/v1/file-refs?solution={G}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Элемент индекса пути `path` или `Null`.
fn path_of<'a>(idx: &'a Value, path: &str) -> &'a Value {
    idx["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["path"] == path)
        .unwrap_or(&Value::Null)
}

/// Состояние ссылки записи `ref_id` на путь `path` в индексе.
fn state(idx: &Value, path: &str, ref_id: &str) -> String {
    path_of(idx, path)["items"]
        .as_array()
        .unwrap_or_else(|| panic!("нет пути {path}: {idx}"))
        .iter()
        .find(|i| i["id"] == ref_id)
        .unwrap_or_else(|| panic!("нет {ref_id} у {path}: {idx}"))["state"]
        .as_str()
        .unwrap()
        .to_string()
}

fn unprocessable(r: &Resp, needle: &str) {
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert!(r.text().contains(needle), "нет «{needle}»: {}", r.text());
}

#[tokio::test]
async fn file_refs_path_normalization() {
    let a = setup("refs-norm").await;
    // принятые формы сводятся к одному пути
    let i = insight(
        &a,
        "Нормализация",
        json!({"files": [{"path": "a\\b.rs"}, {"path": "./c/d.rs"}, {"path": "x/../e.rs"}]}),
    )
    .await;
    let idx = index(&a).await;
    for p in ["a/b.rs", "c/d.rs", "e.rs"] {
        assert_eq!(state(&idx, p, &format!("i:{i}")), "ok", "{p}: {idx}");
    }
    let long = "я".repeat(1001);
    for bad in [
        "/abs",
        "../x",
        "a/../../x",
        "C:\\x",
        "c:/x",
        "\\\\server\\share\\f",
        "",
        "   ",
        "./",
        long.as_str(),
        "a\u{0}b",
        "a\u{1}b",
        "a\tb",
        "a\u{7f}b",
    ] {
        let r = changed(&a, json!([{"path": bad, "hash": h('a')}])).await;
        unprocessable(&r, "files[0]");
        let r = a
            .t(
                Method::POST,
                &sol_entries(),
                Some(
                    json!({"kind": "insight", "title": "Плохой путь", "data": {"kind": "gotcha"},
                            "refs": {"files": [{"path": "ok.rs"}, {"path": bad}]}}),
                ),
            )
            .await;
        unprocessable(&r, "refs.files[1].path");
    }
    // 1000 символов — можно
    let ok = "я".repeat(1000);
    let r = changed(&a, json!([{"path": ok, "hash": h('a')}])).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // хэш обязателен у /files/changed и проверяется
    unprocessable(
        &changed(&a, json!([{"path": "a.rs"}])).await,
        "files[0].hash",
    );
    unprocessable(
        &changed(&a, json!([{"path": "a.rs", "hash": "xyz"}])).await,
        "files[0].hash",
    );
}

#[tokio::test]
async fn refs_on_insight_and_changelog_strings() {
    let a = setup("refs-entries").await;
    let i = insight(
        &a,
        "Кэш токенизатора",
        json!({"files": [{"path": "src/a.rs", "hash": h('A'), "commit": "abc1234"}]}),
    )
    .await;
    let c = created(
        a.t(
            Method::POST,
            &sol_entries(),
            Some(json!({"kind": "changelog", "title": "Правка",
                        "data": {"kind": "fix", "refs": {"files": ["src/b.rs", "../bad", "src/c.rs"]}},
                        "refs": {"files": [{"path": "src/c.rs", "hash": h('c')}]}})),
        )
        .await,
    )
    .await;
    let c = id(&c);
    let idx = index(&a).await;
    let a_path = path_of(&idx, "src/a.rs");
    assert_eq!(a_path["hash"], h('a'), "хэш в нижнем регистре: {idx}");
    assert_eq!(a_path["items"][0]["id"], format!("i:{i}"));
    assert_eq!(a_path["items"][0]["kind"], "insight");
    assert_eq!(a_path["items"][0]["title"], "Кэш токенизатора");
    // строки data.refs.files — ссылки без хэша; ненормализуемая пропущена
    let b_path = path_of(&idx, "src/b.rs");
    assert_eq!(b_path["items"][0]["id"], format!("c:{c}"));
    assert!(b_path.get("hash").is_none(), "{idx}");
    // путь и в refs, и в data — один, с хэшем запроса
    assert_eq!(path_of(&idx, "src/c.rs")["hash"], h('c'));
    assert_eq!(
        path_of(&idx, "src/c.rs")["items"].as_array().unwrap().len(),
        1
    );
    assert_eq!(idx["refs"].as_array().unwrap().len(), 3, "{idx}");
    // PATCH с refs заменяет набор (строки data остаются)
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{c}"),
            Some(json!({"refs": {"files": [{"path": "src/d.rs"}]}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let idx = index(&a).await;
    assert!(path_of(&idx, "src/d.rs").is_object(), "{idx}");
    assert!(path_of(&idx, "src/b.rs").is_object(), "{idx}");
    assert!(
        path_of(&idx, "src/c.rs").is_object(),
        "строка data.refs.files: {idx}"
    );
    // PATCH data changelog без refs — новые строки дописываются
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{c}"),
            Some(json!({"data": {"kind": "fix", "refs": {"files": ["src/e.rs"]}}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(path_of(&index(&a).await, "src/e.rs").is_object());
    // ссылки удалённой записи в индекс не попадают
    let r = a
        .tok_rev(
            &a.token,
            Method::DELETE,
            &format!("/api/v1/entries/{i}"),
            None,
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    assert!(path_of(&index(&a).await, "src/a.rs").is_null());
}

#[tokio::test]
async fn note_frontmatter_refs_and_verified_at() {
    let a = setup("refs-note").await;
    let content = "---\nrefs: [src/a.rs, ../x]\nverified_at: 2026-10-28\n\
                   verified_at_commit: abc1234\n---\n# Сборка\n\ncargo build --release\n";
    let r = a
        .t(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": G, "path": "Команды/Сборка", "kind": "note", "content": content})),
        )
        .await;
    let n = created(r).await;
    let w = n["warnings"].to_string();
    assert!(w.contains("invalid_ref: ../x"), "{n}");
    let nid = id(&n);
    let idx = index(&a).await;
    assert_eq!(state(&idx, "src/a.rs", &format!("n:{nid}")), "ok");
    assert_eq!(path_of(&idx, "src/a.rs")["items"][0]["kind"], "note");
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let nid2 = nid.clone();
    let (at, commit, fm): (Option<String>, Option<String>, String) = db
        .read(move |c| {
            Ok(c.query_row(
                "SELECT verified_at, verified_at_commit, frontmatter FROM nodes WHERE id = ?1",
                [nid2],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(at.as_deref(), Some("2026-10-28"));
    assert_eq!(commit.as_deref(), Some("abc1234"));
    let fm: Value = serde_json::from_str(&fm).unwrap();
    assert_eq!(fm["refs"], json!(["src/a.rs"]));
    assert_eq!(fm["verified_at"], "2026-10-28");
    // ревизия без refs и с неверной датой: ссылки сняты, verified_at — NULL, предупреждение
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{nid}"),
            Some(json!({"content": "---\nverified_at: вчера\n---\n# Сборка\n"})),
            &[("if-match", &n["current_rev"].to_string())],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("verified_at_invalid"), "{}", r.text());
    assert!(path_of(&index(&a).await, "src/a.rs").is_null());
    let at: Option<String> = db
        .read(move |c| {
            Ok(
                c.query_row("SELECT verified_at FROM nodes WHERE id = ?1", [nid], |r| {
                    r.get(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(at, None);
}

#[tokio::test]
async fn files_changed_marks_stale_hash_differs() {
    let a = setup("refs-stale").await;
    let i = insight(
        &a,
        "Пул",
        json!({"files": [{"path": "src/pool.rs", "hash": h('1')}]}),
    )
    .await;
    let r = changed(&a, json!([{"path": "./src/pool.rs", "hash": h('2')}])).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["stale"], 1);
    assert_eq!(
        state(&index(&a).await, "src/pool.rs", &format!("i:{i}")),
        "stale"
    );
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let reason: String = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT stale_reason FROM file_refs WHERE path = 'src/pool.rs'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(reason, "source_changed");
    // чужой путь и чужое решение не трогаются
    let r = changed(&a, json!([{"path": "src/other.rs", "hash": h('2')}])).await;
    assert_eq!(r.json()["stale"], 0);
}

#[tokio::test]
async fn files_changed_same_hash_ok() {
    let a = setup("refs-same").await;
    let i = insight(
        &a,
        "Пул",
        json!({"files": [{"path": "src/pool.rs", "hash": h('1')}]}),
    )
    .await;
    let r = changed(&a, json!([{"path": "src/pool.rs", "hash": h('1')}])).await;
    assert_eq!(r.json()["stale"], 0, "{}", r.text());
    let rf = format!("i:{i}");
    assert_eq!(state(&index(&a).await, "src/pool.rs", &rf), "ok");
    // изменился и вернулся к записанному — снова ok
    assert_eq!(
        changed(&a, json!([{"path": "src/pool.rs", "hash": h('2')}]))
            .await
            .json()["stale"],
        1
    );
    assert_eq!(
        changed(&a, json!([{"path": "src/pool.rs", "hash": h('1')}]))
            .await
            .json()["stale"],
        0
    );
    assert_eq!(state(&index(&a).await, "src/pool.rs", &rf), "ok");
}

#[tokio::test]
async fn ref_without_hash_stale_on_change_after_record() {
    let a = setup("refs-nohash").await;
    let i = insight(&a, "Без хэша", json!({"files": [{"path": "src/x.rs"}]})).await;
    let r = changed(&a, json!([{"path": "src/x.rs", "hash": h('9')}])).await;
    assert_eq!(r.json()["stale"], 1, "{}", r.text());
    assert_eq!(
        state(&index(&a).await, "src/x.rs", &format!("i:{i}")),
        "stale"
    );
    // сообщение не позже записи ссылки устарелости не даёт
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    db.write(|tx| {
        tx.execute(
            "UPDATE file_refs SET state = 'ok', stale_at = NULL, stale_reason = NULL,
                    recorded_at = '2999-01-01T00:00:00.000000Z'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let r = changed(&a, json!([{"path": "src/x.rs", "hash": h('9')}])).await;
    assert_eq!(r.json()["stale"], 0, "{}", r.text());
}

#[tokio::test]
async fn rewrite_with_new_hash_resets_ok() {
    let a = setup("refs-rewrite").await;
    let i = insight(
        &a,
        "Пул",
        json!({"files": [{"path": "src/pool.rs", "hash": h('1')}]}),
    )
    .await;
    changed(&a, json!([{"path": "src/pool.rs", "hash": h('2')}])).await;
    let rf = format!("i:{i}");
    assert_eq!(state(&index(&a).await, "src/pool.rs", &rf), "stale");
    // правка записи с теми же путями и новым хэшем — ok
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{i}"),
            Some(json!({"refs": {"files": [{"path": "src/pool.rs", "hash": h('2')}]}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let idx = index(&a).await;
    assert_eq!(state(&idx, "src/pool.rs", &rf), "ok");
    assert_eq!(path_of(&idx, "src/pool.rs")["hash"], h('2'));
    // и заметка: перезапись с теми же путями — ok
    let n = a
        .note(G, "Пул", "---\nrefs: [src/pool.rs]\n---\nтекст\n")
        .await;
    let nf = format!("n:{}", id(&n));
    changed(&a, json!([{"path": "src/pool.rs", "hash": h('3')}])).await;
    assert_eq!(state(&index(&a).await, "src/pool.rs", &nf), "stale");
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{}", id(&n)),
            Some(json!({"content": "---\nrefs: [src/pool.rs]\n---\nновый текст\n"})),
            &[("if-match", &n["current_rev"].to_string())],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(state(&index(&a).await, "src/pool.rs", &nf), "ok");
}

#[tokio::test]
async fn files_verified() {
    let a = setup("refs-verified").await;
    let i = insight(
        &a,
        "Пул",
        json!({"files": [{"path": "src/pool.rs", "hash": h('1')}]}),
    )
    .await;
    changed(&a, json!([{"path": "src/pool.rs", "hash": h('2')}])).await;
    let rf = format!("i:{i}");
    assert_eq!(state(&index(&a).await, "src/pool.rs", &rf), "stale");
    let r = a
        .t(
            Method::POST,
            "/api/v1/files/verified",
            Some(json!({"solution": G, "files": [{"path": "src\\pool.rs", "hash": h('2')}]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["verified"], 1);
    let idx = index(&a).await;
    assert_eq!(state(&idx, "src/pool.rs", &rf), "ok");
    assert_eq!(path_of(&idx, "src/pool.rs")["hash"], h('2'));
    // новый хэш стал записанным: тот же — ok
    assert_eq!(
        changed(&a, json!([{"path": "src/pool.rs", "hash": h('2')}]))
            .await
            .json()["stale"],
        0
    );
    // не GUID — 422, лишнее поле — 422
    let r = a
        .t(
            Method::POST,
            "/api/v1/files/verified",
            Some(json!({"solution": "knowledge", "files": []})),
        )
        .await;
    unprocessable(&r, "solution");
    let r = a
        .t(
            Method::POST,
            "/api/v1/files/verified",
            Some(json!({"solution": G, "files": [{"path": "a", "x": 1}]})),
        )
        .await;
    unprocessable(&r, "files[0].x");
}

#[tokio::test]
async fn refs_index_etag_304() {
    let a = setup("refs-etag").await;
    insight(
        &a,
        "Пул",
        json!({"files": [{"path": "src/pool.rs", "hash": h('1')}]}),
    )
    .await;
    let uri = format!("/api/v1/file-refs?solution={G}");
    let r = a.t(Method::GET, &uri, None).await;
    assert_eq!(r.status, StatusCode::OK);
    let etag = r.json()["etag"].as_str().unwrap().to_string();
    assert_eq!(r.header("etag"), format!("\"{etag}\""));
    let quoted = format!("\"{etag}\"");
    let r = a
        .tok_h(
            &a.token,
            Method::GET,
            &uri,
            None,
            &[("if-none-match", &quoted)],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    assert!(r.body.is_empty());
    // заголовок X-Zam-Solution вместо параметра
    let r = a
        .tok_h(
            &a.token,
            Method::GET,
            "/api/v1/file-refs",
            None,
            &[("x-zam-solution", G), ("if-none-match", &quoted)],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    // изменение состояния меняет ETag
    changed(&a, json!([{"path": "src/pool.rs", "hash": h('2')}])).await;
    let r = a
        .tok_h(
            &a.token,
            Method::GET,
            &uri,
            None,
            &[("if-none-match", &quoted)],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_ne!(r.json()["etag"], etag.as_str());
    // без решения — 400
    let r = a.t(Method::GET, "/api/v1/file-refs", None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

#[tokio::test]
async fn search_card_stale_refs() {
    let a = setup("refs-search").await;
    let i = insight(
        &a,
        "Кэш токенизатора тормозит",
        json!({"files": [{"path": "src/tok.rs", "hash": h('1')}, {"path": "src/ok.rs", "hash": h('1')}]}),
    )
    .await;
    let search = |fmt: &'static str| {
        let a = &a;
        async move {
            a.t(
                Method::POST,
                "/api/v1/search",
                Some(json!({"query": "токенизатора", "scope": {"solution": G}, "format": fmt})),
            )
            .await
        }
    };
    // пока не устарело — поля нет
    let r = search("json").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let item = r.json()["items"][0].clone();
    assert_eq!(item["entry_id"], i.as_str(), "{item}");
    assert!(item.get("stale_refs").is_none(), "{item}");
    changed(&a, json!([{"path": "src/tok.rs", "hash": h('2')}])).await;
    let item = search("json").await.json()["items"][0].clone();
    assert_eq!(item["stale_refs"], json!(["src/tok.rs"]), "{item}");
    let md = search("md").await.text();
    assert!(
        md.contains("возможно устарело: изменился src/tok.rs"),
        "{md}"
    );
    // /read: json и md
    let read = |fmt: &'static str| {
        let a = &a;
        let i = i.clone();
        async move {
            a.t(
                Method::POST,
                "/api/v1/read",
                Some(json!({"items": [{"id": format!("i:{i}")}], "format": fmt})),
            )
            .await
        }
    };
    let r = read("json").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["items"][0]["stale_refs"], json!(["src/tok.rs"]));
    let md = read("md").await.text();
    assert!(
        md.contains("(возможно устарело: изменился src/tok.rs)"),
        "{md}"
    );
}

#[tokio::test]
async fn filters_file() {
    let a = setup("refs-filter").await;
    let with = insight(
        &a,
        "Пул соединений A",
        json!({"files": [{"path": "src/pool.rs"}]}),
    )
    .await;
    let without = insight(&a, "Пул соединений B", json!(null)).await;
    let n = a
        .note(
            G,
            "Заметка о пуле",
            "---\nrefs: [src/pool.rs]\n---\nПул соединений и тайм-ауты.\n",
        )
        .await;
    let found = |file: &'static str| {
        let a = &a;
        async move {
            let r = a
                .t(
                    Method::POST,
                    "/api/v1/search",
                    Some(json!({"query": "соединений", "scope": {"solution": G},
                                "filters": {"file": file}})),
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            let ids: Vec<String> = r.json()["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| {
                    i["entry_id"]
                        .as_str()
                        .or(i["node_id"].as_str())
                        .unwrap()
                        .to_string()
                })
                .collect();
            ids
        }
    };
    let ids = found(".\\src\\pool.rs").await;
    assert!(ids.contains(&with), "{ids:?}");
    assert!(ids.contains(&id(&n)), "{ids:?}");
    assert!(!ids.contains(&without), "{ids:?}");
    assert!(found("src/other.rs").await.is_empty());
    // некорректный путь — 400
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "пул", "filters": {"file": "../x"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // граф: тот же список
    let r = a
        .t(
            Method::GET,
            &format!("/api/v1/graph?file=src%2Fpool.rs&solution={G}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let g = r.json();
    let refs: Vec<&str> = g["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["ref"].as_str().unwrap())
        .collect();
    assert!(refs.contains(&format!("e:{with}").as_str()), "{g}");
    assert!(refs.contains(&format!("n:{}", id(&n)).as_str()), "{g}");
    assert_eq!(refs.len(), 2, "{g}");
    assert_eq!(g["edges"], json!([]));
    let r = a.t(Method::GET, "/api/v1/graph?file=%2Fabs", None).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
}

#[tokio::test]
async fn participant_ro_cannot_report_changes_403() {
    let a = api("refs-ro", |_| {}).await;
    let (a_id, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Общее"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = a
        .tok(
            &b_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Общее"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let approve = |access: &'static str| {
        let (a, a_tok, b_id) = (&a, a_tok.clone(), b_id.clone());
        async move {
            let r = a
                .tok(
                    &a_tok,
                    Method::POST,
                    &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
                    Some(json!({"access": access})),
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        }
    };
    approve("ro").await;
    // ссылка владельца в решении
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &sol_entries(),
            Some(
                json!({"kind": "insight", "title": "Пул", "data": {"kind": "gotcha"},
                        "refs": {"files": [{"path": "src/pool.rs", "hash": h('1')}]}}),
            ),
        )
        .await;
    let iid = id(&created(r).await);
    let body = json!({"solution": G, "files": [{"path": "src/pool.rs", "hash": h('2')}]});
    for uri in ["/api/v1/files/changed", "/api/v1/files/verified"] {
        let r = a.tok(&b_tok, Method::POST, uri, Some(body.clone())).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{uri}: {}", r.text());
    }
    // ro-токен владельца — тоже 403
    let ro = a.issue(&a_id, "ro").await;
    let r = a
        .tok(
            &ro,
            Method::POST,
            "/api/v1/files/changed",
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // участник ro индекс читает (только пространство решения)
    let r = a
        .tok(
            &b_tok,
            Method::GET,
            &format!("/api/v1/file-refs?solution={G}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(state(&r.json(), "src/pool.rs", &format!("i:{iid}")), "ok");
    // участник rw — может; ссылка в базе владельца
    approve("rw").await;
    let r = a
        .tok(&b_tok, Method::POST, "/api/v1/files/changed", Some(body))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["stale"], 1);
}

#[tokio::test]
async fn refs_max_64_422() {
    let a = setup("refs-max").await;
    let files = |n: usize| -> Value {
        json!({"files": (0..n).map(|i| json!({"path": format!("src/f{i}.rs")})).collect::<Vec<_>>()})
    };
    let r = a
        .t(
            Method::POST,
            &sol_entries(),
            Some(
                json!({"kind": "insight", "title": "Много", "data": {"kind": "gotcha"},
                        "refs": files(65)}),
            ),
        )
        .await;
    unprocessable(&r, "refs.files: не больше 64");
    let i = insight(&a, "Ровно 64", files(64)).await;
    assert_eq!(index(&a).await["refs"].as_array().unwrap().len(), 64);
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{i}"),
            Some(json!({"refs": files(65)})),
        )
        .await;
    unprocessable(&r, "refs.files");
    // сообщение — до 500 файлов
    let many: Vec<Value> = (0..501)
        .map(|i| json!({"path": format!("f{i}"), "hash": h('1')}))
        .collect();
    unprocessable(&changed(&a, json!(many)).await, "files: не больше 500");
}

#[tokio::test]
async fn refs_backfill_on_open() {
    let c = common::cfg_in(common::tmp("refs-backfill"));
    let a = common::api::api_with(c.clone()).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Проект"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let n = a
        .note(
            G,
            "Команды/Тесты",
            "---\nrefs: [src/a.rs]\nverified_at: 2026-10-01\n---\nтекст\n",
        )
        .await;
    let ch = created(
        a.t(
            Method::POST,
            &sol_entries(),
            Some(json!({"kind": "changelog", "title": "Правка",
                        "data": {"kind": "fix", "refs": {"files": ["src/b.rs"]}}})),
        )
        .await,
    )
    .await;
    // база «до миграции 0016»: ссылок и verified_at нет, флаг дозаполнения стоит
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    db.write(|tx| {
        tx.execute("DELETE FROM file_refs", [])?;
        tx.execute("UPDATE nodes SET verified_at = NULL", [])?;
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('refs_backfill', 'pending')",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    a.st.shutdown().await;
    drop(db);
    drop(a);

    let b = common::api::api_with(c).await;
    let token = b.issue(&b.admin_id(), "rw").await;
    let r = b
        .tok(
            &token,
            Method::GET,
            &format!("/api/v1/file-refs?solution={G}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let idx = r.json();
    assert_eq!(state(&idx, "src/a.rs", &format!("n:{}", id(&n))), "ok");
    assert_eq!(state(&idx, "src/b.rs", &format!("c:{}", id(&ch))), "ok");
    let db = b.st.users.get(b.st.admin_id).await.unwrap();
    let (at, flag): (Option<String>, bool) = db
        .read(|c| {
            Ok((
                c.query_row(
                    "SELECT verified_at FROM nodes WHERE kind = 'note'",
                    [],
                    |r| r.get(0),
                )?,
                c.query_row("SELECT 1 FROM meta WHERE key = 'refs_backfill'", [], |_| {
                    Ok(())
                })
                .is_ok(),
            ))
        })
        .await
        .unwrap();
    assert_eq!(at.as_deref(), Some("2026-10-01"));
    assert!(!flag, "флаг дозаполнения снят");
}
