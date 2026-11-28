//! Посторонние вопросы ночной проверки (M7.3 §6.4 п. 4): набор `eval/check-negative.toml`,
//! встроенный в сервер, — 30 вопросов ru/en вне тем памяти (быт, спорт, география). Общий для всех
//! пользователей: перед прогоном каждая база сверяется с набором
//! ([`zam_store::probes::sync_negatives`]), пробы вида `negative` без пространства и целей.
//!
//! Смена набора меняет `check_version` отпечатка прогона ([`super::CHECK_VERSION`]): числа до и
//! после несопоставимы. Отпечаток файла закреплён [`NEGATIVES_HASH`] — тест
//! `negatives_hash_pinned` не даст поменять набор, не подняв версию.

use std::sync::OnceLock;

use serde::Deserialize;
use zam_search::eval::{Split, probe_split};

/// Файл набора.
pub const NEGATIVES_TOML: &str = include_str!("../../../../eval/check-negative.toml");
/// Первые 16 знаков blake3 файла набора при [`super::CHECK_VERSION`] = 1.
pub const NEGATIVES_HASH: &str = "7652f205d67d1bd6";
/// Вопросов в наборе.
pub const NEGATIVES_COUNT: usize = 30;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    question: Vec<Question>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Question {
    lang: String,
    text: String,
}

/// Вопросы набора (текст, язык) — разбираются один раз.
pub fn questions() -> &'static [(String, String)] {
    static SET: OnceLock<Vec<(String, String)>> = OnceLock::new();
    SET.get_or_init(|| match toml::from_str::<File>(NEGATIVES_TOML) {
        Ok(f) => f
            .question
            .into_iter()
            .map(|q| (q.text.trim().to_string(), q.lang))
            .collect(),
        Err(e) => {
            // набор встроен и проверен тестом; сломанный файл — без посторонних вопросов
            tracing::error!(error = %e, "проверка памяти: набор посторонних вопросов не разобран");
            Vec::new()
        }
    })
}

/// Набор для базы: вопрос и часть ([`probe_split`] по тексту вопроса — у постороннего вопроса нет
/// цели).
pub fn set() -> Vec<(String, Split)> {
    questions()
        .iter()
        .map(|(q, _)| (q.clone(), probe_split(q)))
        .collect()
}

/// Отпечаток файла набора: первые 16 знаков blake3.
pub fn file_hash() -> String {
    blake3::hash(NEGATIVES_TOML.as_bytes()).to_hex()[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §6.4 п. 4: 30 вопросов, по 15 ru и en, без повторов и пустых.
    #[test]
    fn probes_negative_set_loaded() {
        let q = questions();
        assert_eq!(q.len(), NEGATIVES_COUNT);
        assert_eq!(q.iter().filter(|(_, l)| l == "ru").count(), 15);
        assert_eq!(q.iter().filter(|(_, l)| l == "en").count(), 15);
        let mut norm: Vec<String> = q
            .iter()
            .map(|(t, _)| zam_store::normalize_query(t))
            .collect();
        norm.sort();
        norm.dedup();
        assert_eq!(norm.len(), NEGATIVES_COUNT);
        assert!(q.iter().all(|(t, _)| !t.is_empty()));
        assert_eq!(set().len(), NEGATIVES_COUNT);
    }

    /// Набор закреплён вместе с версией проверки: смена файла — поднять `CHECK_VERSION` и
    /// [`NEGATIVES_HASH`].
    #[test]
    fn negatives_hash_pinned() {
        assert_eq!(
            file_hash(),
            NEGATIVES_HASH,
            "набор eval/check-negative.toml изменён: поднимите CHECK_VERSION и NEGATIVES_HASH"
        );
        assert_eq!(super::super::CHECK_VERSION, 1);
    }
}
