//! Приёмка M5.6 «Карта кода» (спецификация `docs/specs/2026-11-03-m5.6-code-map.md` §17,
//! задача 9), серверная часть: карта в бюджете и фокус (п. 3, синтетический проект), устарелость
//! по символу (п. 4), раздел `### Код` пакета субагенту (п. 5), невидимость карты в памяти (п. 6),
//! пределы 413/422 (п. 7), права (п. 10), MCP — 11 инструментов и та же карта (п. 11), миграция
//! базы M5.5 (0022) на 0023 (п. 12).
//!
//! Клиентская часть (экстрактор, инкрементальность, враждебные файлы и обрыв, хук, тела, старый
//! кэш) — `crates/zam-cli/tests/m56_acceptance.rs`; замеры на репозитории — `cargo xtask
//! code-bench`. Серверы без моделей (FTS), каталоги — в `CARGO_TARGET_TMPDIR`.
mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::api::{Api, Resp, api, enc};
use serde_json::{Value, json};
use zam_server::config::ServerConfig;
use zam_store::{Access, Db, USER_MIGRATIONS};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e56";

// ---------- помощники ----------

fn ok(r: &Resp, s: StatusCode, ctx: &str) {
    assert_eq!(r.status, s, "{ctx}: {}", r.text());
}

fn unprocessable(r: &Resp, needle: &str, ctx: &str) {
    ok(r, StatusCode::UNPROCESSABLE_ENTITY, ctx);
    let d = r.json()["detail"].as_str().unwrap_or_default().to_string();
    assert!(d.contains(needle), "{ctx}: «{needle}» нет в «{d}»");
}

/// Хэш файла (64 hex) и тела (16 hex) по номеру.
fn h(n: u64) -> String {
    format!("{n:064x}")
}

fn bh(n: u64) -> String {
    format!("{n:016x}")
}

/// Сервер с решением `G` администратора (первый токен).
async fn world(name: &str, tweak: impl FnOnce(&mut ServerConfig)) -> Api {
    let a = api(&format!("m56-{name}"), |c| {
        c.recall.timeout_ms = 30_000;
        c.recall.prompt_min_score = 0.0;
        // сервер теста без модели: порог `prompt` — FTS-only (M6 §9)
        c.recall.prompt_min_score_fts = 0.0;
        tweak(c);
    })
    .await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Карта кода"})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "решение");
    a
}

/// Символ-функция `qual` в строке `line` с сигнатурой `sig`.
fn func_sig(qual: &str, line: u32, sig: &str) -> Value {
    let name = qual.rsplit("::").next().unwrap();
    json!({"name": name, "qual": qual, "kind": "function", "line": line, "end_line": line + 2,
           "sig": sig, "body_hash": bh(u64::from(line)), "exported": true})
}

fn func(qual: &str, line: u32) -> Value {
    func_sig(qual, line, &format!("fn {qual}()"))
}

/// Файл загрузки: путь, номер хэша, символы, ссылки `(имя, число)`.
fn file(path: &str, n: u64, symbols: Vec<Value>, refs: &[(&str, u32)]) -> Value {
    let refs: Vec<Value> = refs
        .iter()
        .map(|(name, count)| json!({"name": name, "kind": "call", "count": count, "first_line": 1}))
        .collect();
    json!({"path": path, "lang": "rust", "hash": h(n), "size": 100, "parse_state": "ok",
           "symbols": symbols, "refs": refs})
}

async fn put_raw_as(a: &Api, tok: &str, files: Vec<Value>) -> Resp {
    a.tok(
        tok,
        Method::PUT,
        "/api/v1/code/files",
        Some(
            json!({"solution": G, "commit": "abcdef1", "branch": "main", "machine": "box",
                    "files": files}),
        ),
    )
    .await
}

async fn put(a: &Api, files: Vec<Value>) {
    ok(
        &put_raw_as(a, &a.token, files).await,
        StatusCode::OK,
        "загрузка",
    );
}

async fn get_ok(a: &Api, uri: &str) -> Value {
    let r = a.t(Method::GET, uri, None).await;
    ok(&r, StatusCode::OK, uri);
    r.json()
}

async fn status(a: &Api) -> Value {
    get_ok(a, &format!("/api/v1/code/status?solution={G}")).await
}

async fn wait_ranked(a: &Api) {
    for _ in 0..400 {
        if status(a).await["rank_state"] == "ok" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("ранги не пересчитаны");
}

fn paths_of(map: &Value) -> Vec<String> {
    map["files"]
        .as_array()
        .unwrap_or_else(|| panic!("{map}"))
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect()
}

fn id_of(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("нет id: {v}"))
        .to_string()
}

