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

/// Невидимые символы и символы управления форматом (диапазоны включительно): мягкий перенос,
/// нулевая ширина, метки и переопределения направления письма (bidi), разделители строк и
/// абзацев, селекторы начертания, пустые «заполнители», управление стенографией, теги Unicode. Человек их не видит, а
/// разборщик, терминал или буфер обмена — видят: ими прячут схему ссылки и подменяют вид пути.
/// Один список на проект: санитайзер HTML (`zam-markdown`) и «копировать путь» WebUI
/// (`web/src/lib/reportLinks.ts`); согласие держит набор `tests/fixtures/invisible_chars.json`,
/// который читают тест этого модуля и тест WebUI.
pub const INVISIBLE_RANGES: [(u32, u32); 17] = [
    (0x00AD, 0x00AD),
    (0x034F, 0x034F),
    (0x061C, 0x061C),
    (0x115F, 0x1160),
    (0x17B4, 0x17B5),
    (0x180B, 0x180F),
    (0x200B, 0x200F),
    (0x2028, 0x202E),
    (0x2060, 0x206F),
    (0x3164, 0x3164),
    (0xFE00, 0xFE0F),
    (0xFEFF, 0xFEFF),
    (0xFFA0, 0xFFA0),
    (0xFFF9, 0xFFFB),
    (0x1BCA0, 0x1BCA3),
    (0x1D173, 0x1D17A),
    (0xE0000, 0xE0FFF),
];

/// Символ из [`INVISIBLE_RANGES`]. Управляющие C0/C1 сюда не входят — их проверяют отдельно
/// (`char::is_control`).
pub fn is_invisible(c: char) -> bool {
    let c = c as u32;
    INVISIBLE_RANGES.iter().any(|&(a, b)| (a..=b).contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Общий с WebUI набор (`web/test/reportLinks.test.ts`): таблица диапазонов совпадает с
    /// фикстурой, примеры — по обе стороны.
    #[test]
    fn invisible_ranges_match_shared_fixture() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/invisible_chars.json")).unwrap();
        let ranges: Vec<(u32, u32)> = v["ranges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r[0].as_u64().unwrap() as u32, r[1].as_u64().unwrap() as u32))
            .collect();
        assert_eq!(ranges, INVISIBLE_RANGES);
        assert!(ranges.windows(2).all(|w| w[0].1 < w[1].0), "по возрастанию");
        let chars = |k: &str| -> Vec<char> {
            v[k].as_array()
                .unwrap()
                .iter()
                .map(|c| char::from_u32(c.as_u64().unwrap() as u32).unwrap())
                .collect()
        };
        assert!(chars("invisible").len() >= 20 && chars("visible").len() >= 10);
        for c in chars("invisible") {
            assert!(is_invisible(c), "U+{:04X}", c as u32);
        }
        for c in chars("visible") {
            assert!(!is_invisible(c), "U+{:04X}", c as u32);
        }
    }

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
