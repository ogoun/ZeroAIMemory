//! DOCX, PPTX, `sniff`, лимиты zip и XML, `locator` (M5, задача 1).
//!
//! Фикстуры собираются в тесте через `zip` (бинарных файлов в репозитории нет).

mod common;

use std::time::{Duration, Instant};

use common::{W, drop_central, dup_central, forge_compressed, zip_of, zip_with};
use proptest::prelude::*;
use zam_docs::{DocError, DocKind, Extracted, Limits, Locator, Unit, extract, sniff};

const P: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn rels(items: &[(&str, &str, &str)]) -> Vec<u8> {
    let mut s = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="{}">"#,
        "http://schemas.openxmlformats.org/package/2006/relationships"
    );
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
        r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="{main}" ContentType="{ct}"/></Types>"#
    )
    .into_bytes()
}

const DOCX_CT: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";
const PPTX_CT: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml";

/// DOCX: тело `body` (содержимое `w:body`), стили, сноски, колонтитул.
fn docx_with(body: &str, styles: &str, footnotes: Option<&str>, header: Option<&str>) -> Vec<u8> {
    let mut doc_rels = vec![("rId1", "styles", "styles.xml")];
    let mut entries = vec![
        ("[Content_Types].xml", content_types("/word/document.xml", DOCX_CT)),
        ("_rels/.rels", rels(&[("rId1", "officeDocument", "word/document.xml")])),
        (
            "word/document.xml",
            format!(r#"<?xml version="1.0" encoding="UTF-8"?><w:document {W}><w:body>{body}</w:body></w:document>"#)
                .into_bytes(),
        ),
        (
            "word/styles.xml",
            format!(r#"<?xml version="1.0"?><w:styles {W}>{styles}</w:styles>"#).into_bytes(),
        ),
    ];
    if let Some(f) = footnotes {
        doc_rels.push(("rId2", "footnotes", "footnotes.xml"));
        entries.push((
            "word/footnotes.xml",
            format!(r#"<w:footnotes {W}>{f}</w:footnotes>"#).into_bytes(),
        ));
    }
    if let Some(h) = header {
        doc_rels.push(("rId3", "header", "/word/header1.xml"));
        entries.push((
            "word/header1.xml",
            format!(r#"<w:hdr {W}>{h}</w:hdr>"#).into_bytes(),
        ));
    }
    entries.push(("word/_rels/document.xml.rels", rels(&doc_rels)));
    zip_of(&entries)
}

fn docx(body: &str) -> Vec<u8> {
    docx_with(body, "", None, None)
}

fn p(text: &str) -> String {
    format!("<w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p>")
}

fn ps(style: &str, text: &str) -> String {
    format!(r#"<w:p><w:pPr><w:pStyle w:val="{style}"/></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#)
}

fn style(id: &str, name: &str, based_on: Option<&str>) -> String {
    let b = based_on
        .map(|b| format!(r#"<w:basedOn w:val="{b}"/>"#))
        .unwrap_or_default();
    format!(r#"<w:style w:type="paragraph" w:styleId="{id}"><w:name w:val="{name}"/>{b}</w:style>"#)
}

fn far() -> Instant {
    Instant::now() + Duration::from_secs(3600)
}

fn run(kind: DocKind, bytes: &[u8]) -> Extracted {
    extract(kind, bytes, &Limits::default(), far()).unwrap()
}

fn find<'a>(x: &'a Extracted, needle: &str) -> &'a Unit {
    x.units
        .iter()
        .find(|u| u.text.contains(needle))
        .unwrap_or_else(|| panic!("нет единицы с {needle:?}: {:#?}", x.units))
}

fn all_text(x: &Extracted) -> String {
    x.units
        .iter()
        .map(|u| u.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn docx_headings_tables_footnotes() {
    let styles = [
        style("Title", "Title", None),
        style("Heading1", "heading 1", None),
        // локализованный Word: styleId «2», имя — английское «heading 2»
        style("2", "heading 2", None),
        style("MyHead", "Мой заголовок", Some("2")),
        r#"<w:style w:type="paragraph" w:styleId="Outline3"><w:name w:val="Свой"/><w:pPr><w:outlineLvl w:val="2"/></w:pPr></w:style>"#.to_string(),
    ]
    .concat();
    let body = [
        ps("Title", "Отчёт"),
        p("Вступление."),
        ps("Heading1", "Глава 1"),
        r#"<w:p><w:r><w:t>Текст главы со сноской</w:t></w:r><w:r><w:footnoteReference w:id="1"/></w:r></w:p>"#.to_string(),
        ps("2", "Раздел 1.1"),
        r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>Имя</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Значение</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>a|b</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>42</w:t></w:r></w:p><w:p><w:r><w:t>шт.</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#.to_string(),
        r#"<w:p><w:r><w:rPr><w:vanish/></w:rPr><w:t>СКРЫТО</w:t></w:r><w:r><w:rPr><w:vanish w:val="0"/></w:rPr><w:t>Видно</w:t></w:r><w:del><w:r><w:delText>УДАЛЕНО</w:delText></w:r></w:del><w:ins><w:r><w:t>Вставлено</w:t></w:r></w:ins></w:p>"#.to_string(),
        r#"<w:p><w:r><w:t>Колонка</w:t><w:tab/><w:t>два</w:t><w:br/><w:t>строка &amp; &#x41;</w:t></w:r></w:p>"#.to_string(),
        ps("Heading1", "Глава 2"),
        ps("MyHead", "Подраздел"),
        p("Текст подраздела."),
        ps("Outline3", "Третий уровень"),
        r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/></w:numPr></w:pPr><w:r><w:t>пункт списка</w:t></w:r></w:p>"#.to_string(),
    ]
    .concat();
    let footnotes = r#"<w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:t>разделитель</w:t></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:t>Пояснение сноски</w:t></w:r></w:p></w:footnote>"#;
    let header = r#"<w:p><w:r><w:t>Верхний колонтитул</w:t></w:r></w:p>"#;
    let x = run(
        DocKind::Docx,
        &docx_with(&body, &styles, Some(footnotes), Some(header)),
    );
    assert_eq!(x.kind, DocKind::Docx);
    assert!(x.warnings.is_empty(), "{:?}", x.warnings);
    assert!(!x.ocr_needed);

    assert_eq!(find(&x, "Вступление").heading_path, ["Отчёт"]);
    let ch = find(&x, "Текст главы");
    assert_eq!(ch.heading_path, ["Отчёт", "Глава 1"]);
    let table = find(&x, "| Имя | Значение |");
    assert_eq!(table.heading_path, ["Отчёт", "Глава 1", "Раздел 1.1"]);
    assert!(table.text.contains("| --- | --- |"), "{}", table.text);
    assert!(table.text.contains(r"| a\|b | 42 шт. |"), "{}", table.text);

    let all = all_text(&x);
    assert!(!all.contains("СКРЫТО") && !all.contains("УДАЛЕНО"), "{all}");
    assert!(all.contains("ВидноВставлено"), "{all}");
    assert!(all.contains("Колонка\tдва\nстрока & A"), "{all}");
    assert!(!all.contains("разделитель"), "{all}");

    assert_eq!(
        find(&x, "Текст подраздела").heading_path,
        ["Отчёт", "Глава 2", "Подраздел"]
    );
    let list = find(&x, "- пункт списка");
    assert_eq!(
        list.heading_path,
        ["Отчёт", "Глава 2", "Подраздел", "Третий уровень"]
    );

    // сноски и колонтитулы — отдельные единицы в конце, после тела
    let n = x.units.len();
    let notes = &x.units[n - 2];
    assert_eq!(notes.heading_path, ["Сноски"]);
    assert_eq!(notes.text, "[1] Пояснение сноски");
    // сноска — к абзацу со ссылкой: Title 1, «Вступление» 2, «Глава 1» 3, ссылка 4
    assert_eq!(notes.locator, Locator::Para(4));
    assert_eq!(
        ch.locator,
        Locator::Para(3),
        "единица начинается заголовком"
    );
    let hf = &x.units[n - 1];
    assert_eq!(hf.heading_path, ["Колонтитулы"]);
    assert_eq!(hf.text, "Верхний колонтитул");
}

#[test]
fn docx_para_locator() {
    let long = |n: u32| format!("P{n} {}", "слово ".repeat(100));
    let mut body = vec![
        ps("Heading1", "Глава A"), // 1
        p("P2"),
        p("P3"),
        p("P4"),
        ps("Heading1", "Глава B"), // 5
        // таблица: абзацы ячеек 6 и 7
        r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>c1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>c2</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#.to_string(),
        "<w:p/>".to_string(), // 8, пустой
        ps("Heading1", "Глава C"), // 9
    ];
    for n in 10..=16 {
        body.push(p(&long(n)));
    }
    let x = run(
        DocKind::Docx,
        &docx_with(
            &body.concat(),
            &style("Heading1", "heading 1", None),
            None,
            None,
        ),
    );
    assert_eq!(find(&x, "Глава A").locator, Locator::Para(1));
    assert!(find(&x, "Глава A").text.contains("P4"));
    assert_eq!(find(&x, "Глава B").locator, Locator::Para(5));
    assert_eq!(find(&x, "Глава C").locator, Locator::Para(9));
    // длинные абзацы режут единицы; locator — номер первого абзаца единицы
    let mut seen = Vec::new();
    for u in &x.units {
        if let Some(rest) = u.text.strip_prefix('P') {
            let n: u32 = rest.split_whitespace().next().unwrap().parse().unwrap();
            if n >= 10 {
                assert_eq!(
                    u.locator,
                    Locator::Para(n),
                    "{}",
                    u.text.chars().take(20).collect::<String>()
                );
                assert_eq!(u.heading_path, ["Глава C"]);
                seen.push(n);
            }
        }
        assert!(
            u.text.chars().count() <= 1500 + 700,
            "единица не соразмерна"
        );
    }
    assert!(seen.len() >= 3, "{seen:?}");
}

/// PPTX: слайды в порядке `sldIdLst`, имена файлов — в обратном порядке.
fn pptx() -> Vec<u8> {
    let slide = |title: &str, body: &str, extra: &str| {
        format!(
            r#"<p:sld {P}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="T"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>{title}</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="B"/><p:cNvSpPr/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>{body}</a:t></a:r><a:br/><a:r><a:t>вторая строка</a:t></a:r></a:p><a:p><a:r><a:t>третья</a:t></a:r></a:p></p:txBody></p:sp>{extra}</p:spTree></p:cSld></p:sld>"#
        )
        .into_bytes()
    };
    let table = r#"<p:graphicFrame><a:graphic><a:graphicData><a:tbl><a:tr><a:tc><a:txBody><a:p><a:r><a:t>К1</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:p><a:r><a:t>К2</a:t></a:r></a:p></a:txBody></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>"#;
    let notes = format!(
        r#"<p:notes {P}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="I"/><p:cNvSpPr/><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="N"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Заметка один</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id="4" name="S"/><p:cNvSpPr/><p:nvPr><p:ph type="sldNum"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>777</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>"#
    );
    zip_of(&[
        ("[Content_Types].xml", content_types("/ppt/presentation.xml", PPTX_CT)),
        ("_rels/.rels", rels(&[("rId1", "officeDocument", "ppt/presentation.xml")])),
        (
            "ppt/presentation.xml",
            format!(r#"<p:presentation {P}><p:sldIdLst><p:sldId id="256" r:id="rId3"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst></p:presentation>"#).into_bytes(),
        ),
        (
            "ppt/_rels/presentation.xml.rels",
            rels(&[("rId2", "slide", "slides/slide1.xml"), ("rId3", "slide", "slides/slide2.xml")]),
        ),
        ("ppt/slides/slide1.xml", slide("Второй", "Текст второго", "")),
        ("ppt/slides/slide2.xml", slide("Первый", "Текст первого", table)),
        (
            "ppt/slides/_rels/slide2.xml.rels",
            rels(&[("rId1", "notesSlide", "../notesSlides/notesSlide1.xml")]),
        ),
        ("ppt/notesSlides/notesSlide1.xml", notes.into_bytes()),
    ])
}

#[test]
fn pptx_slides_and_notes_order() {
    let x = run(DocKind::Pptx, &pptx());
    assert!(x.warnings.is_empty(), "{:?}", x.warnings);
    let got: Vec<(Locator, Vec<String>)> = x
        .units
        .iter()
        .map(|u| (u.locator.clone(), u.heading_path.clone()))
        .collect();
    assert_eq!(
        got,
        vec![
            (Locator::Slide(1), vec!["Первый".to_string()]),
            (
                Locator::Slide(1),
                vec!["Первый".to_string(), "Заметки докладчика".to_string()]
            ),
            (Locator::Slide(2), vec!["Второй".to_string()]),
        ]
    );
    assert_eq!(
        x.units[0].text,
        "Первый\nТекст первого\nвторая строка\nтретья\n| К1 | К2 |\n| --- | --- |"
    );
    assert_eq!(x.units[1].text, "Заметка один");
    assert!(x.units[2].text.starts_with("Второй\nТекст второго"));
}

#[test]
fn sniff_by_signature_not_extension() {
    let d = docx(&p("x"));
    assert_eq!(sniff(&d, "report.pdf"), Some(DocKind::Docx));
    assert_eq!(sniff(&d, "без расширения"), Some(DocKind::Docx));
    assert_eq!(sniff(&pptx(), "slides.docx"), Some(DocKind::Pptx));
    let xlsx = zip_of(&[
        (
            "[Content_Types].xml",
            content_types(
                "/xl/workbook.xml",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
            ),
        ),
        ("xl/workbook.xml", b"<workbook/>".to_vec()),
    ]);
    assert_eq!(sniff(&xlsx, "a.csv"), Some(DocKind::Xlsx));
    let ods = zip_of(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.spreadsheet".to_vec(),
        ),
        ("content.xml", b"<office:document-content/>".to_vec()),
    ]);
    assert_eq!(sniff(&ods, "t.xlsx"), Some(DocKind::Ods));
    // zip без [Content_Types].xml и mimetype — не документ, даже с именем .docx
    assert_eq!(sniff(&zip_of(&[("a.txt", b"x".to_vec())]), "a.docx"), None);

    assert_eq!(
        sniff(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n", "a.docx"),
        Some(DocKind::Pdf)
    );
    assert_eq!(
        sniff(b"\r\n  junk %PDF-1.4 ...", "a.bin"),
        Some(DocKind::Pdf)
    );
    let mut ole = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    ole.extend([0u8; 504]);
    assert_eq!(sniff(&ole, "book.bin"), Some(DocKind::Xls));
    assert_eq!(sniff(&ole, "letter.doc"), None);

    assert_eq!(sniff("a;b\n1;2\n".as_bytes(), "t.CSV"), Some(DocKind::Csv));
    assert_eq!(sniff("# Заметка\n".as_bytes(), "n.md"), Some(DocKind::Text));
    assert_eq!(sniff(b"\x00\x01\x02binary", "n.txt"), None);
    // текст о PDF — текст; у текстового расширения `%PDF-` считается только в начале
    assert_eq!(
        sniff(b"see header %PDF-1.7 here", "n.md"),
        Some(DocKind::Text)
    );
    assert_eq!(sniff(b"\n  %PDF-1.7\n", "n.txt"), Some(DocKind::Pdf));
    assert_eq!(sniff(b"\x89PNG\r\n\x1a\n....", "a.docx"), None);
    assert_eq!(sniff(b"PK\x03\x04garbage", "a.docx"), None);
    assert_eq!(sniff(b"", "a.docx"), None);
}

#[test]
fn zip_bomb_ratio_truncates() {
    // 32 МиБ пустых абзацев сжимаются в ~1000 раз: читаем до порога степени сжатия
    let mut doc = format!(r#"<w:document {W}><w:body>{}"#, p("Начало бомбы"));
    doc.push_str(&"<w:p/>".repeat(32 * 1024 * 1024 / 6));
    doc.push_str("</w:body></w:document>");
    let bytes = zip_of(&[
        (
            "[Content_Types].xml",
            content_types("/word/document.xml", DOCX_CT),
        ),
        ("word/document.xml", doc.into_bytes()),
    ]);
    assert!(
        bytes.len() < 1024 * 1024,
        "фикстура — бомба: {}",
        bytes.len()
    );
    let t = Instant::now();
    let x = run(DocKind::Docx, &bytes);
    assert!(
        x.warnings.contains(&"truncated: max_ratio".to_string()),
        "{:?}",
        x.warnings
    );
    assert_eq!(x.units.len(), 1);
    assert_eq!(x.units[0].text, "Начало бомбы");
    assert!(t.elapsed() < Duration::from_secs(20), "{:?}", t.elapsed());

    // суммарный объём: лимит max_unpacked меньше части — тоже частичный результат
    let limits = Limits {
        max_unpacked: 4096,
        max_ratio: u32::MAX,
        ..Limits::default()
    };
    let x = extract(DocKind::Docx, &bytes, &limits, far()).unwrap();
    assert!(
        x.warnings.contains(&"truncated: max_unpacked".to_string()),
        "{:?}",
        x.warnings
    );

    // размер файла — отказ, а не частичный результат
    let limits = Limits {
        max_bytes: 100,
        ..Limits::default()
    };
    assert_eq!(
        extract(DocKind::Docx, &bytes, &limits, far()),
        Err(DocError::TooLarge)
    );
}

#[test]
fn zip_too_many_entries() {
    let mut entries = vec![
        (
            "[Content_Types].xml".to_string(),
            content_types("/word/document.xml", DOCX_CT),
        ),
        (
            "word/document.xml".to_string(),
            format!(
                r#"<w:document {W}><w:body>{}</w:body></w:document>"#,
                p("текст")
            )
            .into_bytes(),
        ),
    ];
    for i in 0..60 {
        entries.push((format!("junk/{i}.bin"), vec![b'x']));
    }
    let refs: Vec<(&str, Vec<u8>)> = entries
        .iter()
        .map(|(n, d)| (n.as_str(), d.clone()))
        .collect();
    let bytes = zip_of(&refs);
    let limits = Limits {
        max_entries: 50,
        ..Limits::default()
    };
    for kind in [DocKind::Docx, DocKind::Pptx] {
        let x = extract(kind, &bytes, &limits, far()).unwrap();
        assert!(x.units.is_empty());
        assert_eq!(x.warnings, ["truncated: max_entries"]);
    }
    // в пределах лимита тот же файл разбирается
    let x = extract(DocKind::Docx, &bytes, &Limits::default(), far()).unwrap();
    assert_eq!(x.units.len(), 1);
}

#[test]
fn xml_doctype_rejected() {
    let xxe = format!(
        r#"<?xml version="1.0"?><!DOCTYPE w:document [<!ENTITY xxe SYSTEM "file:///etc/passwd"><!ENTITY lol "lol">]><w:document {W}><w:body><w:p><w:r><w:t>&xxe;&lol;</w:t></w:r></w:p></w:body></w:document>"#
    );
    let bytes = zip_of(&[
        (
            "[Content_Types].xml",
            content_types("/word/document.xml", DOCX_CT),
        ),
        ("word/document.xml", xxe.into_bytes()),
    ]);
    match extract(DocKind::Docx, &bytes, &Limits::default(), far()) {
        Err(DocError::Malformed(m)) => assert!(m.contains("DOCTYPE"), "{m}"),
        other => panic!("{other:?}"),
    }
    // без DTD неизвестная сущность — тоже ошибка, а не подстановка
    let bytes = docx(&p("&xxe;"));
    assert!(matches!(
        extract(DocKind::Docx, &bytes, &Limits::default(), far()),
        Err(DocError::Malformed(_))
    ));
    // DOCTYPE в отношениях PPTX
    let bytes = zip_of(&[
        (
            "[Content_Types].xml",
            content_types("/ppt/presentation.xml", PPTX_CT),
        ),
        (
            "_rels/.rels",
            b"<!DOCTYPE r [<!ENTITY a \"b\">]><Relationships/>".to_vec(),
        ),
    ]);
    assert!(matches!(
        extract(DocKind::Pptx, &bytes, &Limits::default(), far()),
        Err(DocError::Malformed(_))
    ));
}

#[test]
fn xml_deep_nesting_truncates() {
    let deep = format!(
        "{}{}{}",
        "<w:sdt>".repeat(5000),
        p("глубоко"),
        "</w:sdt>".repeat(5000)
    );
    let body = format!("{}{deep}{}", p("До вложенности"), p("После"));
    let x = run(DocKind::Docx, &docx(&body));
    assert!(
        x.warnings.contains(&"truncated: max_xml_depth".to_string()),
        "{:?}",
        x.warnings
    );
    let all = all_text(&x);
    assert!(all.contains("До вложенности"), "{all}");
    assert!(!all.contains("глубоко") && !all.contains("После"), "{all}");
}

#[test]
fn deadline_partial_result() {
    let body: String = (1..=5000).map(|n| p(&format!("P{n}"))).collect();
    let bytes = docx(&body);
    let full = run(DocKind::Docx, &bytes);
    assert!(all_text(&full).contains("P5000"));
    assert!(full.warnings.is_empty());

    let x = extract(DocKind::Docx, &bytes, &Limits::default(), Instant::now()).unwrap();
    assert!(
        x.warnings.contains(&"truncated: timeout_ms".to_string()),
        "{:?}",
        x.warnings
    );
    let all = all_text(&x);
    assert!(all.contains("P1"), "частичный результат не пуст");
    assert!(!all.contains("P5000"), "разбор остановлен сроком");
    assert!(x.units.len() < full.units.len());

    // max_chars — тоже частичный результат
    let limits = Limits {
        max_chars: 100,
        ..Limits::default()
    };
    let x = extract(DocKind::Docx, &bytes, &limits, far()).unwrap();
    assert_eq!(x.warnings, ["truncated: max_chars"]);
    // считается текст документа; разделители блоков `\n\n` — разметка извлечения
    let n: usize = x
        .units
        .iter()
        .map(|u| u.text.chars().filter(|c| !c.is_whitespace()).count())
        .sum();
    assert!(n <= 100, "{n}");
}

#[test]
fn locator_render_parse_roundtrip() {
    let cases = [
        (Locator::Page(12), "page:12"),
        (Locator::Slide(3), "slide:3"),
        (
            Locator::Sheet {
                name: "Итоги".into(),
                range: "A1:F40".into(),
            },
            "sheet:Итоги!A1:F40",
        ),
        (
            Locator::Sheet {
                name: "Лист!2".into(),
                range: "B7".into(),
            },
            "sheet:Лист!2!B7",
        ),
        (Locator::Para(40), "para:40"),
    ];
    for (loc, s) in cases {
        assert_eq!(loc.render(), s);
        assert_eq!(Locator::parse(s).unwrap(), loc);
    }
    assert_eq!(
        Locator::parse("sheet:'Итоги 2026'!a1:c3").unwrap(),
        Locator::Sheet {
            name: "Итоги 2026".into(),
            range: "A1:C3".into()
        }
    );
    let quoted = Locator::Sheet {
        name: "'кавычка".into(),
        range: "A1".into(),
    };
    assert_eq!(Locator::parse(&quoted.render()).unwrap(), quoted);
    for bad in [
        "",
        "page",
        "page:",
        "page:0",
        "page:-1",
        "page:+3",
        "page:x",
        "slide:1.5",
        "row:3",
        "sheet:A1",
        "sheet:!A1",
        "sheet:L!",
        "sheet:L!1A",
        "sheet:L!A0",
        "sheet:L!AAAA1",
        "sheet:L!A1:",
        "PAGE:1",
    ] {
        assert!(
            matches!(Locator::parse(bad), Err(DocError::Malformed(_))),
            "{bad:?}"
        );
    }
}

#[test]
fn pdf_not_here() {
    assert_eq!(
        extract(DocKind::Pdf, b"%PDF-1.7", &Limits::default(), far()),
        Err(DocError::Unsupported)
    );
}

#[test]
fn heading_unicode_spaces_collapsed() {
    // NBSP, U+3000, U+2003 и табуляция в заголовке схлопываются, как у `split_whitespace`
    let body = format!(
        "{}{}",
        ps(
            "Heading1",
            "\u{a0}Раздел\u{a0}\u{a0}один\u{3000}\tдва\u{2003}"
        ),
        p("текст")
    );
    let x = run(
        DocKind::Docx,
        &docx_with(&body, &style("Heading1", "heading 1", None), None, None),
    );
    assert_eq!(x.units[0].heading_path, vec!["Раздел один два".to_string()]);
    assert_eq!(x.units[0].text, "Раздел один два\n\nтекст");
}

#[test]
fn text_paragraphs() {
    let x = run(
        DocKind::Text,
        "\u{feff}Первый абзац\nего строка\n\n\nВторой\n".as_bytes(),
    );
    assert_eq!(x.units.len(), 1);
    assert_eq!(x.units[0].locator, Locator::Para(1));
    assert_eq!(x.units[0].text, "Первый абзац\nего строка\n\nВторой");
}

/// DOCX-бомба: абзац «Начало» и `mib` МиБ пустых абзацев (сжатие ~1000:1) + запись `pad.bin`
/// без сжатия размером `pad` байт сразу после документа.
fn bomb_with_pad(mib: usize, pad: usize) -> Vec<u8> {
    let mut doc = format!(r#"<w:document {W}><w:body>{}"#, p("Начало"));
    doc.push_str(&"<w:p/>".repeat(mib * 1024 * 1024 / 6));
    doc.push_str("</w:body></w:document>");
    zip_with(
        &[
            ("word/document.xml", doc.into_bytes()),
            ("pad.bin", vec![7u8; pad]),
        ],
        &["pad.bin"],
    )
}

#[test]
fn zip_forged_compressed_size_truncates() {
    // заявленный compressed_size документа раздут на 3 МиБ (локальный и центральный
    // заголовки) и покрывает «дыру» — данные записи, убранной из каталога, — без перекрытия
    let pad = 3 * 1024 * 1024;
    let mut b = drop_central(&bomb_with_pad(8, pad), "pad.bin");
    forge_compressed(&mut b, "word/document.xml", pad as u32);
    let x = run(DocKind::Docx, &b);
    assert!(
        x.warnings.contains(&"truncated: max_ratio".to_string()),
        "степень сжатия — по фактически прочитанным байтам: {:?}",
        x.warnings
    );
    assert_eq!(x.units.len(), 1);
    assert_eq!(x.units[0].text, "Начало");
}

#[test]
fn zip_overlapping_entries_rejected() {
    let bad = |b: &[u8]| match extract(DocKind::Docx, b, &Limits::default(), far()) {
        Err(DocError::Malformed(m)) => m,
        other => panic!("{other:?}"),
    };
    // раздутый compressed_size накрывает следующую запись
    let pad = 64 * 1024;
    let mut b = bomb_with_pad(1, pad);
    forge_compressed(&mut b, "word/document.xml", pad as u32);
    assert!(bad(&b).contains("overlapping"));
    // две записи каталога на одни данные (бомба Фифилда)
    let d = docx(&p("текст"));
    let fifield = dup_central(&d, "word/document.xml", "word/documenX.xml");
    assert!(bad(&fifield).contains("overlapping"));
    // повтор имени — точный и без учёта регистра (обход проверок по имени части)
    let same = dup_central(&d, "word/document.xml", "word/document.xml");
    assert!(bad(&same).contains("повтор"), "{}", bad(&same));
    let case = zip_of(&[
        ("word/document.xml", b"<w:document/>".to_vec()),
        ("WORD/document.xml", b"<w:document/>".to_vec()),
    ]);
    assert!(bad(&case).contains("повтор"));
}

#[test]
fn max_chars_bounds_single_huge_paragraph() {
    // один абзац на 3 млн символов при max_chars = 1000: накопление обрезается сразу
    let body = format!(
        "{}<w:p><w:r><w:t>{}</w:t></w:r></w:p>{}",
        p("до"),
        "я".repeat(3_000_000),
        p("после")
    );
    // повторы сжимаются сильнее max_ratio: степень сжатия здесь не проверяется
    let limits = Limits {
        max_chars: 1000,
        max_ratio: u32::MAX,
        ..Limits::default()
    };
    let x = extract(DocKind::Docx, &docx(&body), &limits, far()).unwrap();
    assert_eq!(x.warnings, ["truncated: max_chars"]);
    let all = all_text(&x);
    assert!(all.starts_with("до\n\nяяя"), "{}", &all[..20]);
    assert!(!all.contains("после"));
    let n: usize = all.chars().filter(|c| !c.is_whitespace()).count();
    assert!(n <= 1000, "{n}");
    // то же в ячейке таблицы и в PPTX
    let cell = format!(
        "<w:tbl><w:tr><w:tc><w:p><w:r><w:t>{}</w:t></w:r></w:p></w:tc></w:tr></w:tbl>{}",
        "я".repeat(3_000_000),
        p("после")
    );
    let x = extract(DocKind::Docx, &docx(&cell), &limits, far()).unwrap();
    assert_eq!(x.warnings, ["truncated: max_chars"]);
    assert!(
        x.units
            .iter()
            .map(|u| u.text.chars().count())
            .sum::<usize>()
            < 1100
    );
}

#[test]
fn part_cut_inside_table_keeps_rows() {
    // max_unpacked обрывает document.xml посреди таблицы: собранные строки выдаются
    let rows: String = (0..400)
        .map(|i| format!("<w:tr><w:tc><w:p><w:r><w:t>r{i}</w:t></w:r></w:p></w:tc></w:tr>"))
        .collect();
    let doc = format!(
        r#"<w:document {W}><w:body>{}<w:tbl>{rows}</w:tbl>{}</w:body></w:document>"#,
        p("до"),
        p("после")
    );
    let bytes = zip_of(&[("word/document.xml", doc.into_bytes())]);
    let limits = Limits {
        max_unpacked: 4096,
        ..Limits::default()
    };
    let x = extract(DocKind::Docx, &bytes, &limits, far()).unwrap();
    assert!(
        x.warnings.contains(&"truncated: max_unpacked".to_string()),
        "{:?}",
        x.warnings
    );
    let all = all_text(&x);
    assert!(all.starts_with("до\n\n| r0 |\n| --- |\n| r1 |"), "{all}");
    assert!(!all.contains("r399") && !all.contains("после"), "{all}");
    assert_eq!(
        find(&x, "| r0 |").locator,
        Locator::Para(1),
        "единица начата абзацем «до»"
    );
}

#[test]
fn long_heading_path_is_capped() {
    let long = "Заголовок ".repeat(300); // 3000 символов
    let body = format!(
        "{}{}{}",
        ps("Heading1", &long),
        p(&"а".repeat(1400)),
        p(&"б".repeat(1400))
    );
    let x = run(
        DocKind::Docx,
        &docx_with(&body, &style("Heading1", "heading 1", None), None, None),
    );
    assert!(x.units.len() >= 2);
    for u in &x.units {
        assert_eq!(u.heading_path.len(), 1);
        let h = &u.heading_path[0];
        assert_eq!(h.chars().count(), zam_docs::HEADING_PATH_MAX_CHARS + 1);
        assert!(h.ends_with('…') && h.starts_with("Заголовок Заголовок"));
    }
    // полный текст заголовка — в тексте его единицы
    assert!(x.units[0].text.starts_with(long.trim_end()));
}

#[test]
fn table_in_textbox_is_paragraph_text() {
    let body = format!(
        r#"<w:p><w:r><w:t>Перед</w:t></w:r><w:r><w:pict><w:txbxContent><w:tbl><w:tr><w:tc><w:p><w:r><w:t>в надписи</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:txbxContent></w:pict></w:r></w:p>{}"#,
        p("Второй")
    );
    let x = run(DocKind::Docx, &docx(&body));
    let all = all_text(&x);
    assert!(all.contains("Перед в надписи"), "{all}");
    assert!(!all.contains('|'), "таблица надписи — не структура: {all}");
    assert_eq!(x.units[0].locator, Locator::Para(1));
    assert!(x.units[0].text.ends_with("Второй"));
}

#[test]
fn pptx_many_slide_ids_bounded() {
    // 60 000 sldId × 60 000 отношений без совпадений: без квадратичного поиска
    let n = 60_000;
    let mut pres = format!("<p:presentation {P}><p:sldIdLst>");
    for i in 0..n {
        pres.push_str(&format!(r#"<p:sldId r:id="x{i}"/>"#));
    }
    pres.push_str("</p:sldIdLst></p:presentation>");
    let items: Vec<(String, String)> = (0..n)
        .map(|i| (format!("y{i}"), format!("slides/s{i}.xml")))
        .collect();
    let refs: Vec<(&str, &str, &str)> = items
        .iter()
        .map(|(id, t)| (id.as_str(), "slide", t.as_str()))
        .collect();
    let b = zip_of(&[
        ("ppt/presentation.xml", pres.into_bytes()),
        ("ppt/_rels/presentation.xml.rels", rels(&refs)),
    ]);
    let t = Instant::now();
    let x = extract(
        DocKind::Pptx,
        &b,
        &Limits::default(),
        Instant::now() + Duration::from_secs(2),
    )
    .unwrap();
    let took = t.elapsed();
    assert!(took < Duration::from_secs(4), "{took:?}");
    assert!(x.units.is_empty());
    // отношений больше `max_entries` не читается (раунд правок 1 задачи 2)
    assert!(
        x.warnings
            .iter()
            .all(|w| w == "truncated: max_entries" || w == "truncated: timeout_ms"),
        "{:?}",
        x.warnings
    );
}

#[test]
fn docx_external_relationships_not_counted_in_max_entries() {
    // ревью задачи 2: больше `max_entries` гиперссылок (внешние отношения) перед сносками и
    // колонтитулом — их отношения не теряются молча
    let mut s = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="{}">"#,
        "http://schemas.openxmlformats.org/package/2006/relationships"
    );
    for i in 0..10_050 {
        s.push_str(&format!(
            r#"<Relationship Id="h{i}" Type="{REL}/hyperlink" Target="https://example.org/{i}" TargetMode="External"/>"#
        ));
    }
    s.push_str(&format!(
        r#"<Relationship Id="rId2" Type="{REL}/footnotes" Target="footnotes.xml"/><Relationship Id="rId3" Type="{REL}/header" Target="header1.xml"/></Relationships>"#
    ));
    let body =
        r#"<w:p><w:r><w:t>Текст</w:t></w:r><w:r><w:footnoteReference w:id="1"/></w:r></w:p>"#;
    let b = zip_of(&[
        ("[Content_Types].xml", content_types("/word/document.xml", DOCX_CT)),
        ("_rels/.rels", rels(&[("rId1", "officeDocument", "word/document.xml")])),
        (
            "word/document.xml",
            format!(r#"<w:document {W}><w:body>{body}</w:body></w:document>"#).into_bytes(),
        ),
        (
            "word/footnotes.xml",
            format!(
                r#"<w:footnotes {W}><w:footnote w:id="1"><w:p><w:r><w:t>Сноска</w:t></w:r></w:p></w:footnote></w:footnotes>"#
            )
            .into_bytes(),
        ),
        (
            "word/header1.xml",
            format!(r#"<w:hdr {W}><w:p><w:r><w:t>Шапка</w:t></w:r></w:p></w:hdr>"#).into_bytes(),
        ),
        ("word/_rels/document.xml.rels", s.into_bytes()),
    ]);
    let x = run(DocKind::Docx, &b);
    assert!(x.warnings.is_empty(), "{:?}", x.warnings);
    assert_eq!(find(&x, "Сноска").heading_path, ["Сноски"]);
    assert_eq!(find(&x, "Шапка").heading_path, ["Колонтитулы"]);
}

// --- proptest: разбор не паникует на любых байтах и на случайных zip с XML ---

const NAMES: &[&str] = &[
    "[Content_Types].xml",
    "_rels/.rels",
    "word/document.xml",
    "word/styles.xml",
    "word/footnotes.xml",
    "word/_rels/document.xml.rels",
    "ppt/presentation.xml",
    "ppt/_rels/presentation.xml.rels",
    "ppt/slides/slide1.xml",
    "ppt/slides/_rels/slide1.xml.rels",
    "ppt/notesSlides/notesSlide1.xml",
];

const TOKENS: &[&str] = &[
    "<w:p>",
    "</w:p>",
    "<w:r>",
    "</w:r>",
    "<w:t>",
    "</w:t>",
    "<w:tbl>",
    "</w:tbl>",
    "<w:tr>",
    "</w:tr>",
    "<w:tc>",
    "</w:tc>",
    "<w:pPr>",
    "</w:pPr>",
    "<w:pStyle w:val=\"H\"/>",
    "<w:outlineLvl w:val=\"0\"/>",
    "<w:vanish/>",
    "<w:del>",
    "</w:del>",
    "<w:tab/>",
    "<w:br/>",
    "<w:footnoteReference w:id=\"1\"/>",
    "<w:footnote w:id=\"1\">",
    "</w:footnote>",
    "<w:style w:styleId=\"H\">",
    "<w:basedOn w:val=\"H\"/>",
    "</w:style>",
    "<a:p>",
    "</a:p>",
    "<a:t>",
    "</a:t>",
    "<p:sp>",
    "</p:sp>",
    "<p:ph type=\"title\"/>",
    "<p:sldId r:id=\"rId1\"/>",
    "<Relationship Id=\"rId1\" Type=\"x/slide\" Target=\"slides/slide1.xml\"/>",
    "<Relationship Id=\"rId1\" Type=\"x/officeDocument\" Target=\"../../ppt/presentation.xml\"/>",
    "<Relationship Id=\"rId2\" Type=\"x/notesSlide\" Target=\"/ppt/notesSlides/notesSlide1.xml\"/>",
    "<Relationship Id=\"rId3\" Type=\"x/styles\" Target=\"styles.xml\"/>",
    "<Relationship Id=\"rId4\" Type=\"x/footnotes\" Target=\"footnotes.xml\"/>",
    "<mc:Fallback>",
    "</mc:Fallback>",
    "текст",
    "x",
    " ",
    "&amp;",
    "&#x41;",
    "&#0;",
    "&bad;",
    "<![CDATA[c]]>",
    "<!-- c -->",
    "<?pi x?>",
    "<!DOCTYPE d>",
    "<",
    ">",
    "\"",
    "&",
];

fn xml_text() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(TOKENS), 0..60).prop_map(|t| t.concat())
}

fn random_zip() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec((prop::sample::select(NAMES), xml_text()), 0..8).prop_map(|e| {
        let mut seen = std::collections::HashSet::new();
        let entries: Vec<(&str, Vec<u8>)> = e
            .into_iter()
            .filter(|(n, _)| seen.insert(*n))
            .map(|(n, t)| (n, t.into_bytes()))
            .collect();
        zip_of(&entries)
    })
}

const KINDS: [DocKind; 8] = [
    DocKind::Docx,
    DocKind::Pptx,
    DocKind::Text,
    DocKind::Pdf,
    DocKind::Xlsx,
    DocKind::Xls,
    DocKind::Ods,
    DocKind::Csv,
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn extract_never_panics(
        raw in prop::collection::vec(any::<u8>(), 0..512),
        z in random_zip(),
        cut in any::<prop::sample::Index>(),
    ) {
        let limits = Limits { max_xml_depth: 8, ..Limits::default() };
        // обрезанный zip — битый центральный каталог
        let cut = &z[..cut.index(z.len() + 1)];
        for bytes in [raw.as_slice(), z.as_slice(), cut] {
            let _ = sniff(bytes, "f.docx");
            for kind in KINDS {
                let _ = extract(kind, bytes, &limits, far());
            }
        }
    }

    #[test]
    fn locator_roundtrip_any(n in 1u32.., name in "[^\u{0}]{1,12}", col in "[A-Z]{1,3}", row in 1u32..1_000_000) {
        for l in [Locator::Page(n), Locator::Slide(n), Locator::Para(n),
                  Locator::Sheet { name: name.clone(), range: format!("{col}{row}") },
                  Locator::Sheet { name: name.clone(), range: format!("A1:{col}{row}") }] {
            prop_assert_eq!(Locator::parse(&l.render()).unwrap(), l);
        }
    }
}
