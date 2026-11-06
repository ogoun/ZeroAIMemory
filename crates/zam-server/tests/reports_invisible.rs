//! Невидимость отчётов для агента (задача 4 M5.5, спецификация §7, план — Review Focus 1):
//! токен не узнаёт о существовании отчёта ни по одному пути — поиск (с `include_human`, явным
//! `kinds ∋ report`), лента, `GET /entries/{id}` и `/revisions|links|backlinks`, `GET /graph`,
//! `POST /read` (REST и MCP), файлы отчёта, рёбра и `links`, `supersedes_id`, кэш выжимок,
//! `/recall`, `feedback`, изменяющие маршруты. Ответ на отчёт совпадает с ответом на
//! несуществующий id (тест-оракул: статус и тело problem, id заменён на `ID`).
//!
//! Мир — [`common::reports`]: владелец решения (агент владельца пишет отчёты), участники `rw` и
//! `ro`; серверы без моделей (FTS).
mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::api::{Api, Resp};
use common::reports::*;
use serde_json::{Value, json};

/// Слово заголовка отчёта (уникально для FTS).
const TITLE_WORD: &str = "zebraquant";
/// Слово тела отчёта.
const BODY_WORD: &str = "mandragorium";

/// Второе решение владельца (участникам не открыто).
const G2: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e77";

async fn world(name: &str) -> W {
    common::reports::world(&format!("reports-invisible-{name}")).await
}

/// Отчёт с уникальными словами: id.
async fn secret_report(w: &W) -> String {
    let p = png(5, 120);
    w.report_with(
        &format!("Отчёт {TITLE_WORD} обучения"),
        &format!(
            "# Итог\n\nВывод {BODY_WORD}: точность выросла.\n\n![loss](figs/loss.png)\n\n\
             Ошибка {BODY_WORD} failed at step 7.\n"
        ),
        &[("figs/loss.png", &p)],
    )
    .await
}

/// Ответ без id: статус и тело (id отчёта или несуществующий — `ID`).
fn shape(r: &Resp, id: &str) -> (StatusCode, String) {
    (r.status, r.text().replace(id, "ID"))
}

/// Ответ не упоминает отчёт: ни id, ни заголовка, ни слова тела (слово заголовка есть и у
/// видимого токену insight матрицы — ищется заголовок целиком).
fn clean(text: &str, id: &str, what: &str) {
    let title = format!("Отчёт {TITLE_WORD}");
    for needle in [id, title.as_str(), BODY_WORD] {
        assert!(
            !text.contains(needle),
            "{what}: «{needle}» в ответе: {text}"
        );
    }
}

/// Запрос токеном `tok`; `uri` и `body` строит `f` по id.
async fn tok_call(
    a: &Api,
    tok: &str,
    m: &Method,
    uri: &str,
    body: Option<Value>,
    extra: &[(&str, &str)],
) -> Resp {
    a.tok_h(tok, m.clone(), uri, body, extra).await
}

/// Оракул: один и тот же запрос с id отчёта и с несуществующим id даёт одинаковый ответ.
#[allow(clippy::too_many_arguments)]
async fn oracle(
    a: &Api,
    tok: &str,
    what: &str,
    report: &str,
    m: Method,
    uri: impl Fn(&str) -> String,
    body: impl Fn(&str) -> Option<Value>,
    extra: &[(&str, &str)],
) -> Resp {
    let g = common::ghost_like(report);
    let r = tok_call(a, tok, &m, &uri(report), body(report), extra).await;
    let n = tok_call(a, tok, &m, &uri(&g), body(&g), extra).await;
    assert_eq!(
        shape(&r, report),
        shape(&n, &g),
        "{what}: отчёт ≠ нет такого"
    );
    // id — только эхом запроса (там же, где несуществующий): прочего об отчёте нет
    clean(&r.text().replace(report, "ID"), report, what);
    r
}

