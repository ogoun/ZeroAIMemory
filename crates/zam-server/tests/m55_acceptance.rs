//! Приёмка M5.5 «Отчёты» (спецификация §12, задача 8): неизменность и повтор, невидимость для
//! агента при поиске WebUI, пространства и права, файлы без ссылки и пределы. Сценарии 1–2
//! (`zam report` и тот же отчёт через MCP) — `crates/zam-cli/tests/m55_acceptance.rs`, страница и
//! PDF в WebUI — e2e `acceptance_pdf_from_cli_report`.
//!
//! Мир: владелец решения A, участники `rw` (B) и `ro` (C) — у каждого токен и сессия WebUI.
//! Серверы без моделей (FTS).
mod common;

use axum::http::{Method, StatusCode};
use common::reports::*;
use serde_json::json;

/// Слово тела, которого нет в прочих записях.
const WORD: &str = "квазиприёмка";

async fn world(name: &str) -> W {
    common::reports::world(&format!("m55-{name}")).await
}

fn items(r: &common::api::Resp) -> Vec<serde_json::Value> {
    r.json()["items"].as_array().cloned().unwrap_or_default()
}

/// §12 п. 3: правка отчёта — 409 `report-immutable`, содержимое прежнее; повтор того же
/// содержимого — новый отчёт, оба в ленте.
#[tokio::test]
async fn acceptance_patch_409_repeat_new_report() {
    let w = world("patch").await;
    let id = w.report("Итог обучения").await;
    let before = w.stored(&id).await;
    let uri = format!("/api/v1/entries/{id}");
    let r = web_h(
        &w.a,
        &w.o.web,
        Method::PATCH,
        &uri,
        Some(json!({"body": "исправлено"})),
        &[("if-match", "1")],
    )
    .await;
    ok(&r, StatusCode::CONFLICT, "PATCH владельцем");
    assert_eq!(problem(&r), IMMUTABLE);
    assert!(r.text().contains("напишите новый"), "{}", r.text());
    // агенту отчёта нет — 404, как у несуществующей записи
    let r =
        w.a.tok_h(
            &w.o.tok,
            Method::PATCH,
            &uri,
            Some(json!({"body": "исправлено"})),
            &[("if-match", "1")],
        )
        .await;
    ok(&r, StatusCode::NOT_FOUND, "PATCH токеном");
    assert_eq!(w.stored(&id).await, before, "содержимое не изменилось");

    // повтор без ключа — новый отчёт
    let again = w.report("Итог обучения").await;
    assert_ne!(again, id);
    let r = web(
        &w.a,
        &w.o.web,
        Method::GET,
        &format!("/api/v1/spaces/{G}/entries?kind=report"),
        None,
    )
    .await;
    ok(&r, StatusCode::OK, "лента");
    let ids: Vec<String> = items(&r)
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec![again, id]);
}

/// §12 п. 4: агент отчёт не видит ни одним путём (поиск, чтение, лента, `/read`), поиск WebUI
/// его находит.
#[tokio::test]
async fn acceptance_token_sees_nothing_webui_search_finds() {
    let w = world("invisible").await;
    let p = png(4, 90);
    let id = w
        .report_with(
            "Сравнение оптимизаторов",
            &format!("# Итог\n\nВывод {WORD}: Adam лучше.\n\n![loss](loss.png)\n"),
            &[("loss.png", &p)],
        )
        .await;
    for tok in [&w.o.tok, &w.rw.tok, &w.ro.tok] {
        let r =
            w.a.tok(
                tok,
                Method::POST,
                "/api/v1/search",
                Some(json!({"query": WORD, "scope": {"solution": G}})),
            )
            .await;
        ok(&r, StatusCode::OK, "поиск токеном");
        assert!(!r.text().contains(&id), "{}", r.text());
        assert!(!r.text().contains(WORD), "{}", r.text());
        let r =
            w.a.tok(
                tok,
                Method::POST,
                "/api/v1/search",
                Some(json!({"query": WORD, "scope": {"solution": G},
                            "filters": {"kinds": ["report"]}})),
            )
            .await;
        ok(&r, StatusCode::BAD_REQUEST, "kinds=report токеном");
        let r =
            w.a.tok(tok, Method::GET, &format!("/api/v1/entries/{id}"), None)
                .await;
        ok(&r, StatusCode::NOT_FOUND, "чтение токеном");
        let r =
            w.a.tok(
                tok,
                Method::GET,
                &format!("/api/v1/spaces/{G}/entries?kind=report"),
                None,
            )
            .await;
        ok(&r, StatusCode::BAD_REQUEST, "лента kind=report токеном");
        let r =
            w.a.tok(
                tok,
                Method::GET,
                &format!("/api/v1/spaces/{G}/entries"),
                None,
            )
            .await;
        ok(&r, StatusCode::OK, "лента токеном");
        assert!(!r.text().contains(&id), "{}", r.text());
        let r =
            w.a.tok(
                tok,
                Method::POST,
                "/api/v1/read",
                Some(json!({"items": [{"id": format!("p:{id}")}]})),
            )
            .await;
        ok(&r, StatusCode::OK, "/read токеном");
        assert!(r.text().contains("not_found"), "{}", r.text());
        assert!(!r.text().contains(WORD), "{}", r.text());
    }
    // WebUI: поиск находит — и владелец, и участники
    for u in [&w.o.web, &w.rw.web, &w.ro.web] {
        let r = web(
            &w.a,
            u,
            Method::POST,
            "/api/v1/search",
            Some(
                json!({"query": WORD, "scope": {"solution": G}, "filters": {"kinds": ["report"]}}),
            ),
        )
        .await;
        ok(&r, StatusCode::OK, "поиск WebUI");
        let hits = items(&r);
        assert!(
            hits.iter()
                .any(|h| h["kind"] == "report" && h["entry_id"] == id.as_str()),
            "{}",
            r.text()
        );
    }
}

