//! Поиск по вложениям (M5, задача 6): карточка `a:<chunk>` с файлом и `locator`, каналы FTS и
//! векторный (листовой prefetch уровня `attachment`), квота `search.max_per_attachment`, фильтр
//! видов, наследование `area` и `certainty` от заметки-владельца, права участника общего решения
//! и Human.
//!
//! Векторная часть — с MiniLM и Qdrant (`common::ml::have`; в CI с `ZAM_REQUIRE_ML=1` —
//! обязательны), без них тесты идут только по FTS. PDF — с pdfium из `target/deps`.

mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, api};
use common::attach::{
    chunk_locators, docx, pptx, upload, upload_adm, wait_done, wait_points, xlsx,
};
use common::ml::{MINILM, external, have, ml, vector_ready};
use common::qdrant::TestQdrant;
use serde_json::{Value, json};
use zam_docs::test_pdf::{Page, build, pdfium_path};
use zam_store::attachments::Locator;

/// Сервер: векторный канал — если есть MiniLM и Qdrant, pdfium — если найден.
struct World {
    a: Api,
    _q: Option<TestQdrant>,
    vector: bool,
    pdf: bool,
}

async fn world(name: &str, vector: bool) -> World {
    let vector = vector && have(MINILM);
    let q = vector.then(|| TestQdrant::start(name));
    let pdfium = pdfium_path(env!("CARGO_MANIFEST_DIR"));
    let pdf = pdfium.is_some();
    let a = api(name, |c| {
        if let Some(q) = &q {
            ml(c, MINILM);
            external(c, q);
        }
        c.docs.pdfium_dylib = pdfium;
    })
    .await;
    if vector {
        vector_ready(&a).await;
    }
    World {
        a,
        _q: q,
        vector,
        pdf,
    }
}

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

async fn search(a: &Api, token: &str, body: Value) -> Value {
    let r = a
        .tok(token, Method::POST, "/api/v1/search", Some(body))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Поиск по cookie администратора (Human — с `include_human`).
async fn search_adm(a: &Api, body: Value) -> Value {
    let r = a
        .adm(Method::POST, "/api/v1/search", Some(body), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

fn items(j: &Value) -> &Vec<Value> {
    j["items"].as_array().unwrap_or_else(|| panic!("{j}"))
}

/// Карточки вложений выдачи.
fn atts(j: &Value) -> Vec<&Value> {
    items(j)
        .iter()
        .filter(|i| i["kind"] == "attachment")
        .collect()
}

/// Карточки заметок выдачи.
fn notes(j: &Value) -> Vec<&Value> {
    items(j).iter().filter(|i| i["kind"] == "note").collect()
}

fn files(j: &Value) -> Vec<String> {
    let mut v: Vec<String> = atts(j)
        .iter()
        .map(|i| i["file"].as_str().unwrap().to_string())
        .collect();
    v.sort();
    v
}

fn sources(i: &Value) -> Vec<&str> {
    i["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect()
}

fn filler(n: usize) -> String {
    "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor. ".repeat(n)
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
        let same = atts(&j).iter().filter(|i| i["file"] == *file).count();
        assert_eq!(same, 1, "{file}: {j}");
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