/// Заметка решения со ссылками `refs` во frontmatter: id узла.
async fn note_refs(a: &Api, path: &str, refs: &[&str]) -> String {
    let list: Vec<String> = refs.iter().map(|r| format!("\"{r}\"")).collect();
    let content = format!(
        "---\nrefs: [{}]\n---\n# {path}\n\nтекст о коде\n",
        list.join(", ")
    );
    id_of(&a.note(G, path, &content).await)
}

/// Insight решения со ссылками `refs`: id записи.
async fn insight(a: &Api, title: &str, refs: Value) -> String {
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/spaces/{G}/entries"),
            Some(
                json!({"kind": "insight", "title": title, "body": format!("{title}: подробно"),
                        "data": {"kind": "gotcha"}, "refs": refs, "force": true}),
            ),
        )
        .await;
    ok(&r, StatusCode::CREATED, title);
    id_of(&r.json())
}

/// `(qual, тело)` символов тела `/files/changed`.
fn syms(list: &[(&str, u64)]) -> Value {
    Value::from(
        list.iter()
            .map(|(q, b)| json!({"qual": q, "body_hash": bh(*b)}))
            .collect::<Vec<_>>(),
    )
}

async fn changed(a: &Api, files: Value) -> Value {
    let r = a
        .t(
            Method::POST,
            "/api/v1/files/changed",
            Some(json!({"solution": G, "files": files})),
        )
        .await;
    ok(&r, StatusCode::OK, "files/changed");
    r.json()
}

/// `(состояние, причина)` ссылки владельца `owner` на `(путь, символ)` в базе администратора.
async fn ref_state(
    a: &Api,
    owner: &str,
    path: &str,
    symbol: Option<&str>,
) -> (String, Option<String>) {
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let (owner, path, symbol) = (
        owner.to_string(),
        path.to_string(),
        symbol.map(str::to_string),
    );
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT state, stale_reason FROM file_refs
             WHERE owner_id = ?1 AND path = ?2 AND coalesce(symbol, '') = ?3",
            [owner, path, symbol.unwrap_or_default()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
    .await
    .unwrap()
}

fn st(s: &str, reason: Option<&str>) -> (String, Option<String>) {
    (s.to_string(), reason.map(str::to_string))
}

/// `POST /recall` первым токеном с сессией `session`.
async fn recall(a: &Api, session: &str, body: Value) -> Value {
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/recall",
            Some(body),
            &[("x-zam-session", session)],
        )
        .await;
    ok(&r, StatusCode::OK, "recall");
    r.json()
}

/// Раздел `### Код` (до следующего заголовка) или `None`.
fn code_section(t: &str) -> Option<String> {
    let start = t.find("### Код\n")?;
    let rest = &t[start..];
    let end = rest[1..].find("\n### ").map_or(rest.len(), |i| i + 1);
    Some(rest[..end].trim_end().to_string())
}

/// Строка карты кода дайджеста или `None`.
fn code_line(j: &Value) -> Option<String> {
    j["text"]
        .as_str()
        .unwrap()
        .lines()
        .find(|l| l.starts_with("ZeroAIMemory: карта кода:"))
        .map(str::to_string)
}

