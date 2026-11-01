//! Сборка таблиц для тестов: XLSX и ODS (zip в памяти), XLS (BIFF8 в составном файле OLE).

use super::{zip_of, zip_with};

pub const SML: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// XML листа XLSX из содержимого `<sheetData>` (и необязательного `dimension`).
pub fn sheet_xml(dimension: Option<&str>, rows: &str) -> Vec<u8> {
    let dim = dimension.map_or(String::new(), |d| format!(r#"<dimension ref="{d}"/>"#));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="{SML}" xmlns:r="{REL}">{dim}<sheetData>{rows}</sheetData></worksheet>"#
    )
    .into_bytes()
}

/// `sharedStrings.xml` из строк.
pub fn sst_xml(strings: &[&str]) -> Vec<u8> {
    let mut s = format!(
        r#"<sst xmlns="{SML}" count="{0}" uniqueCount="{0}">"#,
        strings.len()
    );
    for t in strings {
        s.push_str(&format!("<si><t>{}</t></si>", esc(t)));
    }
    s.push_str("</sst>");
    s.into_bytes()
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Строки `<row>` из таблицы: число — `<v>`, остальное — встроенная строка.
pub fn grid(rows: &[&[&str]]) -> String {
    let mut s = String::new();
    for (r, row) in rows.iter().enumerate() {
        s.push_str(&format!(r#"<row r="{}">"#, r + 1));
        for (c, v) in row.iter().enumerate() {
            let addr = format!("{}{}", col(c as u32), r + 1);
            if v.is_empty() {
                continue;
            }
            if v.parse::<f64>().is_ok() {
                s.push_str(&format!(r#"<c r="{addr}"><v>{v}</v></c>"#));
            } else {
                s.push_str(&format!(
                    r#"<c r="{addr}" t="inlineStr"><is><t>{}</t></is></c>"#,
                    esc(v)
                ));
            }
        }
        s.push_str("</row>");
    }
    s
}

pub fn col(mut c: u32) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (c % 26) as u8);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

/// XLSX из листов (имя, XML листа) и необязательных общих строк.
pub fn xlsx(sheets: &[(&str, Vec<u8>)], shared: Option<Vec<u8>>) -> Vec<u8> {
    xlsx_styled(sheets, shared, None, "")
}

/// XLSX со стилями `xl/styles.xml` (`styles`) и атрибутами `<workbookPr>` книги (`pr`, пусто —
/// без элемента): даты и форматы чисел.
pub fn xlsx_styled(
    sheets: &[(&str, Vec<u8>)],
    shared: Option<Vec<u8>>,
    styles: Option<Vec<u8>>,
    pr: &str,
) -> Vec<u8> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut ct = String::from(
        r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>"#,
    );
    let pr = if pr.is_empty() {
        String::new()
    } else {
        format!("<workbookPr {pr}/>")
    };
    let mut wb = format!(r#"<workbook xmlns="{SML}" xmlns:r="{REL}">{pr}<sheets>"#);
    let mut rels = String::from(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    for (i, (name, xml)) in sheets.iter().enumerate() {
        let n = i + 1;
        wb.push_str(&format!(
            r#"<sheet name="{}" sheetId="{n}" r:id="rId{n}"/>"#,
            esc(name).replace('"', "&quot;")
        ));
        rels.push_str(&format!(
            r#"<Relationship Id="rId{n}" Type="{REL}/worksheet" Target="worksheets/sheet{n}.xml"/>"#
        ));
        ct.push_str(&format!(
            r#"<Override PartName="/xl/worksheets/sheet{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#
        ));
        entries.push((format!("xl/worksheets/sheet{n}.xml"), xml.clone()));
    }
    if let Some(s) = shared {
        rels.push_str(&format!(
            r#"<Relationship Id="rIdS" Type="{REL}/sharedStrings" Target="sharedStrings.xml"/>"#
        ));
        entries.push(("xl/sharedStrings.xml".into(), s));
    }
    if let Some(s) = styles {
        rels.push_str(&format!(
            r#"<Relationship Id="rIdT" Type="{REL}/styles" Target="styles.xml"/>"#
        ));
        entries.push(("xl/styles.xml".into(), s));
    }
    wb.push_str("</sheets></workbook>");
    rels.push_str("</Relationships>");
    ct.push_str("</Types>");
    let mut all: Vec<(String, Vec<u8>)> = vec![
        ("[Content_Types].xml".into(), ct.into_bytes()),
        (
            "_rels/.rels".into(),
            format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#).into_bytes(),
        ),
        ("xl/workbook.xml".into(), wb.into_bytes()),
        ("xl/_rels/workbook.xml.rels".into(), rels.into_bytes()),
    ];
    all.extend(entries);
    let refs: Vec<(&str, Vec<u8>)> = all.iter().map(|(n, d)| (n.as_str(), d.clone())).collect();
    zip_of(&refs)
}

pub const ODS_NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0""#;

/// ODS из содержимого `<office:spreadsheet>` (элементы `table:table`).
pub fn ods(tables: &[u8]) -> Vec<u8> {
    let mut content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content {ODS_NS}><office:body><office:spreadsheet>"#
    )
    .into_bytes();
    content.extend_from_slice(tables);
    content.extend_from_slice(b"</office:spreadsheet></office:body></office:document-content>");
    zip_with(
        &[
            (
                "mimetype",
                b"application/vnd.oasis.opendocument.spreadsheet".to_vec(),
            ),
            ("META-INF/manifest.xml", b"<manifest:manifest xmlns:manifest=\"urn:oasis:names:tc:opendocument:xmlns:manifest:1.0\"/>".to_vec()),
            ("content.xml", content),
        ],
        &["mimetype"],
    )
}

/// Лист BIFF8: имя и ячейки (строка с 0, столбец с 0, значение).
pub type XSheet<'a> = (&'a str, Vec<(u16, u16, XCell)>);

/// Ячейка BIFF8 для [`xls`].
#[derive(Clone)]
pub enum XCell {
    /// Номер строки `SST`.
    Sst(u32),
    Label(String),
    Number(f64),
    /// Сырое значение RK.
    Rk(u32),
    Bool(bool),
    /// Формула со строковым результатом (`FORMULA` + `STRING`).
    FormulaStr(String),
    /// Формула с числовым результатом.
    FormulaNum(f64),
}

fn rec(ty: u16, data: &[u8]) -> Vec<u8> {
    let mut v = ty.to_le_bytes().to_vec();
    v.extend_from_slice(&u16::try_from(data.len()).unwrap().to_le_bytes());
    v.extend_from_slice(data);
    v
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// `XLUnicodeString` (UTF-16): `cch`, флаги 1, символы.
fn xl_string(s: &str) -> Vec<u8> {
    let mut v = u16::try_from(s.encode_utf16().count())
        .unwrap()
        .to_le_bytes()
        .to_vec();
    v.push(1);
    v.extend(utf16(s));
    v
}

/// Запись `SST` с продолжениями `CONTINUE`: записи по `max` байт, символы строки на границе —
/// с новым байтом флагов (как пишет Excel).
fn sst_records(strings: &[String], max: usize) -> Vec<u8> {
    let mut recs: Vec<Vec<u8>> = Vec::new();
    let n = u32::try_from(strings.len()).unwrap().to_le_bytes();
    let mut cur: Vec<u8> = [n, n].concat();
    for s in strings {
        let units: Vec<u16> = s.encode_utf16().collect();
        if cur.len() + 5 > max {
            recs.push(std::mem::take(&mut cur));
        }
        cur.extend_from_slice(&u16::try_from(units.len()).unwrap().to_le_bytes());
        cur.push(1);
        let mut i = 0;
        while i < units.len() {
            if cur.len() + 2 > max {
                recs.push(std::mem::take(&mut cur));
                cur.push(1);
            }
            let k = ((max - cur.len()) / 2).min(units.len() - i);
            for u in &units[i..i + k] {
                cur.extend_from_slice(&u.to_le_bytes());
            }
            i += k;
        }
    }
    recs.push(cur);
    let mut out = Vec::new();
    for (i, r) in recs.iter().enumerate() {
        out.extend(rec(if i == 0 { 0x00FC } else { 0x003C }, r));
    }
    out
}

/// Поток `Workbook` BIFF8: листы (имя, ячейки (строка с 0, столбец с 0, значение)) и `SST`.
pub fn biff8(sheets: &[XSheet<'_>], sst: &[String]) -> Vec<u8> {
    let bof = |dt: u16| {
        let mut d = vec![0u8; 16];
        d[0..2].copy_from_slice(&0x0600u16.to_le_bytes());
        d[2..4].copy_from_slice(&dt.to_le_bytes());
        rec(0x0809, &d)
    };
    let mut out = bof(0x0005);
    out.extend(rec(0x0042, &1200u16.to_le_bytes()));
    let mut patches = Vec::new();
    for (name, _) in sheets {
        let mut d = vec![0u8; 6];
        patches.push(out.len() + 4);
        let units: Vec<u8> = utf16(name);
        d.push(u8::try_from(name.encode_utf16().count()).unwrap());
        d.push(1);
        d.extend(units);
        out.extend(rec(0x0085, &d));
    }
    if !sst.is_empty() {
        out.extend(sst_records(sst, 8224));
    }
    out.extend(rec(0x000A, &[]));
    for (i, (_, cells)) in sheets.iter().enumerate() {
        let at = u32::try_from(out.len()).unwrap();
        out[patches[i]..patches[i] + 4].copy_from_slice(&at.to_le_bytes());
        out.extend(bof(0x0010));
        for (r, c, v) in cells {
            let mut d = r.to_le_bytes().to_vec();
            d.extend_from_slice(&c.to_le_bytes());
            d.extend_from_slice(&0u16.to_le_bytes());
            match v {
                XCell::Sst(i) => {
                    d.extend_from_slice(&i.to_le_bytes());
                    out.extend(rec(0x00FD, &d));
                }
                XCell::Label(s) => {
                    d.extend(xl_string(s));
                    out.extend(rec(0x0204, &d));
                }
                XCell::Number(f) => {
                    d.extend_from_slice(&f.to_le_bytes());
                    out.extend(rec(0x0203, &d));
                }
                XCell::Rk(v) => {
                    d.extend_from_slice(&v.to_le_bytes());
                    out.extend(rec(0x027E, &d));
                }
                XCell::Bool(b) => {
                    d.push(u8::from(*b));
                    d.push(0);
                    out.extend(rec(0x0205, &d));
                }
                XCell::FormulaStr(s) => {
                    d.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0xFF, 0xFF]);
                    d.extend_from_slice(&[0u8; 6]);
                    out.extend(rec(0x0006, &d));
                    out.extend(rec(0x0207, &xl_string(s)));
                }
                XCell::FormulaNum(f) => {
                    d.extend_from_slice(&f.to_le_bytes());
                    d.extend_from_slice(&[0u8; 6]);
                    out.extend(rec(0x0006, &d));
                }
            }
        }
        out.extend(rec(0x000A, &[]));
    }
    out
}

/// Составной файл OLE (v3, сектор 512 байт): потоки верхнего уровня; потоки меньше 4096 байт —
/// в мини-потоке.
pub fn cfb(streams: &[(&str, Vec<u8>)]) -> Vec<u8> {
    const SS: usize = 512;
    const END: u32 = 0xFFFF_FFFE;
    let mut data: Vec<u8> = Vec::new(); // секторы после заголовка, без FAT
    let mut fat: Vec<u32> = Vec::new();
    let alloc = |bytes: &[u8], data: &mut Vec<u8>, fat: &mut Vec<u32>| -> u32 {
        if bytes.is_empty() {
            return END;
        }
        let start = u32::try_from(fat.len()).unwrap();
        let n = bytes.len().div_ceil(SS);
        for k in 0..n {
            fat.push(if k + 1 == n {
                END
            } else {
                start + k as u32 + 1
            });
        }
        data.extend_from_slice(bytes);
        data.resize(fat.len() * SS, 0);
        start
    };
    // мини-поток
    let mut mini: Vec<u8> = Vec::new();
    let mut minifat: Vec<u32> = Vec::new();
    let mut starts = Vec::new();
    for (_, s) in streams {
        if s.len() < 4096 {
            let start = u32::try_from(minifat.len()).unwrap();
            let n = s.len().div_ceil(64).max(1);
            for k in 0..n {
                minifat.push(if k + 1 == n {
                    END
                } else {
                    start + k as u32 + 1
                });
            }
            mini.extend_from_slice(s);
            mini.resize(minifat.len() * 64, 0);
            starts.push(start);
        } else {
            starts.push(alloc(s, &mut data, &mut fat));
        }
    }
    let mini_start = alloc(&mini, &mut data, &mut fat);
    let minifat_bytes: Vec<u8> = minifat.iter().flat_map(|v| v.to_le_bytes()).collect();
    let minifat_start = alloc(&minifat_bytes, &mut data, &mut fat);
    // каталог: корень и потоки — правая цепочка братьев от ребёнка корня
    let entry = |name: &str, kind: u8, right: u32, child: u32, start: u32, size: u32| {
        let mut e = vec![0u8; 128];
        let units = utf16(name);
        e[..units.len()].copy_from_slice(&units);
        e[0x40..0x42].copy_from_slice(&u16::try_from(units.len() + 2).unwrap().to_le_bytes());
        e[0x42] = kind;
        e[0x43] = 1;
        e[0x44..0x48].copy_from_slice(&u32::MAX.to_le_bytes());
        e[0x48..0x4C].copy_from_slice(&right.to_le_bytes());
        e[0x4C..0x50].copy_from_slice(&child.to_le_bytes());
        e[0x74..0x78].copy_from_slice(&start.to_le_bytes());
        e[0x78..0x7C].copy_from_slice(&size.to_le_bytes());
        e
    };
    let mut dir = entry(
        "Root Entry",
        5,
        u32::MAX,
        if streams.is_empty() { u32::MAX } else { 1 },
        mini_start,
        u32::try_from(mini.len()).unwrap(),
    );
    for (i, (name, s)) in streams.iter().enumerate() {
        let right = if i + 1 < streams.len() {
            i as u32 + 2
        } else {
            u32::MAX
        };
        dir.extend(entry(
            name,
            2,
            right,
            u32::MAX,
            starts[i],
            u32::try_from(s.len()).unwrap(),
        ));
    }
    let dir_start = alloc(&dir, &mut data, &mut fat);
    // FAT и DIFAT в конце: n секторов FAT и d секторов DIFAT покрывают и себя
    let per = SS / 4;
    let difat_of = |n: usize| n.saturating_sub(109).div_ceil(per - 1);
    let mut n = 1;
    while fat.len() + n + difat_of(n) > n * per {
        n += 1;
    }
    let d = difat_of(n);
    let fat_first = u32::try_from(fat.len()).unwrap();
    let difat_first = fat_first + u32::try_from(n).unwrap();
    fat.extend(std::iter::repeat_n(0xFFFF_FFFD, n));
    fat.extend(std::iter::repeat_n(0xFFFF_FFFC, d));
    fat.resize(n * per, 0xFFFF_FFFF);
    let fat_ids: Vec<u32> = (0..n as u32).map(|i| fat_first + i).collect();
    let mut difat: Vec<u32> = Vec::new();
    for k in 0..d {
        let from = 109 + k * (per - 1);
        let ids = &fat_ids[from.min(n)..(from + per - 1).min(n)];
        difat.extend_from_slice(ids);
        difat.resize((k + 1) * per - 1, 0xFFFF_FFFF);
        difat.push(if k + 1 < d {
            difat_first + k as u32 + 1
        } else {
            END
        });
    }
    let mut h = vec![0u8; SS];
    h[..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
    h[0x18..0x1A].copy_from_slice(&0x3Eu16.to_le_bytes());
    h[0x1A..0x1C].copy_from_slice(&3u16.to_le_bytes());
    h[0x1C..0x1E].copy_from_slice(&0xFFFEu16.to_le_bytes());
    h[0x1E..0x20].copy_from_slice(&9u16.to_le_bytes());
    h[0x20..0x22].copy_from_slice(&6u16.to_le_bytes());
    h[0x2C..0x30].copy_from_slice(&u32::try_from(n).unwrap().to_le_bytes());
    h[0x30..0x34].copy_from_slice(&dir_start.to_le_bytes());
    h[0x38..0x3C].copy_from_slice(&4096u32.to_le_bytes());
    h[0x3C..0x40].copy_from_slice(&minifat_start.to_le_bytes());
    let nmini = u32::try_from(minifat_bytes.len().div_ceil(SS)).unwrap();
    h[0x40..0x44].copy_from_slice(&nmini.to_le_bytes());
    let first_difat = if d > 0 { difat_first } else { END };
    h[0x44..0x48].copy_from_slice(&first_difat.to_le_bytes());
    h[0x48..0x4C].copy_from_slice(&u32::try_from(d).unwrap().to_le_bytes());
    for i in 0..109 {
        let v = fat_ids.get(i).copied().unwrap_or(u32::MAX);
        h[0x4C + i * 4..0x50 + i * 4].copy_from_slice(&v.to_le_bytes());
    }
    let mut out = h;
    out.extend(data);
    out.extend(fat.iter().flat_map(|v| v.to_le_bytes()));
    out.extend(difat.iter().flat_map(|v| v.to_le_bytes()));
    out
}

/// XLS: книга BIFF8 в составном файле.
pub fn xls(sheets: &[XSheet<'_>], sst: &[String]) -> Vec<u8> {
    cfb(&[("Workbook", biff8(sheets, sst))])
}
