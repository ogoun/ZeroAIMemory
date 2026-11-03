//! Связь карты кода с памятью (M5.6 задача 6, спецификация §8, §10, §17 п. 4, 6): ссылки
//! `путь#qual` записей и заметок, устарелость по символу (`POST /files/changed` с `symbols`),
//! подтверждение с символами (`POST /files/verified`), символ в индексе для хуков и в
//! `GET /graph?file=`, Knowledge участника, невидимость символов в поиске и подсказках, тип
//! подсказок `code_map`.
//!
//! Решение `G` — у администратора (первый токен). Сервер без моделей.
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api, enc};
use serde_json::{Value, json};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e66";

fn sol_entries() -> String {
    format!("/api/v1/spaces/{G}/entries")
}

/// Хэш файла (64 hex) по номеру.
fn h(n: u64) -> String {
    format!("{n:064x}")
}

/// Хэш тела (16 hex) по номеру.
fn bh(n: u64) -> String {
    format!("{n:016x}")
}

fn ok(r: &Resp, s: StatusCode, ctx: &str) {
    assert_eq!(r.status, s, "{ctx}: {}", r.text());
}

fn unprocessable(r: &Resp, needle: &str) {
    ok(r, StatusCode::UNPROCESSABLE_ENTITY, needle);
    assert!(r.text().contains(needle), "нет «{needle}»: {}", r.text());
}

async fn setup(name: &str) -> Api {
    let a = api(&format!("codemem-{name}"), |c| c.recall.timeout_ms = 30_000).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Память и код"})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "решение");
    a
}

fn id(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("нет id: {v}"))
        .to_string()
}

/// Insight в решении со ссылками `refs`.
async fn insight(a: &Api, title: &str, refs: Value) -> String {
    let r = a
        .t(
            Method::POST,
            &sol_entries(),
            Some(
                json!({"kind": "insight", "title": title, "body": format!("{title}: подробно"),
                        "data": {"kind": "gotcha"}, "refs": refs, "force": true}),
            ),
        )
        .await;
    ok(&r, StatusCode::CREATED, title);
    id(&r.json())
}

/// Заметка в решении со строками `refs:` во frontmatter; ответ записи.
async fn note(a: &Api, path: &str, refs: &[&str]) -> Value {
    let list: Vec<String> = refs.iter().map(|r| format!("\"{r}\"")).collect();
    let content = format!(
        "---\nrefs: [{}]\n---\n# {path}\n\nтекст о коде\n",
        list.join(", ")
    );
    a.note(G, path, &content).await
}

/// Символ `(qual, тело)` в теле сообщения.
fn syms(list: &[(&str, u64)]) -> Value {
    Value::from(
        list.iter()
            .map(|(q, b)| json!({"qual": q, "body_hash": bh(*b)}))
            .collect::<Vec<_>>(),
    )
}

async fn changed_tok(a: &Api, tok: &str, files: Value) -> Value {
    let r = a
        .tok(
            tok,
            Method::POST,
            "/api/v1/files/changed",
            Some(json!({"solution": G, "files": files})),
        )
        .await;
    ok(&r, StatusCode::OK, "files/changed");
    r.json()
}

async fn changed(a: &Api, files: Value) -> Value {
    changed_tok(a, &a.token, files).await
}

/// Ссылки владельца `owner` (id записи или заметки) в базе пользователя `user`:
/// `(путь, символ, состояние, причина, хэш тела)` по пути и символу.
type Row = (
    String,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
);