/// JSON-RPC `tools/call` к `/mcp` по токену: текст результата.
async fn mcp(a: &Api, tok: &str, name: &str, args: Value) -> String {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": name, "arguments": args}});
    let r = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::HOST, "memory.lan:7700")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {tok}"))
        .header("x-zam-session", "sess-mcp")
        .body(Body::from(body.to_string()))
        .unwrap();
    let r = a.send(r).await;
    ok(&r, StatusCode::OK, name);
    let j = r.json();
    j["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Запись журнала токеном в решении `G`: id.
async fn entry(w: &W, tok: &str, body: Value) -> String {
    let r =
        w.a.tok(
            tok,
            Method::POST,
            &format!("/api/v1/spaces/{G}/entries"),
            Some(body),
        )
        .await;
    ok(&r, StatusCode::CREATED, "запись");
    r.json()["id"].as_str().unwrap().to_string()
}

/// Insight в решении `G` токеном владельца: id.
async fn insight(w: &W, title: &str) -> String {
    entry(
        w,
        &w.o.tok,
        json!({"kind": "insight", "title": title, "body": "Обычный вывод агента.",
               "data": {"kind": "discovery"}}),
    )
    .await
}

/// Поиск токеном `tok`.
async fn search(a: &Api, tok: &str, body: Value) -> Resp {
    a.tok(tok, Method::POST, "/api/v1/search", Some(body)).await
}

/// Поиск сессией WebUI.
async fn web_search(a: &Api, u: &Web, body: Value) -> Resp {
    web(a, u, Method::POST, "/api/v1/search", Some(body)).await
}

/// Токен `ro` владельца.
async fn ro_token(w: &W) -> String {
    w.a.issue(&w.o.id, "ro").await
}

/// Матрица Review Focus 1: каждый путь чтения и изменения по id отчёта отвечает токену так же,
/// как на несуществующий id; токены `rw` и `ro` владельца, токены участников `rw` и `ro`.
#[tokio::test]
async fn token_never_sees_report_matrix() {
    let w = world("matrix").await;
    let id = secret_report(&w).await;
    let ins = insight(&w, &format!("Инсайт {TITLE_WORD}")).await;
    let ro = ro_token(&w).await;
    let loss = w.file(&id, "loss.png").await;
    let sha = loss.sha256.clone();
    let rel = entry(
        &w,
        &w.o.tok,
        json!({"kind": "release", "title": "Релиз 1.0", "data": {"version": "1.0.0"}}),
    )
    .await;
    let none = |_: &str| None;
    let toks = [
        ("rw", w.o.tok.clone()),
        ("ro", ro),
        ("участник", w.rw.tok.clone()),
        ("участник-ro", w.ro.tok.clone()),
    ];
    for (who, tok) in &toks {
        let tok = tok.as_str();
        // чтение
        for (what, uri) in [
            ("GET /entries", "/api/v1/entries/{}"),
            ("GET /entries?format=json", "/api/v1/entries/{}?format=json"),
            ("GET /entries?format=html", "/api/v1/entries/{}?format=html"),
            ("GET /revisions", "/api/v1/entries/{}/revisions"),
            ("GET /links", "/api/v1/entries/{}/links"),
            ("GET /backlinks", "/api/v1/entries/{}/backlinks"),
            ("GET /graph?entry", "/api/v1/graph?entry={}"),
            ("GET /graph?entry depth 3", "/api/v1/graph?entry={}&depth=3"),
            ("GET /releases/compare", "/api/v1/releases/{}/compare"),
        ] {
            let r = oracle(
                &w.a,
                tok,
                &format!("{who}: {what}"),
                &id,
                Method::GET,
                |x| uri.replace("{}", x),
                none,
                &[],
            )
            .await;
            ok(&r, StatusCode::NOT_FOUND, &format!("{who}: {what}"));
        }
        // сравнение релиза с отчётом (`with=<id>`)
        let r = oracle(
            &w.a,
            tok,
            &format!("{who}: compare with"),
            &id,
            Method::GET,
            |x| format!("/api/v1/releases/{rel}/compare?with={x}"),
            none,
            &[],
        )
        .await;
        ok(&r, StatusCode::NOT_FOUND, "compare with");
        let r = oracle(
            &w.a,
            tok,
            &format!("{who}: файл отчёта"),
            &id,
            Method::GET,
            |x| format!("/api/v1/entries/{x}/attachments/{sha}/loss.png"),
            none,
            &[],
        )
        .await;
        ok(&r, StatusCode::NOT_FOUND, "файл");
        // изменения: PATCH любого поля, DELETE, restore, операции записей
        for (what, m, uri, body) in [
            (
                "PATCH title",
                Method::PATCH,
                "/api/v1/entries/{}",
                json!({"title": "x"}),
            ),
            (
                "PATCH triggers",
                Method::PATCH,
                "/api/v1/entries/{}",
                json!({"triggers": [{"type": "keyword", "pattern": "x"}]}),
            ),
            (
                "PATCH пусто",
                Method::PATCH,
                "/api/v1/entries/{}",
                json!({}),
            ),
            ("DELETE", Method::DELETE, "/api/v1/entries/{}", Value::Null),
            (
                "restore",
                Method::POST,
                "/api/v1/entries/{}/restore",
                Value::Null,
            ),
            (
                "supersede",
                Method::POST,
                "/api/v1/entries/{}/supersede",
                json!({"title": "Новый", "body": "т", "data": {"kind": "gotcha"}}),
            ),
            (
                "retract",
                Method::POST,
                "/api/v1/entries/{}/retract",
                json!({"reason": "неверно"}),
            ),
            (
                "answer",
                Method::POST,
                "/api/v1/entries/{}/answer",
                json!({"text": "да"}),
            ),
            ("claim", Method::POST, "/api/v1/entries/{}/claim", json!({})),
            (
                "release",
                Method::POST,
                "/api/v1/entries/{}/release",
                json!({}),
            ),
            (
                "feedback",
                Method::POST,
                "/api/v1/entries/{}/feedback",
                json!({"value": "useful"}),
            ),
        ] {
            let body = (!body.is_null()).then_some(body);
            let r = oracle(
                &w.a,
                tok,
                &format!("{who}: {what}"),
                &id,
                m,
                |x| uri.replace("{}", x),
                |_| body.clone(),
                &[("if-match", "1"), ("x-zam-session", "sess-agent")],
            )
            .await;
            ok(&r, StatusCode::NOT_FOUND, &format!("{who}: {what}"));
        }
        // рёбра с отчётом на любом конце
        let note = format!("n:{}", w.note(&format!("notes/{who}")).await);
        for (what, src_is_report) in [("ребро от отчёта", true), ("ребро к отчёту", false)]
        {
            let note = note.clone();
            let r = oracle(
                &w.a,
                tok,
                &format!("{who}: {what}"),
                &id,
                Method::POST,
                |_| "/api/v1/edges".into(),
                |x| {
                    let e = format!("e:{x}");
                    let (s, d) = if src_is_report {
                        (e, note.clone())
                    } else {
                        (note.clone(), e)
                    };
                    Some(json!({"src": s, "type": "relates_to", "dst": d}))
                },
                &[],
            )
            .await;
            assert!(r.status.is_client_error(), "{who}: {what}: {}", r.text());
        }
        // insight: supersedes_id и links на отчёт
        for (what, body) in [
            (
                "supersedes_id",
                json!({"kind": "insight", "title": "Новый", "body": "т",
                       "data": {"kind": "gotcha"}, "supersedes_id": "{}"}),
            ),
            (
                "links",
                json!({"kind": "insight", "title": "Связанный", "body": "т",
                       "data": {"kind": "gotcha"},
                       "links": [{"type": "relates_to", "target": "entry:{}"}]}),
            ),
        ] {
            let tpl = body.to_string();
            let r = oracle(
                &w.a,
                tok,
                &format!("{who}: insight {what}"),
                &id,
                Method::POST,
                |_| format!("/api/v1/spaces/{G}/entries"),
                |x| Some(serde_json::from_str(&tpl.replace("{}", x)).unwrap()),
                &[],
            )
            .await;
            assert!(r.status.is_client_error(), "{who}: {what}: {}", r.text());
        }
        // POST /read: p:, e:, i: и голый id — `not_found` элемента
        for pre in ["p:", "e:", "i:", ""] {
            let r = oracle(
                &w.a,
                tok,
                &format!("{who}: /read {pre}"),
                &id,
                Method::POST,
                |_| "/api/v1/read".into(),
                |x| Some(json!({"items": [{"id": format!("{pre}{x}")}]})),
                &[],
            )
            .await;
            ok(&r, StatusCode::OK, &format!("{who}: /read {pre}"));
            assert!(r.text().contains("not_found"), "{}", r.text());
        }
        // поиск: без видов, с include_human, явный kinds ∋ report — 400 как у неизвестного вида
        for body in [
            json!({"query": TITLE_WORD, "scope": {"solution": G}}),
            json!({"query": BODY_WORD, "scope": {"solution": G}, "format": "md"}),
            json!({"query": TITLE_WORD, "scope": {"solution": G, "include_human": true}}),
            json!({"query": TITLE_WORD, "scope": {"all_solutions": true}, "include_history": true}),
            json!({"query": TITLE_WORD, "scope": {"solution": G}, "expand": "graph"}),
        ] {
            let r = search(&w.a, tok, body.clone()).await;
            ok(&r, StatusCode::OK, &format!("{who}: поиск {body}"));
            clean(&r.text(), &id, &format!("{who}: поиск {body}"));
        }
        let r = search(
            &w.a,
            tok,
            json!({"query": TITLE_WORD, "filters": {"kinds": ["report"]}}),
        )
        .await;
        ok(&r, StatusCode::BAD_REQUEST, "kinds report");
        // лента
        let r =
            w.a.tok(
                tok,
                Method::GET,
                &format!("/api/v1/spaces/{G}/entries"),
                None,
            )
            .await;
        ok(&r, StatusCode::OK, "лента");
        clean(&r.text(), &id, &format!("{who}: лента"));
        let r =
            w.a.tok(
                tok,
                Method::GET,
                &format!("/api/v1/spaces/{G}/entries?kind=report"),
                None,
            )
            .await;
        ok(&r, StatusCode::BAD_REQUEST, "лента kind=report");
        // MCP memory_read и memory_link
        for pre in ["p:", "e:"] {
            let g = common::ghost_like(&id);
            let a = mcp(
                &w.a,
                tok,
                "memory_read",
                json!({"items": [{"id": format!("{pre}{id}")}]}),
            )
            .await;
            let b = mcp(
                &w.a,
                tok,
                "memory_read",
                json!({"items": [{"id": format!("{pre}{g}")}]}),
            )
            .await;
            assert_eq!(
                a.replace(&id, "ID"),
                b.replace(&g, "ID"),
                "{who}: MCP read {pre}"
            );
            clean(&a.replace(&id, "ID"), &id, "MCP memory_read");
            let a = mcp(
                &w.a,
                tok,
                "memory_link",
                json!({"src": note, "type": "relates_to", "dst": format!("{pre}{id}")}),
            )
            .await;
            let b = mcp(
                &w.a,
                tok,
                "memory_link",
                json!({"src": note, "type": "relates_to", "dst": format!("{pre}{g}")}),
            )
            .await;
            assert_eq!(
                a.replace(&id, "ID"),
                b.replace(&g, "ID"),
                "{who}: MCP link {pre}"
            );
        }
    }
    // инсайт с тем же словом находится — поиск работает, отчёта в нём нет
    let r = search(
        &w.a,
        &w.o.tok,
        json!({"query": TITLE_WORD, "scope": {"solution": G}}),
    )
    .await;
    assert!(r.text().contains(&ins), "{}", r.text());
    // отчёт цел
    let (rev, _) = w.stored(&id).await;
    assert_eq!(rev, 1);
    let e = zam_store::get_entry(&w.db().await, id.parse().unwrap())
        .await
        .unwrap();
    assert!(e.deleted_at.is_none());
}

#[tokio::test]
async fn token_search_default_and_include_human_no_report() {
    let w = world("search").await;
    let id = secret_report(&w).await;
    for tok in [&w.o.tok, &w.rw.tok, &w.ro.tok] {
        for scope in [
            json!({"solution": G}),
            json!({"solution": G, "include_human": true}),
            json!({"all_solutions": true}),
        ] {
            for q in [TITLE_WORD, BODY_WORD] {
                for mode in ["brief", "chunks", "notes"] {
                    let r = search(
                        &w.a,
                        tok,
                        json!({"query": q, "scope": scope, "mode": mode, "include_history": true}),
                    )
                    .await;
                    ok(&r, StatusCode::OK, "поиск");
                    clean(&r.text(), &id, &format!("поиск {q} {scope} {mode}"));
                }
            }
        }
    }
}

#[tokio::test]
async fn token_search_kinds_report_400_like_unknown() {
    let w = world("search-kinds").await;
    let _id = secret_report(&w).await;
    let detail = |r: &Resp| r.json()["detail"].as_str().unwrap().to_string();
    for kinds in [json!(["report"]), json!(["note", "report"])] {
        let r = search(
            &w.a,
            &w.o.tok,
            json!({"query": TITLE_WORD, "filters": {"kinds": kinds}}),
        )
        .await;
        ok(&r, StatusCode::BAD_REQUEST, "report");
        let xyz: Value = serde_json::from_str(&kinds.to_string().replace("report", "xyz")).unwrap();
        let u = search(
            &w.a,
            &w.o.tok,
            json!({"query": TITLE_WORD, "filters": {"kinds": xyz}}),
        )
        .await;
        ok(&u, StatusCode::BAD_REQUEST, "xyz");
        assert_eq!(detail(&r).replace("report", "xyz"), detail(&u));
        assert!(!detail(&r).contains("допустимы note, insight, changelog, release, term, task, question, folder, attachment, report"));
    }
    // MCP memory_search — то же
    let a = mcp(
        &w.a,
        &w.o.tok,
        "memory_search",
        json!({"query": TITLE_WORD, "kinds": ["report"]}),
    )
    .await;
    let b = mcp(
        &w.a,
        &w.o.tok,
        "memory_search",
        json!({"query": TITLE_WORD, "kinds": ["xyz"]}),
    )
    .await;
    assert_eq!(a.replace("report", "xyz"), b, "MCP memory_search");
}

#[tokio::test]
async fn token_entries_list_hides_report_kind_report_400() {
    let w = world("list").await;
    let id = secret_report(&w).await;
    let cl = entry(
        &w,
        &w.o.tok,
        json!({"kind": "changelog", "title": "Сделано", "data": {"kind": "feat"}}),
    )
    .await;
    for tok in [&w.o.tok, &w.rw.tok, &w.ro.tok] {
        let r =
            w.a.tok(
                tok,
                Method::GET,
                &format!("/api/v1/spaces/{G}/entries"),
                None,
            )
            .await;
        ok(&r, StatusCode::OK, "лента");
        assert!(r.text().contains(&cl), "{}", r.text());
        clean(&r.text(), &id, "лента");
        let detail = |r: &Resp| r.json()["detail"].as_str().unwrap().to_string();
        for (k, extra) in [("report", ""), ("report", "&deleted=only")] {
            let r =
                w.a.tok(
                    tok,
                    Method::GET,
                    &format!("/api/v1/spaces/{G}/entries?kind={k}{extra}"),
                    None,
                )
                .await;
            let u =
                w.a.tok(
                    tok,
                    Method::GET,
                    &format!("/api/v1/spaces/{G}/entries?kind=xyz{extra}"),
                    None,
                )
                .await;
            ok(&r, StatusCode::BAD_REQUEST, "kind=report");
            assert_eq!(detail(&r).replace("report", "xyz"), detail(&u));
        }
    }
}

#[tokio::test]
async fn token_get_revisions_links_backlinks_404() {
    let w = world("get").await;
    let id = secret_report(&w).await;
    let ro = ro_token(&w).await;
    for tok in [&w.o.tok, &ro, &w.rw.tok] {
        for suffix in ["", "/revisions", "/links", "/backlinks"] {
            let r = oracle(
                &w.a,
                tok,
                &format!("GET /entries/{{id}}{suffix}"),
                &id,
                Method::GET,
                |x| format!("/api/v1/entries/{x}{suffix}"),
                |_| None,
                &[],
            )
            .await;
            ok(&r, StatusCode::NOT_FOUND, suffix);
        }
    }
    // человеку — видно
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/entries/{id}"),
        None,
    )
    .await;
    ok(&r, StatusCode::OK, "владелец WebUI");
    let r = web(
        &w.a,
        &w.ro.web,
        Method::GET,
        &format!("/api/v1/entries/{id}/revisions"),
        None,
    )
    .await;
    ok(&r, StatusCode::OK, "участник ro WebUI");
}

