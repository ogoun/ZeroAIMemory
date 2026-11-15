//! Keyset-пагинация по `(created_at, id)` (фикстура карты кода ТС-2, задача 4 M6).

/// Позиция после последней строки страницы.
#[derive(Clone, Debug, PartialEq)]
pub struct KeysetCursor {
    pub created_at: String,
    pub id: i64,
}

/// Страница выдачи и курсор следующей.
pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<KeysetCursor>,
}

/// Условие SQL следующей страницы.
pub fn after_clause(c: &KeysetCursor) -> String {
    format!("(created_at, id) > ('{}', {})", c.created_at, c.id)
}

/// Предел строк страницы: не больше 1000.
pub fn page_limit(requested: usize) -> usize {
    requested.clamp(1, 1000)
}
