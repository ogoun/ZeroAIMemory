//! Адрес единицы в документе: `page:12`, `slide:3`, `sheet:Итоги!A1:F40`, `para:40`.

use crate::DocError;

/// Адрес единицы в документе (спецификация M5 §1, §4).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Locator {
    /// Страница PDF, с 1.
    Page(u32),
    /// Слайд PPTX по порядку показа, с 1.
    Slide(u32),
    /// Лист и диапазон `A1:F40` (или одна ячейка `A1`).
    Sheet { name: String, range: String },
    /// Абзац тела DOCX (или текста), с 1.
    Para(u32),
}

impl Locator {
    /// Строковая форма для API и чанков.
    pub fn render(&self) -> String {
        match self {
            Locator::Page(n) => format!("page:{n}"),
            Locator::Slide(n) => format!("slide:{n}"),
            // имя, начинающееся с кавычки, — в кавычках, чтобы разбор вернул его как есть
            Locator::Sheet { name, range } if name.starts_with('\'') => {
                format!("sheet:'{}'!{range}", name.replace('\'', "''"))
            }
            Locator::Sheet { name, range } => format!("sheet:{name}!{range}"),
            Locator::Para(n) => format!("para:{n}"),
        }
    }

    /// Подпись для человека и агента (карточка поиска, M5 §3): `стр. 12`, `лист «Итоги» A1:F40`,
    /// `слайд 3`, `абз. 40`.
    pub fn label(&self) -> String {
        match self {
            Locator::Page(n) => format!("стр. {n}"),
            Locator::Slide(n) => format!("слайд {n}"),
            Locator::Sheet { name, range } => format!("лист «{name}» {range}"),
            Locator::Para(n) => format!("абз. {n}"),
        }
    }

    /// Разбор строковой формы; имя листа может содержать `!` (берётся последний)
    /// и быть в одинарных кавычках, как в формулах (`'Итоги 2026'!A1`).
    pub fn parse(s: &str) -> Result<Self, DocError> {
        let bad = |why: &str| DocError::Malformed(format!("locator {s:?}: {why}"));
        let (kind, rest) = s.split_once(':').ok_or_else(|| bad("нет вида"))?;
        let num = |r: &str| match r.parse::<u32>() {
            Ok(n) if n >= 1 && !r.starts_with('+') => Ok(n),
            _ => Err(bad("номер — целое от 1")),
        };
        match kind {
            "page" => Ok(Locator::Page(num(rest)?)),
            "slide" => Ok(Locator::Slide(num(rest)?)),
            "para" => Ok(Locator::Para(num(rest)?)),
            "sheet" => {
                let (name, range) = rest.rsplit_once('!').ok_or_else(|| bad("нет диапазона"))?;
                let name = match name.strip_prefix('\'').and_then(|n| n.strip_suffix('\'')) {
                    Some(q) => q.replace("''", "'"),
                    None => name.to_string(),
                };
                if name.is_empty() {
                    return Err(bad("пустое имя листа"));
                }
                let range = range.to_ascii_uppercase();
                let ok = match range.split_once(':') {
                    Some((a, b)) => is_cell(a) && is_cell(b),
                    None => is_cell(&range),
                };
                if !ok {
                    return Err(bad("диапазон вида A1:F40"));
                }
                Ok(Locator::Sheet { name, range })
            }
            _ => Err(bad("вид page|slide|sheet|para")),
        }
    }
}

/// Прямоугольник диапазона листа: столбцы и строки с 1, `(c1, r1, c2, r2)`, углы упорядочены.
type Rect = (u32, u32, u32, u32);

impl Locator {
    /// Адреса пересекаются (`/read` по `locator`, M5 §4): страница, слайд, абзац — равны; лист —
    /// то же имя и пересечение диапазонов (`A2:B2` попадает в блок `A1:F40`).
    pub fn overlaps(&self, other: &Locator) -> bool {
        match (self, other) {
            (Locator::Sheet { name: a, range: ra }, Locator::Sheet { name: b, range: rb }) => {
                a == b
                    && match (rect(ra), rect(rb)) {
                        (Some(x), Some(y)) => x.0 <= y.2 && y.0 <= x.2 && x.1 <= y.3 && y.1 <= x.3,
                        _ => false,
                    }
            }
            _ => self == other,
        }
    }

