//! Писатели OOXML корпуса: DOCX (заголовки, абзацы, таблицы, сноски), XLSX (общие строки, стили
//! дат `numFmt`/`cellXfs`), PPTX (слайды с заголовком, пунктами и заметками докладчика).
//! zip — с фиксированной датой записей (1980-01-01): одинаковые байты при каждом запуске.

use std::collections::HashMap;
use std::io::Write as _;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, System, ZipWriter};

const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PKG_REL: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const SML: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const PML_NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;

/// Параметры записи zip: deflate, фиксированная дата и система «создано в» — без неё zip 9
/// пишет DOS на Windows и Unix на прочих, и байты набора зависят от платформы (CI 736a58f).
pub fn zip_options() -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(DateTime::default())
        .system(System::Unix)
}

/// zip из записей по порядку.
pub fn zip_of(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut w = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, data) in entries {
        w.start_file(name.as_str(), zip_options())
            .expect("zip: запись");
        w.write_all(data).expect("zip: данные");
    }
    w.finish().expect("zip: каталог").into_inner()
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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

fn content_types(overrides: &[(String, &str)]) -> Vec<u8> {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>"#,
    );
    for (part, ct) in overrides {
        s.push_str(&format!(
            r#"<Override PartName="{part}" ContentType="{ct}"/>"#
        ));
    }
    s.push_str("</Types>");
    s.into_bytes()
}

// ---------------------------------------------------------------- DOCX

/// Блок тела DOCX.
#[derive(Debug, Clone)]
pub enum Block {
    /// Заголовок уровня 1..=3 (стиль `Heading{n}`, `outlineLvl`).
    Heading(u8, String),
    Para(String),
    /// Абзац со ссылкой на сноску в конце.
    ParaNote(String, String),
    /// Таблица: первая строка — шапка.
    Table(Vec<Vec<String>>),
}

const DOCX_STYLES: &str = r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:pPr><w:outlineLvl w:val="0"/></w:pPr></w:style><w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/><w:pPr><w:outlineLvl w:val="1"/></w:pPr></w:style><w:style w:type="paragraph" w:styleId="Heading3"><w:name w:val="heading 3"/><w:basedOn w:val="Normal"/><w:pPr><w:outlineLvl w:val="2"/></w:pPr></w:style>"#;