/// §12 п. 5: Knowledge — 422; участник `ro` читает, но не пишет; агент участника `rw` пишет, а
/// удалить отчёт может только владелец в своей сессии WebUI; токен — 404 (решение задачи 4).
#[tokio::test]
async fn acceptance_spaces_and_rights() {
    let w = world("rights").await;
    let p = png(6, 80);
    let r = w.report_as(&w.o.tok, "knowledge", "Т", "текст", &[]).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "Knowledge");
    // агент участника rw пишет в решение владельца
    let r = w
        .report_as(
            &w.rw.tok,
            G,
            "От участника",
            "![a](a.png)",
            &[("a.png", &p)],
        )
        .await;
    ok(&r, StatusCode::CREATED, "агент участника rw");
    let id = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(w.files_of(&id).await.len(), 1, "файл — в базе владельца");
    // участник ro пишет — 403
    let r = w.report_as(&w.ro.tok, G, "От ro", "текст", &[]).await;
    ok(&r, StatusCode::FORBIDDEN, "агент участника ro");
    // читают все сессии решения
    for (who, u) in [("A", &w.o.web), ("B", &w.rw.web), ("C", &w.ro.web)] {
        let r = web(
            &w.a,
            u,
            Method::GET,
            &format!("/api/v1/entries/{id}?format=html"),
            None,
        )
        .await;
        ok(&r, StatusCode::OK, &format!("страница {who}"));
        let url = format!("/{}", r.json()["files"][0]["url"].as_str().unwrap());
        let r = web(&w.a, u, Method::GET, &url, None).await;
        ok(&r, StatusCode::OK, &format!("файл {who}"));
    }
    let uri = format!("/api/v1/entries/{id}");
    let restore = format!("{uri}/restore");
    let ifm = [("if-match", "1")];
    // участник rw — 403, токены — 404
    let r = web_h(&w.a, &w.rw.web, Method::DELETE, &uri, None, &ifm).await;
    ok(&r, StatusCode::FORBIDDEN, "удаление участником rw");
    assert!(r.text().contains("только владелец решения"), "{}", r.text());
    for tok in [&w.o.tok, &w.rw.tok] {
        let r = w.a.tok_h(tok, Method::DELETE, &uri, None, &ifm).await;
        ok(&r, StatusCode::NOT_FOUND, "удаление токеном");
    }
    // владелец удаляет и восстанавливает
    let r = web_h(&w.a, &w.o.web, Method::DELETE, &uri, None, &ifm).await;
    ok(&r, StatusCode::NO_CONTENT, "удаление владельцем");
    for tok in [&w.o.tok, &w.rw.tok] {
        let r = w.a.tok(tok, Method::POST, &restore, None).await;
        ok(&r, StatusCode::NOT_FOUND, "восстановление токеном");
    }
    let r = web(&w.a, &w.rw.web, Method::POST, &restore, None).await;
    ok(&r, StatusCode::FORBIDDEN, "восстановление участником rw");
    let r = web(&w.a, &w.o.web, Method::POST, &restore, None).await;
    ok(&r, StatusCode::OK, "восстановление владельцем");
    assert!(r.json()["deleted_at"].is_null());
}

/// §12 п. 6: файл без ссылки в теле — 422; число файлов — 422, файл и сумма сверх пределов — 413;
/// ничего не создаётся.
#[tokio::test]
async fn acceptance_unreferenced_image_and_limits() {
    let w = common::reports::world_cfg("m55-limits", |c| {
        c.reports.max_files = 2;
        c.reports.max_total_bytes = 1024;
        c.reports.mcp_max_bytes = 1024;
        c.limits.blob_max_bytes = 700;
    })
    .await;
    let tok = w.o.tok.clone();
    let (a, b) = (png(1, 100), png(2, 100));
    let r = w
        .report_as(&tok, G, "Т", "![a](a.png)", &[("a.png", &a), ("b.png", &b)])
        .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "файл без ссылки");
    let three = "![a](a.png) ![b](b.png) ![c](c.png)";
    let c = png(3, 100);
    let r = w
        .report_as(
            &tok,
            G,
            "Т",
            three,
            &[("a.png", &a), ("b.png", &b), ("c.png", &c)],
        )
        .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY, "число файлов");
    let big = png(4, 800);
    let r = w
        .report_as(&tok, G, "Т", "![a](a.png)", &[("a.png", &big)])
        .await;
    ok(&r, StatusCode::PAYLOAD_TOO_LARGE, "файл");
    assert!(r.text().contains("blob_max_bytes"), "{}", r.text());
    let (x, y) = (png(5, 600), png(6, 600));
    let r = w
        .report_as(
            &tok,
            G,
            "Т",
            "![x](x.png) ![y](y.png)",
            &[("x.png", &x), ("y.png", &y)],
        )
        .await;
    ok(&r, StatusCode::PAYLOAD_TOO_LARGE, "сумма");
    assert!(r.text().contains("max_total_bytes"), "{}", r.text());
    let n: i64 = w
        .db()
        .await
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM entries WHERE kind = 'report'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(n, 0, "ни одного отчёта");
    // в пределах — создаётся
    let r = w
        .report_as(
            &tok,
            G,
            "Т",
            "![a](a.png) ![b](b.png)",
            &[("a.png", &a), ("b.png", &b)],
        )
        .await;
    ok(&r, StatusCode::CREATED, "в пределах");
}