#[tokio::test]
async fn token_read_p_and_e_not_found() {
    let w = world("read").await;
    let id = secret_report(&w).await;
    for tok in [&w.o.tok, &w.rw.tok] {
        for pre in ["p:", "e:", ""] {
            let g = common::ghost_like(&id);
            let body = |x: &str| json!({"items": [{"id": format!("{pre}{x}")}]});
            let r =
                w.a.tok(tok, Method::POST, "/api/v1/read", Some(body(&id)))
                    .await;
            let n =
                w.a.tok(tok, Method::POST, "/api/v1/read", Some(body(&g)))
                    .await;
            ok(&r, StatusCode::OK, "read");
            assert_eq!(shape(&r, &id), shape(&n, &g), "/read {pre}");
            assert_eq!(r.json()["items"][0]["error"], "not_found", "{}", r.text());
            clean(&r.text().replace(&id, "ID"), &id, "read");
            let a = mcp(
                &w.a,
                tok,
                "memory_read",
                json!({"items": [{"id": format!("{pre}{id}")}]}),
            )
            .await;
            let b = mcp(
                &w.a,
                tok,
                "memory_read",
                json!({"items": [{"id": format!("{pre}{g}")}]}),
            )
            .await;
            assert_eq!(a.replace(&id, "ID"), b.replace(&g, "ID"), "MCP {pre}");
            assert!(a.contains("not_found"), "{a}");
        }
    }
    // человеку `/read p:` отдаёт текст отчёта
    let r = web(
        &w.a,
        &w.o.web,
        Method::POST,
        "/api/v1/read",
        Some(json!({"items": [{"id": format!("p:{id}")}]})),
    )
    .await;
    ok(&r, StatusCode::OK, "read человеком");
    assert!(r.text().contains(BODY_WORD), "{}", r.text());
}