fn w_p(text: &str, style: Option<&str>, note: Option<usize>) -> String {
    let ppr = style.map_or(String::new(), |s| {
        format!(r#"<w:pPr><w:pStyle w:val="{s}"/></w:pPr>"#)
    });
    let note = note.map_or(String::new(), |id| {
        format!(r#"<w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteReference w:id="{id}"/></w:r>"#)
    });
    format!(
        r#"<w:p>{ppr}<w:r><w:t xml:space="preserve">{}</w:t></w:r>{note}</w:p>"#,
        esc(text)
    )
}

/// DOCX из блоков; второе значение — номер (`para:n`, с 1) первого абзаца каждого блока по
/// правилу нумерации `zam-docs`: каждый `w:p` без предка `w:p`, абзацы ячеек — тоже.
pub fn docx(blocks: &[Block]) -> (Vec<u8>, Vec<u32>) {
    let mut body = String::new();
    let mut notes = String::new();
    let mut para = 0u32;
    let mut first = Vec::with_capacity(blocks.len());
    let mut note_id = 0usize;
    for b in blocks {
        first.push(para + 1);
        match b {
            Block::Heading(level, t) => {
                para += 1;
                body.push_str(&w_p(t, Some(&format!("Heading{level}")), None));
            }
            Block::Para(t) => {
                para += 1;
                body.push_str(&w_p(t, None, None));
            }
            Block::ParaNote(t, note) => {
                para += 1;
                note_id += 1;
                body.push_str(&w_p(t, None, Some(note_id)));
                notes.push_str(&format!(
                    r#"<w:footnote w:id="{note_id}">{}</w:footnote>"#,
                    w_p(note, None, None)
                ));
            }
            Block::Table(rows) => {
                body.push_str("<w:tbl>");
                for row in rows {
                    body.push_str("<w:tr>");
                    for cell in row {
                        para += 1;
                        body.push_str(&format!("<w:tc>{}</w:tc>", w_p(cell, None, None)));
                    }
                    body.push_str("</w:tr>");
                }
                body.push_str("</w:tbl>");
            }
        }
    }
    let ns = format!(r#"xmlns:w="{W_NS}" xmlns:r="{REL}""#);
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {ns}><w:body>{body}</w:body></w:document>"#
    );
    let styles = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles {ns}>{DOCX_STYLES}</w:styles>"#
    );
    let footnotes = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:footnotes {ns}><w:footnote w:type="separator" w:id="-1"><w:p/></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p/></w:footnote>{notes}</w:footnotes>"#
    );
    let wml = "application/vnd.openxmlformats-officedocument.wordprocessingml";
    let entries = vec![
        (
            "[Content_Types].xml".to_string(),
            content_types(&[
                (
                    "/word/document.xml".into(),
                    &format!("{wml}.document.main+xml"),
                ),
                ("/word/styles.xml".into(), &format!("{wml}.styles+xml")),
                (
                    "/word/footnotes.xml".into(),
                    &format!("{wml}.footnotes+xml"),
                ),
            ]),
        ),
        (
            "_rels/.rels".into(),
            rels(&[("rId1".into(), "officeDocument", "word/document.xml".into())]),
        ),
        (
            "word/_rels/document.xml.rels".into(),
            rels(&[
                ("rId1".into(), "styles", "styles.xml".into()),
                ("rId2".into(), "footnotes", "footnotes.xml".into()),
            ]),
        ),
        ("word/document.xml".into(), doc.into_bytes()),
        ("word/styles.xml".into(), styles.into_bytes()),
        ("word/footnotes.xml".into(), footnotes.into_bytes()),
    ];
    (zip_of(&entries), first)
}

// ---------------------------------------------------------------- XLSX

/// Формат даты ячейки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateFmt {
    /// Свой `numFmt` 164 `dd.mm.yyyy` (стиль 1).
    Custom,
    /// Встроенный `numFmtId` 14 (стиль 2; Excel показывает по локали, `m/d/yyyy` в en-US).
    Builtin14,
}

/// Ячейка XLSX.
#[derive(Debug, Clone)]
pub enum Cell {
    Str(String),
    Num(f64),
    /// Серийный номер даты (система 1900) и формат.
    Date(f64, DateFmt),
}

/// Лист XLSX: имя и строки (первая — шапка).
#[derive(Debug, Clone)]
pub struct Sheet {
    pub name: String,
    pub rows: Vec<Vec<Cell>>,
}

/// Серийный номер даты Excel (система 1900, с ошибкой високосного 1900 года) из y-m-d.
pub fn excel_serial(y: i32, m: u32, d: u32) -> f64 {
    // дни от 1899-12-30 (то же, что серийный номер Excel для дат после 1900-03-01)
    let days = days_from_civil(y, m, d) - days_from_civil(1899, 12, 30);
    days as f64
}

/// Дни от 1970-01-01 (алгоритм Хиннанта).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Буквы столбца с 0: 0 → `A`, 26 → `AA`.
pub fn col_name(mut c: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (c % 26) as u8);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).expect("ASCII")
}