async fn rows_of(a: &Api, user: zam_core::Id, owner: &str) -> Vec<Row> {
    let db = a.st.users.get(user).await.unwrap();
    let owner = owner.to_string();
    db.read(move |c| {
        let mut st = c.prepare(
            "SELECT path, symbol, state, stale_reason, symbol_hash FROM file_refs
             WHERE owner_id = ?1 ORDER BY path, coalesce(symbol, '')",
        )?;
        let rows = st
            .query_map([owner], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<Vec<Row>, _>>()?;
        Ok(rows)
    })
    .await
    .unwrap()
}

async fn rows(a: &Api, owner: &str) -> Vec<Row> {
    rows_of(a, a.st.admin_id, owner).await
}

/// `(состояние, причина)` ссылки владельца на `(путь, символ)`.
async fn state(a: &Api, owner: &str, path: &str, symbol: Option<&str>) -> (String, Option<String>) {
    let all = rows(a, owner).await;
    let r = all
        .iter()
        .find(|r| r.0 == path && r.1.as_deref() == symbol)
        .unwrap_or_else(|| panic!("нет ссылки {path}#{symbol:?}: {all:?}"));
    (r.2.clone(), r.3.clone())
}

fn st(s: &str, reason: Option<&str>) -> (String, Option<String>) {
    (s.to_string(), reason.map(str::to_string))
}

async fn index(a: &Api) -> Value {
    let r = a
        .t(
            Method::GET,
            &format!("/api/v1/file-refs?solution={G}"),
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "file-refs");
    r.json()
}

/// §17 п. 4: заметка `refs: ["src/a.rs#foo"]` и запись с файловой ссылкой на `src/a.rs`.
async fn scenario(a: &Api) -> (String, String) {
    let n = id(&note(a, "Код/Фу", &["src/a.rs#foo"]).await);
    let i = insight(
        a,
        "Файл целиком",
        json!({"files": [{"path": "src/a.rs", "hash": h(1)}]}),
    )
    .await;
    (n, i)
}

#[tokio::test]
async fn files_changed_symbols_rule_via_rest() {
    let a = setup("rule").await;
    let (n, i) = scenario(&a).await;
    // ориентир: первый хэш файла и тела символов (у символьной ссылки хэшей не было)
    let j = changed(
        &a,
        json!([{"path": "src/a.rs", "hash": h(1), "symbols": syms(&[("foo", 1), ("bar", 2)])}]),
    )
    .await;
    assert_eq!(j["stale"], 0, "{j}");
    assert_eq!(
        rows(&a, &n).await[0].4.as_deref(),
        Some(bh(1).as_str()),
        "ориентир тела"
    );
    // правка `bar`: файловая — stale, символьная — ok
    let j = changed(
        &a,
        json!([{"path": "src/a.rs", "hash": h(2), "symbols": syms(&[("foo", 1), ("bar", 3)])}]),
    )
    .await;
    assert_eq!(j["stale"], 1, "{j}");
    assert_eq!(
        state(&a, &i, "src/a.rs", None).await,
        st("stale", Some("source_changed"))
    );
    assert_eq!(state(&a, &n, "src/a.rs", Some("foo")).await, st("ok", None));
    // правка `foo` — symbol_changed
    changed(
        &a,
        json!([{"path": "src/a.rs", "hash": h(3), "symbols": syms(&[("foo", 9), ("bar", 3)])}]),
    )
    .await;
    assert_eq!(
        state(&a, &n, "src/a.rs", Some("foo")).await,
        st("stale", Some("symbol_changed"))
    );
    // удаление `foo` — symbol_removed
    changed(
        &a,
        json!([{"path": "src/a.rs", "hash": h(4), "symbols": syms(&[("bar", 3)])}]),
    )
    .await;
    assert_eq!(
        state(&a, &n, "src/a.rs", Some("foo")).await,
        st("stale", Some("symbol_removed"))
    );
    // без `symbols` — по хэшу файла, как в M4.5 (тело не известно — символ не судится)
    let n2 = id(&note(&a, "Код/Бар", &["src/b.rs#bar"]).await);
    changed(&a, json!([{"path": "src/b.rs", "hash": h(5)}])).await;
    changed(&a, json!([{"path": "src/b.rs", "hash": h(6)}])).await;
    assert_eq!(
        state(&a, &n2, "src/b.rs", Some("bar")).await,
        st("stale", Some("source_changed"))
    );
    // удалённый файл: символьная — symbol_removed
    let n3 = id(&note(&a, "Код/Баз", &["src/c.rs#baz"]).await);
    changed(&a, json!([{"path": "src/c.rs", "hash": "0".repeat(64)}])).await;
    assert_eq!(
        state(&a, &n3, "src/c.rs", Some("baz")).await,
        st("stale", Some("symbol_removed"))
    );
    // валидация символов
    for (bad, needle) in [
        (json!([{"qual": "a/b", "body_hash": bh(1)}]), "qual"),
        (json!([{"qual": "a", "body_hash": "zz"}]), "body_hash"),
        (
            json!([{"qual": "a", "body_hash": bh(1), "x": 1}]),
            "неизвестное поле",
        ),
        (json!("foo"), "ожидается массив"),
    ] {
        let r = a
            .t(
                Method::POST,
                "/api/v1/files/changed",
                Some(
                    json!({"solution": G, "files": [{"path": "a.rs", "hash": h(1),
                                                      "symbols": bad}]}),
                ),
            )
            .await;
        unprocessable(&r, needle);
    }
}

#[tokio::test]
async fn files_verified_with_symbols() {
    let a = setup("verified").await;
    let (n, _) = scenario(&a).await;
    changed(
        &a,
        json!([{"path": "src/a.rs", "hash": h(1), "symbols": syms(&[("foo", 1)])}]),
    )
    .await;
    changed(
        &a,
        json!([{"path": "src/a.rs", "hash": h(2), "symbols": syms(&[("foo", 2)])}]),
    )
    .await;
    assert_eq!(
        state(&a, &n, "src/a.rs", Some("foo")).await,
        st("stale", Some("symbol_changed"))
    );
    let r = a
        .t(
            Method::POST,
            "/api/v1/files/verified",
            Some(
                json!({"solution": G, "files": [{"path": "src/a.rs", "hash": h(2),
                                                  "symbols": syms(&[("foo", 2), ("bar", 7)])}]}),
            ),
        )
        .await;
    ok(&r, StatusCode::OK, "verified");
    assert_eq!(r.json()["verified"], 2, "{}", r.text());
    let row = rows(&a, &n).await;
    assert_eq!(
        (row[0].2.as_str(), row[0].4.as_deref()),
        ("ok", Some(bh(2).as_str()))
    );
    // новое тело — ориентир: то же тело снова ok
    let j = changed(
        &a,
        json!([{"path": "src/a.rs", "hash": h(3), "symbols": syms(&[("foo", 2)])}]),
    )
    .await;
    assert_eq!(
        state(&a, &n, "src/a.rs", Some("foo")).await,
        st("ok", None),
        "{j}"
    );
}

#[tokio::test]
async fn entry_refs_symbol_field_and_hash_suffix() {
    let a = setup("entry-refs").await;
    let i = insight(
        &a,
        "Символы записи",
        json!({"files": [
            {"path": "src/a.rs#Store::open"},
            {"path": "src/a.rs", "symbol": "bar", "symbol_hash": bh(5), "hash": h(1)},
            {"path": "./src/a.rs"},
            {"path": "src/a.rs#Store::open"},
            {"path": "docs/a#b.md"},
            {"path": "src/a.rs#foo#2"},
        ]}),
    )
    .await;
    let got: Vec<(String, Option<String>, Option<String>)> = rows(&a, &i)
        .await
        .into_iter()
        .map(|r| (r.0, r.1, r.4))
        .collect();
    let s = |x: &str| Some(x.to_string());
    assert_eq!(
        got,
        vec![
            ("docs/a#b.md".to_string(), None, None),
            ("src/a.rs".to_string(), None, None),
            ("src/a.rs".to_string(), s("Store::open"), None),
            ("src/a.rs".to_string(), s("bar"), s(&bh(5))),
            ("src/a.rs".to_string(), s("foo#2"), None),
        ]
    );
    // символ и `путь#qual` вместе, неверный хэш тела, `/` в символе — 422
    for (f, needle) in [
        (json!({"path": "a.rs#x", "symbol": "y"}), "symbol"),
        (
            json!({"path": "a.rs", "symbol": "y", "symbol_hash": "zz"}),
            "symbol_hash",
        ),
        (json!({"path": "a.rs", "symbol": "a/b"}), "symbol"),
        (json!({"path": "a.rs", "symbol_hash": bh(1)}), "symbol_hash"),
    ] {
        let r = a
            .t(
                Method::POST,
                &sol_entries(),
                Some(json!({"kind": "insight", "title": "плохо", "body": "плохо",
                            "data": {"kind": "gotcha"}, "refs": {"files": [f]}, "force": true})),
            )
            .await;
        unprocessable(&r, needle);
    }
}

#[tokio::test]
async fn note_frontmatter_symbol_ref() {
    let a = setup("note-refs").await;
    let v = note(
        &a,
        "Код/Заметка",
        &["src/a.rs#foo", "src/a.rs#foo", "./src/a.rs", "../x"],
    )
    .await;
    let n = id(&v);
    let w = v["warnings"].to_string();
    assert!(w.contains("invalid_ref: ../x"), "{v}");
    assert!(!w.contains("refs_truncated"), "{v}");
    let got: Vec<(String, Option<String>)> =
        rows(&a, &n).await.into_iter().map(|r| (r.0, r.1)).collect();
    assert_eq!(
        got,
        vec![
            ("src/a.rs".to_string(), None),
            ("src/a.rs".to_string(), Some("foo".to_string()))
        ]
    );
    // M8 ревью задачи 3: предупреждение о пределе — по ссылкам без повторов
    let mut many: Vec<String> = (0..64).map(|k| format!("src/m{k}.rs#f")).collect();
    many.push("src/m0.rs#f".into());
    many.push("./src/m1.rs#f".into());
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let v = note(&a, "Код/Много", &refs).await;
    assert!(
        !v["warnings"].to_string().contains("refs_truncated"),
        "64 разных ссылки: {v}"
    );
    many.push("src/m64.rs#f".into());
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let v = note(&a, "Код/Сверх", &refs).await;
    assert!(
        v["warnings"].to_string().contains("refs_truncated"),
        "65 разных ссылок: {v}"
    );
}

#[tokio::test]
async fn file_refs_index_has_symbol() {
    let a = setup("index").await;
    let n = id(&note(&a, "Код/Фу", &["src/a.rs#foo"]).await);
    let i = insight(&a, "Файл", json!({"files": [{"path": "src/a.rs"}]})).await;
    let idx = index(&a).await;
    let p = idx["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["path"] == "src/a.rs")
        .unwrap_or_else(|| panic!("нет пути: {idx}"));
    let items = p["items"].as_array().unwrap();
    assert_eq!(items[0]["id"], format!("n:{n}"), "символьные первыми: {p}");
    assert_eq!(items[0]["symbol"], "foo");
    assert_eq!(items[1]["id"], format!("i:{i}"));
    assert!(items[1].get("symbol").is_none(), "файловая без symbol: {p}");
}

#[tokio::test]
async fn graph_file_symbol_knowledge_first() {
    let a = setup("graph").await;
    let n = id(&note(&a, "Код/Фу", &["src/a.rs#foo"]).await);
    // файловая ссылка — новее символьной
    let i = insight(&a, "Файл", json!({"files": [{"path": "src/a.rs"}]})).await;
    for file in ["src/a.rs", "src/a.rs#foo"] {
        let r = a
            .t(
                Method::GET,
                &format!("/api/v1/graph?file={}&solution={G}", enc(file)),
                None,
            )
            .await;
        ok(&r, StatusCode::OK, file);
        let j = r.json();
        let ids: Vec<String> = j["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["ref"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(ids.len(), 2, "{file}: {j}");
        assert!(ids[0].contains(&n), "{file}: символьная первой: {j}");
        assert!(ids[1].contains(&i), "{file}: {j}");
    }
}

#[tokio::test]
async fn code_files_symbol_stale_applies_to_member_knowledge() {
    let a = api("codemem-member", |_| {}).await;
    let (_, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    for (tok, code) in [
        (&a_tok, StatusCode::CREATED),
        (&b_tok, StatusCode::ACCEPTED),
    ] {
        let r = a
            .tok(
                tok,
                Method::POST,
                "/api/v1/solutions",
                Some(json!({"agent_solution_id": G, "name": "Общее"})),
            )
            .await;
        ok(&r, code, "решение");
    }
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
            Some(json!({"access": "rw"})),
        )
        .await;
    ok(&r, StatusCode::OK, "доступ");
    // Knowledge участника со ссылкой на символ решения
    let r = a
        .tok_h(
            &b_tok,
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(
                json!({"kind": "insight", "title": "Личное о foo", "body": "foo течёт",
                        "data": {"kind": "gotcha"}, "force": true,
                        "refs": {"files": [{"path": "src/a.rs#foo"}]}}),
            ),
            &[("x-zam-solution", G)],
        )
        .await;
    ok(&r, StatusCode::CREATED, "knowledge");
    let k = id(&r.json());
    changed_tok(
        &a,
        &b_tok,
        json!([{"path": "src/a.rs", "hash": h(1), "symbols": syms(&[("foo", 1)])}]),
    )
    .await;
    let j = changed_tok(
        &a,
        &b_tok,
        json!([{"path": "src/a.rs", "hash": h(2), "symbols": syms(&[("foo", 2)])}]),
    )
    .await;
    assert_eq!(j["stale"], 1, "{j}");
    let b: zam_core::Id = b_id.parse().unwrap();
    let r = rows_of(&a, b, &k).await;
    assert_eq!(
        (r[0].2.as_str(), r[0].3.as_deref()),
        ("stale", Some("symbol_changed")),
        "{r:?}"
    );
}

#[tokio::test]
async fn stale_count_not_above_m45_same_scenario() {
    // M4.5: те же ссылки, хук без символов — по хэшу файла
    let m45 = setup("m45").await;
    scenario(&m45).await;
    changed(&m45, json!([{"path": "src/a.rs", "hash": h(1)}])).await;
    let without = changed(&m45, json!([{"path": "src/a.rs", "hash": h(2)}])).await["stale"]
        .as_u64()
        .unwrap();
    // M5.6: правка `bar` с хэшами символов
    let m56 = setup("m56").await;
    scenario(&m56).await;
    changed(
        &m56,
        json!([{"path": "src/a.rs", "hash": h(1), "symbols": syms(&[("foo", 1), ("bar", 2)])}]),
    )
    .await;
    let with = changed(
        &m56,
        json!([{"path": "src/a.rs", "hash": h(2), "symbols": syms(&[("foo", 1), ("bar", 3)])}]),
    )
    .await["stale"]
        .as_u64()
        .unwrap();
    assert_eq!(without, 2);
    assert!(with <= without, "M5.6 {with} > M4.5 {without}");
    assert_eq!(with, 1);
}

#[tokio::test]
async fn search_and_prompt_hints_never_show_symbols() {
    let a = setup("invisible").await;
    const NAME: &str = "zyxquonkFrobnicate";
    let r = a
        .t(
            Method::POST,
            "/api/v1/code/sync",
            Some(json!({"solution": G, "manifest": [{"path": "src/z.rs", "hash": h(1)}]})),
        )
        .await;
    ok(&r, StatusCode::OK, "sync");
    let r = a
        .t(
            Method::PUT,
            "/api/v1/code/files",
            Some(json!({"solution": G, "files": [{
                "path": "src/z.rs", "lang": "rust", "hash": h(1), "size": 10,
                "parse_state": "ok", "refs": [],
                "symbols": [{"name": NAME, "qual": NAME, "kind": "function", "line": 1,
                             "end_line": 3, "sig": format!("fn {NAME}()"), "body_hash": bh(1),
                             "exported": true}]}]})),
        )
        .await;
    ok(&r, StatusCode::OK, "put");
    // символ в карте есть
    let s = a
        .t(
            Method::GET,
            &format!("/api/v1/code/symbols?solution={G}&q={NAME}"),
            None,
        )
        .await;
    ok(&s, StatusCode::OK, "symbols");
    assert_eq!(s.json()["symbols"][0]["qual"], NAME);
    // а в поиске памяти и подсказках — нет
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": NAME, "scope": {"solution": G}, "format": "json"})),
        )
        .await;
    ok(&r, StatusCode::OK, "search");
    assert_eq!(r.json()["items"], json!([]), "{}", r.text());
    let r = a
        .t(
            Method::POST,
            "/api/v1/recall",
            Some(json!({"event": "prompt", "solution": G, "prompt": format!("почини {NAME}")})),
        )
        .await;
    ok(&r, StatusCode::OK, "prompt");
    assert_eq!(r.json()["text"], "", "{}", r.text());
    let r = a
        .t(
            Method::POST,
            "/api/v1/recall",
            Some(json!({"event": "post_tool", "solution": G, "tool": "Bash",
                        "error": format!("error: {NAME} failed")})),
        )
        .await;
    ok(&r, StatusCode::OK, "post_tool");
    assert_eq!(r.json()["text"], "", "{}", r.text());
}

#[tokio::test]
async fn hook_kinds_include_code_map_default_true() {
    let a = setup("hook-kinds").await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/recall",
            Some(json!({"event": "session_start", "solution": G})),
        )
        .await;
    ok(&r, StatusCode::OK, "recall");
    let hooks = r.json()["hooks"].clone();
    assert_eq!(hooks["code_map"], true, "{hooks}");
    let keys: Vec<&str> = hooks
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys.len(), 10, "{hooks}");
    let uri = format!("/api/v1/admin/users/{}/settings", a.admin_id());
    let r = a
        .adm(
            Method::PATCH,
            &uri,
            Some(json!({"hooks": {"code_map": false}})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "patch");
    assert_eq!(r.json()["hooks"]["code_map"], false, "{}", r.text());
    assert_eq!(zam_server::api::user_settings::HOOK_KINDS[9], "code_map");
}

// ---- ревью задачи 6 ----

/// I1: тело `/files/changed` больше 2 МиБ (по умолчанию axum) принимается — до
/// `code.max_request_bytes`.
#[tokio::test]
async fn files_changed_body_over_2mib_accepted() {
    let a = setup("big-body").await;
    let (n, _) = scenario(&a).await;
    // 8 файлов × 5000 символов ≈ 2,5 МиБ
    let many: Vec<Value> = (0..5000u64)
        .map(|k| json!({"qual": format!("fn_with_quite_long_name_{k:05}"), "body_hash": bh(k)}))
        .collect();
    let mut files = vec![json!({"path": "src/a.rs", "hash": h(1), "symbols": syms(&[("foo", 1)])})];
    for f in ["x", "y", "z", "u", "v", "w", "p", "q"].map(|n| format!("src/{n}.rs")) {
        files.push(json!({"path": f, "hash": h(2), "symbols": many.clone()}));
    }
    let body = json!({"solution": G, "files": files.clone()});
    assert!(body.to_string().len() > 2 * 1024 * 1024);
    changed(&a, Value::from(files)).await;
    assert_eq!(
        rows(&a, &n).await[0].4.as_deref(),
        Some(bh(1).as_str()),
        "ориентир тела"
    );
}

/// I2: символов больше `code.max_symbols_per_file` — файл судится по хэшу (как неполный
/// разбор), а не 422 на весь запрос.
#[tokio::test]
async fn files_changed_too_many_symbols_judged_by_hash() {
    let a = api("codemem-many-syms", |c| c.code.max_symbols_per_file = 2).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "x"})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "решение");
    let (n, i) = scenario(&a).await;
    changed(
        &a,
        json!([{"path": "src/a.rs", "hash": h(1), "symbols": syms(&[("foo", 1)])}]),
    )
    .await;
    // 3 символа при пределе 2: тело `foo` не судится, по хэшу файла — обе stale
    let j = changed(
        &a,
        json!([{"path": "src/a.rs", "hash": h(2),
                "symbols": syms(&[("foo", 1), ("bar", 2), ("baz", 3)])}]),
    )
    .await;
    assert_eq!(j["stale"], 2, "{j}");
    assert_eq!(
        state(&a, &n, "src/a.rs", Some("foo")).await,
        st("stale", Some("source_changed"))
    );
    assert_eq!(
        state(&a, &i, "src/a.rs", None).await,
        st("stale", Some("source_changed"))
    );
    // /files/verified так же
    let r = a
        .t(
            Method::POST,
            "/api/v1/files/verified",
            Some(
                json!({"solution": G, "files": [{"path": "src/a.rs", "hash": h(2),
                         "symbols": syms(&[("foo", 1), ("bar", 2), ("baz", 3)])}]}),
            ),
        )
        .await;
    ok(&r, StatusCode::OK, "verified");
}

