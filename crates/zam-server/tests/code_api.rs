//! REST карты кода (M5.6 задача 4, спецификация §5, §7, §9, §12–§13): синхронизация по
//! манифесту, загрузка файлов, состояние, карта в бюджете (Markdown и JSON), поиск и карточка
//! символа, влияние правки, пределы и валидация недоверенного входа, выключатель `code.enabled`,
//! `ETag`, ленивый пересчёт рангов. Права и участники — `tests/code_access.rs`.
//!
//! Решение `G` принадлежит пользователю администратора (первый токен). Серверы без моделей:
//! счётчик токенов — приближённый, как у всех `tokens` сервера без bge-m3.
mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api, enc};
use serde_json::{Value, json};
use zam_server::config::ServerConfig;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e31";

/// Сервер с решением `G` администратора.
async fn world(name: &str, tweak: impl FnOnce(&mut ServerConfig)) -> Api {
    let a = api(&format!("code-{name}"), tweak).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Карта"})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "решение");
    a
}

fn ok(r: &Resp, s: StatusCode, ctx: &str) {
    assert_eq!(r.status, s, "{ctx}: {}", r.text());
}

/// 422 с `detail`, содержащим `needle`.
fn unprocessable(r: &Resp, needle: &str, ctx: &str) {
    ok(r, StatusCode::UNPROCESSABLE_ENTITY, ctx);
    let d = r.json()["detail"].as_str().unwrap_or_default().to_string();
    assert!(d.contains(needle), "{ctx}: «{needle}» нет в «{d}»");
}

/// Хэш файла (64 hex) по номеру.
fn h(n: u64) -> String {
    format!("{n:064x}")
}

/// Хэш тела (16 hex) по номеру.
fn bh(n: u64) -> String {
    format!("{n:016x}")
}

fn lang_of(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("ts") => "typescript",
        Some("py") => "python",
        _ => "rust",
    }
}

/// Символ: `(name, qual, kind, line)`; `sig` — `fn <qual>()`, тело — по строке.
fn sym(name: &str, qual: &str, kind: &str, line: u32) -> Value {
    json!({"name": name, "qual": qual, "kind": kind, "line": line, "end_line": line + 2,
           "sig": format!("fn {qual}()"), "body_hash": bh(u64::from(line)), "exported": true})
}

/// Функция `name` (qual = name) в строке `line`.
fn func(name: &str, line: u32) -> Value {
    sym(name, name, "function", line)
}

/// Файл загрузки: путь, номер хэша, символы, ссылки `(name, kind, count)`.
fn file(path: &str, n: u64, symbols: Vec<Value>, refs: &[(&str, &str, u32)]) -> Value {
    let refs: Vec<Value> = refs
        .iter()
        .map(|(name, kind, count)| json!({"name": name, "kind": kind, "count": count, "first_line": 1}))
        .collect();
    json!({"path": path, "lang": lang_of(path), "hash": h(n), "size": 100,
           "parse_state": "ok", "symbols": symbols, "refs": refs})
}

async fn sync(a: &Api, manifest: &[(&str, u64)]) -> Resp {
    let m: Vec<Value> = manifest
        .iter()
        .map(|(p, n)| json!({"path": p, "hash": h(*n)}))
        .collect();
    a.t(
        Method::POST,
        "/api/v1/code/sync",
        Some(json!({"solution": G, "commit": "abcdef1", "branch": "main", "manifest": m})),
    )
    .await
}

async fn put_raw(a: &Api, files: Vec<Value>) -> Resp {
    a.t(
        Method::PUT,
        "/api/v1/code/files",
        Some(
            json!({"solution": G, "commit": "abcdef1", "branch": "main", "machine": "box",
                    "files": files}),
        ),
    )
    .await
}

async fn put(a: &Api, files: Vec<Value>) -> Value {
    let r = put_raw(a, files).await;
    ok(&r, StatusCode::OK, "загрузка");
    r.json()
}

async fn get(a: &Api, uri: &str) -> Resp {
    a.t(Method::GET, uri, None).await
}

async fn get_ok(a: &Api, uri: &str) -> Value {
    let r = get(a, uri).await;
    ok(&r, StatusCode::OK, uri);
    r.json()
}

async fn status(a: &Api) -> Value {
    get_ok(a, &format!("/api/v1/code/status?solution={G}")).await
}

/// Карта в JSON (дополнительные параметры — `extra`, уже закодированные).
async fn map_json(a: &Api, extra: &str) -> Value {
    get_ok(
        a,
        &format!("/api/v1/code/map?solution={G}&format=json{extra}"),
    )
    .await
}

