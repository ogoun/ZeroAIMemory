//! Конвейер индексации вложений (M5, задача 5): извлечение в рабочем процессе
//! `zam-server doc-worker`, кэш `blob_extract`, нарезка, чанки FTS, точки Qdrant, Human, сканер
//! секретов, отпечаток нарезки вложений, неудачи в `/index/status`.
//!
//! Тесты без модели (FTS) идут всегда; с моделью и Qdrant — только если они скачаны
//! (`common::ml::have`, в CI с `ZAM_REQUIRE_ML=1` — обязательны). PDF — с pdfium из
//! `target/deps` (`cargo xtask fetch-deps`), без него тест PDF пропускается.

mod common;

use std::io::Write as _;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::api::{Api, api, api_with, vector_base};
use common::ml::{MINILM, external, have, ml, until, vector_ready};
use common::qdrant::TestQdrant;
use serde_json::{Value, json};
use zam_core::Id;
use zam_docs::test_pdf::{Page, build, pdfium_path};
use zam_index::VectorFilter;
use zam_server::config::ServerConfig;
use zam_store::{AttachmentDoc, ExtractState, SpaceKind, list_attachments, list_spaces};

const BOUNDARY: &str = "zam-attach-boundary-5c2e";
const LIMIT: Duration = Duration::from_secs(60);

fn multipart(node: &str, file: &str, data: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(
        format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"node\"\r\n\r\n{node}\r\n")
            .as_bytes(),
    );
    b.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file}\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    b.extend_from_slice(data);
    b.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    b
}

/// Загружает файл к заметке `node` токеном `token`; ответ `POST /blobs`.
async fn upload(a: &Api, token: &str, node: &str, file: &str, data: &[u8]) -> Value {
    let r = Request::post("/api/v1/blobs").header(header::AUTHORIZATION, format!("Bearer {token}"));
    send_upload(a, r, node, file, data).await
}

/// Загрузка администратором по cookie (Human токену недоступен).
async fn upload_adm(a: &Api, node: &str, file: &str, data: &[u8]) -> Value {
    let r = Request::post("/api/v1/blobs")
        .header(header::COOKIE, &a.cookie)
        .header("x-csrf-token", &a.csrf);
    send_upload(a, r, node, file, data).await
}

async fn send_upload(
    a: &Api,
    r: axum::http::request::Builder,
    node: &str,
    file: &str,
    data: &[u8],
) -> Value {
    let r = r
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(multipart(node, file, data)))
        .unwrap();
    let r = a.send(r).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()
}

