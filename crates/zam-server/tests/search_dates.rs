//! Даты в FTS (M7.5 §8.4): `POST /search` без моделей (FTS) находит дату запроса в любой из трёх
//! форм — ISO `ГГГГ-ММ-ДД`, `ДД.ММ.ГГГГ` и `Д.М.ГГГГ` — в заметке и во вложении XLSX (дата ячейки
//! по своему числовому формату `dd.mm.yyyy` — как фикстура дат `zam-docs`), а разбросанные по
//! тексту те же числа датой не считает.
mod common;

use common::attach::{items, search, upload, wait_done, world, zip_of};
use serde_json::{Value, json};
use zam_store::{MARK_END, MARK_START};

const SML: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PKG_REL: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

/// XLSX из одного листа: в `A1` — серийный номер Excel 44270 со своим форматом `dd\.mm\.yyyy`
/// (текст извлечения — «15.03.2021», как показывает Excel), в `B1` — строка `note`.
fn xlsx_dated(note: &str) -> Vec<u8> {
    let ct = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#;
    let root = format!(
        r#"<Relationships xmlns="{PKG_REL}"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#
    );
    let wb = format!(
        r#"<workbook xmlns="{SML}" xmlns:r="{REL}"><sheets><sheet name="Отгрузки" sheetId="1" r:id="rId1"/></sheets></workbook>"#
    );
    let wb_rels = format!(
        r#"<Relationships xmlns="{PKG_REL}"><Relationship Id="rId1" Type="{REL}/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="{REL}/styles" Target="styles.xml"/></Relationships>"#
    );
    let styles = format!(
        r#"<styleSheet xmlns="{SML}"><numFmts count="1"><numFmt numFmtId="164" formatCode="dd\.mm\.yyyy"/></numFmts><cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="164"/></cellXfs></styleSheet>"#
    );
    let sheet = format!(
        r#"<worksheet xmlns="{SML}"><sheetData><row r="1"><c r="A1" s="1"><v>44270</v></c><c r="B1" t="inlineStr"><is><t>{note}</t></is></c></row></sheetData></worksheet>"#
    );
    zip_of(&[
        ("[Content_Types].xml", ct.as_bytes().to_vec()),
        ("_rels/.rels", root.into_bytes()),
        ("xl/workbook.xml", wb.into_bytes()),
        ("xl/_rels/workbook.xml.rels", wb_rels.into_bytes()),
        ("xl/styles.xml", styles.into_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.into_bytes()),
    ])
}

/// Пути карточек выдачи (у вложения — путь заметки-владельца) и сами карточки.
fn found<'a>(j: &'a Value, path: &str) -> Vec<&'a Value> {
    items(j).iter().filter(|i| i["path"] == path).collect()
}

/// Текст сниппета без маркеров подсветки и подсвеченные куски.
fn snippet(it: &Value) -> (String, Vec<String>) {
    let s = it["snippet"].as_str().unwrap_or_default();
    let plain = s.replace([MARK_START, MARK_END], "");
    let marked = s
        .split(MARK_START)
        .skip(1)
        .filter_map(|p| p.split(MARK_END).next())
        .map(str::to_string)
        .collect();
    (plain, marked)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_dates() {
    let w = world("search-dates", false).await;
    let a = &w.a;
    let user = a.st.admin_id;
    a.note(
        "knowledge",
        "даты/совещание",
        "# Совещание\n\nСовещание по кварцевому песку перенесли на 15.03.2021 из-за поставки.\n",
    )
    .await;
    a.note(
        "knowledge",
        "даты/выпуск",
        "# Выпуск\n\nВыпуск пилота прошёл 2021-03-05 после приёмки стенда.\n",
    )
    .await;
    a.note(
        "knowledge",
        "даты/шум",
        "# Шум\n\nЗа 2021 год — 15 строк отчёта и 03 раза по 5 вагонов, 3 смены.\n",
    )
    .await;
    let owner = a
        .note(
            "knowledge",
            "даты/таблица",
            "# Таблица\n\nСводка отгрузок.\n",
        )
        .await;
    let node = owner["id"].as_str().unwrap().to_string();
    upload(
        a,
        &a.token,
        &node,
        "отгрузки.xlsx",
        &xlsx_dated("Отгрузка партии доломита"),
    )
    .await;
    wait_done(a, user, &node, "отгрузки.xlsx").await;

    let q =
        |query: &str| json!({"query": query, "mode": "chunks", "top_k": 20, "budget_tokens": 8000});
    // ISO находит запись с точками; разбросанные числа — нет
    let j = search(a, &a.token, q("2021-03-15")).await;
    assert_eq!(j["degraded"], true, "{j}");
    let meet = found(&j, "даты/совещание");
    assert_eq!(meet.len(), 1, "{j}");
    let (text, marked) = snippet(meet[0]);
    assert!(text.contains("15.03.2021"), "{text}");
    assert!(
        marked.iter().any(|m| m.contains("15")) && marked.iter().any(|m| m.contains("2021")),
        "подсветка формы записи: {marked:?}"
    );
    assert!(found(&j, "даты/шум").is_empty(), "разбросанные числа: {j}");
    // дата ячейки XLSX (`dd.mm.yyyy` → «15.03.2021») — по ISO
    let att: Vec<&Value> = found(&j, "даты/таблица")
        .into_iter()
        .filter(|i| i["kind"] == "attachment")
        .collect();
    assert_eq!(att.len(), 1, "вложение по ISO: {j}");
    assert_eq!(att[0]["file"], "отгрузки.xlsx");
    // `ДД.ММ.ГГГГ` и `Д.М.ГГГГ` находят ISO
    for query in ["05.03.2021", "5.3.2021", "пилот 05.03.2021"] {
        let j = search(a, &a.token, q(query)).await;
        assert_eq!(found(&j, "даты/выпуск").len(), 1, "{query}: {j}");
        assert!(found(&j, "даты/шум").is_empty(), "{query}: {j}");
    }
    // та же дата с точками — та же выдача: заметка и ячейка, без разбросанных чисел
    let j = search(a, &a.token, q("15.03.2021")).await;
    assert_eq!(found(&j, "даты/совещание").len(), 1, "{j}");
    assert!(found(&j, "даты/шум").is_empty(), "{j}");
    let att = found(&j, "даты/таблица");
    assert!(att.iter().any(|i| i["kind"] == "attachment"), "{j}");
}
