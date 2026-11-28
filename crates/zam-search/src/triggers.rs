//! Сопоставление триггеров insight (§10.3; M7.5 §8.9 п. 2) — одно для пакета под задачу `brief`
//! сервера и хука `pre-tool` клиента (`zam`):
//!
//! - `file_glob` ([`glob_matches`]) — маска пути `globset` (тот же диалект, что у проверки
//!   триггеров `zam_store::validate_trigger`) по путям относительно корня решения с `/`; регистр —
//!   по выбору вызывающего (клиент — без учёта на Windows, сервер в `brief` — всегда без учёта:
//!   ОС клиента ему неизвестна);
//! - `keyword` ([`Haystack::has_keyword`], [`keyword_matches`]) — слово или фраза текста целиком,
//!   без учёта регистра (`ё` = `е`), любые пробелы между словами фразы;
//! - пути файлов из текста задания ([`text_paths`]) — слова с `/` или `\` и расширением.
//!
//! `command_regex` и `error_regex` сюда не входят: первый сопоставляет только клиент с командой
//! инструмента, второй — сервер с текстом ошибки (`super::recall::error_regex_matches`).

use globset::GlobBuilder;

/// Путей из текста задания не больше (как путей пакета `brief` для индекса ссылок).
pub const TEXT_PATHS_MAX: usize = 32;

/// Маска `file_glob` совпадает хотя бы с одним путём `paths`; `case_insensitive` — без учёта
/// регистра. Неверная маска — `Err` (вызывающий пишет её в свой журнал); пустой `paths` —
/// `Ok(false)` без сборки маски.
pub fn glob_matches<S: AsRef<str>>(
    pattern: &str,
    paths: &[S],
    case_insensitive: bool,
) -> Result<bool, globset::Error> {
    if paths.is_empty() {
        return Ok(false);
    }
    let m = GlobBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .build()?
        .compile_matcher();
    Ok(paths.iter().any(|p| m.is_match(p.as_ref())))
}

/// Текст для поиска ключевых слов: нижний регистр, `ё` → `е`, пробельные символы — одним
/// пробелом. Строится один раз на текст: ключей у решения бывают сотни.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Haystack(String);

impl Haystack {
    pub fn new(text: &str) -> Self {
        Haystack(norm(text))
    }

    /// Ключевое слово или фраза `keyword` встречается целиком: у края ключа, который — буква,
    /// цифра или `_`, соседний символ текста — не буква, не цифра и не `_` (или край текста).
    /// Пустой ключ не совпадает ни с чем.
    pub fn has_keyword(&self, keyword: &str) -> bool {
        let key = norm(keyword);
        let (Some(first), Some(last)) = (key.chars().next(), key.chars().next_back()) else {
            return false;
        };
        let hay = self.0.as_str();
        let mut from = 0usize;
        while let Some(i) = hay[from..].find(&key) {
            let (s, e) = (from + i, from + i + key.len());
            let before =
                !word_char(first) || hay[..s].chars().next_back().is_none_or(|c| !word_char(c));
            let after = !word_char(last) || hay[e..].chars().next().is_none_or(|c| !word_char(c));
            if before && after {
                return true;
            }
            from = s + first.len_utf8();
        }
        false
    }
}

/// [`Haystack::has_keyword`] для одного ключа.
pub fn keyword_matches(keyword: &str, text: &str) -> bool {
    Haystack::new(text).has_keyword(keyword)
}

/// Символ слова: буква, цифра или `_`.
fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Нижний регистр, `ё` → `е`, пробелы по краям сняты, внутри — одним пробелом.
fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        for l in c.to_lowercase() {
            out.push(if l == 'ё' { 'е' } else { l });
        }
    }
    out
}

/// Знаки, отделяющие слова текста задания (кроме пробелов): кавычки, скобки, перечисление.
fn separator(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '`' | '"'
                | '\''
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '<'
                | '>'
                | ','
                | ';'
                | '«'
                | '»'
        )
}