/// Заметка администратором по cookie.
async fn note_adm(a: &Api, space: &str, path: &str, content: &str) -> String {
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": space, "path": path, "kind": "note", "content": content})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

/// Заметка токеном `token` в пространстве `space`.
async fn note_as(a: &Api, token: &str, space: &str, path: &str, content: &str) -> String {
    let r = a
        .tok(
            token,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": space, "path": path, "kind": "note", "content": content})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

async fn db(a: &Api, user: Id) -> zam_store::Db {
    a.st.users.get(user).await.unwrap()
}

async fn docs(a: &Api, user: Id, node: &str) -> Vec<AttachmentDoc> {
    list_attachments(&db(a, user).await, node.parse().unwrap())
        .await
        .unwrap()
}

/// Ждёт, пока документ `file` заметки `node` не перейдёт в `want`; возвращает документ.
async fn wait_doc(a: &Api, user: Id, node: &str, file: &str, want: ExtractState) -> AttachmentDoc {
    until(&format!("{file}: {want:?}"), LIMIT, async || {
        docs(a, user, node)
            .await
            .iter()
            .any(|d| d.filename == file && d.extract_state == want)
    })
    .await;
    docs(a, user, node)
        .await
        .into_iter()
        .find(|d| d.filename == file)
        .unwrap()
}

/// Чанки документа: `(level, locator, heading_path, text, context_prefix)` по порядку.
async fn chunks(
    a: &Api,
    user: Id,
    att: Id,
) -> Vec<(String, Option<String>, String, String, String)> {
    db(a, user)
        .await
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT level, locator, heading_path, text, context_prefix FROM chunks
                 WHERE attachment_id = ?1 ORDER BY ordinal",
            )?;
            let rows = st.query_map([att.to_string()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap()
}

/// Число чанков документа `att`, найденных FTS по слову `word`.
async fn fts(a: &Api, user: Id, att: Id, word: &str) -> i64 {
    let word = word.to_string();
    db(a, user)
        .await
        .read(move |c| {
            Ok(c.query_row(
                "SELECT count(*) FROM chunks_fts f JOIN chunks c ON c.seq = f.rowid
                 WHERE chunks_fts MATCH ?1 AND c.attachment_id = ?2",
                [word, att.to_string()],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap()
}

/// Выполняет SQL в базе пользователя (подготовка состояний, которые API не создаёт).
async fn exec(a: &Api, user: Id, sql: &'static str) {
    db(a, user)
        .await
        .write(move |t| {
            t.execute_batch(sql)?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn scalar(a: &Api, user: Id, sql: &'static str) -> i64 {
    db(a, user)
        .await
        .read(move |c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .await
        .unwrap()
}

async fn status(a: &Api) -> Value {
    let r = a.adm(Method::GET, "/api/v1/index/status", None, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Точки вложений (`kind = attachment`) пользователя, пространства `space` (`None` — все).
async fn att_points(a: &Api, user: Id, space: Option<Id>) -> u64 {
    let base = match space {
        Some(s) => VectorFilter::scope(user, vec![s]),
        None => vector_base(a, user).await,
    };
    let f = VectorFilter {
        kinds: vec!["attachment".into()],
        ..base
    };
    a.st.index.vector.as_ref().unwrap().count(&f).await.unwrap()
}

/// Точки самой заметки (уровни заметок, без вложений).
async fn note_points(a: &Api, user: Id, path: &str) -> u64 {
    let f = VectorFilter {
        path_prefix: Some(path.into()),
        levels: ["chunk", "section", "note"].map(String::from).to_vec(),
        kinds: vec!["note".into()],
        ..vector_base(a, user).await
    };
    a.st.index.vector.as_ref().unwrap().count(&f).await.unwrap()
}

async fn space_id(a: &Api, user: Id, kind: SpaceKind) -> Id {
    list_spaces(&db(a, user).await)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == kind)
        .unwrap()
        .id
}

/// Заданий извлечения в очереди (документ `pending`). Без модели задания точек остаются в
/// очереди до появления векторного индекса — как у заметок.
async fn extract_jobs(a: &Api, user: Id) -> i64 {
    scalar(
        a,
        user,
        "SELECT count(*) FROM index_jobs j JOIN attachment_docs d ON d.id = j.target_id
         WHERE j.target_kind = 'attachment' AND j.state IN ('queued', 'running')
           AND d.extract_state = 'pending'",
    )
    .await
}

/// Заданий цели вложения в очереди (`queued` | `running`).
async fn active_jobs(a: &Api, user: Id) -> i64 {
    scalar(
        a,
        user,
        "SELECT count(*) FROM index_jobs
         WHERE target_kind = 'attachment' AND state IN ('queued', 'running')",
    )
    .await
}

fn csv(rows: &[&str]) -> Vec<u8> {
    let mut s = String::from("Имя;Описание\n");
    for r in rows {
        s.push_str(r);
        s.push('\n');
    }
    s.into_bytes()
}

/// DOCX из `document.xml` с телом `body` (минимальный пакет OOXML).
fn docx(document: &str) -> Vec<u8> {
    let ct = r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default();
    for (name, data) in [
        ("[Content_Types].xml", ct),
        ("_rels/.rels", rels),
        ("word/document.xml", document),
    ] {
        w.start_file(name, o).unwrap();
        w.write_all(data.as_bytes()).unwrap();
    }
    w.finish().unwrap().into_inner()
}

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;

fn docx_text(paras: &[&str]) -> Vec<u8> {
    let body: String = paras
        .iter()
        .map(|p| format!("<w:p><w:r><w:t>{p}</w:t></w:r></w:p>"))
        .collect();
    docx(&format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><w:document {W}><w:body>{body}</w:body></w:document>"#
    ))
}

/// Конфигурация с моделью и внешним Qdrant теста.
fn with_vector(q: &TestQdrant) -> impl FnOnce(&mut ServerConfig) + '_ {
    move |c| {
        ml(c, MINILM);
        external(c, q);
    }
}

/// CSV-вложение заметки Knowledge: извлечение, чанки `level = attachment` с `locator` и
/// префиксом `[<space>/<путь>] <файл>`, FTS по тексту, точки `kind = attachment` в Qdrant;
/// reindex из кэша и полный возвращают точки вложения.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attachment_indexed_fts_and_vector() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("att-fts-vec");
    let a = api("att-fts-vec", with_vector(&q)).await;
    vector_ready(&a).await;
    let user = a.st.admin_id;
    let node = note_as(
        &a,
        &a.token,
        "knowledge",
        "ops/отчёт",
        "# Отчёт\n\nКратко.\n",
    )
    .await;
    let data = csv(&[
        "Альфа;гиперкуб вращается медленно",
        "Бета;квазикристалл растёт",
    ]);
    upload(&a, &a.token, &node, "итоги.csv", &data).await;
    let d = wait_doc(&a, user, &node, "итоги.csv", ExtractState::Done).await;
    assert!(d.extract_error.is_none(), "{d:?}");
    let ch = chunks(&a, user, d.id).await;
    assert!(!ch.is_empty());
    for (level, locator, _, _, prefix) in &ch {
        assert_eq!(level, "attachment");
        assert!(
            locator.as_deref().is_some_and(|l| l.starts_with("sheet:")),
            "{locator:?}"
        );
        assert!(
            prefix.contains("/ops/отчёт] итоги.csv"),
            "префикс вложения: {prefix}"
        );
    }
    assert!(fts(&a, user, d.id, "гиперкуб").await >= 1);
    assert!(fts(&a, user, d.id, "квазикристалл").await >= 1);
    until("точки вложения", LIMIT, async || {
        att_points(&a, user, None).await == ch.len() as u64
    })
    .await;
    until(
        "задания вложения закрыты",
        LIMIT,
        async || active_jobs(&a, user).await == 0,
    )
    .await;
    // reindex из кэша удаляет точки области и возвращает их — вложения тоже
    for full in [false, true] {
        let r = a
            .adm(
                Method::POST,
                "/api/v1/admin/reindex",
                Some(json!({ "full": full })),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
        until(&format!("reindex full={full}"), LIMIT, async || {
            let s = status(&a).await;
            s["reindex"]["running"] == false
                && s["reindex"]["queued"] == 0
                && s["reindex"]["last"]["full"] == full
                && active_jobs(&a, user).await == 0
                && att_points(&a, user, None).await == ch.len() as u64
        })
        .await;
    }
    a.st.shutdown().await;
}

/// PDF разбирается в рабочем процессе (pdfium): чанк на страницу, `locator = page:N`, FTS по
/// тексту страниц; причина недоступности pdfium в `/index/status` пуста.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pdf_attachment_via_worker() {
    let Some(pdfium) = pdfium_path(env!("CARGO_MANIFEST_DIR")) else {
        eprintln!("pdfium не найден (cargo xtask fetch-deps) — тест PDF пропущен");
        return;
    };
    let a = api("att-pdf", |c| c.docs.pdfium_dylib = Some(pdfium)).await;
    let user = a.st.admin_id;
    let node = note_as(&a, &a.token, "knowledge", "lib/книга", "# Книга\n").await;
    let pdf = build(&[
        Page::default()
            .line(72.0, 700.0, "Zeppelin harbour schedule")
            .line(72.0, 680.0, "first page body"),
        Page::default().line(72.0, 700.0, "Quasar telemetry appendix"),
    ]);
    upload(&a, &a.token, &node, "book.pdf", &pdf).await;
    let d = wait_doc(&a, user, &node, "book.pdf", ExtractState::Done).await;
    let ch = chunks(&a, user, d.id).await;
    let locs: Vec<&str> = ch.iter().filter_map(|c| c.1.as_deref()).collect();
    assert_eq!(locs, ["page:1", "page:2"], "{ch:?}");
    assert_eq!(fts(&a, user, d.id, "zeppelin").await, 1);
    assert_eq!(fts(&a, user, d.id, "quasar").await, 1);
    assert_eq!(a.st.docs.extracted(), 1);
    let s = status(&a).await;
    assert!(s["docs"]["pdfium_error"].is_null(), "{}", s["docs"]);
    a.st.shutdown().await;
}

/// Review Focus 3: один файл в двух заметках — извлечение один раз (кэш `blob_extract`), чанки и
/// документы — у каждого вложения свои; у другого пользователя — своё извлечение в его базе, его
/// чанки не видны в базе первого и наоборот.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extract_cache_shared_by_sha() {
    let a = api("att-cache", |_| {}).await;
    let alice = a.st.admin_id;
    let data = docx_text(&["Общий файл про тахион и мезон."]);
    let n1 = note_as(&a, &a.token, "knowledge", "a/один", "# Один\n").await;
    let n2 = note_as(&a, &a.token, "knowledge", "b/два", "# Два\n").await;
    upload(&a, &a.token, &n1, "общий.docx", &data).await;
    let d1 = wait_doc(&a, alice, &n1, "общий.docx", ExtractState::Done).await;
    upload(&a, &a.token, &n2, "копия.docx", &data).await;
    let d2 = wait_doc(&a, alice, &n2, "копия.docx", ExtractState::Done).await;
    assert_ne!(d1.id, d2.id);
    assert_eq!(d1.blob_sha256, d2.blob_sha256);
    assert_eq!(a.st.docs.extracted(), 1, "второе вложение — из кэша");
    let (c1, c2) = (
        chunks(&a, alice, d1.id).await,
        chunks(&a, alice, d2.id).await,
    );
    assert_eq!(c1.len(), c2.len());
    assert!(c1[0].4.contains("/a/один] общий.docx"), "{}", c1[0].4);
    assert!(c2[0].4.contains("/b/два] копия.docx"), "{}", c2[0].4);
    assert_eq!(
        scalar(&a, alice, "SELECT count(*) FROM blob_extract").await,
        1
    );

    let (bob, bob_token) = a.new_user("bob").await;
    let bob: Id = bob.parse().unwrap();
    let nb = note_as(&a, &bob_token, "knowledge", "c/три", "# Три\n").await;
    upload(&a, &bob_token, &nb, "чужой.docx", &data).await;
    let db_ = wait_doc(&a, bob, &nb, "чужой.docx", ExtractState::Done).await;
    assert_eq!(
        a.st.docs.extracted(),
        2,
        "у другого пользователя — своя база и свой кэш"
    );
    assert_eq!(fts(&a, bob, db_.id, "тахион").await, 1);
    // чужие документы в базе не появляются
    assert_eq!(
        scalar(&a, bob, "SELECT count(*) FROM attachment_docs").await,
        1
    );
    assert_eq!(fts(&a, bob, d1.id, "тахион").await, 0);
    assert_eq!(fts(&a, alice, db_.id, "тахион").await, 0);
    a.st.shutdown().await;
}

/// Human — только FTS: чанки вложения пишутся, точек нет и задание ради точек не ставится;
/// вложение Knowledge рядом получает точки (векторный путь работает).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn human_attachment_fts_only() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("att-human");
    let a = api("att-human", with_vector(&q)).await;
    vector_ready(&a).await;
    let user = a.st.admin_id;
    let h = note_adm(&a, "human", "дневник", "# Дневник\n").await;
    let k = note_as(&a, &a.token, "knowledge", "открыто", "# Открыто\n").await;
    upload_adm(&a, &h, "личное.csv", &csv(&["Пароль;фотонный двигатель"])).await;
    upload(
        &a,
        &a.token,
        &k,
        "общее.csv",
        &csv(&["Тема;нейтринный детектор"]),
    )
    .await;
    let dh = wait_doc(&a, user, &h, "личное.csv", ExtractState::Done).await;
    let dk = wait_doc(&a, user, &k, "общее.csv", ExtractState::Done).await;
    assert_eq!(fts(&a, user, dh.id, "фотонный").await, 1);
    let human = space_id(&a, user, SpaceKind::Human).await;
    let knowledge = space_id(&a, user, SpaceKind::Knowledge).await;
    until("точки Knowledge-вложения", LIMIT, async || {
        att_points(&a, user, Some(knowledge)).await == chunks(&a, user, dk.id).await.len() as u64
            && active_jobs(&a, user).await == 0
    })
    .await;
    assert_eq!(att_points(&a, user, Some(human)).await, 0);
    a.st.shutdown().await;
}

/// Сканер секретов (§7.6) — к тексту вложения и его префиксу: значение токена в чанке и в
/// имени файла заменено меткой.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn secrets_masked_in_attachment_chunks() {
    let a = api("att-secrets", |_| {}).await;
    let user = a.st.admin_id;
    let secret = format!("gh{}_{}", "p", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1eF3hJ5");
    let node = note_as(&a, &a.token, "knowledge", "ops/доступ", "# Доступ\n").await;
    let data = docx_text(&[&format!("Ключ деплоя: {secret} для сервера сборки.")]);
    let file = format!("ключ {secret}.docx");
    upload(&a, &a.token, &node, &file, &data).await;
    let d = wait_doc(&a, user, &node, &file, ExtractState::Done).await;
    let ch = chunks(&a, user, d.id).await;
    assert!(!ch.is_empty());
    for (_, _, heading, text, prefix) in &ch {
        for s in [heading, text, prefix] {
            assert!(!s.contains(&secret), "секрет в чанке: {s}");
        }
        assert!(text.contains(zam_markdown::SECRET_MARK), "{text}");
        assert!(prefix.contains(zam_markdown::SECRET_MARK), "{prefix}");
    }
    assert!(fts(&a, user, d.id, "деплоя").await >= 1);
    a.st.shutdown().await;
}

/// Правка текста заметки переиндексирует только её точки: точки вложения остаются, извлечения
/// нет; удаление заметки убирает точки вложения, восстановление возвращает.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn node_upsert_keeps_attachment_points() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("att-keep");
    let a = api("att-keep", with_vector(&q)).await;
    vector_ready(&a).await;
    let user = a.st.admin_id;
    let node = note_as(
        &a,
        &a.token,
        "knowledge",
        "keep/n",
        "# Заметка\n\nПервый текст.\n",
    )
    .await;
    upload(
        &a,
        &a.token,
        &node,
        "data.csv",
        &csv(&["К;ортогональный базис"]),
    )
    .await;
    let d = wait_doc(&a, user, &node, "data.csv", ExtractState::Done).await;
    let n = chunks(&a, user, d.id).await.len() as u64;
    until(
        "точки заметки и вложения",
        LIMIT,
        async || {
            att_points(&a, user, None).await == n
                && note_points(&a, user, "keep/n").await > 0
                && active_jobs(&a, user).await == 0
        },
    )
    .await;
    let parsed = a.st.docs.extracted();
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{node}"),
            Some(json!({"content": "# Заметка\n\nСовсем другой текст про выпуск релиза.\n"})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    until(
        "правка проиндексирована",
        LIMIT,
        async || {
            let db = db(&a, user).await;
            zam_store::get_node(&db, node.parse().unwrap())
                .await
                .unwrap()
                .index_state
                == "indexed"
        },
    )
    .await;
    assert_eq!(
        att_points(&a, user, None).await,
        n,
        "точки вложения на месте"
    );
    assert_eq!(
        a.st.docs.extracted(),
        parsed,
        "правка заметки не извлекает вложение"
    );

    let r = a
        .t(Method::DELETE, &format!("/api/v1/nodes/{node}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    until(
        "точки вложения удалены",
        LIMIT,
        async || att_points(&a, user, None).await == 0,
    )
    .await;
    let r = a
        .t(Method::POST, &format!("/api/v1/nodes/{node}/restore"), None)
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    until(
        "точки вложения вернулись",
        LIMIT,
        async || att_points(&a, user, None).await == n,
    )
    .await;
    assert_eq!(
        a.st.docs.extracted(),
        parsed,
        "восстановление — без извлечения"
    );
    a.st.shutdown().await;
}

/// Отпечаток нарезки вложений (`meta.chunker_attachments`, включает `EXTRACTOR_VERSION`): при
/// расхождении документы перенарезаются при открытии базы. Другие настройки нарезки — из кэша
/// извлечения без разбора; другая версия извлечения — кэш не подходит, файл разбирается заново.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extractor_version_bump_rechunks() {
    let c = common::cfg_in(common::tmp("att-rechunk"));
    let a = api_with(c.clone()).await;
    let user = a.st.admin_id;
    let node = note_as(&a, &a.token, "knowledge", "v/n", "# N\n").await;
    upload(
        &a,
        &a.token,
        &node,
        "v.docx",
        &docx_text(&["Версия извлечения и пульсар."]),
    )
    .await;
    let d = wait_doc(&a, user, &node, "v.docx", ExtractState::Done).await;
    let n = chunks(&a, user, d.id).await.len();
    let fp = scalar(
        &a,
        user,
        "SELECT count(*) FROM meta WHERE key = 'chunker_attachments'
                AND value LIKE '%;ext=' || 1 || ';%'",
    )
    .await;
    assert_eq!(fp, 1, "отпечаток с версией извлечения записан");
    // настройки нарезки «другие»: перенарезка из кэша (чанки стёрты — их вернёт перенарезка)
    exec(
        &a,
        user,
        "UPDATE meta SET value = 'v0;1/2/3/4/5;' || value WHERE key = 'chunker_attachments';
         DELETE FROM chunks WHERE attachment_id IS NOT NULL;",
    )
    .await;
    a.st.shutdown().await;
    drop(a);

    let b = api_with(c.clone()).await;
    until("перенарезка из кэша", LIMIT, async || {
        chunks(&b, user, d.id).await.len() == n && extract_jobs(&b, user).await == 0
    })
    .await;
    assert_eq!(b.st.docs.extracted(), 0, "кэш подходит: разбора нет");
    assert_eq!(
        docs(&b, user, &node).await[0].extract_state,
        ExtractState::Done
    );
    // версия извлечения «старая»: кэш той версии не годится, файл разбирается заново
    exec(
        &b,
        user,
        "UPDATE meta SET value = replace(value, ';ext=1;', ';ext=0;')
          WHERE key = 'chunker_attachments';
         UPDATE blob_extract SET extractor_version = 0;",
    )
    .await;
    b.st.shutdown().await;
    drop(b);

    let c2 = api_with(c).await;
    until(
        "перенарезка с разбором",
        LIMIT,
        async || {
            scalar(
                &c2,
                user,
                "SELECT count(*) FROM blob_extract WHERE extractor_version = 1",
            )
            .await
                == 1
                && extract_jobs(&c2, user).await == 0
                && docs(&c2, user, &node).await[0].extract_state == ExtractState::Done
        },
    )
    .await;
    assert_eq!(c2.st.docs.extracted(), 1);
    assert_eq!(chunks(&c2, user, d.id).await.len(), n);
    c2.st.shutdown().await;
}

/// Неудачи разбора: PDF без pdfium — `failed` с причиной `pdfium_unavailable`, причина
/// pdfium — в `/index/status` (`docs.pdfium_error`); DOCX с DTD — `parse_error`. Обе — в
/// `failed_items` с `attachment_id` и `reason`. Неподдерживаемый файл — `unsupported`, не
/// неудача.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_extract_reported_in_index_status() {
    let missing = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("no-such-dir")
        .join("pdfium-missing.dll");
    let a = api("att-failed", |c| c.docs.pdfium_dylib = Some(missing)).await;
    let user = a.st.admin_id;
    let node = note_as(&a, &a.token, "knowledge", "f/n", "# N\n").await;
    let pdf = build(&[Page::default().line(72.0, 700.0, "never read")]);
    upload(&a, &a.token, &node, "scan.pdf", &pdf).await;
    let bad = docx(&format!(
        r#"<?xml version="1.0"?><!DOCTYPE x [<!ENTITY e "x">]><w:document {W}><w:body/></w:document>"#
    ));
    upload(&a, &a.token, &node, "broken.docx", &bad).await;
    upload(
        &a,
        &a.token,
        &node,
        "photo.png",
        b"\x89PNG\r\n\x1a\n0000000000",
    )
    .await;
    let p = wait_doc(&a, user, &node, "scan.pdf", ExtractState::Failed).await;
    let x = wait_doc(&a, user, &node, "broken.docx", ExtractState::Failed).await;
    let png = wait_doc(&a, user, &node, "photo.png", ExtractState::Unsupported).await;
    assert!(
        p.extract_error
            .as_deref()
            .is_some_and(|e| e.starts_with("pdfium_unavailable")),
        "{p:?}"
    );
    assert!(
        x.extract_error
            .as_deref()
            .is_some_and(|e| e.starts_with("parse_error")),
        "{x:?}"
    );
    assert!(png.extract_error.is_none());
    assert!(chunks(&a, user, p.id).await.is_empty());
    let s = status(&a).await;
    assert!(
        s["docs"]["pdfium_error"]
            .as_str()
            .is_some_and(|e| e.contains("pdfium")),
        "{}",
        s["docs"]
    );
    let items = s["failed_items"].as_array().unwrap();
    let reason = |id: Id| {
        items
            .iter()
            .find(|i| i["attachment_id"] == id.to_string())
            .map(|i| i["reason"].as_str().unwrap_or_default().to_string())
    };
    assert_eq!(reason(p.id).as_deref(), Some("pdfium_unavailable"), "{s}");
    assert_eq!(reason(x.id).as_deref(), Some("parse_error"), "{s}");
    assert_eq!(reason(png.id), None, "неподдерживаемый вид — не неудача");
    assert!(
        items
            .iter()
            .all(|i| i["path"] == "f/n" || i["attachment_id"].is_null())
    );
    a.st.shutdown().await;
}

/// Документы `pending` без задания (дозаполнение миграции 0019, задания, закрытые воркером до
/// задачи 5) получают задание при запуске конвейера; вид — по содержимому: PNG под именем
/// `.pdf` — `unsupported`, DOCX без расширения — извлекается, файл больше `docs.max_bytes` —
/// `failed`/`too_large`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pending_without_job_sniffed_by_content() {
    let c = common::cfg_in(common::tmp("att-backfill"));
    let a = api_with(c.clone()).await;
    let user = a.st.admin_id;
    let node = note_as(&a, &a.token, "knowledge", "b/n", "# N\n").await;
    upload(
        &a,
        &a.token,
        &node,
        "fake.pdf",
        b"\x89PNG\r\n\x1a\nxxxxxxxxxxxx",
    )
    .await;
    upload(
        &a,
        &a.token,
        &node,
        "report",
        &docx_text(&["Отчёт про магнетар."]),
    )
    .await;
    let big = csv(&[&"строка;большой файл ".repeat(200)]);
    upload(&a, &a.token, &node, "big.csv", &big).await;
    for f in ["report", "big.csv"] {
        wait_doc(&a, user, &node, f, ExtractState::Done).await;
    }
    until(
        "извлечение закончено",
        LIMIT,
        async || extract_jobs(&a, user).await == 0,
    )
    .await;
    // состояние базы M4.5 после миграции 0019: все `pending`, заданий и чанков нет
    exec(
        &a,
        user,
        "UPDATE attachment_docs SET extract_state = 'pending', extract_error = NULL;
         DELETE FROM chunks WHERE attachment_id IS NOT NULL;
         DELETE FROM blob_extract;
         DELETE FROM index_jobs WHERE target_kind = 'attachment';",
    )
    .await;
    a.st.shutdown().await;
    drop(a);

    let mut c = c;
    c.docs.max_bytes = 1000;
    let b = api_with(c).await;
    wait_doc(&b, user, &node, "fake.pdf", ExtractState::Unsupported).await;
    let r = wait_doc(&b, user, &node, "report", ExtractState::Done).await;
    let big = wait_doc(&b, user, &node, "big.csv", ExtractState::Failed).await;
    assert_eq!(big.extract_error.as_deref(), Some("too_large"));
    assert_eq!(fts(&b, user, r.id, "магнетар").await, 1);
    until(
        "извлечение закончено",
        LIMIT,
        async || extract_jobs(&b, user).await == 0,
    )
    .await;
    b.st.shutdown().await;
}

/// Reindex по запросу администратора и ожидание его итога.
async fn reindex(a: &Api, full: bool) {
    let r = a
        .adm(
            Method::POST,
            "/api/v1/admin/reindex",
            Some(json!({ "full": full })),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    until(&format!("reindex full={full}"), LIMIT, async || {
        let s = status(a).await;
        s["reindex"]["running"] == false
            && s["reindex"]["queued"] == 0
            && s["reindex"]["last"]["full"] == full
    })
    .await;
}

/// Задания вложений: срок `not_before` (`far` — отложить, иначе — сейчас).
async fn delay_attachment_jobs(a: &Api, user: Id, far: bool) {
    let sql = if far {
        "UPDATE index_jobs SET not_before = '2999-01-01T00:00:00.000000Z'
         WHERE target_kind = 'attachment' AND state = 'queued'"
    } else {
        "UPDATE index_jobs SET not_before = '2000-01-01T00:00:00.000000Z'
         WHERE target_kind = 'attachment' AND state = 'queued'"
    };
    exec(a, user, sql).await;
}

/// Ревью задачи 5, I-1: документ снова `pending` с прежними чанками (перенарезка при открытии
/// базы), пока он ждёт извлечения, его точки удаляются — reindex из кэша (страница — только
/// `done`) или удаление узла; повторное извлечение даёт ту же нарезку и всё равно ставит задание
/// точек: точки возвращаются.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn points_restored_after_pending_with_same_chunks() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("att-same-chunks");
    let a = api("att-same-chunks", with_vector(&q)).await;
    vector_ready(&a).await;
    let user = a.st.admin_id;
    let node = note_as(&a, &a.token, "knowledge", "same/n", "# N\n").await;
    upload(&a, &a.token, &node, "t.csv", &csv(&["альфа;бета гамма"])).await;
    let d = wait_doc(&a, user, &node, "t.csv", ExtractState::Done).await;
    let n = chunks(&a, user, d.id).await.len() as u64;
    until("точки", LIMIT, async || {
        att_points(&a, user, None).await == n && active_jobs(&a, user).await == 0
    })
    .await;
    let dbh = db(&a, user).await;

    // 1) перенарезка → reindex из кэша, пока документ `pending` → извлечение
    exec(
        &a,
        user,
        "UPDATE meta SET value = 'bogus' WHERE key = 'chunker_attachments'",
    )
    .await;
    assert_eq!(zam_store::rechunk_docs_if_stale(&dbh).await.unwrap(), 1);
    delay_attachment_jobs(&a, user, true).await;
    reindex(&a, false).await;
    assert_eq!(
        att_points(&a, user, None).await,
        0,
        "pending: reindex точки не вернул"
    );
    delay_attachment_jobs(&a, user, false).await;
    a.st.docs.notify();
    until(
        "точки после извлечения той же нарезки",
        LIMIT,
        async || att_points(&a, user, None).await == n && active_jobs(&a, user).await == 0,
    )
    .await;

    // 2) перенарезка → удаление узла и восстановление, пока документ `pending` → извлечение
    exec(
        &a,
        user,
        "UPDATE meta SET value = 'bogus' WHERE key = 'chunker_attachments'",
    )
    .await;
    assert_eq!(zam_store::rechunk_docs_if_stale(&dbh).await.unwrap(), 1);
    delay_attachment_jobs(&a, user, true).await;
    let r = a
        .t(Method::DELETE, &format!("/api/v1/nodes/{node}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    until("точки удалены", LIMIT, async || {
        att_points(&a, user, None).await == 0
    })
    .await;
    let r = a
        .t(Method::POST, &format!("/api/v1/nodes/{node}/restore"), None)
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    delay_attachment_jobs(&a, user, false).await;
    a.st.docs.notify();
    until(
        "точки после восстановления",
        LIMIT,
        async || {
            att_points(&a, user, None).await == n
                && active_jobs(&a, user).await == 0
                && docs(&a, user, &node).await[0].extract_state == ExtractState::Done
        },
    )
    .await;
    assert_eq!(chunks(&a, user, d.id).await.len() as u64, n);
    a.st.shutdown().await;
}

/// Ревью задачи 5, M-3 и M-4: путь установки сервера не виден токену — секция `docs`
/// `/index/status` только администратору, `extract_error` и `failed_items` — код без пути;
/// причина `parse_error` — код и область (часть), без имени сущности документа.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failure_reasons_without_paths_or_document_text() {
    let bogus = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("секрет-путь")
        .join("pdfium.dll");
    let a = api("att-reasons", move |c| c.docs.pdfium_dylib = Some(bogus)).await;
    let user = a.st.admin_id;
    let node = note_as(&a, &a.token, "knowledge", "r/n", "# N\n").await;
    let pdf = build(&[Page::default().line(72.0, 700.0, "hello")]);
    upload(&a, &a.token, &node, "x.pdf", &pdf).await;
    let ent = docx(&format!(
        r#"<?xml version="1.0"?><w:document {W}><w:body><w:p><w:r><w:t>&тайнаимени;</w:t></w:r></w:p></w:body></w:document>"#
    ));
    upload(&a, &a.token, &node, "ent.docx", &ent).await;
    let p = wait_doc(&a, user, &node, "x.pdf", ExtractState::Failed).await;
    let e = wait_doc(&a, user, &node, "ent.docx", ExtractState::Failed).await;
    assert_eq!(p.extract_error.as_deref(), Some("pdfium_unavailable"));
    assert_eq!(
        e.extract_error.as_deref(),
        Some("parse_error: xml (word/document.xml)")
    );
    // токен: секции `docs` нет, путей нет нигде
    let s = a.t(Method::GET, "/api/v1/index/status", None).await;
    assert_eq!(s.status, StatusCode::OK);
    let text = s.text();
    assert!(s.json()["docs"].is_null(), "{}", s.json()["docs"]);
    assert!(
        !text.contains("секрет-путь") && !text.contains("тайнаимени"),
        "{text}"
    );
    let l = a
        .t(
            Method::GET,
            &format!("/api/v1/nodes/{node}/attachments"),
            None,
        )
        .await;
    assert!(!l.text().contains("секрет-путь"), "{}", l.text());
    // администратор: причина pdfium с путём — в `docs.pdfium_error`
    let s = status(&a).await;
    assert!(
        s["docs"]["pdfium_error"]
            .as_str()
            .is_some_and(|e| e.contains("секрет-путь")),
        "{}",
        s["docs"]
    );
    let items = s["failed_items"].as_array().unwrap();
    assert!(
        items.iter().all(|i| !i.to_string().contains("секрет-путь")),
        "{items:?}"
    );
    a.st.shutdown().await;
}

/// Ревью задачи 5, M-5: падающий файл в N заметках разбирается один раз (неудача `parse_error`
/// кэшируется по sha и версии извлечения); у каждого документа своя строка `failed_items`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failing_file_parsed_once_for_many_attachments() {
    let a = api("att-fail-once", |_| {}).await;
    let user = a.st.admin_id;
    let bad = docx(&format!(
        r#"<?xml version="1.0"?><!DOCTYPE x [<!ENTITY e "x">]><w:document {W}><w:body/></w:document>"#
    ));
    let mut nodes = Vec::new();
    for i in 0..4 {
        let node = note_as(&a, &a.token, "knowledge", &format!("f/n{i}"), "# N\n").await;
        upload(&a, &a.token, &node, "bad.docx", &bad).await;
        nodes.push(node);
    }
    for node in &nodes {
        let d = wait_doc(&a, user, node, "bad.docx", ExtractState::Failed).await;
        assert_eq!(
            d.extract_error.as_deref(),
            Some("parse_error: xml (word/document.xml)")
        );
    }
    assert_eq!(
        a.st.docs.extracted(),
        1,
        "разбор один, остальные — из кэша неудачи"
    );
    let s = status(&a).await;
    let failed = s["failed_items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["reason"] == "parse_error")
        .count();
    assert_eq!(failed, 4, "{}", s["failed_items"]);
    a.st.shutdown().await;
}