#[tokio::test]
async fn token_attachment_url_404() {
    let w = world("file").await;
    let id = secret_report(&w).await;
    let loss = w.file(&id, "loss.png").await;
    for tok in [&w.o.tok, &w.rw.tok] {
        let r = oracle(
            &w.a,
            tok,
            "файл",
            &id,
            Method::GET,
            |x| file_uri(x, &loss),
            |_| None,
            &[],
        )
        .await;
        ok(&r, StatusCode::NOT_FOUND, "файл по токену");
        // и по /blobs (блоб файла отчёта — не вложение заметки)
        let r =
            w.a.tok(
                tok,
                Method::GET,
                &format!("/api/v1/blobs/{}/loss.png", loss.sha256),
                None,
            )
            .await;
        ok(&r, StatusCode::NOT_FOUND, "/blobs");
    }
}

#[tokio::test]
async fn token_recall_events_never_mention_report() {
    let w = world("recall").await;
    let id = secret_report(&w).await;
    let title = format!("Отчёт {TITLE_WORD} обучения");
    let events = [
        json!({"event": "session_start", "solution": G}),
        json!({"event": "session_start", "solution": G, "source": "compact"}),
        json!({"event": "prompt", "solution": G, "prompt": format!("{TITLE_WORD} {BODY_WORD}")}),
        json!({"event": "prompt", "solution": G, "prompt": title}),
        json!({"event": "post_tool", "solution": G, "tool": "Bash",
               "error": format!("Ошибка {BODY_WORD} failed at step 7")}),
        json!({"event": "brief", "solution": G, "task": format!("Продолжить: {title}")}),
        json!({"event": "recovery", "solution": G}),
    ];
    for tok in [&w.o.tok, &w.rw.tok] {
        for ev in &events {
            let r =
                w.a.tok_h(
                    tok,
                    Method::POST,
                    "/api/v1/recall",
                    Some(ev.clone()),
                    &[("x-zam-session", "sess-report-1")],
                )
                .await;
            ok(&r, StatusCode::OK, &format!("recall {ev}"));
            clean(&r.text(), &id, &format!("recall {ev}"));
        }
        let o = mcp(
            &w.a,
            tok,
            "memory_recall",
            json!({"event": "brief", "task": title, "solution": G}),
        )
        .await;
        clean(&o, &id, "MCP memory_recall brief");
    }
}