fn num_text(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// XLSX из листов: строки ячеек — общие строки (`sharedStrings.xml`), даты — числа со стилем.
pub fn xlsx(sheets: &[Sheet]) -> Vec<u8> {
    let mut sst: Vec<String> = Vec::new();
    let mut sst_ix: HashMap<String, usize> = HashMap::new();
    let mut sst_count = 0usize;
    let mut ct: Vec<(String, &str)> = vec![
        (
            "/xl/workbook.xml".into(),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
        ),
        (
            "/xl/styles.xml".into(),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml",
        ),
        (
            "/xl/sharedStrings.xml".into(),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml",
        ),
    ];
    let mut wb = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="{SML}" xmlns:r="{REL}"><sheets>"#
    );
    let mut wb_rels = Vec::new();
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    for (i, sh) in sheets.iter().enumerate() {
        let n = i + 1;
        wb.push_str(&format!(
            r#"<sheet name="{}" sheetId="{n}" r:id="rId{n}"/>"#,
            esc(&sh.name)
        ));
        wb_rels.push((
            format!("rId{n}"),
            "worksheet",
            format!("worksheets/sheet{n}.xml"),
        ));
        ct.push((
            format!("/xl/worksheets/sheet{n}.xml"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml",
        ));
        let cols = sh.rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
        let mut data = String::with_capacity(sh.rows.len() * cols * 32);
        for (r, row) in sh.rows.iter().enumerate() {
            data.push_str(&format!(r#"<row r="{}">"#, r + 1));
            for (c, v) in row.iter().enumerate() {
                let addr = format!("{}{}", col_name(c), r + 1);
                match v {
                    Cell::Str(s) => {
                        sst_count += 1;
                        let ix = *sst_ix.entry(s.clone()).or_insert_with(|| {
                            sst.push(s.clone());
                            sst.len() - 1
                        });
                        data.push_str(&format!(r#"<c r="{addr}" t="s"><v>{ix}</v></c>"#));
                    }
                    Cell::Num(x) => {
                        data.push_str(&format!(r#"<c r="{addr}"><v>{}</v></c>"#, num_text(*x)));
                    }
                    Cell::Date(x, f) => {
                        let s = match f {
                            DateFmt::Custom => 1,
                            DateFmt::Builtin14 => 2,
                        };
                        data.push_str(&format!(
                            r#"<c r="{addr}" s="{s}"><v>{}</v></c>"#,
                            num_text(*x)
                        ));
                    }
                }
            }
            data.push_str("</row>");
        }
        let dim = format!("A1:{}{}", col_name(cols - 1), sh.rows.len().max(1));
        parts.push((
            format!("xl/worksheets/sheet{n}.xml"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="{SML}"><dimension ref="{dim}"/><sheetData>{data}</sheetData></worksheet>"#
            )
            .into_bytes(),
        ));
    }
    wb.push_str("</sheets></workbook>");
    let n = sheets.len();
    wb_rels.push((format!("rId{}", n + 1), "styles", "styles.xml".into()));
    wb_rels.push((
        format!("rId{}", n + 2),
        "sharedStrings",
        "sharedStrings.xml".into(),
    ));
    let mut sst_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><sst xmlns="{SML}" count="{sst_count}" uniqueCount="{}">"#,
        sst.len()
    );
    for s in &sst {
        sst_xml.push_str(&format!(
            r#"<si><t xml:space="preserve">{}</t></si>"#,
            esc(s)
        ));
    }
    sst_xml.push_str("</sst>");
    let styles = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><styleSheet xmlns="{SML}"><numFmts count="1"><numFmt numFmtId="164" formatCode="dd\.mm\.yyyy"/></numFmts><fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="3"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/><xf numFmtId="164" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/><xf numFmtId="14" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/></cellXfs></styleSheet>"#
    );
    let mut all: Vec<(String, Vec<u8>)> = vec![
        ("[Content_Types].xml".into(), content_types(&ct)),
        (
            "_rels/.rels".into(),
            rels(&[("rId1".into(), "officeDocument", "xl/workbook.xml".into())]),
        ),
        ("xl/workbook.xml".into(), wb.into_bytes()),
        ("xl/_rels/workbook.xml.rels".into(), rels(&wb_rels)),
        ("xl/styles.xml".into(), styles.into_bytes()),
        ("xl/sharedStrings.xml".into(), sst_xml.into_bytes()),
    ];
    all.extend(parts);
    zip_of(&all)
}

// ---------------------------------------------------------------- PPTX

/// Слайд: заголовок, пункты, заметки докладчика.
#[derive(Debug, Clone)]
pub struct Slide {
    pub title: String,
    pub bullets: Vec<String>,
    pub notes: Option<String>,
}

fn sp(id: u32, name: &str, ph: &str, paras: &[String]) -> String {
    let body: String = paras
        .iter()
        .map(|t| format!("<a:p><a:r><a:t>{}</a:t></a:r></a:p>", esc(t)))
        .collect();
    format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="{name}"/><p:cNvSpPr/><p:nvPr>{ph}</p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/>{body}</p:txBody></p:sp>"#
    )
}

/// PPTX из слайдов по порядку показа.
pub fn pptx(slides: &[Slide]) -> Vec<u8> {
    let pml = "application/vnd.openxmlformats-officedocument.presentationml";
    let mut ct: Vec<(String, String)> = vec![(
        "/ppt/presentation.xml".into(),
        format!("{pml}.presentation.main+xml"),
    )];
    let mut ids = String::new();
    let mut pres_rels = Vec::new();
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    for (i, s) in slides.iter().enumerate() {
        let n = i + 1;
        ids.push_str(&format!(r#"<p:sldId id="{}" r:id="rId{n}"/>"#, 255 + n));
        pres_rels.push((format!("rId{n}"), "slide", format!("slides/slide{n}.xml")));
        ct.push((
            format!("/ppt/slides/slide{n}.xml"),
            format!("{pml}.slide+xml"),
        ));
        let tree = format!(
            "{}{}",
            sp(
                2,
                "Title",
                r#"<p:ph type="title"/>"#,
                std::slice::from_ref(&s.title)
            ),
            sp(3, "Body", r#"<p:ph idx="1"/>"#, &s.bullets)
        );
        parts.push((
            format!("ppt/slides/slide{n}.xml"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sld {PML_NS}><p:cSld><p:spTree>{tree}</p:spTree></p:cSld></p:sld>"#
            )
            .into_bytes(),
        ));
        if let Some(notes) = &s.notes {
            ct.push((
                format!("/ppt/notesSlides/notesSlide{n}.xml"),
                format!("{pml}.notesSlide+xml"),
            ));
            parts.push((
                format!("ppt/slides/_rels/slide{n}.xml.rels"),
                rels(&[(
                    "rId1".into(),
                    "notesSlide",
                    format!("../notesSlides/notesSlide{n}.xml"),
                )]),
            ));
            let tree = sp(
                2,
                "Notes",
                r#"<p:ph type="body" idx="1"/>"#,
                std::slice::from_ref(notes),
            );
            parts.push((
                format!("ppt/notesSlides/notesSlide{n}.xml"),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:notes {PML_NS}><p:cSld><p:spTree>{tree}</p:spTree></p:cSld></p:notes>"#
                )
                .into_bytes(),
            ));
        }
    }
    let ct_ref: Vec<(String, &str)> = ct.iter().map(|(p, c)| (p.clone(), c.as_str())).collect();
    let mut all: Vec<(String, Vec<u8>)> = vec![
        ("[Content_Types].xml".into(), content_types(&ct_ref)),
        (
            "_rels/.rels".into(),
            rels(&[("rId1".into(), "officeDocument", "ppt/presentation.xml".into())]),
        ),
        (
            "ppt/presentation.xml".into(),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:presentation {PML_NS}><p:sldIdLst>{ids}</p:sldIdLst></p:presentation>"#
            )
            .into_bytes(),
        ),
        ("ppt/_rels/presentation.xml.rels".into(), rels(&pres_rels)),
    ];
    all.extend(parts);
    zip_of(&all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excel_serial_matches_known_dates() {
        assert_eq!(excel_serial(2023, 11, 1), 45231.0);
        assert_eq!(excel_serial(1900, 3, 1), 61.0);
        assert_eq!(col_name(0), "A");
        assert_eq!(col_name(25), "Z");
        assert_eq!(col_name(26), "AA");
    }
}
