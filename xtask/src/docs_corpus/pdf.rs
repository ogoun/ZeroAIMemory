//! Писатель PDF корпуса: строки ASCII — стандартным шрифтом Helvetica (WinAnsi), прочие (кириллица,
//! CJK, смешанные) — своими шрифтами Type3 с `ToUnicode`: глиф — закрашенный прямоугольник
//! (ширина 600 или 1000 единиц на em), код → символ Unicode — по CMap. Шрифтовых файлов и чужих
//! глифов нет (лицензия, кроссплатформенность), а текстовый слой точный: pdfium и `pdf_oxide`
//! читают символы по `ToUnicode`, их прямоугольники — по процедурам глифов (`d1`).
//!
//! До 255 кодов на шрифт (код 32 — пробел в каждом шрифте); символы назначаются шрифтам по
//! первому появлению в документе — детерминированно.

use std::collections::BTreeMap;

use super::text::is_wide;

/// Строка страницы: левый край и базовая линия (пункты, ось y вверх), кегль, текст.
#[derive(Debug, Clone)]
pub struct Line {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub text: String,
}

/// Страница: строки в порядке потока содержимого, картинка на всю страницу («скан»).
#[derive(Debug, Clone)]
pub struct Page {
    pub lines: Vec<Line>,
    pub image: bool,
    pub w: f32,
    pub h: f32,
}

impl Default for Page {
    fn default() -> Self {
        Page {
            lines: Vec::new(),
            image: false,
            w: PAGE_W,
            h: PAGE_H,
        }
    }
}

/// Документ: страницы и необязательный JavaScript действия открытия (враждебный PDF).
#[derive(Debug, Clone, Default)]
pub struct Doc {
    pub pages: Vec<Page>,
    pub open_action_js: Option<String>,
}

/// A4, пунктов.
pub const PAGE_W: f32 = 595.0;
pub const PAGE_H: f32 = 842.0;

/// Строка — только печатные ASCII (шрифт Helvetica).
fn is_ascii_line(s: &str) -> bool {
    s.bytes().all(|b| (0x20..0x7f).contains(&b))
}

/// Ширина символа Helvetica, доли em (упрощённые классы метрик AFM: оценка для переноса строк).
fn helv_em(c: char) -> f32 {
    match c {
        ' ' => 0.278,
        'i' | 'j' | 'l' | '.' | ',' | ';' | ':' | '\'' | '|' | '!' | 'I' => 0.278,
        'f' | 't' | 'r' | '(' | ')' | '-' => 0.333,
        'm' | 'w' | 'M' | 'W' => 0.89,
        'A'..='Z' => 0.70,
        '0'..='9' => 0.556,
        _ => 0.556,
    }
}

/// Ширина символа шрифта Type3, единиц на 1000 em.
fn t3_units(c: char) -> u32 {
    if c == ' ' {
        300
    } else if is_wide(c) {
        1000
    } else {
        600
    }
}

/// Ширина строки кеглем `size`, пунктов.
pub fn text_width(s: &str, size: f32) -> f32 {
    if is_ascii_line(s) {
        s.chars().map(helv_em).sum::<f32>() * size
    } else {
        s.chars().map(|c| t3_units(c) as f32 / 1000.0).sum::<f32>() * size
    }
}