#[tokio::test]
async fn token_cache_put_report_source_rejected() {
    let w = world("cache").await;
    let id = secret_report(&w).await;
    let body = |src: &str| {
        json!({"query": "итог обучения", "summary": "Точность выросла.",
               "solution": G, "sources": [{"id": src}]})
    };
    for pre in ["p:", "e:", ""] {
        // владелец (та же база) и участник rw (база владельца решения — его кэш решения)
        for tok in [&w.o.tok, &w.rw.tok] {
            let r = oracle(
                &w.a,
                tok,
                &format!("cache_put {pre}"),
                &id,
                Method::POST,
                |_| "/api/v1/cache".into(),
                |x| Some(body(&format!("{pre}{x}"))),
                &[],
            )
            .await;
            ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "cache_put");
            assert_eq!(problem(&r), "/problems/cache-source-not-found");
        }
        // участник rw — кэш своего Knowledge, источник в базе владельца
        let r = oracle(
            &w.a,
            &w.rw.tok,
            &format!("cache_put Knowledge {pre}"),
            &id,
            Method::POST,
            |_| "/api/v1/cache".into(),
            |x| {
                Some(
                    json!({"query": "итог обучения", "summary": "Точность выросла.",
                            "sources": [{"id": format!("{pre}{x}")}]}),
                )
            },
            &[],
        )
        .await;
        ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "cache_put Knowledge");
    }
    // MCP memory_cache_put
    let g = common::ghost_like(&id);
    let args = |x: &str| {
        json!({"query": "итог обучения", "summary": "Точность выросла.",
               "sources": [{"id": format!("p:{x}")}], "solution": G})
    };
    let a = mcp(&w.a, &w.o.tok, "memory_cache_put", args(&id)).await;
    let b = mcp(&w.a, &w.o.tok, "memory_cache_put", args(&g)).await;
    assert_eq!(a.replace(&id, "ID"), b.replace(&g, "ID"), "MCP cache_put");
    // человеку источник-отчёт тоже нельзя: выжимки читает агент
    let r = web(
        &w.a,
        &w.o.web,
        Method::POST,
        "/api/v1/cache",
        Some(body(&format!("p:{id}"))),
    )
    .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "cache_put человеком");
    assert_eq!(problem(&r), "/problems/cache-forbidden-source");
}

