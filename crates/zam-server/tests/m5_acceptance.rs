//! Приёмка M5 «Документы» (спецификация этапа §7, задача 12): сервер целиком — от HTTP до
//! рабочего процесса `zam-server doc-worker`, базы и Qdrant.
//!
//! - `documents_indexed_searchable_readable` — заметка с DOCX, XLSX, PDF и PPTX → извлечение →
//!   поиск находит фрагмент каждого формата с верным `locator` → `/read` по `locator` отдаёт
//!   текст страницы, листа, слайда, абзаца и только его;
//! - `hostile_files_contained` — враждебные файлы (zip-бомба, XXE, «миллиард смешков», глубокая
//!   вложенность, битые и обрезанные PDF/DOCX, PDF с JavaScript, поток глифов, неверное
//!   расширение) — допустимый исход каждого (`failed` с причиной или частичный результат), без
//!   падения, срока и нехватки памяти рабочего процесса под лимитом 1 ГиБ; сервер работает дальше;
//! - `worker_killed_attachment_failed_server_alive` — рабочий процесс убит посреди разбора:
//!   вложение `failed`, следующий файл разбирает новый процесс, повтор (reindex) — `done`;
//! - `xlsx_preview_under_1s` — первый экран листа XLSX 10 тыс. строк × 3 листа (ТЗ §17);
//! - `human_attachment_not_in_qdrant` — Human-вложение только в FTS: ни точек, ни байтов в
//!   хранилище Qdrant, токену не выдаётся.
//!
//! Подробные матрицы — `attachments_index.rs`, `attachments_search.rs`, `read_attachments.rs`,
//! `previews.rs`, `doc_worker.rs`; здесь — сквозные сценарии. Враждебные файлы — те же виды,
//! что в наборе `eval/docs/hostile/` (`xtask docs-corpus`, его допустимые исходы — `ACCEPT` в
//! `xtask/src/docs_corpus/hostile.rs`), в меньших размерах: генератор набора — в бинарнике
//! xtask, тестам сервера недоступен. Без MiniLM и Qdrant векторные проверки пропускаются, без
//! pdfium — PDF; в CI (`ZAM_REQUIRE_ML=1`) — падают.
mod common;

use std::io::Write as _;
use std::process::Command;
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::api::Api;
use common::attach::{
    LIMIT, atts, big_sheet, chunk_locators, db, docx, docx_raw, files, filler, fts_card, pdfium,
    pptx, read1, search, search_adm, sources, text, upload, upload_adm, wait_done, wait_points,
    world, world_with, xlsx, xlsx_owned, zip_of,
};
use common::ml::until;
use serde_json::{Value, json};
use zam_core::Id;
use zam_docs::test_pdf::{Page, assemble, build};
use zam_store::{AttachmentDoc, ExtractState, list_attachments};

