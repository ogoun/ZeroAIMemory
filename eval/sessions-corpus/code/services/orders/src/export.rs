//! Выгрузка заказов партнёрам в CSV (фикстура карты кода ТС-2, задача 4 M6).

use crate::pagination::{KeysetCursor, Page};

/// Строка выгрузки.
pub struct OrderRow {
    pub id: i64,
    pub created_at: String,
    pub total_cents: i64,
}

/// Потоковая выгрузка: страницы keyset-пагинации пишутся в `out` без сборки файла в памяти.
pub fn export_csv<W: std::io::Write>(
    out: &mut W,
    mut next_page: impl FnMut(Option<KeysetCursor>) -> Page<OrderRow>,
) -> std::io::Result<usize> {
    writeln!(out, "id,created_at,total_cents")?;
    let mut cursor = None;
    let mut rows = 0;
    loop {
        let page = next_page(cursor);
        for r in &page.items {
            writeln!(out, "{},{},{}", r.id, r.created_at, r.total_cents)?;
            rows += 1;
        }
        match page.next {
            Some(c) => cursor = Some(c),
            None => return Ok(rows),
        }
    }
}
