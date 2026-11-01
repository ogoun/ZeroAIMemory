//! Вложения в интеграционных тестах (M5): загрузка `POST /blobs`, ожидание извлечения, сборка
//! документов DOCX, XLSX и PPTX в памяти (минимальные пакеты OOXML).

use std::io::Write as _;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::Value;
use zam_core::Id;
use zam_store::{AttachmentDoc, ExtractState, list_attachments};

use super::api::Api;
use super::ml::until;

const BOUNDARY: &str = "zam-attach-boundary-7d41";
/// Предел ожидания извлечения.
pub const LIMIT: Duration = Duration::from_secs(90);

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
pub async fn upload(a: &Api, token: &str, node: &str, file: &str, data: &[u8]) -> Value {
    let r = Request::post("/api/v1/blobs")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
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

/// Загрузка администратором по cookie (Human токену недоступен).
pub async fn upload_adm(a: &Api, node: &str, file: &str, data: &[u8]) -> Value {
    let r = Request::post("/api/v1/blobs")
        .header(header::COOKIE, &a.cookie)
        .header("x-csrf-token", &a.csrf)
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

pub async fn db(a: &Api, user: Id) -> zam_store::Db {
    a.st.users.get(user).await.unwrap()
}

/// Ждёт, пока документ `file` заметки `node` пользователя `user` не станет `done`.
pub async fn wait_done(a: &Api, user: Id, node: &str, file: &str) -> AttachmentDoc {
    let node: Id = node.parse().unwrap();
    let docs = async || list_attachments(&db(a, user).await, node).await.unwrap();
    until(&format!("{file}: done"), LIMIT, async || {
        docs()
            .await
            .iter()
            .any(|d| d.filename == file && d.extract_state == ExtractState::Done)
    })
    .await;
    docs()
        .await
        .into_iter()
        .find(|d| d.filename == file)
        .unwrap()
}

/// `chunk_id → locator` чанков документа `att`.
pub async fn chunk_locators(a: &Api, user: Id, att: Id) -> Vec<(String, Option<String>)> {
    db(a, user)
        .await
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT id, locator FROM chunks WHERE attachment_id = ?1 ORDER BY ordinal",
            )?;
            let rows = st.query_map([att.to_string()], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap()
}

/// Ждёт точек вложений (`kind = attachment`) пользователя `user` в Qdrant: не меньше `n`.
pub async fn wait_points(a: &Api, user: Id, n: u64) {
    let base = super::api::vector_base(a, user).await;
    let f = zam_index::VectorFilter {
        kinds: vec!["attachment".into()],
        ..base
    };
    let v = a.st.index.vector.clone().unwrap();
    until("точки вложений в Qdrant", LIMIT, async || {
        v.count(&f).await.unwrap_or(0) >= n
    })
    .await;
}

fn zip_of(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default();
    for (name, data) in entries {
        w.start_file(*name, o).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PKG_REL: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn rels(items: &[(String, &str, String)]) -> Vec<u8> {
    let mut s =
        format!(r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="{PKG_REL}">"#);
    for (id, ty, target) in items {
        s.push_str(&format!(
            r#"<Relationship Id="{id}" Type="{REL}/{ty}" Target="{target}"/>"#
        ));
    }
    s.push_str("</Relationships>");
    s.into_bytes()
}

fn content_types(main: &str, ct: &str) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="{main}" ContentType="{ct}"/></Types>"#
    )
    .into_bytes()
}

/// DOCX из абзацев без стилей.
pub fn docx(paras: &[&str]) -> Vec<u8> {
    let w = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;
    let body: String = paras
        .iter()
        .map(|p| format!("<w:p><w:r><w:t>{}</w:t></w:r></w:p>", esc(p)))
        .collect();
    zip_of(&[
        (
            "[Content_Types].xml",
            content_types(
                "/word/document.xml",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
            ),
        ),
        (
            "_rels/.rels",
            rels(&[(
                "rId1".into(),
                "officeDocument",
                "word/document.xml".into(),
            )]),
        ),
        (
            "word/document.xml",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><w:document {w}><w:body>{body}</w:body></w:document>"#
            )
            .into_bytes(),
        ),
    ])
}

/// XLSX с листами `(имя, строки)`; ячейки — встроенные строки.
pub fn xlsx(sheets: &[(&str, &[&[&str]])]) -> Vec<u8> {
    let sml = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    let mut ct = String::from(
        r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>"#,
    );
    let mut wb = format!(r#"<workbook xmlns="{sml}" xmlns:r="{REL}"><sheets>"#);
    let mut wb_rels = Vec::new();
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    for (i, (name, rows)) in sheets.iter().enumerate() {
        let n = i + 1;
        wb.push_str(&format!(
            r#"<sheet name="{}" sheetId="{n}" r:id="rId{n}"/>"#,
            esc(name).replace('"', "&quot;")
        ));
        wb_rels.push((
            format!("rId{n}"),
            "worksheet",
            format!("worksheets/sheet{n}.xml"),
        ));
        ct.push_str(&format!(
            r#"<Override PartName="/xl/worksheets/sheet{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#
        ));
        let mut data = String::new();
        for (r, row) in rows.iter().enumerate() {
            data.push_str(&format!(r#"<row r="{}">"#, r + 1));
            for (c, v) in row.iter().enumerate() {
                let addr = format!("{}{}", (b'A' + c as u8) as char, r + 1);
                data.push_str(&format!(
                    r#"<c r="{addr}" t="inlineStr"><is><t>{}</t></is></c>"#,
                    esc(v)
                ));
            }
            data.push_str("</row>");
        }
        parts.push((
            format!("xl/worksheets/sheet{n}.xml"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="{sml}"><sheetData>{data}</sheetData></worksheet>"#
            )
            .into_bytes(),
        ));
    }
    wb.push_str("</sheets></workbook>");
    ct.push_str("</Types>");
    let mut all: Vec<(String, Vec<u8>)> = vec![
        ("[Content_Types].xml".into(), ct.into_bytes()),
        (
            "_rels/.rels".into(),
            rels(&[("rId1".into(), "officeDocument", "xl/workbook.xml".into())]),
        ),
        ("xl/workbook.xml".into(), wb.into_bytes()),
        ("xl/_rels/workbook.xml.rels".into(), rels(&wb_rels)),
    ];
    all.extend(parts);
    let refs: Vec<(&str, Vec<u8>)> = all.iter().map(|(n, d)| (n.as_str(), d.clone())).collect();
    zip_of(&refs)
}

/// PPTX со слайдами `(заголовок, текст)` по порядку показа.
pub fn pptx(slides: &[(&str, &str)]) -> Vec<u8> {
    let p = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;
    let mut ids = String::new();
    let mut pres_rels = Vec::new();
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    for (i, (title, body)) in slides.iter().enumerate() {
        let n = i + 1;
        ids.push_str(&format!(r#"<p:sldId id="{}" r:id="rId{n}"/>"#, 255 + n));
        pres_rels.push((format!("rId{n}"), "slide", format!("slides/slide{n}.xml")));
        parts.push((
            format!("ppt/slides/slide{n}.xml"),
            format!(
                r#"<p:sld {p}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="T"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>{}</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="B"/><p:cNvSpPr/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>{}</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
                esc(title),
                esc(body)
            )
            .into_bytes(),
        ));
    }
    let mut all: Vec<(String, Vec<u8>)> = vec![
        (
            "[Content_Types].xml".into(),
            content_types(
                "/ppt/presentation.xml",
                "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml",
            ),
        ),
        (
            "_rels/.rels".into(),
            rels(&[(
                "rId1".into(),
                "officeDocument",
                "ppt/presentation.xml".into(),
            )]),
        ),
        (
            "ppt/presentation.xml".into(),
            format!(r#"<p:presentation {p}><p:sldIdLst>{ids}</p:sldIdLst></p:presentation>"#)
                .into_bytes(),
        ),
        ("ppt/_rels/presentation.xml.rels".into(), rels(&pres_rels)),
    ];
    all.extend(parts);
    let refs: Vec<(&str, Vec<u8>)> = all.iter().map(|(n, d)| (n.as_str(), d.clone())).collect();
    zip_of(&refs)
}
