//! Упаковка выдачи в `budget_tokens` (§9.2 п. 8).
//!
//! Стоимость элемента — токены карточки (путь, заголовок, сниппет или L0) и токены
//! текста (`mode=chunks`: `token_count` чанка или расширенного текста). Элементы берутся
//! по порядку, пока помещаются; текст последнего урезается до остатка бюджета. Хотя бы
//! один элемент возвращается всегда — даже при бюджете 0 (текст тогда пустой).

/// Стоимость элемента в токенах.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cost {
    /// Карточка без текста.
    pub base: usize,
    /// Текст (0 — без текста).
    pub text: usize,
}

/// Сколько текста элемента помещается.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Весь текст (или текста нет).
    Whole,
    /// Только первые `n` токенов текста.
    Truncated(usize),
}

/// Элементы, помещающиеся в `budget` (по порядку `costs`): `Fit` для каждого взятого.
pub fn pack(costs: &[Cost], budget: usize) -> Vec<Fit> {
    let mut used = 0usize;
    let mut out = Vec::new();
    for c in costs {
        let first = out.is_empty();
        if !first && used + c.base > budget {
            break;
        }
        let room = budget.saturating_sub(used + c.base);
        if c.text == 0 || c.text <= room {
            used += c.base + c.text;
            out.push(Fit::Whole);
            continue;
        }
        if !first && room == 0 {
            break;
        }
        // текст урезан: бюджет исчерпан
        out.push(Fit::Truncated(room));
        break;
    }
    out
}

/// Наибольший префикс `text` (по границе символа, с концом строки или пробела, если
/// можно) не длиннее `max` токенов по `count`. Бинарный поиск: `O(log n)` подсчётов.
pub fn truncate_to_tokens<'a>(text: &'a str, max: usize, count: &dyn Fn(&str) -> usize) -> &'a str {
    if count(text) <= max {
        return text;
    }
    let bounds: Vec<usize> = text.char_indices().map(|(i, _)| i).skip(1).collect();
    // наибольший префикс в пределе (число токенов монотонно по длине префикса)
    let (mut lo, mut hi) = (0usize, bounds.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if count(&text[..bounds[mid - 1]]) <= max {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        return "";
    }
    let cut = &text[..bounds[lo - 1]];
    // не рвать слово, если до него есть хоть одна граница
    match cut.rfind(char::is_whitespace) {
        Some(i) if i > 0 && !text[cut.len()..].starts_with(char::is_whitespace) => {
            cut[..i].trim_end()
        }
        _ => cut.trim_end(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(base: usize, text: usize) -> Cost {
        Cost { base, text }
    }

    #[test]
    fn packs_until_budget_and_truncates_last() {
        assert_eq!(
            pack(&[c(10, 50), c(10, 50), c(10, 50)], 150),
            vec![Fit::Whole, Fit::Whole, Fit::Truncated(20)]
        );
        // карточки без текста
        assert_eq!(
            pack(&[c(10, 0), c(10, 0), c(10, 0)], 25),
            vec![Fit::Whole; 2]
        );
        // следующая карточка не влезает целиком — стоп
        assert_eq!(pack(&[c(10, 5), c(10, 5)], 20), vec![Fit::Whole]);
    }

    #[test]
    fn at_least_one_item() {
        assert_eq!(pack(&[c(10, 50), c(1, 1)], 0), vec![Fit::Truncated(0)]);
        assert_eq!(pack(&[c(10, 0), c(1, 0)], 0), vec![Fit::Whole]);
        assert_eq!(pack(&[c(10, 50)], 30), vec![Fit::Truncated(20)]);
        assert!(pack(&[], 100).is_empty());
    }

    #[test]
    fn truncate_by_tokens_on_word_boundary() {
        let words = |s: &str| s.split_whitespace().count();
        assert_eq!(
            truncate_to_tokens("один два три", 5, &words),
            "один два три"
        );
        assert_eq!(truncate_to_tokens("один два три", 2, &words), "один два");
        assert_eq!(truncate_to_tokens("один два три", 0, &words), "");
        // по символам: 1 токен = 1 символ
        let chars = |s: &str| s.chars().count();
        assert_eq!(truncate_to_tokens("ёжик", 2, &chars), "ёж");
        assert_eq!(truncate_to_tokens("аб вг", 4, &chars), "аб");
    }
}
