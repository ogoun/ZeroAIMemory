//! Поиск по вложениям (M5, задача 6): карточка `a:<chunk>` с файлом и `locator`, каналы FTS и
//! векторный (листовой prefetch уровня `attachment`), квота `search.max_per_attachment`, фильтр
//! видов, наследование `area` и `certainty` от заметки-владельца, права участника общего решения
//! и Human.
//!
//! Векторная часть — с MiniLM и Qdrant (`common::ml::have`; в CI с `ZAM_REQUIRE_ML=1` —
//! обязательны), без них тесты идут только по FTS. PDF — с pdfium из `target/deps`.

mod common;

use axum::http::{Method, StatusCode};
use common::api::Api;
use common::attach::{
    atts, chunk_locators, docx, files, filler, items, pptx, search, search_adm, sources, upload,
    upload_adm, wait_done, wait_points, world, xlsx,
};
use serde_json::{Value, json};
use zam_docs::test_pdf::{Page, build};
use zam_store::attachments::Locator;

/// Заметка токеном `token`; `area` — заголовок `X-Zam-Area`.
async fn note(
    a: &Api,
    token: &str,
    space: &str,
    path: &str,
    content: &str,
    area: Option<&str>,
) -> String {
    let extra: Vec<(&str, &str)> = area.map(|v| ("x-zam-area", v)).into_iter().collect();
    let r = a
        .tok_h(
            token,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": space, "path": path, "kind": "note", "content": content})),
            &extra,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

/// Карточки заметок выдачи.
fn notes(j: &Value) -> Vec<&Value> {
    items(j).iter().filter(|i| i["kind"] == "note").collect()
}

/// DOCX, PDF, XLSX и PPTX одной заметки: каждый формат находится по своему слову карточкой
/// `kind: attachment` с `id = a:<chunk>`, файлом, `locator` чанка и его подписью; заметка-владелец
/// без этого слова в выдаче не появляется. С векторным каналом — те же карточки и с `vector`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_finds_docx_pdf_xlsx_pptx_with_locator() {
    let w = world("att-search-formats", true).await;
    let a = &w.a;
    let user = a.st.admin_id;
    let node = note(
        a,
        &a.token,
        "knowledge",
        "docs/отчёт",
        "# Отчёт\n\nСводка.\n",
        None,
    )
    .await;
    // абзацы по ~400 символов: единицы DOCX режутся по мягкому пределу, слово — не в первой
    let mut paras: Vec<String> = (0..6).map(|_| filler(5)).collect();
    paras.push("Zirconium flamingo reactor overview.".into());
    let paras: Vec<&str> = paras.iter().map(String::as_str).collect();
    let mut cases: Vec<(&str, Vec<u8>, &str, &str)> = vec![
        ("план.docx", docx(&paras), "zirconium", "para:"),
        (
            "итоги.xlsx",
            xlsx(&[(
                "Итоги",
                &[
                    &["Name", "Note"],
                    &["Alpha", "tungsten pelican harbour"],
                    &["Beta", "ordinary row"],
                ],
            )]),
            "tungsten",
            "sheet:Итоги!",
        ),
        (
            "доклад.pptx",
            pptx(&[
                ("Intro", "nothing special"),
                ("Second", "cobalt walrus migration"),
            ]),
            "cobalt",
            "slide:2",
        ),
    ];
    if w.pdf {
        cases.push((
            "книга.pdf",
            build(&[
                Page::default().line(72.0, 700.0, "first page body"),
                Page::default().line(72.0, 700.0, "Nickel albatross telemetry"),
            ]),
            "nickel",
            "page:2",
        ));
    } else {
        eprintln!("pdfium не найден (cargo xtask fetch-deps) — PDF в тесте пропущен");
    }
    let mut total = 0;
    let mut docs = Vec::new();
    for (file, data, _, _) in &cases {
        upload(a, &a.token, &node, file, data).await;
    }
    for (file, _, _, _) in &cases {
        let d = wait_done(a, user, &node, file).await;
        total += chunk_locators(a, user, d.id).await.len() as u64;
        docs.push(d);
    }
    if w.vector {
        wait_points(a, user, total).await;
    }
    for ((file, _, word, loc_prefix), d) in cases.iter().zip(&docs) {
        let j = search(
            a,
            &a.token,
            json!({"query": word, "top_k": 5, "budget_tokens": 4000}),
        )
        .await;
        // первым — фрагмент со словом (FTS); прочее в малом корпусе приходит только вектором
        let it = &items(&j)[0];
        assert_eq!(it["kind"], "attachment", "{file}: {j}");
        assert!(sources(it).contains(&"fts"), "{file}: {it}");
        // FTS-совпадение у файла одно; вторая его карточка (только вектором) допустима в пределах
        // квоты: порядок близких косинусов малого корпуса разный на Linux и Windows (CI abd604c)
        let same: Vec<_> = atts(&j)
            .into_iter()
            .filter(|i| i["file"] == *file)
            .collect();
        let same_fts = same.iter().filter(|i| sources(i).contains(&"fts")).count();
        assert_eq!(same_fts, 1, "{file}: {j}");
        assert!(same.len() <= 2, "max_per_attachment: {file}: {j}");
        for x in items(&j).iter().skip(1) {
            assert!(!sources(x).contains(&"fts"), "без слова — не FTS: {x}");
        }
        assert_eq!(it["file"], *file);
        assert_eq!(it["node_id"], node.as_str());
        assert_eq!(it["attachment_id"], d.id.to_string());
        assert_eq!(it["path"], "docs/отчёт");
        assert_eq!(it["level"], "attachment");
        assert_eq!(it["blob_sha256"], d.blob_sha256.as_str());
        let chunk = it["chunk_ids"][0].as_str().unwrap();
        assert_eq!(it["id"], format!("a:{chunk}"));
        let locs = chunk_locators(a, user, d.id).await;
        let db_loc = locs
            .iter()
            .find(|(id, _)| id == chunk)
            .and_then(|(_, l)| l.clone())
            .unwrap();
        assert_eq!(it["locator"], db_loc.as_str(), "{file}");
        assert!(db_loc.starts_with(loc_prefix), "{file}: {db_loc}");
        if *loc_prefix == "para:" {
            assert_ne!(db_loc, "para:1", "слово DOCX — не в первом блоке");
        }
        let label = Locator::parse(&db_loc).unwrap().label();
        assert_eq!(it["locator_text"], label.as_str());
        if w.vector {
            assert!(sources(it).contains(&"vector"), "{file}: {it}");
        }
        // Markdown: `a:<chunk> <space>/<путь> › <файл> … · <locator_text>`
        let r = a
            .tok(
                &a.token,
                Method::POST,
                "/api/v1/search",
                Some(json!({"query": word, "top_k": 5, "format": "md"})),
            )
            .await;
        let md = r.text();
        let line = md
            .lines()
            .find(|l| l.contains(&format!("a:{chunk}")))
            .unwrap_or_else(|| panic!("{md}"));
        assert!(line.contains(&format!("/docs/отчёт › {file}")), "{line}");
        assert!(line.contains(&format!("· {label}")), "{line}");
    }
    a.st.shutdown().await;
}

/// Вложение — не заметка: во всех режимах (`brief`, `chunks`, `notes`) фрагмент файла — карточка
/// вложения, а не заметки-владельца; `expand=neighbors` заметки не тянет текст вложения. Скрытый
/// документ (узел удалён — чанки остаются в FTS; откреплён) не выдаётся ни `bm25`, ни `trigram`;
/// восстановление узла возвращает его без извлечения.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attachment_not_listed_as_note() {
    let w = world("att-not-note", false).await;
    let a = &w.a;
    let user = a.st.admin_id;
    let node = note(
        a,
        &a.token,
        "knowledge",
        "a/хозяин",
        "# Хозяин\n\nОбычный текст заметки про погоду.\n",
        None,
    )
    .await;
    let data = docx(&["Плутониевый квазар и метка zx_9137 в документе."]);
    upload(a, &a.token, &node, "факт.docx", &data).await;
    let d = wait_done(a, user, &node, "факт.docx").await;
    for mode in ["brief", "chunks", "notes"] {
        let j = search(a, &a.token, json!({"query": "квазар", "mode": mode})).await;
        assert_eq!(items(&j).len(), 1, "{mode}: {j}");
        let it = &items(&j)[0];
        assert_eq!(it["kind"], "attachment", "{mode}: {j}");
        assert_eq!(it["node_id"], node.as_str());
        match mode {
            "chunks" => assert!(it["text"].as_str().unwrap().contains("квазар"), "{it}"),
            _ => assert!(
                it["snippet"].as_str().unwrap().contains("квазар"),
                "{mode}: {it}"
            ),
        }
    }
    // trigram — идентификатор
    let j = search(a, &a.token, json!({"query": "zx_9137"})).await;
    assert_eq!(files(&j), ["факт.docx"], "{j}");
    // соседи листа заметки — только листы заметки
    let j = search(
        a,
        &a.token,
        json!({"query": "погоду", "mode": "chunks", "expand": "neighbors"}),
    )
    .await;
    assert_eq!(notes(&j).len(), 1, "{j}");
    assert!(atts(&j).is_empty());
    assert!(!notes(&j)[0]["text"].as_str().unwrap().contains("квазар"));

    let hidden = |j: &Value| atts(j).is_empty() && items(j).is_empty();
    let r = a
        .t(Method::DELETE, &format!("/api/v1/nodes/{node}"), None)
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    // чанки вложения остаются в FTS (восстановление без извлечения), но не выдаются
    let in_fts: i64 = common::attach::db(a, user)
        .await
        .read(move |c| {
            Ok(c.query_row(
                "SELECT count(*) FROM chunks_fts f JOIN chunks c ON c.seq = f.rowid
                 WHERE chunks_fts MATCH 'квазар' AND c.attachment_id = ?1",
                [d.id.to_string()],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(in_fts, 1);
    for q in ["квазар", "zx_9137"] {
        let j = search(a, &a.token, json!({"query": q})).await;
        assert!(hidden(&j), "{q}: {j}");
    }
    let r = a
        .t(Method::POST, &format!("/api/v1/nodes/{node}/restore"), None)
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let j = search(a, &a.token, json!({"query": "квазар"})).await;
    assert_eq!(files(&j), ["факт.docx"], "{j}");
    // открепление
    let r = a
        .t(
            Method::DELETE,
            &format!(
                "/api/v1/nodes/{node}/attachments/{}/{}",
                d.blob_sha256,
                common::api::enc("факт.docx")
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let j = search(a, &a.token, json!({"query": "квазар"})).await;
    assert!(hidden(&j), "{j}");
    a.st.shutdown().await;
}

/// Квота: с одного узла — не больше `search.max_per_node` (3) чанков заметки, с одного документа
/// вложения — не больше `search.max_per_attachment` (2); вложение не съедает квоту заметки.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attachment_quota_separate() {
    let w = world("att-quota", false).await;
    let a = &w.a;
    let user = a.st.admin_id;
    let sections: String = (1..=4)
        .map(|i| format!("## Раздел {i}\n\nОсмий в разделе {i}. {}\n\n", filler(40)))
        .collect();
    let node = note(
        a,
        &a.token,
        "knowledge",
        "q/узел",
        &format!("# Узел\n\n{sections}"),
        None,
    )
    .await;
    let slides: Vec<(String, String)> = (1..=4)
        .map(|i| (format!("Слайд {i}"), format!("Осмий на слайде {i}.")))
        .collect();
    let slides: Vec<(&str, &str)> = slides
        .iter()
        .map(|(t, b)| (t.as_str(), b.as_str()))
        .collect();
    upload(a, &a.token, &node, "осмий.pptx", &pptx(&slides)).await;
    wait_done(a, user, &node, "осмий.pptx").await;
    let j = search(
        a,
        &a.token,
        json!({"query": "осмий", "top_k": 10, "budget_tokens": 20000}),
    )
    .await;
    assert_eq!(notes(&j).len(), 3, "квота заметки: {j}");
    assert_eq!(atts(&j).len(), 2, "квота вложения: {j}");
    // notes: по карточке на заметку и на документ
    let j = search(a, &a.token, json!({"query": "осмий", "mode": "notes"})).await;
    assert_eq!((notes(&j).len(), atts(&j).len()), (1, 1), "{j}");
    a.st.shutdown().await;
}

/// `filters.kinds`: `note` — без вложений, `attachment` — только вложения, по умолчанию — оба.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kinds_note_only_excludes_attachments() {
    let w = world("att-kinds", true).await;
    let a = &w.a;
    let user = a.st.admin_id;
    let node = note(
        a,
        &a.token,
        "knowledge",
        "k/иридий",
        "# Iridium\n\nIridium crucible in the note body.\n",
        None,
    )
    .await;
    upload(
        a,
        &a.token,
        &node,
        "иридий.docx",
        &docx(&["Iridium crucible inside the attached document."]),
    )
    .await;
    let d = wait_done(a, user, &node, "иридий.docx").await;
    if w.vector {
        wait_points(a, user, chunk_locators(a, user, d.id).await.len() as u64).await;
    }
    let q = |kinds: Option<&[&str]>| {
        let mut b = json!({"query": "iridium crucible"});
        if let Some(k) = kinds {
            b["filters"] = json!({ "kinds": k });
        }
        b
    };
    let j = search(a, &a.token, q(None)).await;
    assert_eq!((notes(&j).len(), atts(&j).len()), (1, 1), "{j}");
    let j = search(a, &a.token, q(Some(&["note"]))).await;
    assert!(atts(&j).is_empty() && notes(&j).len() == 1, "{j}");
    let j = search(a, &a.token, q(Some(&["attachment"]))).await;
    assert!(notes(&j).is_empty() && atts(&j).len() == 1, "{j}");
    if w.vector {
        // и векторный канал отдаёт точку вложения только при `attachment` в видах
        assert!(sources(atts(&j)[0]).contains(&"vector"), "{j}");
    }
    a.st.shutdown().await;
}

/// Фильтры выдачи вложения наследуются от заметки-владельца в момент поиска: `scope.area` —
/// `nodes.area`, `filters.certainty` — текущий `certainty` заметки (правка frontmatter без
/// переизвлечения сразу меняет выдачу); в векторном канале точки вложений эти условия проходят,
/// их проверяет SQLite.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attachment_filters_inherit_owner_note() {
    let w = world("att-inherit", true).await;
    let a = &w.a;
    let user = a.st.admin_id;
    let ops = note(
        a,
        &a.token,
        "knowledge",
        "f/ops",
        "# Ops\n\nПро сеть.\n",
        Some("ops"),
    )
    .await;
    let other = note(
        a,
        &a.token,
        "knowledge",
        "f/other",
        "# Other\n\nПро кухню.\n",
        None,
    )
    .await;
    let text = "Ruthenium lighthouse calibration procedure.";
    upload(a, &a.token, &ops, "ops.docx", &docx(&[text])).await;
    upload(a, &a.token, &other, "other.docx", &docx(&[text])).await;
    let mut n = 0;
    for (node, file) in [(&ops, "ops.docx"), (&other, "other.docx")] {
        let d = wait_done(a, user, node, file).await;
        n += chunk_locators(a, user, d.id).await.len() as u64;
    }
    if w.vector {
        wait_points(a, user, n).await;
    }
    let q = json!({"query": "ruthenium lighthouse"});
    assert_eq!(
        files(&search(a, &a.token, q.clone()).await),
        ["ops.docx", "other.docx"]
    );
    let j = search(
        a,
        &a.token,
        json!({"query": "ruthenium lighthouse", "scope": {"area": "ops"}}),
    )
    .await;
    assert_eq!(files(&j), ["ops.docx"], "{j}");
    let cert = json!({"query": "ruthenium lighthouse", "filters": {"certainty": ["hypothesis"]}});
    assert!(atts(&search(a, &a.token, cert.clone()).await).is_empty());
    // правка frontmatter заметки: certainty появляется — вложение проходит фильтр без извлечения
    let r = a
        .adm_h(
            Method::PUT,
            &format!("/api/v1/nodes/{other}"),
            Some(json!({"content": "---\ncertainty: hypothesis\n---\n# Other\n\nПро кухню.\n"})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = search(a, &a.token, cert).await;
    assert_eq!(files(&j), ["other.docx"], "{j}");
    assert_eq!(atts(&j)[0]["certainty"], "hypothesis");
    if w.vector {
        assert!(sources(atts(&j)[0]).contains(&"vector"), "{j}");
    }
    a.st.shutdown().await;
}

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e77";

/// Права: участник общего решения видит только вложения пространства решения (не Knowledge
/// владельца); Human-вложение не выдаётся токену (и владельцу без `include_human`), только
/// администратору с `include_human` — каналом FTS.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn participant_sees_only_solution_attachments() {
    let w = world("att-participant", true).await;
    let a = &w.a;
    let owner = a.st.admin_id;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let r = a
        .tok(
            &a.token,
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
    let r = a
        .tok(
            &a.token,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let text = "Osmium gearbox lubrication schedule.";
    let k = note(
        a,
        &a.token,
        "knowledge",
        "p/знания",
        "# Знания\n\nК.\n",
        None,
    )
    .await;
    let s = note(a, &a.token, G, "p/решение", "# Решение\n\nР.\n", None).await;
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "human", "path": "p/дневник", "kind": "note",
                        "content": "# Дневник\n\nД.\n"}),
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let h = r.json()["id"].as_str().unwrap().to_string();
    upload(a, &a.token, &k, "k.docx", &docx(&[text])).await;
    upload(a, &a.token, &s, "s.docx", &docx(&[text])).await;
    upload_adm(a, &h, "h.docx", &docx(&[text])).await;
    let mut n = 0;
    for (node, file) in [(&k, "k.docx"), (&s, "s.docx"), (&h, "h.docx")] {
        let d = wait_done(a, owner, node, file).await;
        if file != "h.docx" {
            n += chunk_locators(a, owner, d.id).await.len() as u64;
        }
    }
    if w.vector {
        wait_points(a, owner, n).await;
    }
    let q = |scope: Value| json!({"query": "osmium gearbox", "scope": scope});
    // участник: только вложение пространства решения
    let j = search(a, &b_tok, q(json!({"solution": G}))).await;
    assert_eq!(files(&j), ["s.docx"], "{j}");
    let it = atts(&j)[0];
    assert_eq!(it["agent_solution_id"], G);
    if w.vector {
        assert!(sources(it).contains(&"vector"), "{j}");
    }
    let j = search(a, &b_tok, q(json!({"solution": G, "all_solutions": true}))).await;
    assert_eq!(files(&j), ["s.docx"], "{j}");
    // владелец по токену: Knowledge и решение, Human — нет
    let j = search(a, &a.token, q(json!({"solution": G}))).await;
    assert_eq!(files(&j), ["k.docx", "s.docx"], "{j}");
    let j = search(
        a,
        &a.token,
        q(json!({"solution": G, "include_human": true})),
    )
    .await;
    assert!(!files(&j).contains(&"h.docx".to_string()), "{j}");
    // администратор с include_human — Human-вложение каналом FTS
    let j = search_adm(a, q(json!({"solution": G, "include_human": true}))).await;
    assert_eq!(files(&j), ["h.docx", "k.docx", "s.docx"], "{j}");
    let hit = atts(&j)
        .into_iter()
        .find(|i| i["file"] == "h.docx")
        .unwrap();
    assert!(!sources(hit).contains(&"vector"), "{hit}");
    a.st.shutdown().await;
}

/// Раунд 1 (M-3): источник выжимки `a:<chunk>` из карточки вложения — заметка-владелец
/// (`n:<узел>`, `rev` карточки вложения отбрасывается); несуществующий фрагмент — 422
/// `cache-source-not-found`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cache_source_attachment_maps_to_owner_note() {
    let w = world("att-cache-src", false).await;
    let a = &w.a;
    let user = a.st.admin_id;
    let node = note(
        a,
        &a.token,
        "knowledge",
        "c/заметка",
        "# Заметка\n\nТекст.\n",
        None,
    )
    .await;
    upload(
        a,
        &a.token,
        &node,
        "лантан.docx",
        &docx(&["Лантановый отчёт о печах."]),
    )
    .await;
    wait_done(a, user, &node, "лантан.docx").await;
    let j = search(a, &a.token, json!({"query": "лантановый"})).await;
    let id = atts(&j)[0]["id"].as_str().unwrap().to_string();
    let put = |sources: Value| {
        let body = json!({"query": "лантан печи", "summary": "Отчёт о печах.", "sources": sources});
        async move {
            a.tok(&a.token, Method::POST, "/api/v1/cache", Some(body))
                .await
        }
    };
    let r = put(json!([{"id": id, "rev": 0}])).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        r.json()["sources"][0]["id"],
        format!("n:{node}"),
        "{}",
        r.text()
    );
    let r = put(json!([{"id": "a:00000000000000000000000000000000"}])).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert_eq!(r.json()["type"], "/problems/cache-source-not-found");
    a.st.shutdown().await;
}

/// Раунд 2 (утечка id в M-3): участник общего решения, передав источником выжимки решения
/// фрагмент Knowledge-документа владельца, получает тот же 422, что и на несуществующий
/// фрагмент, — без `n:<id>` заметки владельца; фрагмент пространства решения принимается.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cache_source_attachment_no_owner_id_leak() {
    let w = world("att-cache-leak", false).await;
    let a = &w.a;
    let owner = a.st.admin_id;
    const G2: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e78";
    let (b_id, b_tok) = a.new_user("Борис").await;
    let reg = json!({"agent_solution_id": G2, "name": "Общее"});
    let r = a
        .tok(
            &a.token,
            Method::POST,
            "/api/v1/solutions",
            Some(reg.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = a
        .tok(&b_tok, Method::POST, "/api/v1/solutions", Some(reg))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let uri = format!("/api/v1/solutions/{G2}/access/{b_id}/approve");
    let r = a.tok(&a.token, Method::POST, &uri, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let k = note(
        a,
        &a.token,
        "knowledge",
        "p/скрытая",
        "# Скрытая\n\nК.\n",
        None,
    )
    .await;
    let s = note(a, &a.token, G2, "p/общая", "# Общая\n\nР.\n", None).await;
    upload(
        a,
        &a.token,
        &k,
        "k.docx",
        &docx(&["Скандиевый секрет владельца."]),
    )
    .await;
    upload(
        a,
        &a.token,
        &s,
        "s.docx",
        &docx(&["Скандиевый отчёт решения."]),
    )
    .await;
    let dk = wait_done(a, owner, &k, "k.docx").await;
    let ds = wait_done(a, owner, &s, "s.docx").await;
    let ck = format!("a:{}", chunk_locators(a, owner, dk.id).await[0].0);
    let cs = format!("a:{}", chunk_locators(a, owner, ds.id).await[0].0);
    let b_tok = b_tok.as_str();
    let put = |id: String| {
        let body = json!({"query": "скандий", "summary": "Скандий.", "solution": G2,
                          "sources": [{"id": id}]});
        async move {
            a.tok(b_tok, Method::POST, "/api/v1/cache", Some(body))
                .await
        }
    };
    let hidden = put(ck.clone()).await;
    let fake = "a:0123456789abcdef0123456789abcdef";
    let missing = put(fake.into()).await;
    assert_eq!(
        hidden.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        hidden.text()
    );
    assert_eq!(missing.status, hidden.status);
    assert!(
        !hidden.text().contains("n:") && !hidden.text().contains(&k),
        "{}",
        hidden.text()
    );
    // ответы неотличимы: тело то же с точностью до ссылки
    assert_eq!(
        hidden.text().replace(&ck, "REF"),
        missing.text().replace(fake, "REF")
    );
    let ok = put(cs).await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.text());
    assert_eq!(ok.json()["sources"][0]["id"], format!("n:{s}"));
    a.st.shutdown().await;
}

/// Раунд 3: prefetch вложений решается по наличию чанков, а не по `extract_state` — документ,
/// ждущий переизвлечения (`done` → `pending` после смены версии извлечения) или с неудачей
/// (`failed`), с прежними чанками и точками находится векторным каналом.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attachment_found_while_reextracting_or_failed() {
    let w = world("att-reextract", true).await;
    if !w.vector {
        return;
    }
    let a = &w.a;
    let user = a.st.admin_id;
    let node = note(a, &a.token, "knowledge", "r/док", "# Док\n\nК.\n", None).await;
    upload(
        a,
        &a.token,
        &node,
        "r.docx",
        &docx(&["Hafnium turbine blade inspection."]),
    )
    .await;
    let d = wait_done(a, user, &node, "r.docx").await;
    wait_points(a, user, chunk_locators(a, user, d.id).await.len() as u64).await;
    for state in ["pending", "failed"] {
        let id = d.id.to_string();
        common::attach::db(a, user)
            .await
            .write(move |t| {
                t.execute(
                    "UPDATE attachment_docs SET extract_state = ?2 WHERE id = ?1",
                    [id, state.to_string()],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let j = search(
            a,
            &a.token,
            json!({"query": "hafnium turbine", "filters": {"kinds": ["attachment"]}}),
        )
        .await;
        assert_eq!(files(&j), ["r.docx"], "{state}: {j}");
        assert!(sources(atts(&j)[0]).contains(&"vector"), "{state}: {j}");
    }
    a.st.shutdown().await;
}

/// Раунд правок 2 задачи 7: карточки фрагментов двух листов с секретами в именах (одна маска)
/// различимы — `locator` `sheet:#N!…` и подпись `лист №N «…»`, номер как у `/read`; `/read d:<id>`
/// с `locator` карточки отдаёт текст того же листа. Сырого имени листа в выдаче нет.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn secret_sheet_cards_numbered_and_readable() {
    let w = world("att-secret-sheets", false).await;
    let a = &w.a;
    let user = a.st.admin_id;
    let node = note(
        a,
        &a.token,
        "knowledge",
        "docs/ключи",
        "# Ключи\n\nК.\n",
        None,
    )
    .await;
    let (s1, s2) = ("AKIAIOSFODNN7EXAMPLE", "AKIAJ2345678ABCDEFGH");
    let book = xlsx(&[
        ("Итоги", &[&["Name", "Note"], &["Alpha", "plain"]]),
        (s1, &[&["Key", "Val"], &["vanadium", "1"]]),
        (s2, &[&["Key", "Val"], &["molybdenum", "2"]]),
    ]);
    upload(a, &a.token, &node, "ключи.xlsx", &book).await;
    wait_done(a, user, &node, "ключи.xlsx").await;
    let mut seen = Vec::new();
    for (word, other, no) in [("vanadium", "molybdenum", 2), ("molybdenum", "vanadium", 3)] {
        let j = search(a, &a.token, json!({"query": word})).await;
        assert!(!j.to_string().contains("AKIA"), "{j}");
        let it = atts(&j)[0].clone();
        let loc = it["locator"].as_str().unwrap().to_string();
        assert_eq!(loc, format!("sheet:#{no}!A2:B2"), "{it}");
        let label = it["locator_text"].as_str().unwrap();
        assert!(label.starts_with(&format!("лист №{no} «")), "{it}");
        // md — та же подпись
        let r = a
            .tok(
                &a.token,
                Method::POST,
                "/api/v1/search",
                Some(json!({"query": word, "format": "md"})),
            )
            .await;
        assert!(
            r.text().contains(&format!("· лист №{no} «")),
            "{}",
            r.text()
        );
        assert!(!r.text().contains("AKIA"), "{}", r.text());
        let doc = it["attachment_id"].as_str().unwrap();
        let r = a
            .tok(
                &a.token,
                Method::POST,
                "/api/v1/read",
                Some(json!({"items": [{"id": format!("d:{doc}"), "locator": &loc}]})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let item = &r.json()["items"][0];
        let text = item["text"].as_str().unwrap_or_else(|| panic!("{item}"));
        assert!(text.contains(word) && !text.contains(other), "{item}");
        assert_eq!(item["locator"], loc.as_str());
        seen.push(loc);
    }
    assert_ne!(seen[0], seen[1]);
    a.st.shutdown().await;
}

// ------------------------------------------------------------- M6 §11: вес вложений w_attach

/// Эталоны выдачи до задачи 11 (M6 §11, `search.w_attach`): мир FTS (без модели) и гибрид
/// (MiniLM + Qdrant). Гибрид сверяется на платформе, где снят эталон: порядок близких косинусов
/// малого корпуса разный на Linux и Windows (см. `common::attach::fts_card`).
const W_ATTACH_FIXTURE_FTS: &str = "tests/fixtures/w_attach_m57_fts.json";
const W_ATTACH_FIXTURE_HYBRID: &str = "tests/fixtures/w_attach_m57_hybrid.json";

/// Заметки мира вложений: темы эксплуатации, частью те же, что у документов (вложения и заметки
/// соревнуются в выдаче).
const W_NOTES: &[(&str, &str)] = &[
    (
        "ops/pump-bearings",
        "# Подшипники насоса\n\nЗамена подшипников главного насоса — по вибрации и наработке в моточасах; журнал вибрации ведёт смена.\n",
    ),
    (
        "ops/filter-backwash",
        "# Промывка фильтра\n\nФильтр грубой очистки промывают обратным током раз в смену; перепад давления — признак засора.\n",
    ),
    (
        "ops/generator-test",
        "# Дизель-генератор\n\nРезервный генератор проверяют под нагрузкой ежемесячно, протокол — в папке энергетика.\n",
    ),
    (
        "ops/valve-lockout",
        "# Блокировка задвижек\n\nПеред ремонтом задвижку закрывают, вешают замок и бирку; ключ — у ответственного.\n",
    ),
    (
        "ops/compressor-oil",
        "# Масло компрессора\n\nУровень масла компрессора смотрят на остановленной машине; доливают маслом той же марки.\n",
    ),
    (
        "ops/boiler-water",
        "# Вода котла\n\nЖёсткость питательной воды котла меряют ежедневно; превышение — продувка и проверка умягчителя.\n",
    ),
    (
        "ops/conveyor-belt",
        "# Лента конвейера\n\nСход ленты конвейера правят натяжной станцией; износ кромки — повод заказать ленту.\n",
    ),
    (
        "ops/forklift-charge",
        "# Зарядка погрузчиков\n\nТяговые батареи погрузчиков заряжают в отдельном помещении с вытяжкой, полный цикл — восемь часов.\n",
    ),
    (
        "it/backup-rotation",
        "# Ротация резервных копий\n\nКопии сервера учёта хранят по схеме дед-отец-сын: дневные неделю, недельные месяц, месячные год.\n",
    ),
    (
        "it/printer-queue",
        "# Очередь печати\n\nЗависшую очередь печати сбрасывают перезапуском службы диспетчера печати.\n",
    ),
    (
        "it/vpn-access",
        "# Удалённый доступ\n\nДоступ к VPN выдаёт отдел ИТ по заявке руководителя; ключи действуют год.\n",
    ),
    (
        "hr/shift-handover",
        "# Передача смены\n\nПри передаче смены старший расписывается в журнале и называет незакрытые наряды.\n",
    ),
    (
        "hr/ppe-issue",
        "# Средства защиты\n\nКаски, очки и перчатки выдают по ведомости; срок носки перчаток — месяц.\n",
    ),
    (
        "safety/fire-drill",
        "# Учебная тревога\n\nУчебную пожарную тревогу проводят раз в полгода, сбор — у главной проходной.\n",
    ),
    (
        "safety/hot-work",
        "# Огневые работы\n\nНа огневые работы оформляют наряд-допуск, место очищают от горючего, рядом — огнетушитель.\n",
    ),
    (
        "quality/sample-retention",
        "# Хранение проб\n\nКонтрольные пробы партии хранят три месяца с этикеткой даты и номера партии.\n",
    ),
];

/// Запросы мира вложений: слова документов, слова заметок, общие темы и пересказ.
const W_QUERIES: &[&str] = &[
    "замена подшипников главного насоса",
    "промывка фильтра обратным током",
    "проверка генератора под нагрузкой",
    "остаток подшипников на складе",
    "график внедрения системы учёта",
    "сколько хранить контрольные пробы",
    "как часто меняют подшипники",
    "перепад давления на фильтре",
    "ротация резервных копий",
    "срок носки перчаток",
];

/// Мир M6 §11: 16 заметок и три документа (DOCX — регламент, XLSX — склад, PPTX — план) одной
/// заметки-владельца; без затухания по свежести (баллы не зависят от времени между снятиями).
/// С вектором — ждёт точек вложений и векторной индексации всех заметок.
async fn w_attach_world(
    name: &str,
    vector: bool,
    tweak: impl FnOnce(&mut zam_server::config::ServerConfig),
) -> common::attach::World {
    let w = common::attach::world_with(name, vector, |c| {
        c.search.recency_half_life_days = 0.0;
        tweak(c);
    })
    .await;
    let a = &w.a;
    let user = a.st.admin_id;
    let mut ids = Vec::new();
    for (path, body) in W_NOTES {
        ids.push(note(a, &a.token, "knowledge", path, body, None).await);
    }
    let owner = note(
        a,
        &a.token,
        "knowledge",
        "docs/регламенты",
        "# Регламенты\n\nДокументы службы эксплуатации.\n",
        None,
    )
    .await;
    ids.push(owner.clone());
    let files: Vec<(&str, Vec<u8>)> = vec![
        (
            "регламент.docx",
            docx(&[
                "Замена подшипников главного насоса выполняется после 8000 моточасов или при росте вибрации выше 7 мм/с.",
                "Фильтр грубой очистки промывается обратным током при перепаде давления 0,5 бар.",
                "Резервный дизель-генератор проверяется под нагрузкой не реже одного раза в месяц.",
                "Методику измерения вибраций согласовали с главным механиком и службой надёжности.",
                "Контрольные пробы каждой партии хранятся девяносто дней в лаборатории.",
                "Огневые работы на складе масел запрещены без письменного наряда-допуска.",
            ]),
        ),
        (
            "склад.xlsx",
            xlsx(&[(
                "Остатки",
                &[
                    &["Позиция", "Количество", "Место"],
                    &["Подшипник 6312", "14", "стеллаж А3"],
                    &["Сальник насоса", "40", "стеллаж А4"],
                    &["Фильтроэлемент ФГО", "6", "стеллаж Б1"],
                    &["Перчатки рабочие", "300", "кладовая"],
                ],
            )]),
        ),
        (
            "план.pptx",
            pptx(&[
                (
                    "План внедрения",
                    "Система учёта ремонтов: пилот в цехе насосов",
                ),
                (
                    "Сроки",
                    "График внедрения: пилот в марте, весь завод к сентябрю",
                ),
                ("Риски", "Нет резервных копий базы учёта до перехода"),
            ]),
        ),
    ];
    for (file, data) in &files {
        upload(a, &a.token, &owner, file, data).await;
    }
    let mut total = 0;
    for (file, _) in &files {
        let d = wait_done(a, user, &owner, file).await;
        total += chunk_locators(a, user, d.id).await.len() as u64;
    }
    if w.vector {
        wait_points(a, user, total).await;
        let db = common::attach::db(a, user).await;
        common::ml::until(
            "заметки проиндексированы",
            std::time::Duration::from_secs(240),
            async || {
                for id in &ids {
                    let n = zam_store::get_node(&db, id.parse().unwrap()).await.unwrap();
                    if n.index_state != "indexed" {
                        return false;
                    }
                }
                true
            },
        )
        .await;
    }
    w
}

/// Выдача запросов мира: по запросу — вид, путь, файл, `locator`, каналы и балл каждой карточки
/// (смешанная выдача и одни вложения).
async fn w_attach_answers(a: &Api) -> Value {
    let card = |i: &Value| {
        json!({
            "kind": i["kind"], "path": i["path"], "file": i["file"], "locator": i["locator"],
            "sources": i["sources"], "score": i["score"],
        })
    };
    let mut out = Vec::new();
    for q in W_QUERIES {
        let body = json!({"query": q, "top_k": 10, "budget_tokens": 100000});
        let mixed = search(a, &a.token, body.clone()).await;
        let mut only = body;
        only["filters"] = json!({"kinds": ["attachment"]});
        let only = search(a, &a.token, only).await;
        out.push(json!({
            "query": q,
            "mixed": items(&mixed).iter().map(card).collect::<Vec<_>>(),
            "attach": items(&only).iter().map(card).collect::<Vec<_>>(),
        }));
    }
    json!(out)
}

fn w_fixture(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Снимает эталоны [`W_ATTACH_FIXTURE_FTS`] и [`W_ATTACH_FIXTURE_HYBRID`] (код до задачи 11).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "снимает эталон; запускать только на коде до правки M6 §11 (w_attach)"]
async fn snapshot_w_attach_fixture() {
    let w = w_attach_world("watt-snap-fts", false, |_| {}).await;
    let v = json!({"platform": std::env::consts::OS, "answers": w_attach_answers(&w.a).await});
    std::fs::write(
        w_fixture(W_ATTACH_FIXTURE_FTS),
        serde_json::to_string_pretty(&v).unwrap() + "\n",
    )
    .unwrap();
    w.a.st.shutdown().await;
    let w = w_attach_world("watt-snap-hyb", true, |_| {}).await;
    assert!(w.vector, "эталон гибрида — с MiniLM и Qdrant");
    let v = json!({"platform": std::env::consts::OS, "answers": w_attach_answers(&w.a).await});
    std::fs::write(
        w_fixture(W_ATTACH_FIXTURE_HYBRID),
        serde_json::to_string_pretty(&v).unwrap() + "\n",
    )
    .unwrap();
    w.a.st.shutdown().await;
}