#[tokio::test]
async fn token_graph_and_link_to_report_404() {
    let w = world("graph").await;
    let id = secret_report(&w).await;
    let own = format!("n:{}", w.note("notes/own").await);
    // участник rw: заметка в своём Knowledge (другая база) → отчёт в базе владельца
    let r =
        w.a.tok(
            &w.rw.tok,
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "knowledge", "path": "мои/заметка", "kind": "note",
                        "content": "текст"}),
            ),
        )
        .await;
    ok(&r, StatusCode::CREATED, "заметка участника");
    let foreign = format!("n:{}", r.json()["id"].as_str().unwrap());
    for (who, tok, src) in [
        ("владелец", &w.o.tok, &own),
        ("участник: другая база", &w.rw.tok, &foreign),
        ("участник: решение", &w.rw.tok, &own),
    ] {
        for pre in ["e:", "entry:"] {
            let r = oracle(
                &w.a,
                tok,
                &format!("{who}: POST /edges {pre}"),
                &id,
                Method::POST,
                |_| "/api/v1/edges".into(),
                |x| Some(json!({"src": src, "type": "relates_to", "dst": format!("{pre}{x}")})),
                &[],
            )
            .await;
            ok(
                &r,
                StatusCode::NOT_FOUND,
                &format!("{who}: ребро к отчёту {pre}"),
            );
        }
        // links insight (в своём Knowledge у участника — другая база)
        let space = if src == &foreign { "knowledge" } else { G };
        let r = oracle(
            &w.a,
            tok,
            &format!("{who}: links"),
            &id,
            Method::POST,
            |_| format!("/api/v1/spaces/{space}/entries"),
            |x| {
                Some(json!({"kind": "insight", "title": "Связанный", "body": "т",
                            "data": {"kind": "gotcha"},
                            "links": [{"type": "relates_to", "target": format!("entry:{x}")}]}))
            },
            &[],
        )
        .await;
        ok(&r, StatusCode::NOT_FOUND, &format!("{who}: links"));
        // supersedes_id
        let r = oracle(
            &w.a,
            tok,
            &format!("{who}: supersedes_id"),
            &id,
            Method::POST,
            |_| format!("/api/v1/spaces/{space}/entries"),
            |x| {
                Some(json!({"kind": "insight", "title": "Новый", "body": "т",
                            "data": {"kind": "gotcha"}, "supersedes_id": x}))
            },
            &[],
        )
        .await;
        ok(&r, StatusCode::NOT_FOUND, &format!("{who}: supersedes_id"));
        let r = oracle(
            &w.a,
            tok,
            &format!("{who}: GET /graph"),
            &id,
            Method::GET,
            |x| format!("/api/v1/graph?entry={x}&depth=2"),
            |_| None,
            &[],
        )
        .await;
        ok(&r, StatusCode::NOT_FOUND, "graph");
    }
    // граф заметки без следов отчёта
    let r =
        w.a.tok(
            &w.o.tok,
            Method::GET,
            &format!("/api/v1/graph?node={}&depth=3", &own[2..]),
            None,
        )
        .await;
    ok(&r, StatusCode::OK, "граф заметки");
    clean(&r.text(), &id, "граф заметки");
    // человеку supersedes_id на отчёт — 409 report-immutable
    let r = web(
        &w.a,
        &w.o.web,
        Method::POST,
        &format!("/api/v1/spaces/{G}/entries"),
        Some(json!({"kind": "insight", "title": "Новый", "body": "т",
                    "data": {"kind": "gotcha"}, "supersedes_id": id})),
    )
    .await;
    ok(&r, StatusCode::CONFLICT, "supersedes_id человеком");
    assert_eq!(problem(&r), IMMUTABLE);
}

