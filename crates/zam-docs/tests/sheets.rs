//! Таблицы: XLSX, ODS, XLS, CSV, `sheet_page` и лимиты (M5, задача 2).
//!
//! Фикстуры собираются в тесте (`common::sheets`), бинарных файлов в репозитории нет.

mod common;

use std::time::{Duration, Instant};

use common::sheets::{XCell, cfb, grid, ods, sheet_xml, sst_xml, xls, xlsx};
use proptest::prelude::*;
use zam_docs::{DocError, DocKind, Extracted, Limits, Locator, extract, sheet_page, sniff};

fn far() -> Instant {
    Instant::now() + Duration::from_secs(3600)
}

fn run(kind: DocKind, bytes: &[u8], limits: &Limits) -> Extracted {
    extract(kind, bytes, limits, far()).unwrap()
}

fn all_text(x: &Extracted) -> String {
    x.units
        .iter()
        .map(|u| u.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn sheet_of(l: &Locator) -> (&str, &str) {
    match l {
        Locator::Sheet { name, range } => (name, range),
        other => panic!("не лист: {other:?}"),
    }
}

#[test]
fn xlsx_sheets_blocks_with_header() {
    // лист 1: пустая первая строка, шапка во 2-й, 300 строк данных; общие строки и `|`
    let mut rows = String::from(
        r#"<row r="2"><c r="A2" t="s"><v>0</v></c><c r="C2" t="s"><v>1</v></c></row>"#,
    );
    for r in 3..=302 {
        rows.push_str(&format!(
            r#"<row r="{r}"><c r="A{r}" t="s"><v>2</v></c><c r="B{r}"><v>{r}.50000000000001</v></c><c r="C{r}" t="inlineStr"><is><t>строка {r} a|b</t></is></c></row>"#
        ));
    }
    let s1 = sheet_xml(Some("A1:C302"), &rows);
    let s2 = sheet_xml(None, &grid(&[&["Код", "Итог"], &["1", "TRUE"]]));
    let x = run(
        DocKind::Xlsx,
        &xlsx(
            &[("Данные", s1), ("Итоги 2026", s2)],
            Some(sst_xml(&["Имя", "Описание", "Иван"])),
        ),
        &Limits::default(),
    );
    assert!(x.warnings.is_empty(), "{:?}", x.warnings);
    let first: Vec<_> = x
        .units
        .iter()
        .filter(|u| sheet_of(&u.locator).0 == "Данные")
        .collect();
    assert!(first.len() > 3, "{}", first.len());
    let mut next = 3;
    for u in &first {
        assert_eq!(u.heading_path, vec!["Данные".to_string()]);
        assert!(
            u.text
                .starts_with("| Имя |  | Описание |\n| --- | --- | --- |\n"),
            "{}",
            u.text
        );
        let (_, range) = sheet_of(&u.locator);
        let (a, b) = range.split_once(':').unwrap();
        assert_eq!(a, format!("A{next}"));
        let to: u32 = b.trim_start_matches(char::is_alphabetic).parse().unwrap();
        assert!(b.starts_with('C'));
        next = to + 1;
        assert!(u.text.chars().count() < 1750 + 200, "{}", u.text.len());
    }
    assert_eq!(next, 303);
    let t = all_text(&x);
    assert!(
        t.contains("| Иван | 3.50000000000001 | строка 3 a\\|b |"),
        "{t}"
    );
    let last = x.units.last().unwrap();
    assert_eq!(
        last.locator,
        Locator::Sheet {
            name: "Итоги 2026".into(),
            range: "A2:B2".into()
        }
    );
    assert_eq!(last.text, "| Код | Итог |\n| --- | --- |\n| 1 | TRUE |");
    assert_eq!(sniff(&xlsx(&[], None), "x.bin"), Some(DocKind::Xlsx));
}

#[test]
fn xlsx_10k_rows_sheet_page_fast() {
    let mut rows = String::from("<row>");
    for c in 0..8 {
        rows.push_str(&format!(
            r#"<c t="inlineStr"><is><t>Столбец {c}</t></is></c>"#
        ));
    }
    rows.push_str("</row>");
    for r in 2..=10_001 {
        rows.push_str("<row>");
        for c in 0..8 {
            rows.push_str(&format!("<c><v>{}</v></c>", r * 10 + c));
        }
        rows.push_str("</row>");
    }
    let bytes = xlsx(&[("Лист1", sheet_xml(None, &rows))], None);
    let t = Instant::now();
    let p = sheet_page(
        DocKind::Xlsx,
        &bytes,
        "",
        5000,
        100,
        &Limits::default(),
        far(),
    )
    .unwrap();
    let took = t.elapsed();
    println!("sheet_page 10k×8: {took:?}");
    assert_eq!(p.sheets, vec!["Лист1".to_string()]);
    assert_eq!(p.sheet, "Лист1");
    assert_eq!(p.header.len(), 8);
    assert_eq!(p.header[7], "Столбец 7");
    assert_eq!(p.total_rows, 10_000);
    assert_eq!(p.cols, 8);
    assert_eq!(p.first_row, 5000);
    assert_eq!(p.rows.len(), 100);
    assert_eq!(p.rows[0][0], "50000");
    assert_eq!(p.rows[99][7], "50997");
    assert!(p.warnings.is_empty());
    if !cfg!(debug_assertions) {
        assert!(took <= Duration::from_millis(200), "{took:?}");
    }
    // окно у шапки и за концом листа
    let p = sheet_page(
        DocKind::Xlsx,
        &bytes,
        "Лист1",
        1,
        2,
        &Limits::default(),
        far(),
    )
    .unwrap();
    assert_eq!((p.first_row, p.rows.len()), (2, 2));
    let p = sheet_page(
        DocKind::Xlsx,
        &bytes,
        "",
        20_000,
        10,
        &Limits::default(),
        far(),
    )
    .unwrap();
    assert!(p.rows.is_empty());
    assert_eq!(
        sheet_page(
            DocKind::Xlsx,
            &bytes,
            "Нет",
            1,
            10,
            &Limits::default(),
            far()
        ),
        Err(DocError::Malformed("нет листа \"Нет\"".into()))
    );
}

#[test]
fn ods_basic() {
    let tables = r#"<table:table table:name="Sheet A"><table:table-column table:number-columns-repeated="3"/>
<table:table-row table:number-rows-repeated="2"><table:table-cell table:number-columns-repeated="1024"/></table:table-row>
<table:table-row><table:table-cell office:value-type="string"><text:p>Name</text:p></table:table-cell><table:table-cell/><table:table-cell><text:p>Sum</text:p></table:table-cell></table:table-row>
<table:table-row table:number-rows-repeated="2"><table:table-cell table:number-columns-repeated="2"><text:p>x<text:s text:c="3"/>y</text:p><text:p>z</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="12.5"/></table:table-row>
<table:table-row><table:table-cell><text:p>a<text:span>b</text:span></text:p><office:annotation><text:p>comment</text:p></office:annotation></table:table-cell><table:covered-table-cell/><table:table-cell office:value-type="float" office:value="3"><text:p>3,00 €</text:p></table:table-cell></table:table-row>
<table:table-row table:number-rows-repeated="1048000"><table:table-cell table:number-columns-repeated="16384"/></table:table-row>
</table:table><table:table table:name="B"><table:table-row><table:table-cell><text:p>only</text:p></table:table-cell></table:table-row></table:table>"#;
    let bytes = ods(tables.as_bytes());
    assert_eq!(sniff(&bytes, "x"), Some(DocKind::Ods));
    let t = Instant::now();
    let x = run(DocKind::Ods, &bytes, &Limits::default());
    assert!(t.elapsed() < Duration::from_secs(5));
    assert!(x.warnings.is_empty(), "{:?}", x.warnings);
    assert_eq!(x.units.len(), 2, "{:#?}", x.units);
    assert_eq!(
        x.units[0].text,
        "| Name |  | Sum |\n| --- | --- | --- |\n| x   y z | x   y z | 12.5 |\n| x   y z | x   y z | 12.5 |\n| ab |  | 3,00 € |"
    );
    assert_eq!(
        x.units[0].locator,
        Locator::Sheet {
            name: "Sheet A".into(),
            range: "A4:C6".into()
        }
    );
    assert_eq!(x.units[1].text, "| only |\n| --- |");
    assert_eq!(sheet_of(&x.units[1].locator), ("B", "A1:A1"));
    let p = sheet_page(DocKind::Ods, &bytes, "", 1, 10, &Limits::default(), far()).unwrap();
    assert_eq!(p.sheets, vec!["Sheet A".to_string(), "B".to_string()]);
    assert_eq!(p.header, vec!["Name", "", "Sum"]);
    assert_eq!((p.first_row, p.total_rows, p.cols), (4, 3, 3));
    assert_eq!(p.rows[2], vec!["ab", "", "3,00 €"]);
}

#[test]
fn xls_biff8_cells_and_sst_continue() {
    // строка SST длиннее записи (8224 байт): символы переходят в CONTINUE
    let long: String = "Ё".repeat(6000);
    let sst = vec!["Имя".to_string(), long.clone(), "Сумма".to_string()];
    let cells = vec![
        (0, 0, XCell::Sst(0)),
        (0, 1, XCell::Sst(2)),
        (1, 0, XCell::Label("Пётр".into())),
        (1, 1, XCell::Number(1234.5)),
        (2, 0, XCell::Sst(1)),
        (2, 1, XCell::Rk((100 << 2) | 2 | 1)),
        (3, 0, XCell::FormulaStr("итог".into())),
        (3, 1, XCell::FormulaNum(2.25)),
        (3, 2, XCell::Bool(true)),
    ];
    let bytes = xls(
        &[
            ("Отчёт", cells),
            ("Пусто", vec![]),
            ("Мал", vec![(4, 3, XCell::Label("x".into()))]),
        ],
        &sst,
    );
    assert_eq!(sniff(&bytes, "a.xls"), Some(DocKind::Xls));
    let x = run(DocKind::Xls, &bytes, &Limits::default());
    assert!(x.warnings.is_empty(), "{:?}", x.warnings);
    let t = all_text(&x);
    assert!(
        t.starts_with("| Имя | Сумма |\n| --- | --- |\n| Пётр | 1234.5 |"),
        "{}",
        &t[..200]
    );
    assert!(t.contains(&format!("| {long} | 1 |")));
    assert!(t.contains("| итог | 2.25 | TRUE |"));
    let last = x.units.last().unwrap();
    assert_eq!(
        last.locator,
        Locator::Sheet {
            name: "Мал".into(),
            range: "A5:D5".into()
        }
    );
    assert_eq!(last.text, "|  |  |  | x |\n| --- | --- | --- | --- |");
    // маленький поток — в мини-потоке OLE
    let small = cfb(&[(
        "Workbook",
        common::sheets::biff8(&[("S", vec![(0, 0, XCell::Label("мини".into()))])], &[]),
    )]);
    let x = run(DocKind::Xls, &small, &Limits::default());
    assert_eq!(x.units[0].text, "| мини |\n| --- |");
    let p = sheet_page(
        DocKind::Xls,
        &bytes,
        "Отчёт",
        1,
        5,
        &Limits::default(),
        far(),
    )
    .unwrap();
    assert_eq!(p.sheets.len(), 3);
    assert_eq!(p.rows[0], vec!["Пётр", "1234.5"]);
    // зашифрованный OOXML в OLE и OLE без книги
    let enc = cfb(&[
        ("EncryptedPackage", vec![1; 5000]),
        ("EncryptionInfo", vec![2; 100]),
    ]);
    let x = run(DocKind::Xls, &enc, &Limits::default());
    assert!(x.units.is_empty() && x.warnings == ["skipped: EncryptedPackage"]);
    assert!(matches!(
        extract(
            DocKind::Xls,
            &cfb(&[("Other", vec![0; 10])]),
            &Limits::default(),
            far()
        ),
        Err(DocError::Malformed(_))
    ));
}

#[test]
fn csv_semicolon_cp1251() {
    let src = "Имя;Город;Сумма\r\nИван;Москва;10,5\r\nПётр;Санкт-Петербург, центр;7\r\n";
    let (b, _, bad) = encoding_rs::WINDOWS_1251.encode(src);
    assert!(!bad);
    assert!(std::str::from_utf8(&b).is_err());
    assert_eq!(
        sniff(&b, "data.csv"),
        None,
        "не UTF-8 — расширение не решает"
    );
    let x = run(DocKind::Csv, &b, &Limits::default());
    assert_eq!(
        x.units[0].text,
        "| Имя | Город | Сумма |\n| --- | --- | --- |\n| Иван | Москва | 10,5 |\n| Пётр | Санкт-Петербург, центр | 7 |"
    );
    assert_eq!(sheet_of(&x.units[0].locator), ("CSV", "A2:C3"));
    // UTF-8 с BOM, табуляция
    let x = run(
        DocKind::Csv,
        "\u{feff}a\tb\n1\t2\n".as_bytes(),
        &Limits::default(),
    );
    assert_eq!(x.units[0].text, "| a | b |\n| --- | --- |\n| 1 | 2 |");
    // UTF-16LE с BOM (выгрузка Excel «Текст Юникод»)
    let mut u16b = vec![0xFF, 0xFE];
    u16b.extend("к\tл\n1\t2\n".encode_utf16().flat_map(u16::to_le_bytes));
    let x = run(DocKind::Csv, &u16b, &Limits::default());
    assert_eq!(x.units[0].text, "| к | л |\n| --- | --- |\n| 1 | 2 |");
}

#[test]
fn csv_quotes_newlines() {
    let src = "a,b,c\n\"x, y\",\"line1\nline2 \"\"q\"\"\",z\r\n\n3,\"\",4\n\"open";
    let x = run(DocKind::Csv, src.as_bytes(), &Limits::default());
    assert_eq!(
        x.units[0].text,
        "| a | b | c |\n| --- | --- | --- |\n| x, y | line1 line2 \"q\" | z |\n| 3 |  | 4 |\n| open |"
    );
    // пустая строка — запись без ячеек: номера строк совпадают с записями
    assert_eq!(sheet_of(&x.units[0].locator).1, "A2:C5");
    let p = sheet_page(
        DocKind::Csv,
        src.as_bytes(),
        "",
        1,
        10,
        &Limits::default(),
        far(),
    )
    .unwrap();
    assert_eq!(
        p.rows,
        vec![
            vec!["x, y".to_string(), "line1\nline2 \"q\"".into(), "z".into()],
            vec![],
            vec!["3".into(), String::new(), "4".into()],
            vec!["open".into()],
        ]
    );
    assert_eq!(p.total_rows, 4);
}

#[test]
fn max_cells_truncates() {
    let rows: Vec<Vec<String>> = (0..20)
        .map(|r| (0..5).map(|c| format!("v{r}_{c}")).collect())
        .collect();
    let refs: Vec<Vec<&str>> = rows
        .iter()
        .map(|r| r.iter().map(String::as_str).collect())
        .collect();
    let refs: Vec<&[&str]> = refs.iter().map(Vec::as_slice).collect();
    let bytes = xlsx(
        &[
            ("A", sheet_xml(None, &grid(&refs))),
            ("B", sheet_xml(None, &grid(&[&["b1"], &["b2"]]))),
        ],
        None,
    );
    let limits = Limits {
        max_cells: 12,
        ..Limits::default()
    };
    let x = run(DocKind::Xlsx, &bytes, &limits);
    assert_eq!(x.warnings, ["truncated: max_cells"]);
    let t = all_text(&x);
    assert!(t.contains("v2_1") && !t.contains("v2_2"), "{t}");
    // предел — на лист: второй лист разобран
    assert!(t.contains("b2"));
    // CSV и ODS — тот же предел
    let csv: String = (0..20).map(|r| format!("{r},{r},{r}\n")).collect();
    let x = run(DocKind::Csv, csv.as_bytes(), &limits);
    assert_eq!(x.warnings, ["truncated: max_cells"]);
    let rep = br#"<table:table table:name="R"><table:table-row table:number-rows-repeated="1000000"><table:table-cell table:number-columns-repeated="16384"><text:p>x</text:p></table:table-cell></table:table-row></table:table>"#;
    let t0 = Instant::now();
    let x = run(DocKind::Ods, &ods(rep), &limits);
    assert_eq!(x.warnings, ["truncated: max_cells"]);
    assert_eq!(all_text(&x).matches("| x ").count(), 12);
    assert!(t0.elapsed() < Duration::from_secs(5));
    // страница превью: ячейки окна с пустыми в разрывах
    let page_limits = Limits {
        max_cells: 30,
        ..Limits::default()
    };
    let p = sheet_page(DocKind::Xlsx, &bytes, "A", 1, 100, &page_limits, far()).unwrap();
    assert!(p.rows.iter().map(Vec::len).sum::<usize>() + p.header.len() <= 30);
    assert_eq!(p.warnings, ["truncated: max_cells"]);
}

#[test]
fn shared_string_bomb_truncates() {
    // одна общая строка 100 тыс. символов и 20 тыс. ссылок на неё (файл ~100 КиБ)
    let big = "щ".repeat(100_000);
    let mut rows = String::new();
    for r in 1..=20_000 {
        rows.push_str(&format!(
            r#"<row r="{r}"><c r="A{r}" t="s"><v>0</v></c></row>"#
        ));
    }
    let bytes = xlsx(
        &[("Бомба", sheet_xml(Some("A1:XFD1048576"), &rows))],
        Some(sst_xml(&[&big])),
    );
    assert!(bytes.len() < 200_000, "{}", bytes.len());
    let limits = Limits {
        max_chars: 1_000_000,
        ..Limits::default()
    };
    let x = run(DocKind::Xlsx, &bytes, &limits);
    assert_eq!(x.warnings, ["truncated: max_chars"]);
    let total: usize = x
        .units
        .iter()
        .map(|u| {
            u.text.chars().count()
                + u.heading_path
                    .iter()
                    .map(|h| h.chars().count())
                    .sum::<usize>()
                + 64
        })
        .sum();
    assert!(total <= 1_000_000, "{total}");
    assert!(x.units.len() >= 2);
    // общие строки длиннее max_chars обрезаются при чтении таблицы строк
    let small = Limits {
        max_chars: 50_000,
        ..Limits::default()
    };
    let x = run(DocKind::Xlsx, &bytes, &small);
    assert_eq!(x.warnings, ["truncated: max_chars"]);
    assert!(all_text(&x).chars().count() <= 50_000);
    // XLS: SST-бомба
    let cells: Vec<_> = (0..20_000u16).map(|r| (r, 0, XCell::Sst(0))).collect();
    let bytes = xls(&[("Бомба", cells)], &["ж".repeat(30_000)]);
    let x = run(DocKind::Xls, &bytes, &limits);
    assert_eq!(x.warnings, ["truncated: max_chars"]);
    assert!(all_text(&x).chars().count() <= 1_000_000);
}

#[test]
fn xlsx_huge_dimension_and_empty_cells_cheap() {
    // `dimension` на весь лист, ячейка в XFD1048576 и 200 тыс. пустых ячеек со стилем
    let mut rows =
        String::from(r#"<row r="1"><c r="A1" t="inlineStr"><is><t>h</t></is></c></row>"#);
    for r in 2..2000 {
        rows.push_str(&format!(r#"<row r="{r}">"#));
        for c in 0..100 {
            rows.push_str(&format!(r#"<c r="{}{r}" s="1"/>"#, common::sheets::col(c)));
        }
        rows.push_str("</row>");
    }
    rows.push_str(
        r#"<row r="1048576"><c r="XFD1048576"><v>1</v></c><c r="XFE1048576"><v>2</v></c></row>"#,
    );
    let bytes = xlsx(&[("S", sheet_xml(Some("A1:XFD1048576"), &rows))], None);
    let x = run(DocKind::Xlsx, &bytes, &Limits::default());
    assert_eq!(x.units.len(), 1);
    assert_eq!(sheet_of(&x.units[0].locator).1, "A1048576:XFD1048576");
    // ячейка правее XFD отброшена
    assert!(!x.units[0].text.contains(" 2 "));
    let p = sheet_page(DocKind::Xlsx, &bytes, "", 1, 3, &Limits::default(), far()).unwrap();
    assert_eq!((p.total_rows, p.cols, p.rows.len()), (1_048_575, 16_384, 3));
    assert!(p.rows.iter().all(Vec::is_empty));
}

#[test]
fn tables_garbage_is_error_not_panic() {
    for kind in [DocKind::Xlsx, DocKind::Xls, DocKind::Ods] {
        assert!(matches!(
            extract(kind, b"not a table", &Limits::default(), far()),
            Err(DocError::Malformed(_))
        ));
    }
    assert!(run(DocKind::Csv, b"", &Limits::default()).units.is_empty());
    assert_eq!(
        sheet_page(DocKind::Docx, b"", "", 1, 1, &Limits::default(), far()),
        Err(DocError::Unsupported)
    );
}

const XLSX_NAMES: &[&str] = &[
    "[Content_Types].xml",
    "_rels/.rels",
    "xl/workbook.xml",
    "xl/_rels/workbook.xml.rels",
    "xl/worksheets/sheet1.xml",
    "xl/sharedStrings.xml",
    "content.xml",
    "META-INF/manifest.xml",
];

const XLSX_TOKENS: &[&str] = &[
    "<workbook><sheets>",
    "<sheet name=\"S\" r:id=\"rId1\"/>",
    "<sheet name=\"S\" r:id=\"rId1\"/><sheet name=\"T\" r:id=\"rId1\"/>",
    "<Relationship Id=\"rId1\" Type=\"x/worksheet\" Target=\"worksheets/sheet1.xml\"/>",
    "<Relationship Id=\"rId2\" Type=\"x/sharedStrings\" Target=\"sharedStrings.xml\"/>",
    "<Relationship Id=\"rId1\" Type=\"x/officeDocument\" Target=\"xl/workbook.xml\"/>",
    "<sheetData>",
    "</sheetData>",
    "<row>",
    "<row r=\"3\">",
    "<row r=\"0\">",
    "<row r=\"4294967295\">",
    "</row>",
    "<c r=\"B2\" t=\"s\">",
    "<c r=\"XFD1\">",
    "<c r=\"ZZZZZZZ9\" t=\"inlineStr\">",
    "<c t=\"b\">",
    "<c t=\"e\">",
    "<c/>",
    "</c>",
    "<v>",
    "</v>",
    "0",
    "99999999999999999999",
    "1e308",
    "-0.5",
    "<is><t>",
    "</t></is>",
    "<f>SUM(A1)</f>",
    "<sst><si><t>",
    "</t></si>",
    "<si/>",
    "<rPh><t>ph</t></rPh>",
    "<table:table table:name=\"T\">",
    "</table:table>",
    "<table:table-row table:number-rows-repeated=\"1000000\">",
    "<table:table-row>",
    "</table:table-row>",
    "<table:table-cell table:number-columns-repeated=\"70000\">",
    "<table:table-cell office:value-type=\"float\" office:value=\"1\"/>",
    "<table:table-cell>",
    "</table:table-cell>",
    "<text:p>",
    "</text:p>",
    "<text:s text:c=\"99999999\"/>",
    "<office:annotation>",
    "</office:annotation>",
    "текст|",
    "&amp;",
    "<!DOCTYPE d>",
    "<",
    "\"",
];

fn xml_tokens() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(XLSX_TOKENS), 0..60).prop_map(|t| t.concat())
}

fn table_zip() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec((prop::sample::select(XLSX_NAMES), xml_tokens()), 0..8).prop_map(|e| {
        let mut seen = std::collections::HashSet::new();
        let entries: Vec<(&str, Vec<u8>)> = e
            .into_iter()
            .filter(|(n, _)| seen.insert(*n))
            .map(|(n, t)| (n, t.into_bytes()))
            .collect();
        common::zip_of(&entries)
    })
}

const BIFF_TYPES: &[u16] = &[
    0x0809, 0x000A, 0x0085, 0x00FC, 0x003C, 0x00FD, 0x0204, 0x00D6, 0x0203, 0x027E, 0x00BD, 0x0205,
    0x0006, 0x0207, 0x0042, 0x002F,
];

fn biff_stream() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(
        (
            prop::sample::select(BIFF_TYPES),
            prop::collection::vec(any::<u8>(), 0..40),
        ),
        0..30,
    )
    .prop_map(|recs| {
        let mut v = common::sheets::biff8(&[("S", vec![(0, 0, XCell::Sst(0))])], &["a".into()]);
        v.truncate(v.len() / 2);
        for (ty, d) in recs {
            v.extend_from_slice(&ty.to_le_bytes());
            v.extend_from_slice(&(d.len() as u16).to_le_bytes());
            v.extend(d);
        }
        v
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn tables_never_panic(
        z in table_zip(),
        biff in biff_stream(),
        csv in prop::collection::vec(prop::sample::select(&b",;\t\"\n\ra\xd1\x8f\xff "[..]), 0..300),
        cut in any::<prop::sample::Index>(),
    ) {
        let limits = Limits { max_xml_depth: 8, max_cells: 50, ..Limits::default() };
        let ole = cfb(&[("Workbook", biff)]);
        let ole_cut = &ole[..cut.index(ole.len() + 1)];
        for bytes in [z.as_slice(), ole.as_slice(), ole_cut, csv.as_slice()] {
            for kind in [DocKind::Xlsx, DocKind::Xls, DocKind::Ods, DocKind::Csv] {
                let _ = extract(kind, bytes, &limits, far());
                let _ = sheet_page(kind, bytes, "", 1, 5, &limits, far());
            }
        }
    }
}

// ---- Раунд правок 1 ревью задачи 2 ----

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}

fn put32(b: &mut [u8], i: usize, v: u32) {
    b[i..i + 4].copy_from_slice(&v.to_le_bytes());
}

fn rec(ty: u16, data: &[u8]) -> Vec<u8> {
    let mut v = ty.to_le_bytes().to_vec();
    v.extend_from_slice(&(data.len() as u16).to_le_bytes());
    v.extend_from_slice(data);
    v
}

fn biff_bof() -> Vec<u8> {
    let mut d = vec![0u8; 16];
    d[1] = 0x06;
    d[2] = 0x05;
    rec(0x0809, &d)
}

/// Разбор в отдельном потоке: должен закончиться не позже срока + запас.
fn finishes_in(kind: DocKind, bytes: Vec<u8>, deadline: Duration, margin: Duration) -> Extracted {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = extract(kind, &bytes, &Limits::default(), Instant::now() + deadline);
        let _ = tx.send(r);
    });
    match rx.recv_timeout(deadline + margin) {
        Ok(r) => r.unwrap(),
        Err(_) => panic!("разбор не закончился за {:?}", deadline + margin),
    }
}

#[test]
fn xls_continue_past_end_terminates() {
    // CONTINUE с длиной 0xFFFF за концом потока: прежде — вечный цикл (peek без next)
    let mut wb = biff_bof();
    wb.extend(rec(0x00FC, &[0u8; 8]));
    wb.extend_from_slice(&[0x3C, 0x00, 0xFF, 0xFF]);
    let x = finishes_in(
        DocKind::Xls,
        cfb(&[("Workbook", wb)]),
        Duration::from_secs(2),
        Duration::from_secs(10),
    );
    assert!(x.units.is_empty());
    // миллион пустых CONTINUE — до конца потока, без накопления
    let mut wb = biff_bof();
    wb.extend(rec(0x00FC, &[0u8; 8]));
    for _ in 0..1_000_000 {
        wb.extend_from_slice(&[0x3C, 0, 0, 0]);
    }
    wb.extend(rec(0x000A, &[]));
    let x = finishes_in(
        DocKind::Xls,
        cfb(&[("Workbook", wb)]),
        Duration::from_secs(5),
        Duration::from_secs(10),
    );
    assert!(
        x.units.is_empty() && x.warnings.is_empty(),
        "{:?}",
        x.warnings
    );
}

#[test]
fn cfb_cutoff_and_cycles_rejected() {
    let small = common::sheets::biff8(&[("S", vec![(0, 0, XCell::Label("a".repeat(100)))])], &[]);
    assert!(small.len() > 128 && small.len() < 4096);
    let ok = cfb(&[("Workbook", small.clone())]);
    assert_eq!(run(DocKind::Xls, &ok, &Limits::default()).units.len(), 1);
    let malformed = |b: &[u8]| {
        matches!(
            extract(DocKind::Xls, b, &Limits::default(), far()),
            Err(DocError::Malformed(_))
        )
    };
    // порог мини-потока не 4096
    let mut b = ok.clone();
    put32(&mut b, 0x38, u32::MAX);
    assert!(malformed(&b));
    // цикл мини-FAT: 0 → 1 → 0
    let mut b = ok.clone();
    let mf = 512 + u32_at(&b, 0x3C) as usize * 512;
    put32(&mut b, mf + 4, 0);
    assert!(malformed(&b));
    // цикл FAT в цепочке большого потока
    let mut big = small.clone();
    big.resize(20_000, 0);
    let ok = cfb(&[("Workbook", big)]);
    assert_eq!(run(DocKind::Xls, &ok, &Limits::default()).units.len(), 1);
    let dir = 512 + u32_at(&ok, 0x30) as usize * 512;
    let start = u32_at(&ok, dir + 128 + 0x74) as usize;
    let fat = 512 + u32_at(&ok, 0x4C) as usize * 512;
    let mut b = ok.clone();
    put32(&mut b, fat + (start + 2) * 4, start as u32);
    assert!(malformed(&b));
}

#[test]
fn ods_row_cells_bounded_and_wide_skipped() {
    // 20 тыс. пустых ячеек, затем текст: правее XFD — отброшен с предупреждением
    let mut t = String::from(
        r#"<table:table table:name="S"><table:table-row><table:table-cell><text:p>a</text:p></table:table-cell>"#,
    );
    t.push_str(&"<table:table-cell/>".repeat(20_000));
    t.push_str("<table:table-cell><text:p>far</text:p></table:table-cell></table:table-row>");
    t.push_str(r#"<table:table-row><table:table-cell table:number-columns-repeated="2"/><table:table-cell><text:p>c3</text:p></table:table-cell></table:table-row></table:table>"#);
    let x = run(DocKind::Ods, &ods(t.as_bytes()), &Limits::default());
    assert_eq!(x.warnings, ["skipped: columns after XFD"]);
    assert_eq!(x.units[0].text, "| a |\n| --- |\n|  |  | c3 |");
    // 4·10⁹ строк без видимых ячеек (текст правее XFD) — сдвиг номера, а не цикл
    let t = r#"<table:table table:name="S"><table:table-row table:number-rows-repeated="4000000000"><table:table-cell table:number-columns-repeated="16384"/><table:table-cell><text:p>x</text:p></table:table-cell></table:table-row><table:table-row><table:table-cell><text:p>end</text:p></table:table-cell></table:table-row></table:table>"#;
    let t0 = Instant::now();
    let x = run(DocKind::Ods, &ods(t.as_bytes()), &Limits::default());
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
    assert_eq!(
        x.units[0].locator,
        Locator::Sheet {
            name: "S".into(),
            range: "A4000000001:A4000000001".into()
        }
    );
}

#[test]
fn xlsx_long_sheet_names_capped() {
    let name = "Имя".repeat(10_000);
    let sheet = sheet_xml(None, &grid(&[&["h"]]));
    let x = run(
        DocKind::Xlsx,
        &xlsx(&[(&name, sheet.clone()), ("B", sheet)], None),
        &Limits::default(),
    );
    assert_eq!(x.units.len(), 2);
    let (n, _) = sheet_of(&x.units[0].locator);
    assert_eq!(n.chars().count(), zam_docs::HEADING_PATH_MAX_CHARS + 1);
    assert_eq!(x.units[0].heading_path[0], n);
    // записей `<sheet>` больше max_entries
    let limits = Limits {
        max_entries: 50,
        ..Limits::default()
    };
    let sheets: Vec<(String, Vec<u8>)> = (0..60)
        .map(|i| (format!("L{i}"), sheet_xml(None, &grid(&[&["v"]]))))
        .collect();
    let refs: Vec<(&str, Vec<u8>)> = sheets
        .iter()
        .map(|(n, s)| (n.as_str(), s.clone()))
        .collect();
    let bytes = xlsx(&refs[..40], None);
    assert_eq!(run(DocKind::Xlsx, &bytes, &limits).units.len(), 40);
    let bytes = xlsx(&refs, None);
    let x = extract(DocKind::Xlsx, &bytes, &limits, far()).unwrap();
    assert!(
        x.warnings.contains(&"truncated: max_entries".to_string()),
        "{:?}",
        x.warnings
    );
}

#[test]
fn sheet_page_deadline_partial() {
    let mut rows = String::new();
    for r in 1..=50_000 {
        rows.push_str(&format!(r#"<row r="{r}"><c><v>{r}</v></c></row>"#));
    }
    let bytes = xlsx(&[("S", sheet_xml(None, &rows))], None);
    let p = sheet_page(
        DocKind::Xlsx,
        &bytes,
        "",
        1,
        10,
        &Limits::default(),
        Instant::now(),
    )
    .unwrap();
    assert_eq!(p.warnings, ["truncated: timeout_ms"]);
    assert!(p.total_rows < 49_999);
}

#[test]
fn csv_mostly_utf8_and_cr_only() {
    // неполный символ в конце (обрезка) — всё ещё UTF-8
    let mut b = "имя,город\nИван,Москва".as_bytes().to_vec();
    b.extend_from_slice(&"ё".as_bytes()[..1]);
    let x = run(DocKind::Csv, &b, &Limits::default());
    assert!(
        x.units[0].text.contains("| Иван | Москва\u{FFFD} |"),
        "{}",
        x.units[0].text
    );
    // один неверный байт на 2 КиБ текста — UTF-8 с заменой, а не Windows-1251
    let mut b = "заголовок,второй\n".repeat(60).into_bytes();
    b.extend_from_slice(b"x\xFFy,z\n");
    let x = run(DocKind::Csv, &b, &Limits::default());
    let t = all_text(&x);
    assert!(
        t.contains("| x\u{FFFD}y | z |") && t.contains("заголовок"),
        "{t}"
    );
    // концы строк — одиночный `\r` (старый Mac): разделитель `;`
    let x = run(DocKind::Csv, b"a;b\r1;2\r3;4\r", &Limits::default());
    assert_eq!(
        x.units[0].text,
        "| a | b |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |"
    );
}