    /// Диапазон листа, покрывающий оба (`A1:B5` и `A6:B9` → `A1:B9`); не листы или разные
    /// листы — `None`.
    pub fn sheet_union(&self, other: &Locator) -> Option<Locator> {
        let (Locator::Sheet { name: a, range: ra }, Locator::Sheet { name: b, range: rb }) =
            (self, other)
        else {
            return None;
        };
        if a != b {
            return None;
        }
        let (x, y) = (rect(ra)?, rect(rb)?);
        let (c1, r1, c2, r2) = (x.0.min(y.0), x.1.min(y.1), x.2.max(y.2), x.3.max(y.3));
        Some(Locator::Sheet {
            name: a.clone(),
            range: format!("{}{r1}:{}{r2}", col_letters(c1), col_letters(c2)),
        })
    }
}

/// Ячейка `B12` → (столбец с 1, строка с 1).
fn cell(s: &str) -> Option<(u32, u32)> {
    let letters = s.bytes().take_while(u8::is_ascii_uppercase).count();
    let mut col = 0u32;
    for b in s[..letters].bytes() {
        col = col * 26 + u32::from(b - b'A' + 1);
    }
    let row = s[letters..].parse().ok()?;
    (letters > 0).then_some((col, row))
}

/// Диапазон `A1:F40` или ячейка `A1` → прямоугольник.
fn rect(range: &str) -> Option<Rect> {
    let (a, b) = range.split_once(':').unwrap_or((range, range));
    let ((c1, r1), (c2, r2)) = (cell(a)?, cell(b)?);
    Some((c1.min(c2), r1.min(r2), c1.max(c2), r1.max(r2)))
}

/// Буквы столбца с 1 (`1` → `A`, `27` → `AA`).
fn col_letters(mut c: u32) -> String {
    let mut out = Vec::new();
    while c > 0 {
        c -= 1;
        out.push(b'A' + (c % 26) as u8);
        c /= 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Ячейка `A1`: 1–3 латинские буквы и номер строки от 1.
fn is_cell(s: &str) -> bool {
    let letters = s.bytes().take_while(u8::is_ascii_uppercase).count();
    let digits = &s[letters..];
    (1..=3).contains(&letters)
        && !digits.is_empty()
        && !digits.starts_with('0')
        && digits.len() <= 7
        && digits.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Подпись `locator` в карточке выдачи (задача 6 M5): страница, лист и диапазон, слайд, абзац.
    #[test]
    fn locator_label_for_cards() {
        let l = |s: &str| Locator::parse(s).unwrap().label();
        assert_eq!(l("page:12"), "стр. 12");
        assert_eq!(l("sheet:Итоги!A1:F40"), "лист «Итоги» A1:F40");
        assert_eq!(l("sheet:'Q1 ''26'!B2"), "лист «Q1 '26» B2");
        assert_eq!(l("slide:3"), "слайд 3");
        assert_eq!(l("para:40"), "абз. 40");
    }

    /// Пересечение адресов для `/read` (задача 7 M5) и объединение диапазонов листа.
    #[test]
    fn locator_overlaps_and_sheet_union() {
        let p = |s: &str| Locator::parse(s).unwrap();
        assert!(p("page:2").overlaps(&p("page:2")));
        assert!(!p("page:2").overlaps(&p("page:3")));
        assert!(!p("page:2").overlaps(&p("slide:2")));
        let block = p("sheet:Итоги!A1:F40");
        for yes in [
            "sheet:Итоги!A1:F40",
            "sheet:Итоги!B2",
            "sheet:Итоги!F40:Z99",
        ] {
            assert!(block.overlaps(&p(yes)), "{yes}");
            assert!(p(yes).overlaps(&block), "{yes}");
        }
        for no in ["sheet:Итоги!G1:H2", "sheet:Итоги!A41", "sheet:Склад!A1"] {
            assert!(!block.overlaps(&p(no)), "{no}");
        }
        assert!(p("sheet:L!AA10:AB12").overlaps(&p("sheet:L!Z11:AA11")));
        assert_eq!(
            p("sheet:Итоги!A1:B5").sheet_union(&p("sheet:Итоги!A6:C9")),
            Some(p("sheet:Итоги!A1:C9"))
        );
        assert_eq!(
            p("sheet:L!Y1:Z2").sheet_union(&p("sheet:L!AA3:AB4")),
            Some(p("sheet:L!Y1:AB4"))
        );
        assert_eq!(p("sheet:A!A1").sheet_union(&p("sheet:B!A1")), None);
        assert_eq!(p("page:1").sheet_union(&p("page:2")), None);
    }
}