#[tokio::test]
async fn token_feedback_report_404() {
    let w = world("feedback").await;
    let id = secret_report(&w).await;
    let ro = ro_token(&w).await;
    for tok in [&w.o.tok, &ro, &w.rw.tok] {
        for v in ["useful", "useless"] {
            let r = oracle(
                &w.a,
                tok,
                "feedback",
                &id,
                Method::POST,
                |x| format!("/api/v1/entries/{x}/feedback"),
                |_| Some(json!({"value": v})),
                &[("x-zam-session", "sess-agent")],
            )
            .await;
            ok(&r, StatusCode::NOT_FOUND, "feedback");
        }
    }
    let usage: i64 = w
        .db()
        .await
        .read(|c| Ok(c.query_row("SELECT count(*) FROM usage", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(usage, 0);
}

#[tokio::test]
async fn report_not_in_qdrant_no_jobs() {
    let w = world("index").await;
    let status = async || {
        let r =
            w.a.tok(&w.o.tok, Method::GET, "/api/v1/index/status", None)
                .await;
        ok(&r, StatusCode::OK, "index/status");
        r
    };
    let before = status().await.json()["queue"].clone();
    let id = secret_report(&w).await;
    let rid = id.clone();
    let (jobs, state): (i64, String) = w
        .db()
        .await
        .read(move |c| {
            let jobs = c.query_row(
                "SELECT count(*) FROM index_jobs WHERE target_id = ?1",
                [&rid],
                |r| r.get(0),
            )?;
            let state = c.query_row(
                "SELECT index_state FROM entries WHERE id = ?1",
                [&rid],
                |r| r.get(0),
            )?;
            Ok((jobs, state))
        })
        .await
        .unwrap();
    assert_eq!((jobs, state.as_str()), (0, "excluded"));
    let r = status().await;
    clean(&r.text(), &id, "index/status");
    assert_eq!(
        r.json()["queue"],
        before,
        "очередь не выросла: {}",
        r.text()
    );
}

#[tokio::test]
async fn human_search_default_includes_report() {
    let w = world("human-search").await;
    let id = secret_report(&w).await;
    let r = web_search(
        &w.a,
        &w.o.web,
        json!({"query": TITLE_WORD, "scope": {"solution": G}}),
    )
    .await;
    ok(&r, StatusCode::OK, "Ctrl+K");
    let j = r.json();
    let card = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|it| it["kind"] == "report")
        .unwrap_or_else(|| panic!("нет карточки отчёта: {j}"));
    assert_eq!(card["id"], format!("p:{id}"), "{card}");
    assert_eq!(card["entry_id"], id, "{card}");
    assert!(
        card["title"].as_str().unwrap().contains(TITLE_WORD),
        "{card}"
    );
    assert!(card["snippet"].as_str().is_some(), "{card}");
    assert!(card["created_at"].as_str().is_some(), "{card}");
    assert!(card["space"].is_object(), "{card}");
    // по телу — тоже; явный kinds ∋ report — только отчёты
    let r = web_search(
        &w.a,
        &w.o.web,
        json!({"query": BODY_WORD, "scope": {"solution": G}, "filters": {"kinds": ["report"]}}),
    )
    .await;
    ok(&r, StatusCode::OK, "kinds report");
    let items = r.json()["items"].as_array().unwrap().clone();
    assert!(!items.is_empty(), "{}", r.text());
    assert!(items.iter().all(|it| it["kind"] == "report"), "{items:?}");
    // md-карточка — ссылка p:
    let r = web_search(
        &w.a,
        &w.o.web,
        json!({"query": TITLE_WORD, "scope": {"solution": G}, "format": "md"}),
    )
    .await;
    ok(&r, StatusCode::OK, "md");
    assert!(r.text().contains(&format!("p:{id} report")), "{}", r.text());
    // администратор от имени владельца — тоже
    let r =
        w.a.adm(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": TITLE_WORD, "scope": {"solution": G}})),
            Some(&w.o.id),
        )
        .await;
    ok(&r, StatusCode::OK, "администратор");
    assert!(r.text().contains(&format!("p:{id}")), "{}", r.text());
}

#[tokio::test]
async fn human_member_search_sees_solution_reports_only() {
    let w = world("member-search").await;
    let shared = secret_report(&w).await;
    // второе решение владельца — участникам не открыто
    let r =
        w.a.tok(
            &w.o.tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G2, "name": "Закрытое"})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "второе решение");
    let r =
        w.a.tok(
            &w.o.tok,
            Method::POST,
            &format!("/api/v1/spaces/{G2}/entries"),
            Some(
                json!({"kind": "report", "title": format!("Закрытый {TITLE_WORD}"),
                        "body": format!("Тайна {BODY_WORD}")}),
            ),
        )
        .await;
    ok(&r, StatusCode::CREATED, "закрытый отчёт");
    let closed = r.json()["id"].as_str().unwrap().to_string();
    for (who, u) in [("ro", &w.ro.web), ("rw", &w.rw.web)] {
        for scope in [json!({"solution": G}), json!({"all_solutions": true})] {
            let r = web_search(&w.a, u, json!({"query": TITLE_WORD, "scope": scope})).await;
            ok(&r, StatusCode::OK, who);
            let t = r.text();
            assert!(t.contains(&format!("p:{shared}")), "{who} {scope}: {t}");
            assert!(!t.contains(&closed), "{who} {scope}: {t}");
        }
        // закрытое решение — не видно
        let r = web_search(
            &w.a,
            u,
            json!({"query": TITLE_WORD, "scope": {"solution": G2}}),
        )
        .await;
        assert!(!r.text().contains(&closed), "{who}: {}", r.text());
    }
    // владелец видит оба
    let r = web_search(
        &w.a,
        &w.o.web,
        json!({"query": TITLE_WORD, "scope": {"all_solutions": true}}),
    )
    .await;
    let t = r.text();
    assert!(t.contains(&shared) && t.contains(&closed), "{t}");
}

