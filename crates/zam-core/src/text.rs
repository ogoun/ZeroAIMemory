//! Общие правила слов текста без зависимостей: стоп-список ru/en (границы разделов чанкера
//! `zam-markdown`, совпадение слов подсказок `zam-search`). Перенесён из
//! `zam-markdown::boundary` побайтово (задача 9 M6), чтобы `zam-search` не тянул `zam-markdown`.

use std::collections::HashSet;
use std::sync::OnceLock;

/// Стоп-слова ru/en через пробел — служебные слова. Мешку основ границ нужны слова от
/// 4 символов (короткие он отбрасывает до проверки); трёхбуквенные — для подтверждения
/// подсказок-идей сервера ([`is_stop_word`], финальное ревью M5.7).
pub const STOP_WORDS: &str = concat!(
    "that this with from have were which when they them their there also into been what ",
    "then than will would could should after before about because while where these those ",
    "only other этот этой этого этом эти этих который которая которое которые которых были ",
    "было была если когда тоже также чтобы потому только после через между перед более ",
    "менее очень даже уже ещё еще всех всего всё весь свой своих можно нужно есть ",
    "the and for not are was you its our who how why can has had any all but ",
    "что как для при или его она они так был без над под про это эта все там тут где чем ",
);

/// Служебное ли слово (ru/en, в нижнем регистре) — общий стоп-список границ разделов,
/// подтверждения подсказок-идей и совпадения слов `post_tool`.
pub fn is_stop_word(w: &str) -> bool {
    // множество строится один раз: линейный разбор строки на каждом слове стоил подсказкам-идеям
    // сервера ~3 мс на `prompt` в отладочной сборке
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| STOP_WORDS.split_whitespace().collect())
        .contains(w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_words_lowercase_only() {
        assert!(is_stop_word("чтобы") && is_stop_word("the") && is_stop_word("ещё"));
        assert!(!is_stop_word("The") && !is_stop_word("changelog") && !is_stop_word(""));
        // повторов в списке нет: множество того же размера, что и список
        let n = STOP_WORDS.split_whitespace().count();
        let set: HashSet<&str> = STOP_WORDS.split_whitespace().collect();
        assert_eq!(set.len(), n);
    }
}