fn paths_of(map: &Value) -> Vec<String> {
    map["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect()
}

/// Ждёт пересчёта рангов (фоновая задача после `rank pending`).
async fn wait_ranked(a: &Api) {
    for _ in 0..200 {
        if status(a).await["rank_state"] == "ok" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("ранги не пересчитаны");
}

async fn impact(a: &Api, files: Value) -> Resp {
    a.t(
        Method::POST,
        "/api/v1/code/impact",
        Some(json!({"solution": G, "files": files})),
    )
    .await
}

/// Заметка в решении со ссылками `refs` во frontmatter.
async fn note_refs(a: &Api, path: &str, refs: &[&str]) -> String {
    let list: Vec<String> = refs.iter().map(|r| format!("\"{r}\"")).collect();
    let content = format!("---\nrefs: [{}]\n---\nтекст о коде\n", list.join(", "));
    a.note(G, path, &content).await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Insight в решении со ссылкой на файл `path`.
async fn insight_on(a: &Api, title: &str, path: &str) -> String {
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/spaces/{G}/entries"),
            Some(
                json!({"kind": "insight", "title": title, "body": format!("{title}: подробно"),
                        "data": {"kind": "gotcha"}, "refs": {"files": [{"path": path}]},
                        "force": true}),
            ),
        )
        .await;
    ok(&r, StatusCode::CREATED, "insight");
    r.json()["id"].as_str().unwrap().to_string()
}

/// Id символа `qual` поиском.
async fn symbol_id(a: &Api, qual: &str) -> i64 {
    let s = get_ok(
        a,
        &format!("/api/v1/code/symbols?solution={G}&q={}", enc(qual)),
    )
    .await;
    s["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["qual"] == qual)
        .unwrap_or_else(|| panic!("нет символа {qual}: {s}"))["id"]
        .as_i64()
        .unwrap()
}

#[tokio::test]
async fn sync_need_removed_etag() {
    let a = world("sync", |_| {}).await;
    let r = sync(&a, &[("src/a.rs", 1), ("src/b.rs", 2), ("web/c.ts", 3)]).await;
    ok(&r, StatusCode::OK, "первая синхронизация");
    let j = r.json();
    assert_eq!(
        j["need"],
        json!(["src/a.rs", "src/b.rs", "web/c.ts"]),
        "{j}"
    );
    assert_eq!(j["removed"], 0);
    assert_eq!(j["stale_refs"], 0);
    let etag0 = j["etag"].as_str().unwrap().to_string();
    put(
        &a,
        vec![
            file("src/a.rs", 1, vec![func("alpha", 1)], &[]),
            file(
                "src/b.rs",
                2,
                vec![func("beta", 1)],
                &[("alpha", "call", 1)],
            ),
            file("web/c.ts", 3, vec![func("gamma", 1)], &[]),
        ],
    )
    .await;
    let j = sync(&a, &[("src/a.rs", 1), ("src/b.rs", 2), ("web/c.ts", 3)])
        .await
        .json();
    assert_eq!(j["need"], json!([]), "{j}");
    assert_eq!(j["removed"], 0);
    let etag1 = j["etag"].as_str().unwrap().to_string();
    assert_ne!(etag0, etag1, "загрузка меняет etag");
    // другой хэш — снова нужен; пропавший файл — удалён
    let j = sync(&a, &[("src/a.rs", 9), ("src/b.rs", 2)]).await.json();
    assert_eq!(j["need"], json!(["src/a.rs"]), "{j}");
    assert_eq!(j["removed"], 1);
    assert_ne!(j["etag"], json!(etag1));
    let s = status(&a).await;
    assert_eq!(s["files"], 2, "{s}");
}

#[tokio::test]
async fn put_files_then_status_by_lang_skips_age() {
    let a = world("status", |_| {}).await;
    let before = status(&a).await;
    assert_eq!(before["exists"], false, "{before}");
    assert_eq!(before["files"], 0);
    let mut skipped = file("web/min.ts", 4, vec![], &[]);
    skipped["parse_state"] = json!("skipped");
    skipped["skip_reason"] = json!("minified");
    let j = put(
        &a,
        vec![
            file(
                "src/a.rs",
                1,
                vec![func("alpha", 1), func("alpha2", 5)],
                &[],
            ),
            file(
                "src/b.rs",
                2,
                vec![func("beta", 1)],
                &[("alpha", "call", 2)],
            ),
            file("web/c.ts", 3, vec![func("gamma", 1)], &[]),
            skipped,
        ],
    )
    .await;
    assert_eq!(j["accepted"], 4, "{j}");
    assert_eq!(j["stale_refs"], 0);
    assert!(j["warnings"].is_array(), "{j}");
    wait_ranked(&a).await;
    let s = status(&a).await;
    assert_eq!(s["exists"], true, "{s}");
    assert_eq!(
        (s["files"].clone(), s["symbols"].clone()),
        (json!(4), json!(4))
    );
    assert_eq!(
        s["by_lang"],
        json!([{"lang": "rust", "files": 2, "symbols": 3},
               {"lang": "typescript", "files": 2, "symbols": 1}]),
        "{s}"
    );
    assert_eq!(
        s["skipped"],
        json!([{"path": "web/min.ts", "reason": "minified"}])
    );
    assert_eq!(s["skipped_total"], 1);
    assert_eq!(
        (s["commit"].clone(), s["branch"].clone()),
        (json!("abcdef1"), json!("main"))
    );
    assert_eq!(s["machine"], "box");
    assert!(s["age_s"].as_i64().unwrap() >= 0, "{s}");
    assert!(s["updated_at"].is_string());
    assert_eq!(s["other_share"], 0.0);
    // статус пересчитывает ранги: центральные символы — с путём и `qual`
    assert_eq!(s["rank_state"], "ok", "{s}");
    let central = s["central"].as_array().unwrap();
    assert!(!central.is_empty(), "{s}");
    assert_eq!(central[0]["qual"], "alpha", "{s}");
    assert_eq!(central[0]["path"], "src/a.rs");
    assert!(s["digest"].as_str().unwrap().contains("4 файла"), "{s}");
}

#[tokio::test]
async fn put_files_signatures_masked() {
    let a = world("mask", |_| {}).await;
    let secret = format!(
        "ghp_{}",
        "A1b2C3d4E5".repeat(4).chars().take(36).collect::<String>()
    );
    let mut s = func("token", 1);
    s["sig"] = json!(format!("const TOKEN: &str = \"{secret}\";"));
    put(&a, vec![file("src/a.rs", 1, vec![s], &[])]).await;
    let r = get(&a, &format!("/api/v1/code/symbols?solution={G}&q=token")).await;
    ok(&r, StatusCode::OK, "поиск");
    assert!(!r.text().contains(&secret), "секрет в выдаче: {}", r.text());
    let sig = r.json()["symbols"][0]["sig"].as_str().unwrap().to_string();
    assert!(sig.starts_with("const TOKEN"), "{sig}");
    let m = get(&a, &format!("/api/v1/code/map?solution={G}")).await;
    assert!(!m.text().contains(&secret), "секрет в карте");
}

#[tokio::test]
async fn put_files_without_sig_ok() {
    let a = world("nosig", |_| {}).await;
    let mut s = sym("open", "Store::open", "method", 3);
    s.as_object_mut().unwrap().remove("sig");
    put(&a, vec![file("src/a.rs", 1, vec![s], &[])]).await;
    let m = get_ok(&a, &format!("/api/v1/code/map?solution={G}")).await;
    let text = m["text"].as_str().unwrap();
    assert!(text.contains("  method Store::open :3"), "{text}");
}

/// Корпус: `n` файлов по `per` символов с длинными сигнатурами; файл `i` ссылается на имена
/// файлов `0..i`.
async fn corpus(a: &Api, n: usize, per: usize) {
    let files: Vec<Value> = (0..n)
        .map(|i| {
            let syms: Vec<Value> = (0..per)
                .map(|j| {
                    let mut s = func(&format!("item_{i:02}_handler_{j:02}"), (j * 3 + 1) as u32);
                    s["sig"] = json!(format!(
                        "pub fn item_{i:02}_handler_{j:02}(ctx: &Context, req: Request) -> Result<Response, Error>"
                    ));
                    s
                })
                .collect();
            let names: Vec<String> = (0..i).map(|k| format!("item_{k:02}_handler_00")).collect();
            let refs: Vec<(&str, &str, u32)> =
                names.iter().map(|s| (s.as_str(), "call", 2)).collect();
            file(&format!("src/m{i:02}.rs"), i as u64 + 1, syms, &refs)
        })
        .collect();
    put(a, files).await;
}

#[tokio::test]
async fn map_md_under_budget_200_500_1000_2000() {
    let a = world("budget", |_| {}).await;
    corpus(&a, 60, 10).await;
    for budget in [200usize, 500, 1000, 2000] {
        let m = get_ok(
            &a,
            &format!("/api/v1/code/map?solution={G}&budget_tokens={budget}"),
        )
        .await;
        let text = m["text"].as_str().unwrap();
        let tokens = m["tokens"].as_u64().unwrap() as usize;
        assert!(tokens <= budget, "{budget}: {tokens}");
        assert_eq!(zam_store::count_tokens(text), tokens, "счётчик сервера");
        assert!(
            tokens * 10 >= budget * 7,
            "бюджет используется: {budget} → {tokens}"
        );
        assert!(text.contains("… ещё"), "{budget}: хвост");
        assert!(m["files_left"].as_u64().unwrap() > 0);
    }
    // по умолчанию — code.map_tokens (1000)
    let m = get_ok(&a, &format!("/api/v1/code/map?solution={G}")).await;
    assert!(m["tokens"].as_u64().unwrap() <= 1000);
    assert!(m["tokens"].as_u64().unwrap() > 700, "{m}");
}

#[tokio::test]
async fn map_json_format_edges_between_shown_max_120() {
    let a = world("edges", |_| {}).await;
    // 30 файлов: каждый определяет своё имя и ссылается на имена всех остальных — 870 рёбер
    let names: Vec<String> = (0..30)
        .map(|i| format!("shared_name_{i:02}_value"))
        .collect();
    let files: Vec<Value> = (0..30)
        .map(|i| {
            let refs: Vec<(&str, &str, u32)> = names
                .iter()
                .enumerate()
                .filter(|(k, _)| *k != i)
                .map(|(k, n)| (n.as_str(), "call", (k % 7 + 1) as u32))
                .collect();
            file(
                &format!("src/e{i:02}.rs"),
                i as u64 + 1,
                vec![func(&names[i], 1)],
                &refs,
            )
        })
        .collect();
    put(&a, files).await;
    wait_ranked(&a).await;
    let m = map_json(&a, "&budget_tokens=20000").await;
    let shown: Vec<i64> = m["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_i64().unwrap())
        .collect();
    assert_eq!(shown.len(), 30, "{m}");
    let f0 = &m["files"][0];
    assert!(f0["rank"].as_f64().unwrap() > 0.0);
    assert_eq!(f0["lang"], "rust");
    let s0 = &f0["symbols"][0];
    for k in ["id", "kind", "qual", "sig", "line", "rank"] {
        assert!(!s0[k].is_null(), "{k}: {s0}");
    }
    let edges = m["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 120, "первые 120 по весу");
    let mut prev = f64::INFINITY;
    for e in edges {
        let (s, d) = (e["src"].as_i64().unwrap(), e["dst"].as_i64().unwrap());
        assert!(shown.contains(&s) && shown.contains(&d) && s != d, "{e}");
        let w = e["weight"].as_f64().unwrap();
        assert!(w <= prev, "по убыванию веса");
        prev = w;
    }
    assert!(m["header"].as_str().unwrap().contains("30"), "{m}");
    assert_eq!(m["files_left"], 0);
    assert_eq!(m["symbols_left"], 0);
    assert!(m["tokens"].as_u64().unwrap() > 0);
    assert!(m["warnings"].is_array());
    // в небольшом бюджете — рёбра только между показанными файлами
    let m = map_json(&a, "&budget_tokens=150").await;
    let shown: Vec<i64> = m["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_i64().unwrap())
        .collect();
    assert!(shown.len() < 30 && !shown.is_empty(), "{m}");
    for e in m["edges"].as_array().unwrap() {
        assert!(shown.contains(&e["src"].as_i64().unwrap()));
        assert!(shown.contains(&e["dst"].as_i64().unwrap()));
    }
}

#[tokio::test]
async fn map_focus_paths_in_top5() {
    let a = world("focus", |_| {}).await;
    // хабы h0..h4 (на них ссылаются почти все), файл фокуса f20 ссылается на три цели t25..t27
    let hubs: Vec<String> = (0..5).map(|k| format!("hub_function_{k}")).collect();
    let mut files = Vec::new();
    for i in 0..30usize {
        let path = format!("src/f{i:02}.rs");
        let (defs, refs): (Vec<Value>, Vec<(String, u32)>) = match i {
            0..=4 => (vec![func(&hubs[i], 1)], vec![]),
            20 => (
                vec![func("focus_entry_point", 1)],
                (25..28).map(|t| (format!("target_name_{t}"), 3)).collect(),
            ),
            25..=27 => (vec![func(&format!("target_name_{i}"), 1)], vec![]),
            _ => (
                vec![func(&format!("plain_worker_{i}"), 1)],
                hubs.iter().map(|h| (h.clone(), 2)).collect(),
            ),
        };
        let refs: Vec<(&str, &str, u32)> =
            refs.iter().map(|(n, c)| (n.as_str(), "call", *c)).collect();
        files.push(file(&path, i as u64 + 1, defs, &refs));
    }
    put(&a, files).await;
    wait_ranked(&a).await;
    let top = |m: &Value| paths_of(m).into_iter().take(5).collect::<Vec<_>>();
    let global = top(&map_json(&a, "").await);
    for p in ["src/f20.rs", "src/f25.rs", "src/f26.rs", "src/f27.rs"] {
        assert!(
            !global.contains(&p.to_string()),
            "без фокуса {p} не в пятёрке: {global:?}"
        );
    }
    let focus = enc(&json!({"paths": ["src/f20.rs"]}).to_string());
    let m = map_json(&a, &format!("&focus={focus}")).await;
    let top5 = top(&m);
    for p in ["src/f20.rs", "src/f25.rs", "src/f26.rs", "src/f27.rs"] {
        assert!(
            top5.contains(&p.to_string()),
            "с фокусом {p} в пятёрке: {top5:?}"
        );
    }
    // фокус по имени и тексту задачи — тот же файл
    let focus = enc(&json!({"text": "поправить focus_entry_point в парсере"}).to_string());
    let m = map_json(&a, &format!("&focus={focus}")).await;
    assert!(top(&m).contains(&"src/f20.rs".to_string()), "{m}");
    // область — только файлы каталога
    let m = map_json(&a, "&area=src%2Ff2").await;
    assert!(
        paths_of(&m).is_empty(),
        "область — каталог, не префикс имени: {m}"
    );
    let only = map_json(&a, &format!("&area={}", enc("src/f20.rs"))).await;
    assert_eq!(paths_of(&only), ["src/f20.rs"]);
}

#[tokio::test]
async fn map_etag_304_without_focus_none_with_focus() {
    let a = world("etag", |_| {}).await;
    put(&a, vec![file("src/a.rs", 1, vec![func("alpha", 1)], &[])]).await;
    let uri = format!("/api/v1/code/map?solution={G}&budget_tokens=500");
    let r = get(&a, &uri).await;
    ok(&r, StatusCode::OK, "карта");
    let etag = r.header("etag").to_string();
    assert!(etag.starts_with("W/\""), "слабый ETag: {etag}");
    let same = a
        .tok_h(
            &a.token,
            Method::GET,
            &uri,
            None,
            &[("if-none-match", &etag)],
        )
        .await;
    ok(&same, StatusCode::NOT_MODIFIED, "та же карта");
    // другие параметры — другой ETag
    let other = get(
        &a,
        &format!("/api/v1/code/map?solution={G}&budget_tokens=400"),
    )
    .await;
    assert_ne!(other.header("etag"), etag);
    // с фокусом ETag нет и If-None-Match не действует
    let focus = enc(&json!({"idents": ["alpha"]}).to_string());
    let furi = format!("{uri}&focus={focus}");
    let f = a
        .tok_h(
            &a.token,
            Method::GET,
            &furi,
            None,
            &[("if-none-match", &etag)],
        )
        .await;
    ok(&f, StatusCode::OK, "с фокусом");
    assert!(f.headers.get("etag").is_none());
    // статус — тоже с ETag
    let suri = format!("/api/v1/code/status?solution={G}");
    let s = get(&a, &suri).await;
    let setag = s.header("etag").to_string();
    let s304 = a
        .tok_h(
            &a.token,
            Method::GET,
            &suri,
            None,
            &[("if-none-match", &setag)],
        )
        .await;
    ok(&s304, StatusCode::NOT_MODIFIED, "статус");
    // загрузка меняет оба
    put(&a, vec![file("src/b.rs", 2, vec![func("beta", 1)], &[])]).await;
    let r = a
        .tok_h(
            &a.token,
            Method::GET,
            &uri,
            None,
            &[("if-none-match", &etag)],
        )
        .await;
    ok(&r, StatusCode::OK, "карта изменилась");
    let s = a
        .tok_h(
            &a.token,
            Method::GET,
            &suri,
            None,
            &[("if-none-match", &setag)],
        )
        .await;
    ok(&s, StatusCode::OK, "статус изменился");
}

#[tokio::test]
async fn map_rank_pending_warning_on_timeout() {
    let a = world("pending", |c| c.code.rank_timeout_ms = 0).await;
    put(
        &a,
        vec![
            file("src/a.rs", 1, vec![func("alpha", 1)], &[]),
            file(
                "src/b.rs",
                2,
                vec![func("beta", 1)],
                &[("alpha", "call", 1)],
            ),
        ],
    )
    .await;
    let m = map_json(&a, "").await;
    assert_eq!(m["warnings"], json!(["rank pending"]), "{m}");
    // старые (нулевые) ранги — карта всё равно отдаётся
    assert_eq!(paths_of(&m).len(), 2);
    // пересчёт идёт в фоне и завершается
    wait_ranked(&a).await;
    let m = map_json(&a, "").await;
    assert_eq!(m["warnings"], json!([]), "{m}");
    assert_eq!(m["rank_state"], "ok", "повторный запрос после фона: {m}");
    assert_eq!(paths_of(&m)[0], "src/a.rs", "на a.rs ссылаются");
}

#[tokio::test]
async fn symbols_search_fts_exact_first_limit() {
    let a = world("symbols", |_| {}).await;
    put(
        &a,
        vec![
            file(
                "src/a.rs",
                1,
                vec![
                    sym("getUserById", "Api::getUserById", "method", 1),
                    func("user_count", 5),
                    func("get_user", 9),
                ],
                &[],
            ),
            file("src/b.rs", 2, vec![func("unrelated", 1)], &[]),
        ],
    )
    .await;
    let q = |s: &str| format!("/api/v1/code/symbols?solution={G}&{s}");
    let r = get_ok(&a, &q("q=get_user")).await;
    let quals: Vec<&str> = r["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["qual"].as_str().unwrap())
        .collect();
    assert_eq!(quals[0], "get_user", "точное совпадение первым: {quals:?}");
    assert!(
        quals.contains(&"Api::getUserById"),
        "FTS по словам имени: {quals:?}"
    );
    assert!(!quals.contains(&"unrelated"));
    let hit = &r["symbols"][0];
    assert_eq!(hit["path"], "src/a.rs");
    assert_eq!(hit["line"], 9);
    assert_eq!(hit["kind"], "function");
    let r = get_ok(&a, &q("q=user&limit=2")).await;
    assert_eq!(r["symbols"].as_array().unwrap().len(), 2, "{r}");
    let r = get_ok(&a, &q("q=user&kind=method")).await;
    assert_eq!(r["symbols"].as_array().unwrap().len(), 1, "{r}");
    let r = get_ok(&a, &q("q=user&path=src%2Fb.rs")).await;
    assert!(r["symbols"].as_array().unwrap().is_empty(), "{r}");
    unprocessable(&get(&a, &q("q=user&limit=101")).await, "100", "limit");
    unprocessable(&get(&a, &q("q=user&kind=lambda")).await, "kind", "вид");
    unprocessable(&get(&a, &q("q=")).await, "q", "пустой запрос");
}

/// Мир карточки и влияния: `a.rs` (foo, bar, baz), `b.rs` ссылается на foo, `c.rs` — на baz;
/// заметки о `a.rs#foo` и `a.rs#bar`, insight о файле `a.rs`.
struct Linked {
    a: Api,
    foo_note: String,
    bar_note: String,
    insight: String,
}

async fn linked(name: &str) -> Linked {
    let a = world(name, |_| {}).await;
    put(
        &a,
        vec![
            file(
                "a.rs",
                1,
                vec![func("foo", 1), func("bar", 10), func("baz", 20)],
                &[("helper", "call", 1)],
            ),
            file("b.rs", 2, vec![func("user_b", 1)], &[("foo", "call", 3)]),
            file("c.rs", 3, vec![func("user_c", 1)], &[("baz", "call", 1)]),
        ],
    )
    .await;
    let foo_note = note_refs(&a, "О foo", &["a.rs#foo"]).await;
    let bar_note = note_refs(&a, "О bar", &["a.rs#bar"]).await;
    let insight = insight_on(&a, "Файл a.rs", "a.rs").await;
    Linked {
        a,
        foo_note,
        bar_note,
        insight,
    }
}

/// Id знаний выдачи (без префикса ссылки `n:`/`i:`).
fn knowledge_ids(v: &Value) -> Vec<String> {
    v["knowledge"]
        .as_array()
        .unwrap_or_else(|| panic!("нет knowledge: {v}"))
        .iter()
        .map(|k| {
            let id = k["id"].as_str().unwrap();
            id.split_once(':').map_or(id, |(_, x)| x).to_string()
        })
        .collect()
}

#[tokio::test]
async fn symbol_card_refs_and_knowledge() {
    let w = linked("card").await;
    let id = symbol_id(&w.a, "foo").await;
    let c = get_ok(&w.a, &format!("/api/v1/code/symbols/{id}?solution={G}")).await;
    assert_eq!(c["symbol"]["qual"], "foo", "{c}");
    assert_eq!(c["symbol"]["path"], "a.rs");
    assert_eq!(c["symbol"]["sig"], "fn foo()");
    assert!(c["symbol"]["body_hash"].is_string());
    assert_eq!(
        c["callers"],
        json!([{"file_id": c["callers"][0]["file_id"], "path": "b.rs", "count": 3}])
    );
    assert_eq!(c["uses"][0]["name"], "helper");
    let ids = knowledge_ids(&c);
    assert_eq!(
        ids,
        [w.foo_note.clone(), w.insight.clone()],
        "символьная первой, чужой символ — нет: {c}"
    );
    assert!(!ids.contains(&w.bar_note));
    assert_eq!(c["knowledge"][0]["symbol"], "foo");
    assert_eq!(c["knowledge_left"], 0);
    assert!(c["knowledge"][1]["symbol"].is_null());
    assert!(c["tokens"].as_u64().unwrap() > 0);
    assert_eq!(c["symbol"]["ref"], "a.rs#foo");
    // постоянный адрес символа — путь#qual (id меняется при перезагрузке файла)
    let by_ref = get_ok(
        &w.a,
        &format!("/api/v1/code/symbols/{}?solution={G}", enc("a.rs#foo")),
    )
    .await;
    assert_eq!(by_ref["symbol"]["id"], id);
    // нет символа — 404 своего типа (MCP отличает его от выключенной карты, ревью задачи 7)
    for uri in [
        format!("/api/v1/code/symbols/{}?solution={G}", enc("a.rs#nope")),
        format!("/api/v1/code/symbols/999999?solution={G}"),
    ] {
        let r = get(&w.a, &uri).await;
        ok(&r, StatusCode::NOT_FOUND, &uri);
        assert_eq!(
            r.json()["type"],
            "/problems/code-symbol-not-found",
            "{uri}: {}",
            r.text()
        );
    }
    // символ без решения — 400
    ok(
        &get(&w.a, &format!("/api/v1/code/symbols/{id}")).await,
        StatusCode::BAD_REQUEST,
        "без решения",
    );
}

#[tokio::test]
async fn impact_changed_removed_consumers_knowledge() {
    let w = linked("impact").await;
    // foo — другое тело, bar — то же, baz — удалён
    let r = impact(
        &w.a,
        json!([{"path": "a.rs", "symbols": [
            {"qual": "foo", "body_hash": bh(99)},
            {"qual": "bar", "body_hash": bh(10)},
        ]}]),
    )
    .await;
    ok(&r, StatusCode::OK, "влияние");
    let j = r.json();
    let quals = |k: &str| -> Vec<String> {
        j[k].as_array()
            .unwrap()
            .iter()
            .map(|s| s["qual"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(quals("changed"), ["foo"], "{j}");
    assert_eq!(quals("removed"), ["baz"], "{j}");
    let consumers: Vec<&str> = j["consumers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert_eq!(consumers, ["b.rs", "c.rs"], "по числу ссылок: {j}");
    let ids = knowledge_ids(&j);
    assert!(
        ids.contains(&w.foo_note),
        "знание об изменённом символе: {j}"
    );
    assert!(ids.contains(&w.insight), "знание о файле: {j}");
    assert!(!ids.contains(&w.bar_note), "bar не менялся: {j}");
    assert_eq!(j["knowledge"][0]["path"], "a.rs");
    assert!(j["paths_only"].as_array().unwrap().is_empty());
    assert!(j["tokens"].as_u64().unwrap() > 0);
    // ничего не изменилось — влияния нет
    let j = impact(
        &w.a,
        json!([{"path": "a.rs", "symbols": [
            {"qual": "foo", "body_hash": bh(1)},
            {"qual": "bar", "body_hash": bh(10)},
            {"qual": "baz", "body_hash": bh(20)},
        ]}]),
    )
    .await
    .json();
    assert!(j["changed"].as_array().unwrap().is_empty(), "{j}");
    assert!(j["knowledge"].as_array().unwrap().is_empty(), "{j}");
}

#[tokio::test]
async fn impact_paths_only_knowledge_by_path() {
    let w = linked("impact-paths").await;
    let r = impact(&w.a, json!([{"path": "./a.rs"}, {"path": "zz.rs"}])).await;
    ok(&r, StatusCode::OK, "по путям");
    let j = r.json();
    assert_eq!(j["paths_only"], json!(["a.rs", "zz.rs"]), "{j}");
    let ids = knowledge_ids(&j);
    for want in [&w.foo_note, &w.bar_note, &w.insight] {
        assert!(ids.contains(want), "все знания о пути: {j}");
    }
    assert_eq!(ids[2], w.insight, "символьные первыми: {j}");
}

#[tokio::test]
async fn limits_413_422_with_limit_in_detail() {
    let a = world("limits", |c| {
        c.code.max_files = 3;
        c.code.max_symbols_per_file = 2;
        c.code.max_refs_per_file = 2;
        c.code.max_request_bytes = 4096;
    })
    .await;
    let r = sync(&a, &[("a.rs", 1), ("b.rs", 2), ("c.rs", 3), ("d.rs", 4)]).await;
    unprocessable(&r, "3", "манифест > max_files");
    let r = put_raw(
        &a,
        vec![file(
            "a.rs",
            1,
            vec![func("x1", 1), func("x2", 2), func("x3", 3)],
            &[],
        )],
    )
    .await;
    unprocessable(&r, "2", "символов > max_symbols_per_file");
    let r = put_raw(
        &a,
        vec![file(
            "a.rs",
            1,
            vec![],
            &[("p", "call", 1), ("q", "call", 1), ("r", "call", 1)],
        )],
    )
    .await;
    unprocessable(&r, "2", "ссылок > max_refs_per_file");
    // I4: карта целиком — не больше max_files (3): текущие файлы + новые пути загрузки
    let three: Vec<Value> = ["a.rs", "b.rs", "c.rs"]
        .iter()
        .enumerate()
        .map(|(i, p)| file(p, i as u64 + 1, vec![], &[]))
        .collect();
    put(&a, three).await;
    let r = put_raw(&a, vec![file("d.rs", 4, vec![], &[])]).await;
    unprocessable(&r, "3", "карта > max_files");
    assert!(
        r.json()["detail"]
            .as_str()
            .unwrap()
            .contains("code.max_files")
    );
    ok(
        &put_raw(&a, vec![file("a.rs", 9, vec![], &[])]).await,
        StatusCode::OK,
        "тот же путь",
    );
    let r = a
        .t(
            Method::PUT,
            "/api/v1/code/files",
            Some(json!({"solution": G, "files": [], "pad": "x".repeat(5000)})),
        )
        .await;
    ok(
        &r,
        StatusCode::PAYLOAD_TOO_LARGE,
        "тело > max_request_bytes",
    );
    assert!(
        r.json()["detail"].as_str().unwrap().contains("4096"),
        "{}",
        r.text()
    );
    let r = a
        .t(
            Method::POST,
            "/api/v1/code/sync",
            Some(json!({"solution": G, "manifest": [], "pad": "x".repeat(5000)})),
        )
        .await;
    ok(
        &r,
        StatusCode::PAYLOAD_TOO_LARGE,
        "манифест > max_request_bytes",
    );
    let m = |s: &str| format!("/api/v1/code/map?solution={G}&{s}");
    unprocessable(&get(&a, &m("budget_tokens=0")).await, "20000", "бюджет 0");
    unprocessable(
        &get(&a, &m("budget_tokens=20001")).await,
        "20000",
        "бюджет 20001",
    );
    let long = enc(&json!({"text": "a".repeat(2001)}).to_string());
    unprocessable(
        &get(&a, &m(&format!("focus={long}"))).await,
        "2000",
        "focus.text",
    );
    unprocessable(
        &get(&a, &m("focus=%7Bnot-json")).await,
        "focus",
        "focus не JSON",
    );
    unprocessable(&get(&a, &m("format=xml")).await, "format", "формат");

    // файлов в одной загрузке ≤ 500 и во влиянии — тоже (предел тела по умолчанию)
    let b = world("limits-500", |_| {}).await;
    let many: Vec<Value> = (0..501)
        .map(|i| file(&format!("m{i}.rs"), i, vec![], &[]))
        .collect();
    unprocessable(&put_raw(&b, many).await, "500", "файлов > 500");
    let paths: Vec<Value> = (0..501)
        .map(|i| json!({"path": format!("m{i}.rs")}))
        .collect();
    unprocessable(&impact(&b, json!(paths)).await, "500", "влияние > 500");
}

#[tokio::test]
async fn validation_names_qual_control_chars_422() {
    let a = world("validate", |_| {}).await;
    let bad_sym = |k: &str, v: Value| {
        let mut s = func("ok_name", 1);
        s[k] = v;
        vec![file("a.rs", 1, vec![s], &[])]
    };
    let cases: Vec<(Vec<Value>, &str)> = vec![
        (
            bad_sym("name", json!("n".repeat(257))),
            "files[0].symbols[0].name",
        ),
        (
            bad_sym("name", json!("bad\u{7}name")),
            "files[0].symbols[0].name",
        ),
        (bad_sym("name", json!("")), "files[0].symbols[0].name"),
        (
            bad_sym("qual", json!("q".repeat(513))),
            "files[0].symbols[0].qual",
        ),
        (bad_sym("qual", json!("A::\nB")), "files[0].symbols[0].qual"),
        (bad_sym("qual", json!("a/b::c")), "files[0].symbols[0].qual"),
        (
            bad_sym("sig", json!("s".repeat(161))),
            "files[0].symbols[0].sig",
        ),
        (
            bad_sym("sig", json!("fn a()\u{0}")),
            "files[0].symbols[0].sig",
        ),
        (
            bad_sym("body_hash", json!("xyz")),
            "files[0].symbols[0].body_hash",
        ),
        (bad_sym("line", json!(0)), "files[0].symbols[0].line"),
        (bad_sym("kind", json!("lambda")), "files[0]"),
        (
            vec![file("a.rs", 1, vec![], &[("bad\u{1b}ref", "call", 1)])],
            "files[0].refs[0].name",
        ),
        (vec![file("../a.rs", 1, vec![], &[])], "files[0].path"),
        (vec![file("/abs/a.rs", 1, vec![], &[])], "files[0].path"),
    ];
    for (files, needle) in cases {
        let r = put_raw(&a, files).await;
        unprocessable(&r, needle, needle);
    }
    let mut f = file("a.rs", 1, vec![], &[]);
    f["hash"] = json!("abc");
    unprocessable(&put_raw(&a, vec![f]).await, "files[0].hash", "хэш файла");
    let mut f = file("a.rs", 1, vec![], &[]);
    f["git_oid"] = json!("1234");
    unprocessable(&put_raw(&a, vec![f]).await, "files[0].git_oid", "git oid");
    let mut f = file("a.rs", 1, vec![], &[]);
    f["git_oid"] = json!("a".repeat(40));
    ok(&put_raw(&a, vec![f]).await, StatusCode::OK, "oid sha1");
    let r = a
        .t(
            Method::PUT,
            "/api/v1/code/files",
            Some(json!({"solution": G, "branch": "main\u{1}", "files": []})),
        )
        .await;
    unprocessable(&r, "branch", "ветка с управляющим");
    let r = a
        .t(
            Method::PUT,
            "/api/v1/code/files",
            Some(json!({"solution": G, "commit": "xyz", "files": []})),
        )
        .await;
    unprocessable(&r, "commit", "коммит");
    // символ с табуляцией внутри сигнатуры допустим
    let mut s = func("tabbed", 1);
    s["sig"] = json!("type T struct {\tA int");
    ok(
        &put_raw(&a, vec![file("t.rs", 2, vec![s], &[])]).await,
        StatusCode::OK,
        "табуляция",
    );
}

#[tokio::test]
async fn code_sync_manifest_duplicate_and_outside_root_422() {
    let a = world("manifest", |_| {}).await;
    let raw = |m: Value| {
        a.t(
            Method::POST,
            "/api/v1/code/sync",
            Some(json!({"solution": G, "manifest": m})),
        )
    };
    let r = raw(json!([{"path": "src/a.rs", "hash": h(1)}, {"path": "./src//a.rs", "hash": h(2)}]))
        .await;
    unprocessable(&r, "manifest[1]: повтор пути", "повтор после нормализации");
    let r =
        raw(json!([{"path": "src\\A.rs", "hash": h(1)}, {"path": "src/A.rs", "hash": h(1)}])).await;
    unprocessable(&r, "manifest[1]: повтор пути", "разделитель Windows");
    let r = raw(json!([{"path": "../outside.rs", "hash": h(1)}])).await;
    unprocessable(&r, "manifest[0].path", "выше корня");
    let r = raw(json!([{"path": "ok.rs", "hash": h(1)}, {"path": "/abs.rs", "hash": h(1)}])).await;
    unprocessable(&r, "manifest[1].path", "абсолютный путь");
    let r = raw(json!([{"path": "C:/abs.rs", "hash": h(1)}])).await;
    unprocessable(&r, "manifest[0].path", "буква диска");
    let r = raw(json!([{"path": "a.rs", "hash": "nothex"}])).await;
    unprocessable(&r, "manifest[0].hash", "хэш");
    // регистр различается: разные пути
    let r = raw(
        json!([{"path": "src\\Store.RS", "hash": h(1)}, {"path": "src/store.rs", "hash": h(2)}]),
    )
    .await;
    ok(&r, StatusCode::OK, "регистр значим");
    assert_eq!(r.json()["need"], json!(["src/Store.RS", "src/store.rs"]));
}

#[tokio::test]
async fn code_disabled_404() {
    let a = world("disabled", |c| c.code.enabled = false).await;
    let routes: [(Method, String, Option<Value>); 9] = [
        (
            Method::GET,
            format!("/api/v1/code/files?solution={G}"),
            None,
        ),
        (
            Method::POST,
            "/api/v1/code/sync".into(),
            Some(json!({"solution": G, "manifest": []})),
        ),
        (
            Method::PUT,
            "/api/v1/code/files".into(),
            Some(json!({"solution": G, "files": []})),
        ),
        (
            Method::GET,
            format!("/api/v1/code/status?solution={G}"),
            None,
        ),
        (Method::GET, format!("/api/v1/code/map?solution={G}"), None),
        (
            Method::GET,
            format!("/api/v1/code/symbols?solution={G}&q=x"),
            None,
        ),
        (
            Method::GET,
            format!("/api/v1/code/symbols/1?solution={G}"),
            None,
        ),
        (
            Method::POST,
            "/api/v1/code/impact".into(),
            Some(json!({"solution": G, "files": []})),
        ),
        (Method::DELETE, format!("/api/v1/code?solution={G}"), None),
    ];
    for (m, uri, body) in routes {
        let r = a.adm(m.clone(), &uri, body, None).await;
        ok(&r, StatusCode::NOT_FOUND, &format!("{m} {uri}"));
        assert_eq!(r.header("content-type"), "application/problem+json");
    }
}

#[tokio::test]
async fn responses_have_tokens() {
    let w = linked("tokens").await;
    let a = &w.a;
    let id = symbol_id(a, "foo").await;
    for uri in [
        format!("/api/v1/code/map?solution={G}"),
        format!("/api/v1/code/map?solution={G}&format=json"),
        format!("/api/v1/code/symbols?solution={G}&q=foo"),
        format!("/api/v1/code/symbols/{id}?solution={G}"),
    ] {
        let j = get_ok(a, &uri).await;
        assert!(j["tokens"].as_u64().unwrap() > 0, "{uri}: {j}");
    }
    let j = impact(a, json!([{"path": "a.rs"}])).await.json();
    assert!(j["tokens"].as_u64().unwrap() > 0, "{j}");
}

#[tokio::test]
async fn code_routes_not_counted_as_memory_writes() {
    let a = world("writes", |_| {}).await;
    let sess = [("x-zam-session", "s-code")];
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/code/sync",
            Some(json!({"solution": G, "manifest": [{"path": "a.rs", "hash": h(1)}]})),
            &sess,
        )
        .await;
    ok(&r, StatusCode::OK, "синхронизация");
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            "/api/v1/code/files",
            Some(json!({"solution": G, "files": [file("a.rs", 1, vec![func("alpha", 1)], &[])]})),
            &sess,
        )
        .await;
    ok(&r, StatusCode::OK, "загрузка");
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/code/impact",
            Some(json!({"solution": G, "files": [{"path": "a.rs"}]})),
            &sess,
        )
        .await;
    ok(&r, StatusCode::OK, "влияние");
    // одна запись в память той же сессией — счётчик ровно 1
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": G, "path": "Заметка", "kind": "note", "content": "x"})),
            &sess,
        )
        .await;
    ok(&r, StatusCode::CREATED, "заметка");
    let s = get_ok(&a, "/api/v1/sessions/s-code").await;
    assert_eq!(s["memory_writes"], 1, "{s}");
}

/// `git rm` у владельца (ревью задачи 3, I3): файл исчезает из карты, ссылки на его символы —
/// `stale` (их число — `stale_refs` ответа `sync`), файловая ссылка не трогается.
#[tokio::test]
async fn sync_removed_file_marks_symbol_refs_stale() {
    let w = linked("sync-rm").await;
    let r = sync(&w.a, &[("b.rs", 2), ("c.rs", 3)]).await;
    ok(&r, StatusCode::OK, "синхронизация без a.rs");
    let j = r.json();
    assert_eq!(j["removed"], 1, "{j}");
    assert_eq!(j["stale_refs"], 2, "заметки о foo и bar: {j}");
    let idx = get_ok(&w.a, &format!("/api/v1/file-refs?solution={G}")).await;
    let items = idx["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == "a.rs")
        .unwrap()["items"]
        .clone();
    for it in items.as_array().unwrap() {
        let want = if it["symbol"].is_string() {
            "stale"
        } else {
            "ok"
        };
        assert_eq!(it["state"], want, "{it}");
    }
    // повтор той же синхронизации — ничего не удалено
    let j = sync(&w.a, &[("b.rs", 2), ("c.rs", 3)]).await.json();
    assert_eq!(
        (j["removed"].clone(), j["stale_refs"].clone()),
        (json!(0), json!(0))
    );
}

/// Две фазы карты: файлов больше первой порции (64) — символы дочитываются, пока все
/// прочитанные файлы показаны; хвост считает всю карту.
#[tokio::test]
async fn map_two_phase_many_files_tail_counts_all() {
    let a = world("two-phase", |_| {}).await;
    let files: Vec<Value> = (0..300u64)
        .map(|i| {
            let refs: &[(&str, &str, u32)] = if i > 0 {
                &[("root_fn_000", "call", 1)]
            } else {
                &[]
            };
            file(
                &format!("p/f{i:03}.rs"),
                i + 1,
                vec![func(&format!("root_fn_{i:03}"), 1)],
                refs,
            )
        })
        .collect();
    put(&a, files).await;
    wait_ranked(&a).await;
    let m = map_json(&a, "&budget_tokens=20000").await;
    assert_eq!(m["files_shown"], 300, "все файлы: {}", m["files_left"]);
    assert_eq!(m["symbols_shown"], 300);
    assert_eq!(m["files_left"], 0);
    assert_eq!(paths_of(&m)[0], "p/f000.rs", "на него ссылаются все");
    for budget in [300, 1000] {
        let m = map_json(&a, &format!("&budget_tokens={budget}")).await;
        let (shown, left) = (
            m["files_shown"].as_u64().unwrap(),
            m["files_left"].as_u64().unwrap(),
        );
        assert_eq!(shown + left, 300, "{budget}");
        assert!(shown > 0 && left > 0, "{budget}: {shown}/{left}");
        let syms = m["symbols_shown"].as_u64().unwrap() + m["symbols_left"].as_u64().unwrap();
        assert_eq!(syms, 300);
    }
    // дешёвые файлы (без символов, ≈ 3 токена): показано больше первой порции (бюджет / 8)
    let b = world("two-phase-cheap", |_| {}).await;
    for part in [0..300u64, 300..600] {
        let cheap: Vec<Value> = part
            .map(|i| file(&format!("q/a{i}.rs"), i + 1, vec![], &[]))
            .collect();
        put(&b, cheap).await;
    }
    let m = map_json(&b, "&budget_tokens=2000").await;
    let shown = m["files_shown"].as_u64().unwrap();
    assert!(shown > 2000 / 8, "порция дочитана: {shown}");
    assert_eq!(shown + m["files_left"].as_u64().unwrap(), 600);
    assert!(m["tokens"].as_u64().unwrap() <= 2000);
    let md = get_ok(
        &b,
        &format!("/api/v1/code/map?solution={G}&budget_tokens=2000"),
    )
    .await;
    let text = md["text"].as_str().unwrap();
    assert!(
        !text.lines().any(str::is_empty),
        "непрочитанные файлы не показаны: {text}"
    );
    assert_eq!(
        text.lines().filter(|l| l.starts_with("q/a")).count() as u64,
        shown
    );
    let md = get_ok(
        &a,
        &format!("/api/v1/code/map?solution={G}&budget_tokens=300"),
    )
    .await;
    let text = md["text"].as_str().unwrap();
    let left = md["files_left"].as_u64().unwrap();
    assert!(text.contains(&format!("… ещё {left} ")), "хвост: {text}");
}

/// Ревью задачи 4, I1: манифест на `code.max_files` длинных путей укладывается в предел тела
/// `POST /code/sync` (50 000 путей по 120 символов — ≈ 11 МБ при `max_request_bytes` 8 МиБ).
#[tokio::test]
async fn sync_manifest_50000_long_paths_passes() {
    let a = world("sync-big", |_| {}).await;
    let dir = "d".repeat(100);
    let manifest: Vec<Value> = (0..50_000)
        .map(|i| json!({"path": format!("{dir}/f{i:013}.rs"), "hash": h(i)}))
        .collect();
    assert_eq!(format!("{dir}/f{:013}.rs", 0).chars().count(), 118);
    let body = json!({"solution": G, "manifest": manifest});
    assert!(
        body.to_string().len() > 8_388_608,
        "тело больше max_request_bytes"
    );
    let r = a.t(Method::POST, "/api/v1/code/sync", Some(body)).await;
    ok(&r, StatusCode::OK, "большой манифест");
    assert_eq!(r.json()["need"].as_array().unwrap().len(), 50_000);
}

/// Ревью задачи 4, I3: ранжирование с фокусом дольше срока — карта без фокуса и
/// предупреждение, заголовок фокус не называет.
#[tokio::test]
async fn focus_rank_timeout_falls_back_with_warning() {
    let a = world("focus-timeout", |c| c.code.rank_timeout_ms = 1).await;
    put(
        &a,
        vec![
            file("src/a.rs", 1, vec![func("alpha_function", 1)], &[]),
            file(
                "src/b.rs",
                2,
                vec![func("beta_function", 1)],
                &[("alpha_function", "call", 3)],
            ),
        ],
    )
    .await;
    wait_ranked(&a).await;
    let global = paths_of(&map_json(&a, "").await);
    let focus = enc(&json!({"paths": ["src/b.rs"]}).to_string());
    // без задержки — фокус работает
    let m = map_json(&a, &format!("&focus={focus}")).await;
    assert_eq!(paths_of(&m)[0], "src/b.rs", "{m}");
    assert!(m["header"].as_str().unwrap().contains("с фокусом"));
    // счёт дольше срока max(rank_timeout_ms, 200) — глобальный порядок
    a.st.code
        .hooks
        .focus_delay_ms
        .store(400, std::sync::atomic::Ordering::SeqCst);
    let m = map_json(&a, &format!("&focus={focus}")).await;
    assert_eq!(m["warnings"], json!(["focus rank timeout"]), "{m}");
    assert_eq!(paths_of(&m), global);
    assert!(!m["header"].as_str().unwrap().contains("с фокусом"), "{m}");
}

/// Ревью задачи 4: загрузка во время пересчёта — ранги по старым входам не сохраняются,
/// ответ с `rank pending`; следующий пересчёт доводит до `ok`.
#[tokio::test]
async fn upload_during_ranking_keeps_pending_warning() {
    let a = world("rank-race", |c| c.code.rank_timeout_ms = 5000).await;
    put(
        &a,
        vec![
            file("src/a.rs", 1, vec![func("alpha", 1)], &[]),
            file(
                "src/b.rs",
                2,
                vec![func("beta", 1)],
                &[("alpha", "call", 1)],
            ),
        ],
    )
    .await;
    a.st.code
        .hooks
        .rank_delay_ms
        .store(400, std::sync::atomic::Ordering::SeqCst);
    let (m, ()) = tokio::join!(map_json(&a, ""), async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        put(&a, vec![file("src/c.rs", 3, vec![func("gamma", 1)], &[])]).await;
    });
    assert_eq!(m["warnings"], json!(["rank pending"]), "{m}");
    a.st.code
        .hooks
        .rank_delay_ms
        .store(0, std::sync::atomic::Ordering::SeqCst);
    let m = map_json(&a, "").await;
    assert_eq!(m["warnings"], json!([]), "{m}");
    assert_eq!(m["rank_state"], "ok");
}

/// Ревью задачи 4: два одновременных запроса карты — один пересчёт рангов (single-flight).
#[tokio::test]
async fn parallel_maps_run_one_ranking() {
    let a = world("single-flight", |c| c.code.rank_timeout_ms = 5000).await;
    put(&a, vec![file("src/a.rs", 1, vec![func("alpha", 1)], &[])]).await;
    a.st.code
        .hooks
        .rank_delay_ms
        .store(1500, std::sync::atomic::Ordering::SeqCst);
    let (m1, m2) = tokio::join!(map_json(&a, ""), map_json(&a, ""));
    let mut w = [m1["warnings"].clone(), m2["warnings"].clone()];
    w.sort_by_key(|v| v.as_array().map_or(0, Vec::len));
    assert_eq!(
        w,
        [json!([]), json!(["rank pending"])],
        "второй — без второго пересчёта"
    );
    assert_eq!(
        a.st.code
            .hooks
            .rank_runs
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

/// Ревью задачи 4, I5: таблица файлов по рангу постранично, фильтр языка и подстроки пути.
#[tokio::test]
async fn code_files_page_lang_q_offset_limit() {
    let a = world("files-page", |_| {}).await;
    let mut files: Vec<Value> = (0..5u64)
        .map(|i| {
            file(
                &format!("src/r{i}.rs"),
                i + 1,
                vec![func(&format!("r_fn_{i}"), 1)],
                &[("hub_fn", "call", 1)],
            )
        })
        .collect();
    files.push(file("src/hub.rs", 10, vec![func("hub_fn", 1)], &[]));
    files.push(file(
        "web/App.ts",
        11,
        vec![func("app", 1), func("app2", 2)],
        &[],
    ));
    put(&a, files).await;
    let q = |s: &str| format!("/api/v1/code/files?solution={G}&{s}");
    let j = get_ok(&a, &q("limit=3")).await;
    assert_eq!(j["total"], 7, "{j}");
    assert_eq!(
        (j["offset"].clone(), j["limit"].clone()),
        (json!(0), json!(3))
    );
    let page: Vec<&Value> = j["files"].as_array().unwrap().iter().collect();
    assert_eq!(page.len(), 3);
    assert_eq!(page[0]["path"], "src/hub.rs", "по рангу: {j}");
    for k in ["id", "lang", "rank", "symbols"] {
        assert!(!page[0][k].is_null(), "{k}");
    }
    let j = get_ok(&a, &q("limit=3&offset=6")).await;
    assert_eq!(j["files"].as_array().unwrap().len(), 1);
    let j = get_ok(&a, &q("lang=typescript")).await;
    assert_eq!(j["total"], 1);
    assert_eq!(j["files"][0]["symbols"], 2);
    let j = get_ok(&a, &q("q=APP")).await;
    assert_eq!(j["files"][0]["path"], "web/App.ts", "без учёта регистра");
    unprocessable(&get(&a, &q("limit=201")).await, "200", "предел страницы");
    unprocessable(&get(&a, &q("lang=cobol")).await, "lang", "язык");
}

/// Ревью задачи 4, O1: чтение входов для фокуса — отдельной задачей, одной на решение и снимок;
/// срок запроса его не отменяет: первые запросы откатываются без фокуса, следующий — с фокусом
/// из кэша; чтение — одно на два параллельных запроса.
#[tokio::test]
async fn focus_inputs_load_survives_timeout_single_read() {
    use std::sync::atomic::Ordering;
    let a = world("focus-load", |c| c.code.rank_timeout_ms = 1).await;
    put(
        &a,
        vec![
            file("src/a.rs", 1, vec![func("alpha_function", 1)], &[]),
            file(
                "src/b.rs",
                2,
                vec![func("beta_function", 1)],
                &[("alpha_function", "call", 3)],
            ),
        ],
    )
    .await;
    wait_ranked(&a).await;
    let hooks = &a.st.code.hooks;
    a.st.code.forget_inputs();
    hooks.inputs_delay_ms.store(3000, Ordering::SeqCst);
    let focus = enc(&json!({"paths": ["src/b.rs"]}).to_string());
    let extra = format!("&focus={focus}");
    let (m1, m2) = tokio::join!(map_json(&a, &extra), map_json(&a, &extra));
    for m in [&m1, &m2] {
        assert_eq!(m["warnings"], json!(["focus rank timeout"]), "{m}");
    }
    assert_eq!(
        hooks.inputs_reads.load(Ordering::SeqCst),
        1,
        "одно чтение на два запроса"
    );
    // чтение дочиталось в фоне и заполнило кэш
    for _ in 0..200 {
        if a.st.code.hooks.inputs_reads.load(Ordering::SeqCst) == 1
            && map_json(&a, &extra).await["warnings"] == json!([])
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let m = map_json(&a, &extra).await;
    assert_eq!(m["warnings"], json!([]), "{m}");
    assert!(m["header"].as_str().unwrap().contains("с фокусом"), "{m}");
    assert_eq!(paths_of(&m)[0], "src/b.rs");
    assert_eq!(hooks.inputs_reads.load(Ordering::SeqCst), 1, "из кэша");
}

/// Ревью задачи 4, O3: с `X-Zam-Solution` права проверяются до чтения тела — токен `ro` получает
/// 403 и на теле больше предела; `solution` тела обязан совпадать с заголовком; манифест —
/// строгий `{path, hash}`.
#[tokio::test]
async fn sync_header_checks_rights_before_body() {
    let a = world("sync-header", |c| {
        c.code.max_files = 1;
        c.code.max_request_bytes = 2048;
    })
    .await;
    let ro = a.issue(&a.admin_id(), "ro").await;
    let big = json!({"solution": G, "manifest": [], "pad": "x".repeat(10_000)});
    let hdr = [("x-zam-solution", G)];
    let r = a
        .tok_h(
            &ro,
            Method::POST,
            "/api/v1/code/sync",
            Some(big.clone()),
            &hdr,
        )
        .await;
    ok(&r, StatusCode::FORBIDDEN, "ro до чтения тела");
    let r = a
        .tok(&ro, Method::POST, "/api/v1/code/sync", Some(big.clone()))
        .await;
    ok(
        &r,
        StatusCode::PAYLOAD_TOO_LARGE,
        "без заголовка — сначала тело",
    );
    let r = a
        .tok_h(&a.token, Method::POST, "/api/v1/code/sync", Some(big), &hdr)
        .await;
    ok(&r, StatusCode::PAYLOAD_TOO_LARGE, "rw: тело больше предела");
    let other = json!({"solution": "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e99", "manifest": []});
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/code/sync",
            Some(other),
            &hdr,
        )
        .await;
    unprocessable(&r, "X-Zam-Solution", "решение тела другое");
    let body = json!({"solution": G, "manifest": [{"path": "a.rs", "hash": h(1)}]});
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/code/sync",
            Some(body),
            &hdr,
        )
        .await;
    ok(&r, StatusCode::OK, "с заголовком");
    let body = json!({"solution": G, "manifest": [{"path": "a.rs", "hash": h(1), "x": 1}]});
    let r = a.t(Method::POST, "/api/v1/code/sync", Some(body)).await;
    ok(
        &r,
        StatusCode::BAD_REQUEST,
        "лишнее поле элемента манифеста",
    );
}

/// Ревью задачи 4: паника чтения входов не оставляет «вечную загрузку» — следующий запрос с
/// фокусом читает входы заново и получает карту с фокусом.
#[tokio::test]
async fn focus_inputs_panic_does_not_stick() {
    use std::sync::atomic::Ordering;
    let a = world("focus-panic", |c| c.code.rank_timeout_ms = 5000).await;
    put(
        &a,
        vec![
            file("src/a.rs", 1, vec![func("alpha_function", 1)], &[]),
            file(
                "src/b.rs",
                2,
                vec![func("beta_function", 1)],
                &[("alpha_function", "call", 3)],
            ),
        ],
    )
    .await;
    wait_ranked(&a).await;
    let hooks = &a.st.code.hooks;
    a.st.code.forget_inputs();
    hooks.inputs_panic.store(true, Ordering::SeqCst);
    let focus = enc(&json!({"paths": ["src/b.rs"]}).to_string());
    let uri = format!("/api/v1/code/map?solution={G}&format=json&focus={focus}");
    let r = get(&a, &uri).await;
    ok(&r, StatusCode::INTERNAL_SERVER_ERROR, "паника чтения");
    let m = get_ok(&a, &uri).await;
    assert!(m["header"].as_str().unwrap().contains("с фокусом"), "{m}");
    assert_eq!(hooks.inputs_reads.load(Ordering::SeqCst), 2);
}

/// Ревью задачи 4: запросы с фокусом во время синхронизации (снимок меняется, `etag` другой)
/// не читают входы решения параллельно — второй ждёт первое чтение и читает сам.
#[tokio::test]
async fn focus_inputs_one_read_per_solution_while_map_changes() {
    use std::sync::atomic::Ordering;
    let a = world("focus-flight", |c| c.code.rank_timeout_ms = 10_000).await;
    put(
        &a,
        vec![
            file("src/a.rs", 1, vec![func("alpha_function", 1)], &[]),
            file(
                "src/b.rs",
                2,
                vec![func("beta_function", 1)],
                &[("alpha_function", "call", 3)],
            ),
        ],
    )
    .await;
    wait_ranked(&a).await;
    let hooks = &a.st.code.hooks;
    a.st.code.forget_inputs();
    hooks.inputs_delay_ms.store(800, Ordering::SeqCst);
    let focus = enc(&json!({"paths": ["src/b.rs"]}).to_string());
    let extra = format!("&focus={focus}");
    let (m1, m2) = tokio::join!(map_json(&a, &extra), async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        put(
            &a,
            vec![file("src/c.rs", 3, vec![func("gamma_function", 1)], &[])],
        )
        .await;
        map_json(&a, &extra).await
    });
    for m in [&m1, &m2] {
        assert!(m["header"].as_str().unwrap().contains("с фокусом"), "{m}");
    }
    assert!(paths_of(&m2).contains(&"src/c.rs".to_string()), "{m2}");
    // второе — своё чтение под новым etag или кэш, заполненный пересчётом рангов
    let reads = hooks.inputs_reads.load(Ordering::SeqCst);
    assert!((1..=2).contains(&reads), "чтений {reads}");
    assert_eq!(
        hooks.inputs_active_max.load(Ordering::SeqCst),
        1,
        "чтения решения не параллельны"
    );
}

/// Ревью задачи 4: обрыв тела `POST /code/sync` (не превышение предела) — 400, не 413.
#[tokio::test]
async fn sync_body_stream_error_is_400() {
    use axum::body::{Body, Bytes};
    let a = world("sync-body-err", |_| {}).await;
    let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
        Ok(Bytes::from_static(b"{\"solution\":")),
        Err(std::io::Error::other("обрыв соединения")),
    ];
    let req = common::api::req(Method::POST, "/api/v1/code/sync", Some(&json!({})))
        .header("authorization", format!("Bearer {}", a.token))
        .body(Body::from_stream(futures_util::stream::iter(chunks)))
        .unwrap();
    let r = a.send(req).await;
    ok(&r, StatusCode::BAD_REQUEST, "обрыв тела");
}

/// Ревью задачи 8: у записи со ссылками и на файл, и на его символ — одна строка знания
/// (символьная) в карточке и во влиянии; у знаний — `space` (id пространства записи).
#[tokio::test]
async fn knowledge_collapsed_per_record_with_space() {
    let w = linked("collapse").await;
    let both =
        w.a.note(
            G,
            "О foo и файле",
            "---\nrefs: [\"a.rs\", \"a.rs#foo\"]\n---\nтекст о коде\n",
        )
        .await;
    let both_id = both["id"].as_str().unwrap().to_string();
    let space = both["space_id"].as_str().unwrap().to_string();
    let card = get_ok(
        &w.a,
        &format!("/api/v1/code/symbols/{}?solution={G}", enc("a.rs#foo")),
    )
    .await;
    let ids = knowledge_ids(&card);
    assert_eq!(
        ids.iter().filter(|i| **i == both_id).count(),
        1,
        "одна строка: {card}"
    );
    let row = card["knowledge"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["id"].as_str().unwrap().ends_with(&both_id))
        .unwrap();
    assert_eq!(row["symbol"], "foo", "строка символа: {card}");
    for k in card["knowledge"].as_array().unwrap() {
        assert_eq!(k["space"], space.as_str(), "{card}");
    }
    let j = impact(
        &w.a,
        json!([{"path": "a.rs", "symbols": [
            {"qual": "foo", "body_hash": bh(99)},
            {"qual": "bar", "body_hash": bh(10)},
            {"qual": "baz", "body_hash": bh(20)},
        ]}]),
    )
    .await
    .json();
    let ids = knowledge_ids(&j);
    assert_eq!(ids.iter().filter(|i| **i == both_id).count(), 1, "{j}");
    assert!(
        ids.contains(&w.insight),
        "файловое знание другой записи: {j}"
    );
    assert!(j["knowledge"][0]["space"].is_string(), "{j}");
}

/// Ревью задачи 8: «Обзор» WebUI — `files=N`: показано не больше N файлов (остальные — в
/// `files_left`), рёбра только между ними; вне `1..=1000` — 422.
#[tokio::test]
async fn map_files_param_limits_files_and_edges() {
    let a = world("map-files", |c| c.code.rank_timeout_ms = 5000).await;
    let files: Vec<Value> = (0..6u64)
        .map(|i| {
            let callee = format!("fn_{}", (i + 1) % 6);
            file(
                &format!("src/f{i}.rs"),
                10 + i,
                vec![func(&format!("fn_{i}"), 1)],
                &[(callee.as_str(), "call", 1)],
            )
        })
        .collect();
    put(&a, files).await;
    wait_ranked(&a).await;
    let m = map_json(&a, "&files=3").await;
    let shown = paths_of(&m);
    assert_eq!(shown.len(), 3, "{m}");
    assert_eq!(m["files_shown"], 3, "{m}");
    assert_eq!(m["files_left"], 3, "{m}");
    let ids: Vec<&Value> = m["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| &f["id"])
        .collect();
    for e in m["edges"].as_array().unwrap() {
        assert!(ids.contains(&&e["src"]) && ids.contains(&&e["dst"]), "{m}");
    }
    let all = map_json(&a, "").await;
    assert_eq!(paths_of(&all)[..3], shown[..], "те же первые по рангу");
    for bad in ["0", "1001"] {
        unprocessable(
            &get(&a, &format!("/api/v1/code/map?solution={G}&files={bad}")).await,
            "files",
            bad,
        );
    }
}