async fn disable_code_map(a: &Api) {
    let uri = format!("/api/v1/admin/users/{}/settings", a.admin_id());
    let r = a
        .adm(
            Method::PATCH,
            &uri,
            Some(json!({"hooks": {"code_map": false}})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "code_map = false");
}

/// JSON-RPC к `/mcp` по токену `tok`: `result`.
async fn rpc(a: &Api, tok: &str, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let r = a
        .send(
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(header::HOST, "memory.lan:7700")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header(header::AUTHORIZATION, format!("Bearer {tok}"))
                .header("x-zam-solution", G)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
    ok(&r, StatusCode::OK, method);
    let j = r.json();
    assert!(j.get("error").is_none(), "ошибка JSON-RPC: {j}");
    j["result"].clone()
}

/// Текст инструмента MCP без строк `> warning:` и `~N tok` в конце.
async fn mcp_text(a: &Api, name: &str, args: Value) -> String {
    let r = rpc(
        a,
        &a.token,
        "tools/call",
        json!({"name": name, "arguments": args}),
    )
    .await;
    assert_ne!(r["isError"].as_bool(), Some(true), "{name}: {r}");
    let text = r["content"][0]["text"].as_str().unwrap_or("").to_string();
    let mut lines: Vec<&str> = text.trim_end().lines().collect();
    while lines
        .last()
        .is_some_and(|l| l.starts_with("> warning:") || (l.starts_with('~') && l.ends_with(" tok")))
    {
        lines.pop();
    }
    lines.join("\n")
}

/// Синтетический проект: хабы `src/f00..f04.rs` (на них ссылаются почти все), файл фокуса
/// `src/f20.rs` ссылается на `target_name_25..27` в `src/f25..f27.rs`; 30 файлов.
fn synthetic() -> Vec<Value> {
    let hubs: Vec<String> = (0..5).map(|k| format!("hub_function_{k}")).collect();
    (0..30usize)
        .map(|i| {
            let path = format!("src/f{i:02}.rs");
            let (defs, refs): (Vec<Value>, Vec<(String, u32)>) = match i {
                0..=4 => (vec![func(&hubs[i], 1)], vec![]),
                20 => (
                    vec![func("focus_entry_point", 1)],
                    (25..28).map(|t| (format!("target_name_{t}"), 3)).collect(),
                ),
                25..=27 => (vec![func(&format!("target_name_{i}"), 1)], vec![]),
                _ => {
                    // заполнение бюджета: по 8 длинных сигнатур в файле
                    let defs = (0..8)
                        .map(|j| {
                            func_sig(
                                &format!("plain_worker_{i:02}_{j}"),
                                (j * 3 + 1) as u32,
                                &format!(
                                    "pub fn plain_worker_{i:02}_{j}(ctx: &Context, req: Request) -> Result<Response, Error>"
                                ),
                            )
                        })
                        .collect();
                    (defs, hubs.iter().map(|h| (h.clone(), 2)).collect())
                }
            };
            let refs: Vec<(&str, u32)> = refs.iter().map(|(n, c)| (n.as_str(), *c)).collect();
            file(&path, i as u64 + 1, defs, &refs)
        })
        .collect()
}

// ---------- п. 3 ----------

/// §17 п. 3 (синтетический проект): карта 200/500/1000/2000 ≤ бюджета счётчиком сервера; с
/// `focus.paths = [файл]` этот файл и файлы его целей — в первой пятёрке (без фокуса — нет).
/// Часть «≥ 6 из 10 ключевых типов на репозитории проекта» — `cargo xtask code-bench`.
#[tokio::test]
async fn acceptance_map_budget_and_focus() {
    let a = world("budget-focus", |_| {}).await;
    put(&a, synthetic()).await;
    wait_ranked(&a).await;
    for budget in [200usize, 500, 1000, 2000] {
        for format in ["md", "json"] {
            let m = get_ok(
                &a,
                &format!("/api/v1/code/map?solution={G}&budget_tokens={budget}&format={format}"),
            )
            .await;
            let tokens = m["tokens"].as_u64().unwrap() as usize;
            assert!(
                tokens > 0 && tokens <= budget,
                "{format} {budget}: {tokens}"
            );
            if format == "md" {
                let text = m["text"].as_str().unwrap();
                assert_eq!(zam_store::count_tokens(text), tokens, "счётчик сервера");
            }
        }
    }
    let top5 = |m: &Value| paths_of(m).into_iter().take(5).collect::<Vec<_>>();
    let want = ["src/f20.rs", "src/f25.rs", "src/f26.rs", "src/f27.rs"];
    let global = top5(&get_ok(&a, &format!("/api/v1/code/map?solution={G}&format=json")).await);
    for p in want {
        assert!(!global.contains(&p.to_string()), "без фокуса: {global:?}");
    }
    let focus = enc(&json!({"paths": ["src/f20.rs"]}).to_string());
    let m = get_ok(
        &a,
        &format!("/api/v1/code/map?solution={G}&format=json&focus={focus}&budget_tokens=1000"),
    )
    .await;
    assert!(m["tokens"].as_u64().unwrap() <= 1000);
    let top = top5(&m);
    for p in want {
        assert!(top.contains(&p.to_string()), "с фокусом {p}: {top:?}");
    }
    // центральные символы состояния — хабы
    let st = status(&a).await;
    let central: Vec<&str> = st["central"]
        .as_array()
        .unwrap_or_else(|| panic!("{st}"))
        .iter()
        .take(5)
        .map(|c| c["qual"].as_str().unwrap())
        .collect();
    for k in 0..5 {
        assert!(
            central.contains(&format!("hub_function_{k}").as_str()),
            "{central:?}"
        );
    }
}

// ---------- п. 4 ----------

/// §17 п. 4: заметка `refs: ["src/a.rs#foo"]` и insight с файловой ссылкой на `src/a.rs`.
/// Правка `bar` → файловая `stale`, символьная `ok`; правка `foo` → `symbol_changed`; удаление
/// `foo` → `symbol_removed`; индекс знания о файле называет `foo`; устаревших не больше, чем в
/// M4.5 (без хэшей символов) на том же сценарии.
#[tokio::test]
async fn acceptance_symbol_staleness_scenario() {
    async fn scenario(a: &Api) -> (String, String) {
        let n = note_refs(a, "Код/Фу", &["src/a.rs#foo"]).await;
        let i = insight(
            a,
            "Файл a целиком",
            json!({"files": [{"path": "src/a.rs", "hash": h(1)}]}),
        )
        .await;
        (n, i)
    }
    let a = world("stale", |_| {}).await;
    let (n, i) = scenario(&a).await;
    // индекс знания о файле: символ назван
    let idx = get_ok(&a, &format!("/api/v1/file-refs?solution={G}")).await;
    let items = idx["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == "src/a.rs")
        .unwrap_or_else(|| panic!("{idx}"))["items"]
        .clone();
    assert!(
        items
            .as_array()
            .unwrap()
            .iter()
            .any(|it| it["symbol"] == "foo" && it["id"] == format!("n:{n}")),
        "{items}"
    );
    changed(
        &a,
        json!([{"path": "src/a.rs", "hash": h(1), "symbols": syms(&[("foo", 1), ("bar", 2)])}]),
    )
    .await;
    let with = changed(
        &a,
        json!([{"path": "src/a.rs", "hash": h(2), "symbols": syms(&[("foo", 1), ("bar", 3)])}]),
    )
    .await["stale"]
        .as_u64()
        .unwrap();
    assert_eq!(
        ref_state(&a, &i, "src/a.rs", None).await,
        st("stale", Some("source_changed"))
    );
    assert_eq!(
        ref_state(&a, &n, "src/a.rs", Some("foo")).await,
        st("ok", None)
    );
    changed(
        &a,
        json!([{"path": "src/a.rs", "hash": h(3), "symbols": syms(&[("foo", 9), ("bar", 3)])}]),
    )
    .await;
    assert_eq!(
        ref_state(&a, &n, "src/a.rs", Some("foo")).await,
        st("stale", Some("symbol_changed"))
    );
    changed(
        &a,
        json!([{"path": "src/a.rs", "hash": h(4), "symbols": syms(&[("bar", 3)])}]),
    )
    .await;
    assert_eq!(
        ref_state(&a, &n, "src/a.rs", Some("foo")).await,
        st("stale", Some("symbol_removed"))
    );
    // M4.5 на том же сценарии: хук без хэшей символов — устаревают обе ссылки
    let m45 = world("stale-m45", |_| {}).await;
    scenario(&m45).await;
    changed(&m45, json!([{"path": "src/a.rs", "hash": h(1)}])).await;
    let without = changed(&m45, json!([{"path": "src/a.rs", "hash": h(2)}])).await["stale"]
        .as_u64()
        .unwrap();
    assert_eq!((with, without), (1, 2), "M5.6 {with}, M4.5 {without}");
}

// ---------- п. 5 ----------

/// §17 п. 5: задача с именем существующей функции → `### Код` с её сигнатурой и путём, весь
/// пакет ≤ `recall.brief_tokens`; выключенный `code_map` (хук) — раздела нет.
#[tokio::test]
async fn acceptance_brief_code_section() {
    let a = world("brief", |_| {}).await;
    put(
        &a,
        vec![
            file(
                "src/store.rs",
                1,
                vec![
                    func_sig(
                        "open_store",
                        12,
                        "pub fn open_store(path: &Path) -> Result<Store>",
                    ),
                    func_sig("close_store", 40, "pub fn close_store(s: Store)"),
                ],
                &[],
            ),
            file("src/other.rs", 2, vec![func("unrelated", 3)], &[]),
        ],
    )
    .await;
    let budget = a.st.cfg.recall.brief_tokens as u64;
    let body = json!({"event": "brief", "solution": G, "code_map": true,
                      "task": "Почини open_store: падает на пустом пути"});
    let j = recall(&a, "b1:ag", body.clone()).await;
    let t = j["text"].as_str().unwrap();
    let sec = code_section(t).unwrap_or_else(|| panic!("нет раздела:\n{t}"));
    assert!(
        sec.contains("src/store.rs:12 function pub fn open_store(path: &Path) -> Result<Store>"),
        "{sec}"
    );
    assert!(!sec.contains("unrelated"), "{sec}");
    assert!(j["tokens"].as_u64().unwrap() <= budget, "{budget}: {j}");
    assert!(zam_store::count_tokens(t) as u64 <= budget, "{t}");
    disable_code_map(&a).await;
    let j = recall(&a, "b2:ag", body).await;
    assert_eq!(code_section(j["text"].as_str().unwrap()), None, "{j}");
}

// ---------- п. 6 ----------

/// §17 п. 6: `POST /search` и подсказки `prompt`/`post_tool` символы не выдают; строка дайджеста
/// ≤ 80 токенов; без карты — строки нет.
#[tokio::test]
async fn acceptance_invisible_in_memory() {
    const NAME: &str = "zyxquonkFrobnicateM56";
    let a = world("invisible", |_| {}).await;
    // без карты — строки нет
    let j = recall(
        &a,
        "inv-0",
        json!({"event": "session_start", "solution": G}),
    )
    .await;
    assert_eq!(code_line(&j), None, "{j}");
    put(
        &a,
        vec![
            file("src/z.rs", 1, vec![func(NAME, 1)], &[]),
            file(
                "src/y.rs",
                2,
                vec![func("helper_y", 1), func("Store::open", 5)],
                &[(NAME, 2)],
            ),
        ],
    )
    .await;
    let s = get_ok(&a, &format!("/api/v1/code/symbols?solution={G}&q={NAME}")).await;
    assert_eq!(s["symbols"][0]["qual"], NAME, "символ в карте есть: {s}");
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": NAME, "scope": {"solution": G}, "format": "json"})),
        )
        .await;
    ok(&r, StatusCode::OK, "search");
    assert_eq!(r.json()["items"], json!([]), "{}", r.text());
    for body in [
        json!({"event": "prompt", "solution": G, "prompt": format!("почини {NAME}")}),
        json!({"event": "post_tool", "solution": G, "tool": "Bash",
               "error": format!("error: {NAME} failed")}),
    ] {
        let j = recall(&a, "inv-1", body.clone()).await;
        assert!(!j["text"].as_str().unwrap().contains(NAME), "{body}: {j}");
    }
    let j = recall(
        &a,
        "inv-2",
        json!({"event": "session_start", "solution": G}),
    )
    .await;
    let line = code_line(&j).unwrap_or_else(|| panic!("нет строки карты: {j}"));
    assert!(zam_store::count_tokens(&line) <= 80, "{line}");
    assert!(line.contains("2 файла"), "{line}");
    // положительный контроль: запись со словом NAME в заголовке и теле поиск находит, символов
    // карты в выдаче нет
    let iid = insight(
        &a,
        &format!("Функция {NAME} падает"),
        json!({"files": [{"path": "src/z.rs"}]}),
    )
    .await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": NAME, "scope": {"solution": G}, "format": "json"})),
        )
        .await;
    ok(&r, StatusCode::OK, "search с записью");
    let items = r.json()["items"].as_array().cloned().unwrap_or_default();
    assert!(
        items.iter().any(|i| i["entry_id"] == iid.as_str()),
        "запись найдена: {}",
        r.text()
    );
    assert!(
        items.iter().all(|i| i["entry_id"] == iid.as_str()),
        "в выдаче только запись, без символов карты: {}",
        r.text()
    );
}

// ---------- п. 7 ----------

/// §17 п. 7 (сервер): запросы сверх пределов §12 — 413 (тело) и 422 (число файлов, символов,
/// ссылок, бюджет, фокус) с названием предела.
#[tokio::test]
async fn acceptance_server_limits_413_422() {
    let a = world("limits", |c| {
        c.code.max_files = 3;
        c.code.max_symbols_per_file = 2;
        c.code.max_refs_per_file = 2;
        c.code.max_request_bytes = 4096;
    })
    .await;
    let manifest: Vec<Value> = (1..=4)
        .map(|i| json!({"path": format!("m{i}.rs"), "hash": h(i)}))
        .collect();
    let r = a
        .t(
            Method::POST,
            "/api/v1/code/sync",
            Some(json!({"solution": G, "manifest": manifest})),
        )
        .await;
    unprocessable(&r, "code.max_files", "манифест > max_files");
    let r = put_raw_as(
        &a,
        &a.token,
        vec![file(
            "a.rs",
            1,
            vec![func("x1", 1), func("x2", 2), func("x3", 3)],
            &[],
        )],
    )
    .await;
    unprocessable(
        &r,
        "code.max_symbols_per_file",
        "символов > max_symbols_per_file",
    );
    let r = put_raw_as(
        &a,
        &a.token,
        vec![file("a.rs", 1, vec![], &[("p", 1), ("q", 1), ("r", 1)])],
    )
    .await;
    unprocessable(&r, "code.max_refs_per_file", "ссылок > max_refs_per_file");
    for (m, uri, pad) in [
        (Method::PUT, "/api/v1/code/files", "files"),
        (Method::POST, "/api/v1/code/sync", "manifest"),
    ] {
        let r = a
            .t(
                m,
                uri,
                Some(json!({"solution": G, pad: [], "pad": "x".repeat(5000)})),
            )
            .await;
        ok(&r, StatusCode::PAYLOAD_TOO_LARGE, uri);
        assert!(
            r.json()["detail"].as_str().unwrap().contains("4096"),
            "{}",
            r.text()
        );
    }
    let m = |s: &str| format!("/api/v1/code/map?solution={G}&{s}");
    for (q, needle) in [
        ("budget_tokens=0".to_string(), "20000"),
        ("budget_tokens=20001".to_string(), "20000"),
        (
            format!(
                "focus={}",
                enc(&json!({"text": "a".repeat(2001)}).to_string())
            ),
            "2000",
        ),
        ("format=xml".to_string(), "format"),
    ] {
        unprocessable(&a.t(Method::GET, &m(&q), None).await, needle, &q);
    }
    // в пределах — принимается
    put(&a, vec![file("a.rs", 1, vec![func("x1", 1)], &[("p", 1)])]).await;
}

// ---------- п. 10 ----------

/// §17 п. 10: участник `ro` читает карту и получает 403 на `sync`; `rw` синхронизирует
/// (атрибуция в `code_files`); после отзыва — 403 `solution-access`, как у маршрутов решения
/// (поиск — пусто с предупреждением `revoked`); Knowledge — 422.
#[tokio::test]
async fn acceptance_rights_matrix() {
    use common::reports::{G as RG, web};
    let w = common::reports::world("m56-rights").await;
    let fbody = |path: &str, n: u64| {
        json!({"solution": RG, "machine": "box", "files": [{
            "path": path, "lang": "rust", "hash": h(n), "size": 10, "parse_state": "ok",
            "symbols": [{"name": "alpha", "qual": "alpha", "kind": "function", "line": 1,
                         "end_line": 2, "sig": "fn alpha()", "body_hash": bh(n)}],
            "refs": []}]})
    };
    let sync = json!({"solution": RG, "manifest": [{"path": "a.rs", "hash": h(1)}]});
    let map_uri = format!("/api/v1/code/map?solution={RG}&format=json");
    // rw синхронизирует: файл в базе владельца с его user_id
    let r =
        w.a.tok(
            &w.rw.tok,
            Method::POST,
            "/api/v1/code/sync",
            Some(sync.clone()),
        )
        .await;
    ok(&r, StatusCode::OK, "rw: sync");
    let r =
        w.a.tok(
            &w.rw.tok,
            Method::PUT,
            "/api/v1/code/files",
            Some(fbody("a.rs", 1)),
        )
        .await;
    ok(&r, StatusCode::OK, "rw: загрузка");
    let uploader: String = w
        .db()
        .await
        .read(|c| {
            Ok(c.query_row(
                "SELECT user_id FROM code_files WHERE path = 'a.rs'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(uploader, w.rw.id, "атрибуция участника rw");
    // ro: чтение — да (токен и WebUI), запись — 403
    let r = w.a.tok(&w.ro.tok, Method::GET, &map_uri, None).await;
    ok(&r, StatusCode::OK, "ro: карта");
    assert_eq!(paths_of(&r.json()), ["a.rs"]);
    let r = web(&w.a, &w.ro.web, Method::GET, &map_uri, None).await;
    ok(&r, StatusCode::OK, "ro WebUI: карта");
    let r =
        w.a.tok(
            &w.ro.tok,
            Method::POST,
            "/api/v1/code/sync",
            Some(sync.clone()),
        )
        .await;
    ok(&r, StatusCode::FORBIDDEN, "ro: sync");
    let r =
        w.a.tok(
            &w.ro.tok,
            Method::PUT,
            "/api/v1/code/files",
            Some(fbody("x.rs", 9)),
        )
        .await;
    ok(&r, StatusCode::FORBIDDEN, "ro: загрузка");
    // отзыв rw — 403, тот же ответ, что у поиска по решению
    let r =
        w.a.tok(
            &w.o.tok,
            Method::POST,
            &format!("/api/v1/solutions/{RG}/access/{}/revoke", w.rw.id),
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "отзыв");
    // поиск по решению после отзыва — пусто с предупреждением (§11.6), прочие маршруты
    // решения — 403 `solution-access`; карта — как они
    let r =
        w.a.tok(
            &w.rw.tok,
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "alpha", "scope": {"solution": RG}})),
        )
        .await;
    ok(&r, StatusCode::OK, "поиск после отзыва");
    assert!(
        r.json()["warnings"].to_string().contains("revoked"),
        "{}",
        r.text()
    );
    let tree =
        w.a.tok(
            &w.rw.tok,
            Method::GET,
            &format!("/api/v1/spaces/{RG}/tree"),
            None,
        )
        .await;
    ok(&tree, StatusCode::FORBIDDEN, "дерево после отзыва");
    for (m, uri, body) in [
        (Method::GET, map_uri.clone(), None),
        (Method::POST, "/api/v1/code/sync".to_string(), Some(sync)),
        (
            Method::PUT,
            "/api/v1/code/files".to_string(),
            Some(fbody("b.rs", 2)),
        ),
    ] {
        let r = w.a.tok(&w.rw.tok, m, &uri, body).await;
        ok(&r, StatusCode::FORBIDDEN, &uri);
        assert_eq!(
            common::reports::problem(&r),
            common::reports::problem(&tree),
            "{uri}"
        );
    }
    // Knowledge — 422
    let r =
        w.a.tok(
            &w.o.tok,
            Method::GET,
            "/api/v1/code/map?solution=knowledge",
            None,
        )
        .await;
    unprocessable(&r, "только у пространств решений", "Knowledge");
}

// ---------- п. 11 ----------

/// §17 п. 11: `tools/list` — 11 инструментов, описания ≤ 300 символов; `memory_code action=map`
/// возвращает ту же карту, что REST (с бюджетом и фокусом).
#[tokio::test]
async fn acceptance_mcp_11_tools_same_map() {
    let a = world("mcp", |_| {}).await;
    put(&a, synthetic()).await;
    wait_ranked(&a).await;
    let list = rpc(&a, &a.token, "tools/list", json!({})).await;
    let tools = list["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 11, "{list}");
    assert!(tools.iter().any(|t| t["name"] == "memory_code"), "{list}");
    for t in tools {
        let d = t["description"].as_str().unwrap();
        let n = d.chars().count();
        assert!(n <= 300, "{}: {n} символов", t["name"]);
    }
    let focus = json!({"paths": ["src/f20.rs"]});
    for (args, q) in [
        (json!({"action": "map"}), String::new()),
        (
            json!({"action": "map", "budget_tokens": 500}),
            "&budget_tokens=500".to_string(),
        ),
        (
            json!({"action": "map", "focus": focus}),
            format!("&focus={}", enc(&focus.to_string())),
        ),
    ] {
        let rest = get_ok(&a, &format!("/api/v1/code/map?solution={G}{q}")).await;
        let rest = rest["text"].as_str().unwrap().trim_end().to_string();
        assert!(!rest.is_empty());
        let mcp = mcp_text(&a, "memory_code", args.clone()).await;
        assert_eq!(mcp.trim_end(), rest, "{args}");
    }
}

// ---------- п. 12 ----------

/// §17 п. 12: база пользователя M5.5 (`USER_MIGRATIONS[..22]`, 0022) с заметками и записями
/// со ссылками на файлы (одна `stale`) открывается сервером M5.6 и переходит на 0023: ссылки на
/// месте с прежними состояниями, `symbol` NULL, `foreign_key_check` пуст; карта и символьные
/// ссылки работают на мигрированной базе, старые ссылки устаревают как в M4.5.
#[tokio::test]
async fn acceptance_migration_from_m55_db() {
    let a = api("m56-migration", |_| {}).await;
    // пользователь без открытой базы: файл базы создаётся до первого запроса — схема M5.5
    let u = a.st.system.create_user("Мира", false).await.unwrap();
    let (_, tok) =
        a.st.system
            .issue_token(u.id, "t", Access::Rw)
            .await
            .unwrap();
    let path = a.st.cfg.data_dir.join("users").join(format!("{}.db", u.id));
    assert!(!path.exists(), "{}", path.display());
    assert_eq!(USER_MIGRATIONS[21].version, 22);
    let (sol, kn) = (zam_core::Id::new(), zam_core::Id::new());
    let mut sql = String::from("DELETE FROM meta WHERE key = 'refs_backfill';\n");
    const TS: &str = "2026-10-30T00:00:00.000000Z";
    for (id, kind, name, guid) in [
        (zam_core::Id::new(), "rules", "Rules", "NULL".to_string()),
        (zam_core::Id::new(), "human", "Human", "NULL".to_string()),
        (kn, "knowledge", "Knowledge", "NULL".to_string()),
        (sol, "solution", "Решение M5.5", format!("'{G}'")),
    ] {
        sql.push_str(&format!(
            "INSERT INTO spaces (id, kind, solution_guid, name, created_at)
             VALUES ('{id}', '{kind}', {guid}, '{name}', '{TS}');\n"
        ));
    }
    let fref = |kind: &str, owner: &zam_core::Id, p: &str, hash: Option<&str>| {
        let hash = hash.map_or("NULL".to_string(), |h| format!("'{h}'"));
        format!(
            "INSERT INTO file_refs (owner_kind, owner_id, solution_guid, path, content_hash,
                                    recorded_at, state)
             VALUES ('{kind}', '{owner}', '{G}', '{p}', {hash}, '{TS}', 'ok');\n"
        )
    };
    for i in 0..4 {
        let id = zam_core::Id::new();
        sql.push_str(&format!(
            "INSERT INTO nodes (id, space_id, kind, name, path, index_state, created_at,
                                updated_at)
             VALUES ('{id}', '{sol}', 'note', 'n{i}', 'n{i}', 'excluded', '{TS}', '{TS}');\n"
        ));
        sql.push_str(&fref("node", &id, &format!("src/n{i}.rs"), None));
        sql.push_str(&fref("node", &id, "src/common.rs", None));
    }
    for i in 0..4 {
        let id = zam_core::Id::new();
        sql.push_str(&format!(
            "INSERT INTO entries (id, space_id, kind, title, title_norm, author_kind,
                                  created_at, updated_at)
             VALUES ('{id}', '{sol}', 'changelog', 'правка {i}', 'правка {i}', 'agent',
                     '{TS}', '{TS}');\n"
        ));
        sql.push_str(&fref("entry", &id, &format!("src/e{i}.rs"), Some(&h(1))));
    }
    sql.push_str(&format!(
        "UPDATE file_refs SET state = 'stale', stale_reason = 'source_changed', stale_at = '{TS}'
         WHERE path = 'src/e0.rs';\n"
    ));
    let snapshot = "SELECT owner_kind || ' ' || owner_id || ' ' || path || ' ' || state
                    FROM file_refs ORDER BY owner_kind, owner_id, path";
    let rows = |db: Db| async move {
        db.read(move |c| {
            let v: u32 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            let mut s = c.prepare(snapshot)?;
            let rows = s
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            let nulls: i64 = c.query_row(
                "SELECT count(*) FROM file_refs WHERE symbol IS NULL AND symbol_hash IS NULL",
                [],
                |r| r.get(0),
            )?;
            let mut fk = c.prepare("PRAGMA foreign_key_check")?;
            let violations = fk.query_map([], |_| Ok(()))?.count();
            Ok((v, rows, nulls, violations))
        })
        .await
        .unwrap()
    };
    // снимок — тем же соединением, что заполняло базу 0022 (фоновые задачи сервера могут
    // открыть базу нового пользователя сразу после заполнения)
    let before = {
        let db = Db::open(&path, &USER_MIGRATIONS[..22]).unwrap();
        db.write(move |tx| {
            tx.execute_batch(&sql)?;
            let v: u32 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            assert_eq!(v, 22);
            let mut s = tx.prepare(snapshot)?;
            let rows = s
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap()
    };
    assert_eq!(before.len(), 12);
    // первый запрос пользователя открывает базу сервером — миграция 0023
    let r = a
        .tok(
            &tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Решение M5.5"})),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    assert_eq!(r.json()["id"], sol.to_string(), "прежнее пространство");
    let db = a.st.users.get(u.id).await.unwrap();
    a.st.users.wait_rechunk(u.id).await;
    let (v1, after, nulls, violations) = rows(db.clone()).await;
    assert_eq!(v1, USER_MIGRATIONS.len() as u32);
    assert!(v1 >= 23);
    assert_eq!(after, before, "ссылки на месте с прежними состояниями");
    assert_eq!(nulls, 12);
    assert_eq!(violations, 0, "foreign_key_check не пуст");
    // индекс знания о файлах: старые ссылки без символа
    let r = a
        .tok(
            &tok,
            Method::GET,
            &format!("/api/v1/file-refs?solution={G}"),
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "file-refs");
    let idx = r.json();
    let common = idx["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["path"] == "src/common.rs")
        .unwrap_or_else(|| panic!("{idx}"));
    assert_eq!(common["items"].as_array().unwrap().len(), 4, "{common}");
    assert!(
        common["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i.get("symbol").is_none_or(Value::is_null)),
        "{common}"
    );
    // старые ссылки — как в M4.5: правка с хэшами символов судится по хэшу файла
    let r = a
        .tok(
            &tok,
            Method::POST,
            "/api/v1/files/changed",
            Some(json!({"solution": G, "files": [
                {"path": "src/e1.rs", "hash": h(2), "symbols": syms(&[("foo", 1)])}]})),
        )
        .await;
    ok(&r, StatusCode::OK, "files/changed");
    assert_eq!(r.json()["stale"], 1, "{}", r.text());
    // карта и символьная ссылка на мигрированной базе
    let r = put_raw_as(
        &a,
        &tok,
        vec![file("src/n0.rs", 7, vec![func("foo", 1)], &[])],
    )
    .await;
    ok(&r, StatusCode::OK, "загрузка карты");
    let r = a
        .tok(
            &tok,
            Method::GET,
            &format!("/api/v1/code/symbols?solution={G}&q=foo"),
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "символы");
    assert_eq!(r.json()["symbols"][0]["path"], "src/n0.rs", "{}", r.text());
    let r = a
        .tok(
            &tok,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": G, "path": "Новая", "kind": "note",
                        "content": "---\nrefs: [\"src/n0.rs#foo\"]\n---\nо foo\n"})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "заметка с символьной ссылкой");
    let n: i64 = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM file_refs WHERE path = 'src/n0.rs' AND symbol = 'foo'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(n, 1);
}