/// Заметка токеном `token`.
async fn note(a: &Api, token: &str, space: &str, path: &str) -> String {
    let r = a
        .tok(
            token,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": space, "path": path, "kind": "note",
                        "content": "# Документы\n\nЗаметка с вложениями.\n"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

/// Заметка администратором по cookie (Human токену недоступен).
async fn note_adm(a: &Api, space: &str, path: &str) -> String {
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": space, "path": path, "kind": "note",
                        "content": "# Личное\n\nЗаметка.\n"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

async fn docs_of(a: &Api, user: Id, node: &str) -> Vec<AttachmentDoc> {
    list_attachments(&db(a, user).await, node.parse().unwrap())
        .await
        .unwrap()
}

/// Ждёт, пока у всех документов заметки не кончится извлечение (не `pending`).
async fn wait_settled(a: &Api, user: Id, node: &str, n: usize) -> Vec<AttachmentDoc> {
    until(
        &format!("{n} документов извлечены"),
        LIMIT,
        async || {
            let d = docs_of(a, user, node).await;
            d.len() == n && d.iter().all(|d| d.extract_state != ExtractState::Pending)
        },
    )
    .await;
    docs_of(a, user, node).await
}

/// Предупреждения извлечения файла `sha` (кэш `blob_extract`; API их не отдаёт).
async fn warnings(a: &Api, user: Id, sha: &str) -> Vec<String> {
    let sha = sha.to_string();
    let w: Option<String> = db(a, user)
        .await
        .read(move |c| {
            Ok(c.query_row(
                "SELECT warnings FROM blob_extract WHERE blob_sha256 = ?1
                 ORDER BY extractor_version DESC LIMIT 1",
                [sha],
                |r| r.get(0),
            )
            .ok())
        })
        .await
        .unwrap();
    w.map(|w| serde_json::from_str(&w).unwrap())
        .unwrap_or_default()
}

async fn index_status(a: &Api) -> Value {
    let r = a.adm(Method::GET, "/api/v1/index/status", None, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Сервер отвечает: список заметок пространства и статус индекса.
async fn alive(a: &Api) {
    let r = a.t(Method::GET, "/api/v1/spaces", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    index_status(a).await;
}

// ---- 1. Индексация, поиск, чтение ----

/// Формат приёмки: файл, байты, запрос (редкие английские слова — тестовая модель англоязычная,
/// FTS находит их однозначно), ожидаемый `locator` (`None` — абзац DOCX: проверяется отдельно),
/// текст факта и текст другой единицы того же файла (`/read` по `locator` его не отдаёт).
struct Case {
    file: &'static str,
    data: Vec<u8>,
    query: &'static str,
    locator: Option<&'static str>,
    fact: &'static str,
    other: &'static str,
}

/// DOCX, XLSX (факт на втором листе), PDF (на второй странице) и PPTX (на втором слайде) одной
/// заметки: поиск по умолчанию (`filters.kinds` с `attachment`) находит фрагмент каждого файла
/// карточкой с каналом FTS (и вектором, если он есть), `id = a:<chunk>`, `locator` чанка и его
/// подписью; `/read` по `locator` карточки — текст факта без другой единицы файла; оглавление
/// документа (`b:` без `locator`) перечисляет единицы.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn documents_indexed_searchable_readable() {
    let w = world("m5acc-docs", true).await;
    let a = &w.a;
    let user = a.st.admin_id;
    let tok = a.token.clone();
    let node = note(a, &tok, "knowledge", "приёмка/документы").await;
    // абзацы по ~400 символов: единицы DOCX — блоки по мягкому пределу, факт не в первом
    let mut paras: Vec<String> = (0..6).map(|_| filler(5)).collect();
    paras.push("Molybdenum heron cartography appendix.".into());
    let paras: Vec<&str> = paras.iter().map(String::as_str).collect();
    let mut cases = vec![
        Case {
            file: "отчёт.docx",
            data: docx(&paras),
            query: "molybdenum heron",
            locator: None,
            fact: "Molybdenum heron cartography",
            other: "never-in-docx",
        },
        Case {
            file: "реестр.xlsx",
            data: xlsx(&[
                (
                    "Сводка",
                    &[&["Name", "Note"], &["Gamma", "plain summary row"]],
                ),
                (
                    "Склад",
                    &[&["Item", "Place"], &["Lantern", "tantalum otter warehouse"]],
                ),
            ]),
            query: "tantalum otter",
            // блок строк данных с повтором шапки: диапазон — от первой строки данных
            locator: Some("sheet:Склад!A2:B2"),
            fact: "tantalum otter warehouse",
            other: "plain summary row",
        },
        Case {
            file: "доклад.pptx",
            data: pptx(&[
                ("Opening", "welcome words only"),
                ("Findings", "hafnium badger expedition"),
            ]),
            query: "hafnium badger",
            locator: Some("slide:2"),
            fact: "hafnium badger expedition",
            other: "welcome words only",
        },
    ];
    if pdfium().is_some() {
        cases.push(Case {
            file: "книга.pdf",
            data: build(&[
                Page::default().line(72.0, 700.0, "introductory page text"),
                Page::default().line(72.0, 700.0, "Iridium mongoose observatory"),
            ]),
            query: "iridium mongoose",
            locator: Some("page:2"),
            fact: "Iridium mongoose observatory",
            other: "introductory page text",
        });
    }
    for c in &cases {
        upload(a, &tok, &node, c.file, &c.data).await;
    }
    let mut total = 0;
    let mut docs = Vec::new();
    for c in &cases {
        let d = wait_done(a, user, &node, c.file).await;
        total += chunk_locators(a, user, d.id).await.len() as u64;
        docs.push(d);
    }
    if w.vector {
        wait_points(a, user, total).await;
    }
    for (c, d) in cases.iter().zip(&docs) {
        let j = search(
            a,
            &tok,
            json!({"query": c.query, "top_k": 5, "budget_tokens": 4000}),
        )
        .await;
        assert_eq!(j["degraded"], !w.vector, "{}: {j}", c.file);
        let it = fts_card(&j, c.file);
        assert_eq!(it["node_id"], node.as_str(), "{it}");
        assert_eq!(it["attachment_id"], d.id.to_string(), "{it}");
        assert_eq!(it["blob_sha256"], d.blob_sha256.as_str(), "{it}");
        assert_eq!(it["level"], "attachment", "{it}");
        let chunk = it["chunk_ids"][0].as_str().unwrap();
        assert_eq!(it["id"], format!("a:{chunk}"), "{it}");
        if w.vector {
            assert!(sources(it).contains(&"vector"), "{}: {it}", c.file);
        }
        let loc = it["locator"].as_str().unwrap_or_else(|| panic!("{it}"));
        let db_loc = chunk_locators(a, user, d.id)
            .await
            .into_iter()
            .find(|(id, _)| id == chunk)
            .and_then(|(_, l)| l)
            .unwrap();
        assert_eq!(loc, db_loc, "{}: locator карточки — locator чанка", c.file);
        match c.locator {
            Some(want) => assert_eq!(loc, want, "{}", c.file),
            // блок абзацев с номером первого: факт (абзац 7) — не в первом блоке
            None => {
                let n: u32 = loc.strip_prefix("para:").unwrap().parse().unwrap();
                assert!((2..=7).contains(&n), "{}: {loc}", c.file);
            }
        }
        assert!(it["locator_text"].as_str().is_some_and(|s| !s.is_empty()));
        // `/read` по `locator` карточки — текст единицы с фактом, без другой единицы файла
        let r = read1(a, &tok, json!({"id": it["id"], "locator": loc})).await;
        assert!(r.get("error").is_none(), "{r}");
        assert_eq!(r["locator"], loc, "{r}");
        assert_eq!(r["file"], c.file, "{r}");
        assert!(text(&r).contains(c.fact), "{}: {r}", c.file);
        assert!(!text(&r).contains(c.other), "{}: {r}", c.file);
    }
    // оглавление документа без `locator`: единицы и подсказка
    let pp = docs.iter().find(|d| d.filename == "доклад.pptx").unwrap();
    let toc = read1(a, &tok, json!({"id": format!("b:{}", pp.blob_sha256)})).await;
    let t = toc.to_string();
    assert!(t.contains("slide:1") && t.contains("slide:2"), "{toc}");
    a.st.shutdown().await;
}

// ---- 2. Враждебные файлы ----

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// zip-бомба: `word/document.xml` из `mib` МиБ пробелов внутри `w:t` (сжатие ≈ 1000:1).
fn zip_bomb(mib: usize) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default();
    let ct = br#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    w.start_file("[Content_Types].xml", o).unwrap();
    w.write_all(ct).unwrap();
    w.start_file("word/document.xml", o).unwrap();
    w.write_all(format!(r#"<w:document xmlns:w="{W_NS}"><w:body><w:p><w:r><w:t>"#).as_bytes())
        .unwrap();
    let block = vec![b' '; 1 << 20];
    for _ in 0..mib {
        w.write_all(&block).unwrap();
    }
    w.write_all(b"</w:t></w:r></w:p></w:body></w:document>")
        .unwrap();
    w.finish().unwrap().into_inner()
}

/// «Миллиард смешков» в общих строках XLSX (DTD с вложенными сущностями).
fn billion_laughs() -> Vec<u8> {
    let mut lol = String::from(r#"<?xml version="1.0"?><!DOCTYPE sst [<!ENTITY lol "lol">"#);
    for i in 1..=9 {
        let prev = if i == 1 {
            "lol".to_string()
        } else {
            format!("lol{}", i - 1)
        };
        lol.push_str(&format!(
            r#"<!ENTITY lol{i} "{}">"#,
            format!("&{prev};").repeat(10)
        ));
    }
    lol.push_str(r#"]><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><si><t>&lol9;</t></si></sst>"#);
    let sml = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let pkg = "http://schemas.openxmlformats.org/package/2006/relationships";
    let parts: [(&str, String); 6] = [
        (
            "[Content_Types].xml",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#.into(),
        ),
        (
            "_rels/.rels",
            format!(r#"<Relationships xmlns="{pkg}"><Relationship Id="rId1" Type="{rel}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#),
        ),
        (
            "xl/workbook.xml",
            format!(r#"<workbook xmlns="{sml}" xmlns:r="{rel}"><sheets><sheet name="L" sheetId="1" r:id="rId1"/></sheets></workbook>"#),
        ),
        (
            "xl/_rels/workbook.xml.rels",
            format!(r#"<Relationships xmlns="{pkg}"><Relationship Id="rId1" Type="{rel}/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="{rel}/sharedStrings" Target="sharedStrings.xml"/></Relationships>"#),
        ),
        (
            "xl/worksheets/sheet1.xml",
            format!(r#"<worksheet xmlns="{sml}"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c></row></sheetData></worksheet>"#),
        ),
        ("xl/sharedStrings.xml", lol),
    ];
    let parts: Vec<(&str, Vec<u8>)> = parts
        .into_iter()
        .map(|(n, d)| (n, d.into_bytes()))
        .collect();
    zip_of(&parts)
}

/// PDF с одной страницей Helvetica: `extra` — в словарь страницы, `content` — поток содержимого,
/// `catalog` — в словарь каталога; `more` — объекты с номера 6.
fn raw_pdf(catalog: &str, extra: &str, content: &str, more: &[&str]) -> Vec<u8> {
    let mut objs: Vec<Vec<u8>> = vec![
        format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>").into_bytes(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R {extra} >>"
        )
        .into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        )
        .into_bytes(),
    ];
    objs.extend(more.iter().map(|m| m.as_bytes().to_vec()));
    assemble(&objs)
}

/// Враждебный файл и допустимые исходы: `done` (с предупреждением `warn`, если задано) или
/// `failed` с причиной, начинающейся с `fail`.
struct Hostile {
    file: &'static str,
    data: Vec<u8>,
    done: Option<Option<&'static str>>,
    fail: Option<&'static str>,
}

/// Враждебные файлы набора (спецификация M5 §6, Review Focus 1) к одной заметке: у каждого —
/// допустимый исход, ни один не роняет рабочий процесс, не упирается в срок и не превышает
/// лимит памяти (1 ГиБ вместо 2 ГиБ по умолчанию: `parse_oom` был бы провалом); неудачи — в
/// `failed_items` с причиной; после них сервер разбирает обычный файл, и тот находится поиском.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_files_contained() {
    let w = world_with("m5acc-hostile", false, |c| c.docs.worker_memory_mb = 1024).await;
    let a = &w.a;
    let user = a.st.admin_id;
    let tok = a.token.clone();
    let node = note(a, &tok, "knowledge", "приёмка/враждебные").await;
    let xxe = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE w:document [<!ENTITY xxe SYSTEM "file:///C:/Windows/win.ini">]><w:document xmlns:w="{W_NS}"><w:body><w:p><w:r><w:t>&xxe;</w:t></w:r></w:p></w:body></w:document>"#
    );
    let mut deep =
        format!(r#"<w:document xmlns:w="{W_NS}"><w:body><w:p><w:r><w:t>start</w:t></w:r></w:p>"#);
    deep.push_str(&"<w:sdt><w:sdtContent>".repeat(2_000));
    deep.push_str("<w:p><w:r><w:t>deep</w:t></w:r></w:p>");
    deep.push_str(&"</w:sdtContent></w:sdt>".repeat(2_000));
    deep.push_str("</w:body></w:document>");
    let ok_docx = docx(&["Ordinary text."]);
    let mut set = vec![
        Hostile {
            file: "zip-bomb.docx",
            data: zip_bomb(64),
            done: Some(Some("truncated: max_ratio")),
            fail: Some("parse_error"),
        },
        Hostile {
            file: "xxe.docx",
            data: docx_raw(xxe.as_bytes()),
            done: None,
            fail: Some("parse_error: xml"),
        },
        Hostile {
            file: "billion-laughs.xlsx",
            data: billion_laughs(),
            done: None,
            fail: Some("parse_error: xml"),
        },
        Hostile {
            file: "deep-nesting.docx",
            data: docx_raw(deep.as_bytes()),
            done: Some(Some("truncated: max_xml_depth")),
            fail: Some("parse_error"),
        },
        Hostile {
            file: "truncated.docx",
            data: ok_docx[..ok_docx.len() / 2].to_vec(),
            done: None,
            fail: Some("parse_error: zip"),
        },
    ];
    if pdfium().is_some() {
        let mut junk = b"%PDF-1.7\n".to_vec();
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        junk.extend((0..64 * 1024).map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x & 0xff) as u8
        }));
        let normal = build(&[
            Page::default().line(72.0, 700.0, "Truncated document first page"),
            Page::default().line(72.0, 700.0, "Truncated document second page"),
        ]);
        let n = 2_000;
        let nested = format!("{}0{}", "[".repeat(n), "]".repeat(n));
        let mut glyphs = String::from("BT /F1 4 Tf 10 830 Td 5 TL\n");
        for _ in 0..1_500 {
            glyphs.push_str(&format!("({}) '\n", "x".repeat(200)));
        }
        glyphs.push_str("ET");
        set.extend([
            Hostile {
                file: "broken.pdf",
                data: junk,
                done: None,
                fail: Some("parse_error: pdf"),
            },
            Hostile {
                file: "truncated.pdf",
                data: normal[..normal.len() * 6 / 10].to_vec(),
                done: Some(None),
                fail: Some("parse_error: pdf"),
            },
            Hostile {
                file: "js-action.pdf",
                data: raw_pdf(
                    "/OpenAction 6 0 R",
                    "",
                    "BT /F1 12 Tf 72 700 Td (Scandium kestrel ledger) Tj ET",
                    &["<< /S /JavaScript /JS (app.alert('zam'); \
                       this.submitForm('http://127.0.0.1:9/exfil'); while(true){}) >>"],
                ),
                done: Some(None),
                fail: None,
            },
            Hostile {
                file: "deep-array.pdf",
                data: raw_pdf(
                    "",
                    &format!("/Junk {nested}"),
                    "BT /F1 12 Tf 72 700 Td (deep array page) Tj ET",
                    &[],
                ),
                done: Some(None),
                fail: Some("parse_error"),
            },
            Hostile {
                file: "glyph-flood.pdf",
                data: raw_pdf("", "", &glyphs, &[]),
                done: Some(Some("truncated: page_chars")),
                fail: None,
            },
            // неверное расширение: PDF под именем DOCX — вид по сигнатуре
            Hostile {
                file: "disguised.docx",
                data: build(&[Page::default().line(72.0, 700.0, "Disguised portable text")]),
                done: Some(None),
                fail: None,
            },
        ]);
    }
    for h in &set {
        upload(a, &tok, &node, h.file, &h.data).await;
    }
    // картинка под именем PDF — не документ (`unsupported`), не неудача
    upload(a, &tok, &node, "fake.pdf", b"\x89PNG\r\n\x1a\n0000000000").await;
    let got = wait_settled(a, user, &node, set.len() + 1).await;
    let mut report = Vec::new();
    for h in &set {
        let d = got.iter().find(|d| d.filename == h.file).unwrap();
        let warns = warnings(a, user, &d.blob_sha256).await;
        let ok = match d.extract_state {
            ExtractState::Done => h
                .done
                .is_some_and(|w| w.is_none_or(|w| warns.iter().any(|x| x.starts_with(w)))),
            ExtractState::Failed => h
                .fail
                .is_some_and(|f| d.extract_error.as_deref().is_some_and(|e| e.starts_with(f))),
            _ => false,
        };
        report.push(format!(
            "{}: {:?} {:?} {warns:?}",
            h.file, d.extract_state, d.extract_error
        ));
        assert!(ok, "{}: недопустимый исход; все: {report:#?}", h.file);
        // срок, падение и нехватка памяти рабочего процесса — не исход враждебного файла
        assert!(
            !d.extract_error.as_deref().is_some_and(|e| {
                ["parse_crash", "parse_timeout", "parse_oom"]
                    .iter()
                    .any(|r| e.starts_with(r))
            }),
            "{}: {:?}",
            h.file,
            d.extract_error
        );
    }
    eprintln!("враждебные файлы: {report:#?}");
    let fake = got.iter().find(|d| d.filename == "fake.pdf").unwrap();
    assert_eq!(fake.extract_state, ExtractState::Unsupported, "{fake:?}");
    // неудачи — в `failed_items` с причиной
    let s = index_status(a).await;
    let items = s["failed_items"].as_array().unwrap();
    for d in got
        .iter()
        .filter(|d| d.extract_state == ExtractState::Failed)
    {
        let it = items
            .iter()
            .find(|i| i["attachment_id"] == d.id.to_string())
            .unwrap_or_else(|| panic!("{} нет в failed_items: {s}", d.filename));
        assert!(
            d.extract_error
                .as_deref()
                .is_some_and(|e| e.starts_with(it["reason"].as_str().unwrap())),
            "{it}"
        );
    }
    // PDF с JavaScript: текст извлечён, действие открытия не исполнялось (разбор не завис)
    if pdfium().is_some() {
        let j = search(a, &tok, json!({"query": "scandium kestrel", "top_k": 5})).await;
        assert_eq!(fts_card(&j, "js-action.pdf")["locator"], "page:1", "{j}");
    }
    // сервер работает: обычный файл после враждебных разбирается и находится
    upload(
        a,
        &tok,
        &node,
        "after.docx",
        &docx(&["Ruthenium pelican checkpoint."]),
    )
    .await;
    wait_done(a, user, &node, "after.docx").await;
    let j = search(a, &tok, json!({"query": "ruthenium pelican", "top_k": 5})).await;
    assert_eq!(files(&j), ["after.docx"], "{j}");
    alive(a).await;
    a.st.shutdown().await;
}

// ---- 3. Убийство рабочего процесса ----

/// Жёстко завершает процесс (как сбой, OOM-killer или `kill -9` администратора).
fn kill_pid(pid: u32) {
    let st = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output()
            .map(|o| o.status)
    } else {
        Command::new("kill").args(["-9", &pid.to_string()]).status()
    };
    assert!(
        st.is_ok_and(|s| s.success()),
        "не удалось завершить pid {pid}"
    );
}

/// CSV на `rows` строк по ~70 символов (разбор в debug — секунды).
fn big_csv(rows: usize) -> Vec<u8> {
    let mut s = String::from("Номер;Описание;Сумма\n");
    for i in 0..rows {
        s.push_str(&format!(
            "{i};строка реестра поставок номер {i} без особых примет;{}\n",
            i * 7
        ));
    }
    s.into_bytes()
}

/// Рабочий процесс разбора убит посреди разбора большого файла: документ `failed`
/// (`parse_crash`; на Linux внешний SIGKILL пул принимает за OOM-killer ядра — `parse_oom`),
/// процесс не остаётся, сервер работает; следующий файл разбирает новый процесс; повтор разбора
/// (`reindex_docs` — то, что делает reindex) убитого файла — `done`, он находится поиском.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_killed_attachment_failed_server_alive() {
    let w = world_with("m5acc-kill", false, |c| c.docs.workers = 1).await;
    let a = &w.a;
    let user = a.st.admin_id;
    let tok = a.token.clone();
    let node = note(a, &tok, "knowledge", "приёмка/падение").await;
    let pool = a.st.docs.pool();
    assert!(
        pool.worker_pids().iter().all(|p| *p == 0),
        "процесс заранее"
    );
    // ~3,5 МБ: разбор и нарезка в debug — сотни миллисекунд и дольше, а убийство — через
    // миллисекунды после запуска процесса
    let mut csv = big_csv(50_000);
    csv.extend_from_slice("итог;zirconium walrus ledger;0\n".as_bytes());
    upload(a, &tok, &node, "реестр.csv", &csv).await;
    // процесс запускается по заданию: первый pid — разбор этого файла
    let t = Instant::now();
    let pid = loop {
        if let Some(p) = pool.worker_pids().into_iter().find(|p| *p != 0) {
            break p;
        }
        assert!(t.elapsed() < LIMIT, "рабочий процесс не запустился");
        tokio::time::sleep(Duration::from_millis(2)).await;
    };
    kill_pid(pid);
    until("документ failed", LIMIT, async || {
        docs_of(a, user, &node).await[0].extract_state == ExtractState::Failed
    })
    .await;
    let d = docs_of(a, user, &node).await.remove(0);
    let want = if cfg!(windows) {
        "parse_crash"
    } else {
        "parse_oom"
    };
    assert_eq!(d.extract_error.as_deref(), Some(want), "{d:?}");
    assert!(!zam_index::supervisor::pid_alive(pid), "убитый процесс жив");
    assert_eq!(pool.spawned(), 1);
    let s = index_status(a).await;
    assert!(
        s["failed_items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["attachment_id"] == d.id.to_string() && i["reason"] == want),
        "{s}"
    );
    // сервер работает: следующий файл — новым процессом
    alive(a).await;
    upload(
        a,
        &tok,
        &node,
        "после.docx",
        &docx(&["Cerium heron afterwards."]),
    )
    .await;
    wait_done(a, user, &node, "после.docx").await;
    assert_eq!(pool.spawned(), 2, "новый процесс после убитого");
    // повтор: неудача снимается, файл разбирается заново
    zam_store::reindex_docs(&db(a, user).await, None)
        .await
        .unwrap();
    a.st.docs.notify();
    let d = wait_done(a, user, &node, "реестр.csv").await;
    assert!(d.extract_error.is_none(), "{d:?}");
    let j = search(a, &tok, json!({"query": "zirconium walrus", "top_k": 5})).await;
    assert!(
        fts_card(&j, "реестр.csv")["locator"]
            .as_str()
            .is_some_and(|l| l.starts_with("sheet:")),
        "{j}"
    );
    a.st.shutdown().await;
}

// ---- 4. Превью XLSX ----

/// ТЗ §17: первый экран листа XLSX на 10 000 строк (книга из трёх листов) — не дольше 1 с
/// (холодный кэш, первый запуск рабочего процесса превью); переключение на другой лист и окно
/// в конце листа — тоже. Порог 1 с — для release; в debug — 15 с (разбор без оптимизаций в разы
/// медленнее; ловит только патологию), как `previews::preview_sheet_10k_first_screen_under_1s`.
/// Замер в release: `cargo test -p zam-server --release --test m5_acceptance -- xlsx_preview
/// --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn xlsx_preview_under_1s() {
    let w = world("m5acc-preview", false).await;
    let a = &w.a;
    let tok = a.token.clone();
    let node = note(a, &tok, "knowledge", "приёмка/книга").await;
    let rows = big_sheet(10_000, 8);
    let book = xlsx_owned(&[("Январь", &rows), ("Февраль", &rows), ("Март", &rows)]);
    let up = upload(a, &tok, &node, "книга.xlsx", &book).await;
    let sha = up["sha256"].as_str().unwrap_or_else(|| panic!("{up}"));
    let limit = if cfg!(debug_assertions) {
        Duration::from_secs(15)
    } else {
        Duration::from_secs(1)
    };
    let mut times = Vec::new();
    for (q, sheet, first) in [
        ("kind=sheet&sheet=1&rows=100", "Январь", "20"),
        ("kind=sheet&sheet=2&rows=100", "Февраль", "20"),
        ("kind=sheet&sheet=3&from=9900&rows=100", "Март", "99000"),
    ] {
        let t = Instant::now();
        let r = a
            .tok(
                &tok,
                Method::GET,
                &format!("/api/v1/previews/{sha}?{q}"),
                None,
            )
            .await;
        let took = t.elapsed();
        assert_eq!(r.status, StatusCode::OK, "{q}: {}", r.text());
        let p = r.json();
        assert_eq!(p["total_rows"], 10_000, "{q}");
        assert_eq!(p["rows"].as_array().unwrap().len(), 100, "{q}");
        assert_eq!(p["rows"][0][0], first, "{q}: {}", p["rows"][0]);
        assert!(p.to_string().contains(sheet), "{q}: имя листа");
        times.push((q, took));
        assert!(took <= limit, "{q}: {took:?} > {limit:?}");
    }
    println!("превью XLSX 10k × 8 × 3 листа: {times:?}");
    a.st.shutdown().await;
}

// ---- 5. Human ----

/// Human-вложения (DOCX, CSV, PDF) — только в FTS: точек Qdrant с их блобом или узлом нет,
/// байтов маркера и sha256 их файлов в хранилище Qdrant нет; рядом вложение Knowledge получает
/// точки (векторный путь работает — проверка не пустая). Администратор с `include_human`
/// находит Human-вложение каналом FTS, токен — нет.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn human_attachment_not_in_qdrant() {
    let w = world("m5acc-human", true).await;
    if !w.vector {
        return;
    }
    let a = &w.a;
    let q = w.q.as_ref().unwrap();
    let user = a.st.admin_id;
    let marker = format!("{}{}", "osmiumquokka", "diary");
    let human = note_adm(a, "human", "дневник/вложения").await;
    let open = note(a, &a.token, "knowledge", "приёмка/открыто").await;
    let mut hfiles: Vec<(&str, Vec<u8>)> = vec![
        ("личное.docx", docx(&[&format!("Private {marker} entry.")])),
        (
            "личное.csv",
            format!("Тема;Текст\nзаметка;{marker} secret row\n").into_bytes(),
        ),
    ];
    if pdfium().is_some() {
        hfiles.push((
            "личное.pdf",
            build(&[Page::default().line(72.0, 700.0, &format!("Private {marker} page"))]),
        ));
    }
    for (f, data) in &hfiles {
        upload_adm(a, &human, f, data).await;
    }
    upload(
        a,
        &a.token,
        &open,
        "общее.docx",
        &docx(&["Public niobium falcon note."]),
    )
    .await;
    let mut hdocs = Vec::new();
    for (f, _) in &hfiles {
        hdocs.push(wait_done(a, user, &human, f).await);
    }
    let k = wait_done(a, user, &open, "общее.docx").await;
    let kn = chunk_locators(a, user, k.id).await.len() as u64;
    wait_points(a, user, kn).await;
    // все задания вложений закрыты: Human-точкам больше неоткуда взяться
    let dbh = db(a, user).await;
    until(
        "задания вложений закрыты",
        LIMIT,
        async || {
            dbh.read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM index_jobs
                 WHERE target_kind = 'attachment' AND state IN ('queued', 'running')",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap()
                == 0
        },
    )
    .await;
    let points: Vec<Value> = q
        .points()
        .into_iter()
        .filter(|p| p["payload"]["kind"] == "attachment")
        .collect();
    assert_eq!(points.len() as u64, kn, "точки только Knowledge-вложения");
    for p in &points {
        assert_eq!(p["payload"]["blob_sha256"], k.blob_sha256.as_str(), "{p}");
        assert_ne!(p["payload"]["node_id"], human.as_str(), "{p}");
    }
    for d in &hdocs {
        assert!(
            !points
                .iter()
                .any(|p| p["payload"]["attachment_id"] == d.id.to_string()),
            "{}",
            d.filename
        );
        let hits = common::files_containing(&q.dir, d.blob_sha256.as_bytes());
        assert!(hits.is_empty(), "{}: sha в Qdrant: {hits:?}", d.filename);
    }
    let hits = common::files_containing(&q.dir, marker.as_bytes());
    assert!(hits.is_empty(), "маркер в Qdrant: {hits:?}");
    // FTS: администратор с include_human находит, токен — нет
    let body = json!({"query": marker, "top_k": 10, "scope": {"include_human": true}});
    // (вектором рядом может прийти Knowledge-вложение: близкие косинусы малого корпуса)
    let j = search_adm(a, body.clone()).await;
    let found: Vec<&Value> = atts(&j)
        .into_iter()
        .filter(|i| i["node_id"] == human.as_str())
        .collect();
    let mut got: Vec<&str> = found.iter().map(|i| i["file"].as_str().unwrap()).collect();
    got.sort();
    let mut want: Vec<&str> = hfiles.iter().map(|(f, _)| *f).collect();
    want.sort();
    assert_eq!(got, want, "все Human-вложения каналом FTS: {j}");
    for it in &found {
        assert_eq!(sources(it), ["fts"], "Human — только FTS: {it}");
    }
    let j = search(a, &a.token, body).await;
    assert!(
        atts(&j).iter().all(|i| i["node_id"] != human.as_str()),
        "Human токену: {j}"
    );
    a.st.shutdown().await;
}