/// I3: подтверждение файла без символов (`zam write --ref a.rs`, файл не разобран) не стирает
/// ориентир чужих ссылок на его символы: правка `foo` после него — `symbol_changed`.
#[tokio::test]
async fn files_verified_without_symbols_keeps_symbol_hash() {
    let a = setup("verify-keep").await;
    let na = id(&note(&a, "Код/А", &["src/a.rs#foo"]).await);
    let _nb = id(&note(&a, "Код/Б", &["src/a.rs"]).await);
    changed(
        &a,
        json!([{"path": "src/a.rs", "hash": h(1), "symbols": syms(&[("foo", 1)])}]),
    )
    .await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/files/verified",
            Some(json!({"solution": G, "files": [{"path": "src/a.rs", "hash": h(2)}]})),
        )
        .await;
    ok(&r, StatusCode::OK, "verified");
    assert_eq!(rows(&a, &na).await[0].4.as_deref(), Some(bh(1).as_str()));
    changed(
        &a,
        json!([{"path": "src/a.rs", "hash": h(3), "symbols": syms(&[("foo", 9)])}]),
    )
    .await;
    assert_eq!(
        state(&a, &na, "src/a.rs", Some("foo")).await,
        st("stale", Some("symbol_changed"))
    );
}

/// M3: `GET /graph?file=a.rs#foo` — знания о `foo`, затем о файле; о `bar` — нет.
#[tokio::test]
async fn graph_file_symbol_excludes_other_symbols() {
    let a = setup("graph-other").await;
    let nf = id(&note(&a, "Код/Фу", &["src/a.rs#foo"]).await);
    let nb = id(&note(&a, "Код/Бар", &["src/a.rs#bar"]).await);
    let i = insight(&a, "Файл", json!({"files": [{"path": "src/a.rs"}]})).await;
    let r = a
        .t(
            Method::GET,
            &format!("/api/v1/graph?file={}&solution={G}", enc("src/a.rs#foo")),
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "graph");
    let j = r.json();
    let ids: Vec<String> = j["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["ref"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(ids.len(), 2, "{j}");
    assert!(ids[0].contains(&nf), "{j}");
    assert!(ids[1].contains(&i), "{j}");
    assert!(!ids.iter().any(|x| x.contains(&nb)), "{j}");
}

/// M4: символ проверяется тем же правилом, что `путь#qual`: `#` только суффикс `#N`, символ —
/// только у файла кода.
#[tokio::test]
async fn refs_symbol_rules_like_split_ref() {
    let a = setup("symbol-rules").await;
    for (f, needle) in [
        (json!({"path": "src/a.rs", "symbol": "a#b"}), "qual"),
        (json!({"path": "src/a.rs", "symbol": "#2"}), "qual"),
        (json!({"path": "README.md", "symbol": "foo"}), "файла кода"),
    ] {
        let r = a
            .t(
                Method::POST,
                &sol_entries(),
                Some(json!({"kind": "insight", "title": "плохо", "body": "плохо",
                            "data": {"kind": "gotcha"}, "refs": {"files": [f]}, "force": true})),
            )
            .await;
        unprocessable(&r, needle);
    }
    let r = a
        .t(
            Method::POST,
            "/api/v1/files/changed",
            Some(
                json!({"solution": G, "files": [{"path": "a.rs", "hash": h(1),
                         "symbols": [{"qual": "a#b", "body_hash": bh(1)}]}]}),
            ),
        )
        .await;
    unprocessable(&r, "qual");
    // `foo#2` — допустимый суффикс одноимённых
    insight(
        &a,
        "Перегрузка",
        json!({"files": [{"path": "src/a.rs", "symbol": "foo#2"}]}),
    )
    .await;
}
