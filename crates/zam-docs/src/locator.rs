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
}
