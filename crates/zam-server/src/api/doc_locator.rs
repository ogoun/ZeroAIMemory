//! Показ адреса части вложения (`locator`) для `/read` и карточки поиска (M5, задача 7).
//!
//! Имя листа — текст документа и проходит сканер секретов. У двух листов с секретами маска
//! одинакова, поэтому лист, имя которого [`mask_text`] меняет, показывается **по номеру**:
//! `sheet:#N!A2:B2`, подпись `лист №N «‹маска›» A2:B2` (раунды правок 1–2 задачи 7). Номер в
//! чанках не хранится и выводится из порядка листов среди единиц документа ([`Sheets::of`]):
//! листы идут в порядке книги, блоки листа — подряд; лист без единиц (пустой) номера не получает.
//! `/read` принимает `sheet:#N` и переводит его в имя ([`Sheets::resolve`]).
use zam_docs::Locator;
use zam_markdown::mask_text;

/// Листы документа по номеру с 1.
pub struct Sheets {
    /// Имена листов по номеру (`names[N - 1]`).
    names: Vec<String>,
}

impl Sheets {
    /// Листы из сырых `locator` чанков документа в порядке чанков (повторы и не листы
    /// пропускаются): `/read` — все чанки, поиск — `zam_store::doc_sheet_locators`.
    pub fn of<'a>(locators: impl IntoIterator<Item = &'a str>) -> Sheets {
        let mut names: Vec<String> = Vec::new();
        for raw in locators {
            if let Ok(Locator::Sheet { name, .. }) = Locator::parse(raw)
                && !names.contains(&name)
            {
                names.push(name);
            }
        }
        Sheets { names }
    }

    /// Номер листа `name` (с 1); нет такого — `None`.
    pub fn no(&self, name: &str) -> Option<u32> {
        let i = self.names.iter().position(|n| n == name)?;
        u32::try_from(i + 1).ok()
    }

    /// Адрес запроса в сырой форме чанков: `sheet:#N` — лист N по имени; номер вне документа —
    /// `None` (части нет).
    pub fn resolve(&self, l: &Locator) -> Option<Locator> {
        match l {
            Locator::SheetNo { no, range } => {
                let name = self.names.get(usize::try_from(*no).ok()?.checked_sub(1)?)?;
                Some(Locator::Sheet {
                    name: name.clone(),
                    range: range.clone(),
                })
            }
            other => Some(other.clone()),
        }
    }

    /// Показ адреса и подпись ([`shown`] с номером листа из этого документа).
    pub fn shown(&self, l: &Locator) -> (String, String) {
        let no = match l {
            Locator::Sheet { name, .. } => self.no(name),
            _ => None,
        };
        shown(l, no)
    }
}

/// Имя листа меняется сканером секретов — такой лист показывается по номеру.
pub fn secret_sheet(l: &Locator) -> bool {
    matches!(l, Locator::Sheet { name, .. } if mask_text(name) != *name)
}

/// Показ адреса (`locator`) и подпись (`locator_text`): лист с секретом в имени и известным
/// номером `no` — `sheet:#N!…` и `лист №N «‹маска›» …`; прочие — как есть, через сканер секретов.
pub fn shown(l: &Locator, no: Option<u32>) -> (String, String) {
    if let (Locator::Sheet { name, range }, Some(no)) = (l, no)
        && secret_sheet(l)
    {
        return (
            format!("sheet:#{no}!{range}"),
            format!("лист №{no} «{}» {range}", mask_text(name)),
        );
    }
    (mask_text(&l.render()), mask_text(&l.label()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Номера — порядок листов среди единиц; лист с секретом — по номеру, обычный — по имени.
    #[test]
    fn sheets_numbered_by_unit_order() {
        let s = Sheets::of([
            "sheet:Итоги!A2:B5",
            "sheet:Итоги!A6:B9",
            "sheet:AKIAIOSFODNN7EXAMPLE!A2:B2",
            "page:1",
            "sheet:AKIAJ2345678ABCDEFGH!A2:B2",
        ]);
        let p = |s: &str| Locator::parse(s).unwrap();
        assert_eq!(s.no("Итоги"), Some(1));
        assert_eq!(s.no("AKIAJ2345678ABCDEFGH"), Some(3));
        let (a, la) = s.shown(&p("sheet:AKIAIOSFODNN7EXAMPLE!A2:B2"));
        let (b, _) = s.shown(&p("sheet:AKIAJ2345678ABCDEFGH!A2:B2"));
        assert_eq!(
            (a.as_str(), b.as_str()),
            ("sheet:#2!A2:B2", "sheet:#3!A2:B2")
        );
        assert!(la.starts_with("лист №2 «") && !la.contains("AKIA"), "{la}");
        assert_eq!(s.shown(&p("sheet:Итоги!A2:B5")).0, "sheet:Итоги!A2:B5");
        assert_eq!(
            s.resolve(&p("sheet:#3!A1")),
            Some(p("sheet:AKIAJ2345678ABCDEFGH!A1"))
        );
        assert_eq!(s.resolve(&p("sheet:#4!A1")), None);
        // номер неизвестен — маска (как раньше)
        assert!(
            !shown(&p("sheet:AKIAIOSFODNN7EXAMPLE!A1"), None)
                .0
                .contains("AKIA")
        );
    }
}
