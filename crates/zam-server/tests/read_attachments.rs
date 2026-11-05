//! `/read` вложений по `locator` (M5, задача 7): `a:<chunk>` (документ и `locator` чанка),
//! `b:<sha256>` (первое по дате вложение блоба в доступных пространствах), `d:<id>`; оглавление
//! без `locator`; 422 `locator-invalid`; `session_seen` вида `blob` с частью — `locator`; права
//! (ответ на недоступное — как на несуществующее). Сервер без моделей (FTS); PDF — с pdfium из
//! `target/deps` (без него PDF в тесте пропускается).

mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use common::attach::{
    chunk_locators, db, docx, filler, pptx, read1, text, upload, upload_adm, wait_done, xlsx,
};
use serde_json::{Value, json};
use zam_core::Id;
use zam_docs::test_pdf::{Page, build, pdfium_path};

const READ: &str = "/api/v1/read";
const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e91";

async fn world(name: &str) -> (Api, bool) {
    let pdfium = pdfium_path(env!("CARGO_MANIFEST_DIR"));
    let pdf = pdfium.is_some();
    let a = api(name, |c| c.docs.pdfium_dylib = pdfium).await;
    (a, pdf)
}

async fn note(a: &Api, token: &str, space: &str, path: &str) -> String {
    let r = a
        .tok(
            token,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": space, "path": path, "kind": "note",
                        "content": "# Заметка\n\nТекст заметки.\n"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

async fn read_raw(a: &Api, token: &str, body: Value) -> Resp {
    a.tok(token, Method::POST, READ, Some(body)).await
}

async fn read(a: &Api, token: &str, body: Value) -> Value {
    let r = read_raw(a, token, body).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Чанк документа `att`, текст которого содержит `word`: `(chunk_id, locator)`.
async fn chunk_with(a: &Api, user: Id, att: Id, word: &str) -> (String, String) {
    let word = word.to_string();
    db(a, user)
        .await
        .read(move |c| {
            Ok(c.query_row(
                "SELECT id, locator FROM chunks WHERE attachment_id = ?1 AND instr(text, ?2) > 0
                 ORDER BY ordinal LIMIT 1",
                [att.to_string(), word],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap()
}

/// Части `session_seen` сессии `session` вида `blob` для блоба `sha` (по порядку).
async fn seen_parts(a: &Api, user: Id, session: &str, sha: &str) -> Vec<(String, i64)> {
    let (session, sha) = (session.to_string(), sha.to_string());
    db(a, user)
        .await
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT part, rev FROM session_seen
                 WHERE session_id = ?1 AND target_kind = 'blob' AND target_id = ?2 ORDER BY part",
            )?;
            let rows = st.query_map([session, sha], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap()
}

/// Текст по `locator` для PDF (страница), XLSX (лист и пересечение диапазона с блоками), PPTX
/// (слайд) и DOCX (абзац; номер внутри блока — блок, где он лежит); `a:<chunk>` без `locator` —
/// фрагмент по `locator` чанка; `locator`, которого в документе нет, — `not_found`. Секрет в имени
/// листа в ответе замаскирован, лист находится по сырому имени.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_attachment_by_locator_page_sheet_slide_para() {
    let (a, pdf) = world("read-att-locator").await;
    let user = a.st.admin_id;
    let tok = a.token.clone();
    let node = note(&a, &tok, "knowledge", "docs/отчёт").await;
    let mut paras: Vec<String> = (0..6).map(|_| filler(5)).collect();
    paras.push("Zirconium flamingo reactor overview.".into());
    let paras: Vec<&str> = paras.iter().map(String::as_str).collect();
    let book = xlsx(&[
        (
            "Итоги",
            &[
                &["Name", "Note"],
                &["Alpha", "tungsten pelican harbour"],
                &["Beta", "ordinary row"],
            ],
        ),
        ("Склад", &[&["Item", "Qty"], &["hydrant", "12"]]),
        (
            "AKIAIOSFODNN7EXAMPLE",
            &[&["Key", "Val"], &["vanadium", "1"]],
        ),
    ]);
    let deck = pptx(&[
        ("Intro", "nothing special"),
        ("Second", "cobalt walrus migration"),
    ]);
    upload(&a, &tok, &node, "план.docx", &docx(&paras)).await;
    upload(&a, &tok, &node, "итоги.xlsx", &book).await;
    upload(&a, &tok, &node, "доклад.pptx", &deck).await;
    if pdf {
        let book = build(&[
            Page::default().line(72.0, 700.0, "first page body"),
            Page::default().line(72.0, 700.0, "Nickel albatross telemetry"),
        ]);
        upload(&a, &tok, &node, "книга.pdf", &book).await;
    } else {
        eprintln!("pdfium не найден (cargo xtask fetch-deps) — PDF в тесте пропущен");
    }
    let dd = wait_done(&a, user, &node, "план.docx").await;
    let dx = wait_done(&a, user, &node, "итоги.xlsx").await;
    let dp = wait_done(&a, user, &node, "доклад.pptx").await;

    // a:<chunk> без locator — фрагмент по locator чанка
    let (chunk, loc) = chunk_with(&a, user, dd.id, "Zirconium").await;
    let it = read1(&a, &tok, json!({"id": format!("a:{chunk}")})).await;
    assert_eq!(it["kind"], "attachment", "{it}");
    assert_eq!(it["ref"], format!("a:{chunk}"));
    assert_eq!(it["file"], "план.docx");
    assert_eq!(it["attachment_id"], dd.id.to_string());
    assert_eq!(it["blob_sha256"], dd.blob_sha256.as_str());
    assert_eq!(it["node_id"], node.as_str());
    assert_eq!(it["path"], "docs/отчёт");
    assert_eq!(it["locator"], loc.as_str());
    assert!(text(&it).contains("Zirconium"), "{it}");
    assert!(it.get("error").is_none(), "{it}");
    // абзац внутри блока — блок, где он лежит
    let it = read1(
        &a,
        &tok,
        json!({"id": format!("a:{chunk}"), "locator": "para:7"}),
    )
    .await;
    assert!(text(&it).contains("Zirconium"), "{it}");
    assert_eq!(it["locator"], "para:7");

    // слайд: только его текст
    let b = format!("b:{}", dp.blob_sha256);
    let it = read1(&a, &tok, json!({"id": &b, "locator": "slide:2"})).await;
    assert!(text(&it).contains("cobalt walrus"), "{it}");
    assert!(!text(&it).contains("nothing special"), "{it}");
    assert_eq!(it["locator_text"], "слайд 2");
    let it = read1(&a, &tok, json!({"id": &b, "locator": "slide:9"})).await;
    assert_eq!(it["error"], "not_found", "{it}");

    // лист: блоки, пересекающие диапазон; другой лист — нет
    let bx = format!("b:{}", dx.blob_sha256);
    for range in ["A1:B3", "A2:B2", "b2"] {
        let it = read1(
            &a,
            &tok,
            json!({"id": &bx, "locator": format!("sheet:Итоги!{range}")}),
        )
        .await;
        assert!(text(&it).contains("tungsten"), "{range}: {it}");
        assert!(!text(&it).contains("hydrant"), "{range}: {it}");
    }
    let it = read1(
        &a,
        &tok,
        json!({"id": &bx, "locator": "sheet:Итоги!A50:B60"}),
    )
    .await;
    assert_eq!(it["error"], "not_found", "{it}");
    let it = read1(&a, &tok, json!({"id": &bx, "locator": "sheet:Склад!A1:B2"})).await;
    assert!(text(&it).contains("hydrant"), "{it}");
    // секрет в имени листа: найден по сырому имени, в ответе — маска
    let it = read1(
        &a,
        &tok,
        json!({"id": &bx, "locator": "sheet:AKIAIOSFODNN7EXAMPLE!A1:B2"}),
    )
    .await;
    assert!(text(&it).contains("vanadium"), "{it}");
    assert!(!it.to_string().contains("AKIAIOSFODNN7EXAMPLE"), "{it}");

    // PDF: страница
    if pdf {
        let dpdf = wait_done(&a, user, &node, "книга.pdf").await;
        let it = read1(
            &a,
            &tok,
            json!({"id": format!("b:{}", dpdf.blob_sha256), "locator": "page:2"}),
        )
        .await;
        assert!(text(&it).contains("Nickel albatross"), "{it}");
        assert!(!text(&it).contains("first page body"), "{it}");
        assert_eq!(it["locator_text"], "стр. 2");
    }

    // Markdown: заголовок элемента с файлом и подписью locator
    let r = read_raw(
        &a,
        &tok,
        json!({"items": [{"id": &b, "locator": "slide:2"}], "format": "md"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let md = r.text();
    let head = md.lines().next().unwrap();
    assert!(
        head.starts_with(&format!(
            "### {b} Knowledge/docs/отчёт › доклад.pptx · слайд 2"
        )),
        "{md}"
    );
    assert!(md.contains("cobalt walrus"), "{md}");
    a.st.shutdown().await;
}

/// Без `locator` (`b:`, `d:`, `tier: L1` у `a:`) — оглавление: страницы, листы, слайды с первой
/// строкой и подсказкой `locator`; размер ограничен (документ целиком не выдаётся).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_attachment_toc_without_locator() {
    let (a, _) = world("read-att-toc").await;
    let user = a.st.admin_id;
    let tok = a.token.clone();
    let node = note(&a, &tok, "knowledge", "docs/оглавление").await;
    let deck = pptx(&[
        ("Intro", "nothing special"),
        ("Second", "cobalt walrus migration"),
    ]);
    let book = xlsx(&[
        ("Итоги", &[&["Name", "Note"], &["Alpha", "tungsten"]]),
        ("Склад", &[&["Item", "Qty"], &["hydrant", "12"]]),
    ]);
    // 150 блоков DOCX по ~1600 символов: оглавление не длиннее предела
    let mut paras: Vec<String> = (0..150)
        .map(|i| format!("Раздел {i:03}. {}", filler(20)))
        .collect();
    paras.push("Заключительный ниобий.".into());
    let paras: Vec<&str> = paras.iter().map(String::as_str).collect();
    upload(&a, &tok, &node, "доклад.pptx", &deck).await;
    upload(&a, &tok, &node, "книга.xlsx", &book).await;
    upload(&a, &tok, &node, "длинный.docx", &docx(&paras)).await;
    let dp = wait_done(&a, user, &node, "доклад.pptx").await;
    let dx = wait_done(&a, user, &node, "книга.xlsx").await;
    let dd = wait_done(&a, user, &node, "длинный.docx").await;

    let it = read1(&a, &tok, json!({"id": format!("b:{}", dp.blob_sha256)})).await;
    assert_eq!(it["toc"], true, "{it}");
    let t = text(&it);
    assert!(t.contains("slide:1") && t.contains("Intro"), "{t}");
    assert!(t.contains("slide:2") && t.contains("Second"), "{t}");
    assert!(
        !t.contains("cobalt walrus"),
        "в оглавлении — первая строка: {t}"
    );
    assert!(t.contains("locator"), "подсказка: {t}");
    assert!(it.get("locator").is_none(), "{it}");
    // d:<id> — тот же документ
    let it2 = read1(&a, &tok, json!({"id": format!("d:{}", dp.id)})).await;
    assert_eq!(it2["text"], it["text"]);
    assert_eq!(it2["ref"], format!("d:{}", dp.id));
    // a:<chunk> с tier L1 — оглавление его документа
    let (chunk, _) = chunk_with(&a, user, dp.id, "cobalt").await;
    let it3 = read1(&a, &tok, json!({"id": format!("a:{chunk}"), "tier": "L1"})).await;
    assert_eq!(it3["text"], it["text"]);

    let it = read1(&a, &tok, json!({"id": format!("b:{}", dx.blob_sha256)})).await;
    let t = text(&it);
    // блок листа — строки данных (шапка повторяется в тексте блока)
    assert!(t.contains("sheet:Итоги!A2:B2") && t.contains("Name"), "{t}");
    assert!(t.contains("sheet:Склад!A2:B2") && t.contains("Item"), "{t}");

    let it = read1(&a, &tok, json!({"id": format!("b:{}", dd.blob_sha256)})).await;
    let t = text(&it);
    assert!(t.contains("para:1") && t.contains("Раздел 000"), "{t}");
    assert!(!t.contains("ниобий"), "оглавление ограничено: {t}");
    assert!(t.contains("ещё"), "{t}");
    let tokens = it["tokens"].as_u64().unwrap();
    // без модели — оценка сверху (M6, задача 5): ×1,2–1,4 к bge-m3
    assert!(tokens < 5000, "оглавление {tokens} токенов");
    a.st.shutdown().await;
}

/// `locator` у заметки, записи журнала и пути — 422 `/problems/locator-invalid`.
#[tokio::test]
async fn read_note_with_locator_422() {
    let a = api("read-note-locator", |_| {}).await;
    let n = a.note("knowledge", "n", "# N\n\nТекст.\n").await;
    let nid = n["id"].as_str().unwrap().to_string();
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(json!({"kind": "insight", "title": "Т", "body": "Б",
                        "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let eid = r.json()["id"].as_str().unwrap().to_string();
    for id in [
        format!("n:{nid}"),
        nid.clone(),
        format!("i:{eid}"),
        "knowledge:/n".to_string(),
    ] {
        let r = read_raw(
            &a,
            &a.token,
            json!({"items": [{"id": &id, "locator": "page:1"}]}),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{id}: {}",
            r.text()
        );
        assert_eq!(r.json()["type"], "/problems/locator-invalid", "{id}");
    }
    // locator: null — как без него
    let r = read_raw(
        &a,
        &a.token,
        json!({"items": [{"id": &nid, "locator": null}]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    a.st.shutdown().await;
}

/// Неверный формат `locator` — 422 `/problems/locator-invalid` до чтения.
#[tokio::test]
async fn read_bad_locator_422() {
    let a = api("read-bad-locator", |_| {}).await;
    let fake = "a:0123456789abcdef0123456789abcdef";
    for bad in [
        json!("p.4"),
        json!("page:0"),
        json!("page:+3"),
        json!("slide:x"),
        json!("sheet:Итоги"),
        json!("sheet:Итоги!1A"),
        json!(""),
        json!(42),
        json!({"page": 1}),
    ] {
        let r = read_raw(
            &a,
            &a.token,
            json!({"items": [{"id": fake, "locator": &bad}]}),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad}: {}",
            r.text()
        );
        assert_eq!(r.json()["type"], "/problems/locator-invalid", "{bad}");
    }
    a.st.shutdown().await;
}

/// `b:<sha256>`: блоб в нескольких заметках — первое по дате вложение и `warnings`; регистр hex
/// не важен; Human по токену не виден (в выборе не участвует); неизвестный блоб — `not_found`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_b_sha_ref() {
    let (a, _) = world("read-b-sha").await;
    let user = a.st.admin_id;
    let tok = a.token.clone();
    let deck = pptx(&[("Intro", "nothing special"), ("Second", "cobalt walrus")]);
    let n1 = note(&a, &tok, "knowledge", "p/первая").await;
    upload(&a, &tok, &n1, "a.pptx", &deck).await;
    let d1 = wait_done(&a, user, &n1, "a.pptx").await;
    let n2 = note(&a, &tok, "knowledge", "p/вторая").await;
    upload(&a, &tok, &n2, "b.pptx", &deck).await;
    let d2 = wait_done(&a, user, &n2, "b.pptx").await;
    assert_eq!(d1.blob_sha256, d2.blob_sha256);
    let b = format!("b:{}", d1.blob_sha256);
    let it = read1(&a, &tok, json!({"id": &b, "locator": "slide:2"})).await;
    assert_eq!(it["file"], "a.pptx", "{it}");
    assert_eq!(it["attachment_id"], d1.id.to_string());
    assert_eq!(it["ref"], b.as_str());
    let w = it["warnings"].as_array().unwrap_or_else(|| panic!("{it}"));
    assert_eq!(w.len(), 1, "{it}");
    assert!(w[0].as_str().unwrap().contains('2'), "{it}");
    let up = format!("b:{}", d1.blob_sha256.to_ascii_uppercase());
    let it = read1(&a, &tok, json!({"id": &up, "locator": "slide:2"})).await;
    assert_eq!(it["file"], "a.pptx", "{it}");
    assert!(text(&it).contains("cobalt"), "{it}");

    // Human-вложение того же блоба по токену не видно: выбор — без него
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "human", "path": "p/дневник", "kind": "note",
                        "content": "# Д\n\nД.\n"}),
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let h = r.json()["id"].as_str().unwrap().to_string();
    let other = pptx(&[("Secret", "human only osmium")]);
    upload_adm(&a, &h, "h.pptx", &other).await;
    let dh = wait_done(&a, user, &h, "h.pptx").await;
    let it = read1(&a, &tok, json!({"id": format!("b:{}", dh.blob_sha256)})).await;
    assert_eq!(it["error"], "not_found", "{it}");
    // администратору (сессия WebUI) Human виден
    let r = a
        .adm(
            Method::POST,
            READ,
            Some(json!({"items": [{"id": format!("b:{}", dh.blob_sha256), "locator": "slide:1"}]})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        r.json()["items"][0]["text"]
            .as_str()
            .unwrap()
            .contains("osmium"),
        "{}",
        r.text()
    );

    let fake = format!("b:{}", "0".repeat(64));
    let it = read1(&a, &tok, json!({"id": &fake})).await;
    assert_eq!(it["error"], "not_found", "{it}");
    for bad in ["b:xyz", "b:", "a:zz", "d:нет"] {
        let it = read1(&a, &tok, json!({"id": bad})).await;
        assert_eq!(it["error"], "not_found", "{bad}: {it}");
    }
    a.st.shutdown().await;
}

/// `session_seen`: полный текст по `locator` — часть `blob`/sha256/`locator`, своя на каждый
/// `locator`; оглавление и обрезанный текст — не отмечаются. Поиск `mode=chunks` отмечает чанк
/// вложения (часть — `chunk_id`), и следующий поиск выдаёт карточку `seen: true`; прочитанный
/// через `/read` `locator` тоже делает карточку `seen`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_seen_parts_per_locator() {
    let (a, _) = world("read-seen-locator").await;
    let user = a.st.admin_id;
    let tok = a.token.clone();
    let node = note(&a, &tok, "knowledge", "p/доклад").await;
    let deck = pptx(&[
        ("Intro", "rhodium kestrel overview"),
        ("Second", "cobalt walrus migration"),
        ("Third", "tellurium heron summary"),
    ]);
    upload(&a, &tok, &node, "d.pptx", &deck).await;
    let d = wait_done(&a, user, &node, "d.pptx").await;
    let sha = d.blob_sha256.clone();
    let b = format!("b:{sha}");
    let s = "read-seen-1";
    let rd = |item: Value| {
        let body = json!({"items": [item], "session_id": s});
        let a = &a;
        let tok = tok.clone();
        async move { read(a, &tok, body).await }
    };
    rd(json!({"id": &b})).await; // оглавление
    assert!(seen_parts(&a, user, s, &sha).await.is_empty());
    rd(json!({"id": &b, "locator": "slide:1", "max_tokens": 1})).await; // обрезано
    assert!(seen_parts(&a, user, s, &sha).await.is_empty());
    rd(json!({"id": &b, "locator": "slide:1"})).await;
    rd(json!({"id": &b, "locator": "slide:2"})).await;
    assert_eq!(
        seen_parts(&a, user, s, &sha).await,
        [("slide:1".to_string(), 0), ("slide:2".to_string(), 0)]
    );

    // карточка фрагмента slide:2 уже выдана сессии через /read
    let search = |query: &str, mode: &str, session: &str| {
        let body = json!({"query": query, "mode": mode, "session_id": session});
        let a = &a;
        let tok = tok.clone();
        async move {
            let r = a
                .tok(&tok, Method::POST, "/api/v1/search", Some(body))
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()
        }
    };
    let card = |j: &Value| {
        j["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["kind"] == "attachment")
            .cloned()
            .unwrap_or_else(|| panic!("{j}"))
    };
    let j = search("cobalt walrus", "brief", s).await;
    assert_eq!(card(&j)["seen"], true, "{j}");
    let j = search("tellurium heron", "brief", s).await;
    assert_eq!(card(&j)["seen"], false, "{j}");
    // mode=chunks выдаёт чанк — часть chunk_id; повтор — seen
    let s2 = "read-seen-2";
    let j = search("tellurium heron", "chunks", s2).await;
    let c = card(&j);
    assert_eq!(c["seen"], false, "{j}");
    let chunk = c["chunk_ids"][0].as_str().unwrap().to_string();
    assert_eq!(seen_parts(&a, user, s2, &sha).await, [(chunk, 0)]);
    let j = search("tellurium heron", "chunks", s2).await;
    assert_eq!(card(&j)["seen"], true, "{j}");
    a.st.shutdown().await;
}

/// Права: Human по токену, Knowledge владельца участнику общего решения и любой фрагмент
/// постороннему — ответ тот же, что на несуществующий `a:`/`b:`/`d:` (без id владельца и состояния
/// доступа); фрагмент пространства решения участнику читается.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_attachment_access_like_missing() {
    let (a, _) = world("read-att-access").await;
    let owner = a.st.admin_id;
    let tok = a.token.clone();
    let (b_id, b_tok) = a.new_user("Борис").await;
    let (_c_id, c_tok) = a.new_user("Вера").await;
    let reg = json!({"agent_solution_id": G, "name": "Общее"});
    let r = a
        .tok(&tok, Method::POST, "/api/v1/solutions", Some(reg.clone()))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = a
        .tok(&b_tok, Method::POST, "/api/v1/solutions", Some(reg))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let uri = format!("/api/v1/solutions/{G}/access/{b_id}/approve");
    let r = a.tok(&tok, Method::POST, &uri, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let k = note(&a, &tok, "knowledge", "p/скрытая").await;
    let s = note(&a, &tok, G, "p/общая").await;
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "human", "path": "p/дневник", "kind": "note",
                        "content": "# Д\n\nД.\n"}),
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let h = r.json()["id"].as_str().unwrap().to_string();
    upload(
        &a,
        &tok,
        &k,
        "k.pptx",
        &pptx(&[("K", "knowledge scandium")]),
    )
    .await;
    upload(&a, &tok, &s, "s.pptx", &pptx(&[("S", "solution scandium")])).await;
    upload_adm(&a, &h, "h.pptx", &pptx(&[("H", "human scandium")])).await;
    let dk = wait_done(&a, owner, &k, "k.pptx").await;
    let ds = wait_done(&a, owner, &s, "s.pptx").await;
    let dh = wait_done(&a, owner, &h, "h.pptx").await;
    let ck = chunk_locators(&a, owner, dk.id).await[0].0.clone();
    let cs = chunk_locators(&a, owner, ds.id).await[0].0.clone();
    let ch = chunk_locators(&a, owner, dh.id).await[0].0.clone();
    let refs = |c: &str, d: &zam_store::AttachmentDoc| {
        vec![
            format!("a:{c}"),
            format!("b:{}", d.blob_sha256),
            format!("d:{}", d.id),
        ]
    };
    let missing = [
        "a:0123456789abcdef0123456789abcdef".to_string(),
        // буквы и цифры, как у настоящего sha256: стоимость `~N tok` без модели — по классам
        // символов (M6, задача 5), у одних цифр она другая
        format!("b:{}", "0f".repeat(32)),
        format!("d:{}", Id::new()),
    ];
    // ответ на ссылку `r` — тело элемента без самой ссылки
    let shape = async |token: &str, r: &str, locator: Option<&str>| {
        let mut item = json!({"id": r});
        if let Some(l) = locator {
            item["locator"] = json!(l);
        }
        let resp = read_raw(&a, token, json!({"items": [item], "format": "md"})).await;
        assert_eq!(resp.status, StatusCode::OK, "{}", resp.text());
        let md = resp.text().replace(r, "REF");
        let j = read1(&a, token, item).await;
        (
            md,
            j.to_string().replace(r, "REF"),
            resp.header("x-zam-read-error").to_string(),
        )
    };
    let base = shape(&tok, &missing[0], None).await;
    assert!(base.1.contains("not_found"), "{base:?}");
    let cases: Vec<(&str, Vec<String>)> = vec![
        (&tok, refs(&ch, &dh)),
        (&b_tok, refs(&ck, &dk)),
        (&b_tok, refs(&ch, &dh)),
        (&c_tok, refs(&ck, &dk)),
        (&c_tok, refs(&cs, &ds)),
    ];
    for (who, list) in cases.iter() {
        for r in list.iter().chain(missing.iter()) {
            for loc in [None, Some("slide:1")] {
                // `~N tok` зависит от длины ссылки: сравнение — с несуществующей того же вида
                let same = missing.iter().find(|m| m[..2] == r[..2]).unwrap();
                let got = shape(who, r, loc).await;
                let want = shape(who, same, loc).await;
                assert_eq!(got, want, "{r} {loc:?}");
                assert!(!got.0.contains(&k) && !got.0.contains(&h), "{got:?}");
                assert!(!got.1.contains(&owner.to_string()), "{got:?}");
            }
        }
    }
    // участнику — фрагмент пространства решения
    for r in refs(&cs, &ds) {
        let it = read1(&a, &b_tok, json!({"id": &r, "locator": "slide:1"})).await;
        assert!(text(&it).contains("solution scandium"), "{r}: {it}");
    }
    a.st.shutdown().await;
}

/// Раунд правок 1 (Important 1): два листа с секретами в именах (одна маска) — в оглавлении по
/// номеру `sheet:#N`, строки различимы; чтение по номеру и по сырому имени отдаёт только свой
/// лист; сырого имени в ответах нет. Обычный лист — по имени, номер тоже принимается.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_secret_sheets_by_number() {
    let (a, _) = world("read-secret-sheets").await;
    let user = a.st.admin_id;
    let tok = a.token.clone();
    let node = note(&a, &tok, "knowledge", "docs/ключи").await;
    let (s1, s2) = ("AKIAIOSFODNN7EXAMPLE", "AKIAJ2345678ABCDEFGH");
    let book = xlsx(&[
        ("Итоги", &[&["Name", "Note"], &["Alpha", "tungsten"]]),
        (s1, &[&["Key", "Val"], &["vanadium", "1"]]),
        (s2, &[&["Key", "Val"], &["molybdenum", "2"]]),
    ]);
    upload(&a, &tok, &node, "ключи.xlsx", &book).await;
    let d = wait_done(&a, user, &node, "ключи.xlsx").await;
    let b = format!("b:{}", d.blob_sha256);
    let toc = read1(&a, &tok, json!({"id": &b})).await;
    let t = text(&toc);
    assert!(t.contains("- sheet:Итоги!A2:B2 —"), "{t}");
    assert!(t.contains("- sheet:#2!A2:B2 —"), "{t}");
    assert!(t.contains("- sheet:#3!A2:B2 —"), "{t}");
    assert!(!toc.to_string().contains("AKIA"), "{toc}");
    for (loc, want, not) in [
        ("sheet:#2!A2:B2", "vanadium", "molybdenum"),
        ("sheet:#3!A2:B2", "molybdenum", "vanadium"),
        (
            &format!("sheet:{s2}!A1:B9") as &str,
            "molybdenum",
            "vanadium",
        ),
        (&format!("sheet:{s1}!B2") as &str, "vanadium", "molybdenum"),
    ] {
        let it = read1(&a, &tok, json!({"id": &b, "locator": loc})).await;
        assert!(text(&it).contains(want), "{loc}: {it}");
        assert!(!text(&it).contains(not), "{loc}: {it}");
        let shown = it["locator"].as_str().unwrap();
        assert!(shown.starts_with("sheet:#"), "{loc}: {it}");
        assert!(
            it["locator_text"].as_str().unwrap().starts_with("лист №"),
            "{it}"
        );
        assert!(!it.to_string().contains("AKIA"), "{loc}: {it}");
    }
    // обычный лист: по номеру — тот же лист, показ — по имени
    let it = read1(&a, &tok, json!({"id": &b, "locator": "sheet:#1!A2:B2"})).await;
    assert!(text(&it).contains("tungsten"), "{it}");
    assert_eq!(it["locator"], "sheet:Итоги!A2:B2");
    let it = read1(&a, &tok, json!({"id": &b, "locator": "sheet:#9!A1"})).await;
    assert_eq!(it["error"], "not_found", "{it}");
    a.st.shutdown().await;
}

/// Раунд правок 1 (M-3): администратор (сессия WebUI) читает `a:`/`b:`/`d:` вложения в чужом
/// решении так же, как его заметку `n:` (`node_scope` — все решения сервера).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_reads_foreign_solution_attachment() {
    const G3: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e93";
    let (a, _) = world("read-admin-foreign").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let bid: Id = b_id.parse().unwrap();
    let r = a
        .tok(
            &b_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G3, "name": "Борисово"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let s = note(&a, &b_tok, G3, "p/борис").await;
    upload(&a, &b_tok, &s, "b.pptx", &pptx(&[("B", "boron pelican")])).await;
    let d = wait_done(&a, bid, &s, "b.pptx").await;
    let chunk = chunk_locators(&a, bid, d.id).await[0].0.clone();
    let adm = async |item: Value| {
        let r = a
            .adm(Method::POST, READ, Some(json!({"items": [item]})), None)
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        r.json()["items"][0].clone()
    };
    let it = adm(json!({"id": format!("n:{s}")})).await;
    assert!(it.get("error").is_none(), "{it}");
    for r in [
        format!("a:{chunk}"),
        format!("b:{}", d.blob_sha256),
        format!("d:{}", d.id),
    ] {
        let it = adm(json!({"id": &r, "locator": "slide:1"})).await;
        assert!(
            it["text"].as_str().unwrap_or("").contains("boron pelican"),
            "{r}: {it}"
        );
    }
    // токен администратора (не сессия) чужого решения не видит — как у n:
    let it = read1(&a, &a.token, json!({"id": format!("d:{}", d.id)})).await;
    assert_eq!(it["error"], "not_found", "{it}");
    let it = read1(&a, &a.token, json!({"id": format!("n:{s}")})).await;
    assert_eq!(it["error"], "not_found", "{it}");
    a.st.shutdown().await;
}