/// Пути файлов в тексте задания (M7.5 §8.9 п. 2): слова с `/` или `\` и расширением у последней
/// части (`src/a.rs`, `Services\Runner\Demo.cs`, `cfg/.env`) — без знаков препинания в конце, без
/// суффикса `:<строка>[:<столбец>]`, с `/` вместо `\` и без ведущего `./`; без повторов, до
/// [`TEXT_PATHS_MAX`]. Маски триггеров — от корня решения, поэтому путь не достраивается.
pub fn text_paths(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in text.split(separator) {
        let mut w = w.trim_end_matches(['.', ':', '!', '?']);
        for _ in 0..2 {
            if let Some((a, b)) = w.rsplit_once(':')
                && !b.is_empty()
                && b.chars().all(|c| c.is_ascii_digit())
            {
                w = a;
            }
        }
        if !w.contains(['/', '\\']) {
            continue;
        }
        let p = w.replace('\\', "/");
        let p = p.trim_start_matches("./");
        let last = p.rsplit('/').next().unwrap_or_default();
        let has_ext = last
            .rsplit_once('.')
            .is_some_and(|(_, ext)| !ext.is_empty() && ext.chars().all(char::is_alphanumeric));
        if !has_ext || out.iter().any(|o| o == p) {
            continue;
        }
        out.push(p.to_string());
        if out.len() >= TEXT_PATHS_MAX {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_like_globset_with_case() {
        let paths = ["Services/StrategyRunner/Strategy/DemoStrategy.cs"];
        assert!(glob_matches("**/DemoStrategy.cs", &paths, false).unwrap());
        assert!(glob_matches("Services/StrategyRunner/**", &paths, false).unwrap());
        assert!(!glob_matches("**/demostrategy.cs", &paths, false).unwrap());
        assert!(glob_matches("**/demostrategy.cs", &paths, true).unwrap());
        // умолчание `globset` (как у `pre-tool` с M4): `*` проходит и через `/`
        assert!(glob_matches("*.cs", &paths, false).unwrap());
        assert!(!glob_matches("**/*.rs", &paths, true).unwrap());
        let none: [&str; 0] = [];
        assert!(!glob_matches("**", &none, false).unwrap());
        assert!(glob_matches("a[b", &paths, false).is_err());
    }

    #[test]
    fn keyword_whole_word_or_phrase() {
        let t = "Проверить Комиссию ES и комиссия\tв   бэктесте; CRLF-строки, C++ и .env";
        let h = Haystack::new(t);
        assert!(h.has_keyword("комиссия"));
        assert!(
            h.has_keyword("КОМИССИЯ В бэктесте"),
            "фраза через любые пробелы"
        );
        assert!(h.has_keyword("crlf"));
        assert!(h.has_keyword("c++"));
        assert!(h.has_keyword(".env"));
        assert!(!h.has_keyword("комисси"), "не часть слова");
        assert!(!h.has_keyword("тесте"), "не хвост слова");
        assert!(!h.has_keyword("  "));
        assert!(keyword_matches("ёлка", "Елка и ёж"));
        assert!(!keyword_matches("es", "testes"));
        assert!(keyword_matches("es", "ES"));
        assert!(
            keyword_matches("lf", "crlf и lf"),
            "второе вхождение — целое слово"
        );
    }

    #[test]
    fn paths_from_task_text() {
        let t = "Поправь `crates/cache/src/lib.rs:12:4`, Services\\Runner\\Demo.cs и (./docs/plan.md). \
                 Не пути: v1.2/notes, a/b, DemoStrategy.cs, https://example.com/x; ещё cfg/.env, \
                 crates/cache/src/lib.rs.";
        assert_eq!(
            text_paths(t),
            [
                "crates/cache/src/lib.rs",
                "Services/Runner/Demo.cs",
                "docs/plan.md",
                "cfg/.env",
            ]
        );
        let many: String = (0..40).map(|i| format!("d/f{i}.rs ")).collect();
        assert_eq!(text_paths(&many).len(), TEXT_PATHS_MAX);
    }
}