#[tokio::test]
async fn token_default_kinds_scope_hash_unchanged() {
    let w = world("scope-hash").await;
    let src = insight(&w, "Источник выжимки").await;
    let r =
        w.a.tok(
            &w.o.tok,
            Method::POST,
            "/api/v1/cache",
            Some(json!({"query": "итог", "summary": "Кратко.", "solution": G,
                        "sources": [{"id": format!("i:{src}")}]})),
        )
        .await;
    ok(&r, StatusCode::CREATED, "выжимка");
    let cid = r.json()["id"].as_str().unwrap().to_string();
    // ключ M5: виды по умолчанию без report (CHANGELOG не нужен)
    let want = zam_store::scope_hash(&json!({
        "solution": G,
        "include_knowledge": true,
        "include_rules": true,
        "path_prefix": null,
        "area": null,
        "filters": {
            "kinds": ["attachment", "changelog", "insight", "note", "question", "release", "term"],
            "status": ["active", "answered", "revisit"],
            "certainty": [],
        },
    }));
    let raw = cid.strip_prefix("c_").unwrap_or(&cid).to_string();
    let got: String = w
        .db()
        .await
        .read(move |c| {
            Ok(c.query_row(
                "SELECT scope_hash FROM answer_cache WHERE id = ?1",
                [&raw],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(got, want);
    // поиск агента той же областью находит выжимку (ключ не разошёлся)
    let r = search(
        &w.a,
        &w.o.tok,
        json!({"query": "итог", "scope": {"solution": G}}),
    )
    .await;
    ok(&r, StatusCode::OK, "поиск");
    assert!(r.text().contains(&cid), "{}", r.text());
}

/// MCP-оракулы (ревью задачи 4): `memory_update_entry` — все действия, в том числе `feedback`,
/// и `memory_link` с отчётом-источником (граф) отвечают на `p:|e:|<id>` отчёта так же, как на
/// случайный id; токены владельца `rw`/`ro` и участников `rw`/`ro`. Отчёт цел.
#[tokio::test]
async fn token_mcp_update_link_feedback_oracle() {
    let w = world("mcp-oracle").await;
    let id = secret_report(&w).await;
    let note = format!("n:{}", w.note("notes/mcp-oracle").await);
    let ro = ro_token(&w).await;
    let actions = [
        json!({"action": "supersede", "rev": 1, "title": "Новый", "body": "т",
               "data": {"kind": "gotcha"}}),
        json!({"action": "retract", "rev": 1, "reason": "неверно"}),
        json!({"action": "revisit", "rev": 1}),
        json!({"action": "certify", "rev": 1, "certainty": "hypothesis", "reason": "проверка"}),
        json!({"action": "append", "rev": 1, "body": "ещё"}),
        json!({"action": "feedback", "value": "useful"}),
        json!({"action": "status", "rev": 1, "status": "done"}),
        json!({"action": "claim"}),
        json!({"action": "release"}),
        json!({"action": "answer", "text": "да", "by": "human"}),
    ];
    for (who, tok) in [
        ("rw", w.o.tok.as_str()),
        ("ro", ro.as_str()),
        ("участник", w.rw.tok.as_str()),
        ("участник-ro", w.ro.tok.as_str()),
    ] {
        for pre in ["p:", "e:", ""] {
            for args in &actions {
                let g = common::ghost_like(&id);
                let with = |x: &str| {
                    let mut a = args.clone();
                    a["id"] = json!(format!("{pre}{x}"));
                    a
                };
                let a = mcp(&w.a, tok, "memory_update_entry", with(&id)).await;
                let b = mcp(&w.a, tok, "memory_update_entry", with(&g)).await;
                let what = format!("{who}: memory_update_entry {} {pre}", args["action"]);
                assert_eq!(a.replace(&id, "ID"), b.replace(&g, "ID"), "{what}");
                clean(&a.replace(&id, "ID"), &id, &what);
            }
        }
        // граф: связь от отчёта (к отчёту — в матрице)
        for pre in ["p:", "e:"] {
            let g = common::ghost_like(&id);
            let link =
                |x: &str| json!({"src": format!("{pre}{x}"), "type": "relates_to", "dst": note});
            let a = mcp(&w.a, tok, "memory_link", link(&id)).await;
            let b = mcp(&w.a, tok, "memory_link", link(&g)).await;
            assert_eq!(
                a.replace(&id, "ID"),
                b.replace(&g, "ID"),
                "{who}: memory_link src {pre}"
            );
        }
    }
    let (rev, _) = w.stored(&id).await;
    assert_eq!(rev, 1);
}

/// `POST /cache` токена с `filters.kinds ∋ report` — 400 тем же текстом, что неизвестный вид
/// (ревью задачи 4); сессии WebUI вид `report` в фильтре допустим.
#[tokio::test]
async fn token_cache_put_filters_kinds_report_400() {
    let w = world("cache-kinds").await;
    let src = insight(&w, "Источник выжимки").await;
    let body = |kinds: Value| {
        json!({"query": "итог", "summary": "Кратко.", "solution": G,
               "sources": [{"id": format!("i:{src}")}], "filters": {"kinds": kinds}})
    };
    let detail = |r: &Resp| r.json()["detail"].as_str().unwrap_or_default().to_string();
    for tok in [&w.o.tok, &w.rw.tok] {
        for kinds in [json!(["report"]), json!(["insight", "report"])] {
            let r =
                w.a.tok(
                    tok,
                    Method::POST,
                    "/api/v1/cache",
                    Some(body(kinds.clone())),
                )
                .await;
            ok(&r, StatusCode::BAD_REQUEST, &format!("cache kinds {kinds}"));
            let xyz: Value =
                serde_json::from_str(&kinds.to_string().replace("report", "xyz")).unwrap();
            let u =
                w.a.tok(tok, Method::POST, "/api/v1/cache", Some(body(xyz)))
                    .await;
            ok(&u, StatusCode::BAD_REQUEST, "cache kinds xyz");
            assert_eq!(detail(&r).replace("report", "xyz"), detail(&u));
        }
    }
    let r = web(
        &w.a,
        &w.o.web,
        Method::POST,
        "/api/v1/cache",
        Some(body(json!(["insight", "report"]))),
    )
    .await;
    ok(&r, StatusCode::CREATED, "WebUI: report в фильтре выжимки");
}