/// Строка PDF в скобках (WinAnsi, только ASCII).
fn pdf_string(s: &str) -> String {
    let mut out = String::from("(");
    for c in s.chars() {
        if matches!(c, '\\' | '(' | ')') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push(')');
    out
}

/// Шрифты Type3 документа: символ → (шрифт, код).
struct T3Fonts {
    map: BTreeMap<char, (usize, u8)>,
    /// По шрифту: код → символ (без пробела).
    fonts: Vec<Vec<(u8, char)>>,
}

impl T3Fonts {
    fn new(doc: &Doc) -> T3Fonts {
        let mut t = T3Fonts {
            map: BTreeMap::new(),
            fonts: Vec::new(),
        };
        for p in &doc.pages {
            for l in &p.lines {
                if is_ascii_line(&l.text) {
                    continue;
                }
                for c in l.text.chars() {
                    if c == ' ' || t.map.contains_key(&c) || (c as u32) > 0xFFFF {
                        continue;
                    }
                    // коды 33..=255 (32 — пробел каждого шрифта)
                    if t.fonts.last().is_none_or(|f| f.len() >= 223) {
                        t.fonts.push(Vec::new());
                    }
                    let fi = t.fonts.len() - 1;
                    let code = 33 + t.fonts[fi].len() as u8;
                    t.fonts[fi].push((code, c));
                    t.map.insert(c, (fi, code));
                }
            }
        }
        t
    }

    /// Прогоны строки по шрифтам: (шрифт, коды).
    fn runs(&self, s: &str) -> Vec<(usize, Vec<u8>)> {
        let mut out: Vec<(usize, Vec<u8>)> = Vec::new();
        for c in s.chars() {
            let (fi, code) = match c {
                // пробел — в шрифте текущего прогона (или первого)
                ' ' => (out.last().map_or(0, |r| r.0), 32),
                _ => match self.map.get(&c) {
                    Some(&x) => x,
                    None => continue,
                },
            };
            match out.last_mut() {
                Some(r) if r.0 == fi => r.1.push(code),
                _ => out.push((fi, vec![code])),
            }
        }
        out
    }
}

/// CMap `ToUnicode` шрифта.
fn to_unicode(glyphs: &[(u8, char)]) -> String {
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
1 begincodespacerange\n<00> <FF>\nendcodespacerange\n",
    );
    let mut all: Vec<(u8, char)> = vec![(32, ' ')];
    all.extend_from_slice(glyphs);
    for chunk in all.chunks(100) {
        s.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (code, c) in chunk {
            s.push_str(&format!("<{code:02X}> <{:04X}>\n", *c as u32));
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s
}

fn stream(dict_extra: &str, data: &[u8]) -> Vec<u8> {
    let mut s = format!("<< /Length {}{dict_extra} >>\nstream\n", data.len()).into_bytes();
    s.extend_from_slice(data);
    s.extend_from_slice(b"\nendstream");
    s
}

/// Собирает файл из объектов (`objs[i]` — объект `i + 1`) с таблицей xref; корень — `root`.
pub fn assemble(objs: &[Vec<u8>], root: usize) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::with_capacity(objs.len());
    for (k, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", k + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root {root} 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// PDF документа.
pub fn build(doc: &Doc) -> Vec<u8> {
    let t3 = T3Fonts::new(doc);
    // 1 — каталог, 2 — дерево страниц, 3 — Helvetica, 4 — картинка, 5..7 — процедуры глифов
    // (пробел, узкий, широкий), затем по 2 объекта на шрифт Type3, затем JS, затем страницы
    let mut objs: Vec<Vec<u8>> = vec![Vec::new(); 7];
    objs[2] = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
        .to_vec();
    // «скан»: серое изображение 8×8 с шумом, растянутое на страницу
    let px: Vec<u8> = (0u32..64)
        .map(|i| (i.wrapping_mul(37) % 251) as u8)
        .collect();
    objs[3] = stream(
        " /Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceGray \
/BitsPerComponent 8",
        &px,
    );
    objs[4] = stream("", b"300 0 0 0 0 0 d1");
    objs[5] = stream("", b"600 0 40 0 560 700 d1 40 0 520 700 re f");
    objs[6] = stream("", b"1000 0 50 -80 950 860 d1 50 -80 900 940 re f");
    let mut font_objs = Vec::new();
    for glyphs in &t3.fonts {
        let tu = objs.len() + 1;
        objs.push(stream("", to_unicode(glyphs).as_bytes()));
        let last = glyphs.last().map_or(32, |g| g.0);
        let mut widths = vec![300u32];
        let mut diffs = String::from("32 /sp");
        let mut procs = String::from("/sp 5 0 R");
        for code in 33..=last {
            let c = glyphs.iter().find(|g| g.0 == code).map_or('?', |g| g.1);
            let w = t3_units(c);
            widths.push(w);
            diffs.push_str(&format!(" /g{code}"));
            procs.push_str(&format!(" /g{code} {} 0 R", if w == 1000 { 7 } else { 6 }));
        }
        let widths: Vec<String> = widths.iter().map(u32::to_string).collect();
        objs.push(
            format!(
                "<< /Type /Font /Subtype /Type3 /FontBBox [0 -80 1000 940] \
/FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << {procs} >> \
/Encoding << /Type /Encoding /Differences [{diffs}] >> /FirstChar 32 /LastChar {last} \
/Widths [{}] /ToUnicode {tu} 0 R /Resources << >> >>",
                widths.join(" ")
            )
            .into_bytes(),
        );
        font_objs.push(objs.len());
    }
    let js = doc.open_action_js.as_ref().map(|code| {
        objs.push(
            format!(
                "<< /Type /Action /S /JavaScript /JS {} >>",
                pdf_string(code)
            )
            .into_bytes(),
        );
        objs.len()
    });
    let fonts_res: String = std::iter::once("/F1 3 0 R".to_string())
        .chain(
            font_objs
                .iter()
                .enumerate()
                .map(|(i, o)| format!("/T{i} {o} 0 R")),
        )
        .collect::<Vec<_>>()
        .join(" ");
    let first_page = objs.len() + 1;
    for p in &doc.pages {
        let mut content = String::new();
        if p.image {
            content.push_str(&format!("q {:.2} 0 0 {:.2} 0 0 cm /Im1 Do Q\n", p.w, p.h));
        }
        for l in &p.lines {
            content.push_str(&format!("BT 1 0 0 1 {:.2} {:.2} Tm ", l.x, l.y));
            if is_ascii_line(&l.text) {
                content.push_str(&format!("/F1 {:.1} Tf {} Tj", l.size, pdf_string(&l.text)));
            } else {
                for (fi, codes) in t3.runs(&l.text) {
                    let hex: String = codes.iter().map(|c| format!("{c:02X}")).collect();
                    content.push_str(&format!("/T{fi} {:.1} Tf <{hex}> Tj ", l.size));
                }
            }
            content.push_str(" ET\n");
        }
        let page_no = objs.len() + 1;
        objs.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.0} {:.0}] /Resources << \
/Font << {fonts_res} >> /XObject << /Im1 4 0 R >> >> /Contents {} 0 R >>",
                p.w,
                p.h,
                page_no + 1
            )
            .into_bytes(),
        );
        objs.push(stream("", content.as_bytes()));
    }
    let kids: Vec<String> = (0..doc.pages.len())
        .map(|i| format!("{} 0 R", first_page + 2 * i))
        .collect();
    objs[1] = format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        doc.pages.len()
    )
    .into_bytes();
    objs[0] = match js {
        Some(j) => format!("<< /Type /Catalog /Pages 2 0 R /OpenAction {j} 0 R >>"),
        None => "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
    }
    .into_bytes();
    assemble(&objs, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type3_fonts_cover_all_chars_once() {
        let doc = Doc {
            pages: vec![Page {
                lines: vec![
                    Line {
                        x: 72.0,
                        y: 700.0,
                        size: 12.0,
                        text: "Привет мир 仓库".into(),
                    },
                    Line {
                        x: 72.0,
                        y: 680.0,
                        size: 12.0,
                        text: "plain ascii".into(),
                    },
                ],
                ..Page::default()
            }],
            open_action_js: None,
        };
        let t = T3Fonts::new(&doc);
        // П р и в е т м 仓 库 — 9 различных без пробела («р» и «и» повторяются)
        assert_eq!(t.map.len(), 9);
        let runs = t.runs("Привет мир");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].1.len(), 10);
        let pdf = build(&doc);
        assert!(pdf.starts_with(b"%PDF-1.7"));
        assert!(pdf.ends_with(b"%%EOF\n"));
    }
}
