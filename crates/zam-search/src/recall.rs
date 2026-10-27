//! Проактивное вспоминание `POST /recall` (§10.3) — чистые функции без ввода-вывода:
//! нормализация ошибки инструмента ([`normalize_error`]), сопоставление `error_regex`
//! ([`error_regex_matches`]), покрытие слов ошибки найденным текстом ([`error_terms`],
//! [`term_coverage`], [`coverage_enough`]), строка подсказки ([`hint_line`]) и упаковка
//! дайджеста в бюджет токенов ([`pack_sections`]).
//!
//! Сервер собирает кандидатов (SQLite, поиск), здесь — только то, что от данных не зависит.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Предел нормализованной ошибки, символов (§10.3).
pub const ERROR_MAX_CHARS: usize = 512;
/// Начало каждой подсказки (§10.3, «Пометка»).
pub const HINT_PREFIX: &str = "ZeroAIMemory:";
/// Предел скомпилированного `error_regex` (как при проверке триггера в `zam-store`), байт.
pub const REGEX_SIZE_LIMIT: usize = 1 << 20;
/// Слов ошибки, по которым считается покрытие (как предел слов запроса FTS).
const MAX_TERMS: usize = 32;
/// Минимальная длина слова ошибки для покрытия, символов (как префиксный поиск FTS).
const MIN_TERM_CHARS: usize = 3;
/// Длина «хэша»: шестнадцатеричный токен с цифрой от стольких символов (короткий sha git — 7).
const HASH_MIN_CHARS: usize = 7;

/// Обрамление токена, которое снимается перед классификацией.
fn is_wrap(c: char) -> bool {
    matches!(
        c,
        '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';' | '«' | '»'
    )
}

/// Токен — путь или URL: есть `/` или `\` (кроме одиночного символа).
fn is_path(core: &str) -> bool {
    core.chars().count() > 1 && core.contains(['/', '\\'])
}

/// Токен — UUID (`8-4-4-4-12` шестнадцатеричных цифр).
fn is_uuid(core: &str) -> bool {
    let parts: Vec<&str> = core.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Токен — хэш или длинное шестнадцатеричное число: `0x…`, либо только hex-цифры (в том числе
/// через `-`/`:`) от [`HASH_MIN_CHARS`] знаков с хотя бы одной цифрой.
fn is_hex(core: &str) -> bool {
    let t = core.trim_end_matches([':', '.']);
    if let Some(rest) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_hexdigit());
    }
    let hex: Vec<char> = t.chars().filter(|c| !matches!(c, '-' | ':')).collect();
    hex.len() >= HASH_MIN_CHARS
        && hex.iter().all(char::is_ascii_hexdigit)
        && hex.iter().any(char::is_ascii_digit)
}

/// Числовые хвосты позиции (`:12:5`, `:12:5:`) с конца токена; хвоста нет — токен как есть.
fn strip_numeric_tails(tok: &str) -> &str {
    let mut t = tok;
    let mut stripped = false;
    loop {
        let base = t.trim_end_matches(':');
        match base.rfind(':') {
            Some(i)
                if i > 0
                    && base.len() > i + 1
                    && base[i + 1..].chars().all(|c| c.is_ascii_digit()) =>
            {
                t = &base[..i];
                stripped = true;
            }
            _ => break,
        }
    }
    if stripped { t } else { tok }
}

/// Нормализованная ошибка инструмента (§10.3) для FTS по insight: убраны пути и URL, UUID,
/// хэши и длинные шестнадцатеричные числа (`0x…`), числа (токены без единой буквы) и числовые
/// хвосты позиции (`a.rs:12:5` → `a.rs`); токены с буквой остаются целиком (`E0425`,
/// `LNK1181`, `TS2307`, `sha256`, `x86_64` — по ним и ищется ошибка); пробелы схлопнуты,
/// переводы строк — пробелы. Не длиннее [`ERROR_MAX_CHARS`] символов.
pub fn normalize_error(s: &str) -> String {
    let mut out = String::new();
    let mut chars = 0usize;
    for tok in s.split_whitespace() {
        let core = tok.trim_matches(is_wrap);
        if is_path(core) || is_uuid(core) || is_hex(core) {
            continue;
        }
        if !core.chars().any(char::is_alphabetic) {
            continue;
        }
        let kept = strip_numeric_tails(tok);
        let n = kept.chars().count();
        let sep = usize::from(!out.is_empty());
        if chars + sep + n > ERROR_MAX_CHARS {
            let room = ERROR_MAX_CHARS.saturating_sub(chars + sep);
            if room > 0 {
                if sep == 1 {
                    out.push(' ');
                }
                out.extend(kept.chars().take(room));
            }
            break;
        }
        if sep == 1 {
            out.push(' ');
        }
        out.push_str(kept);
        chars += sep + n;
    }
    out
}

/// Предел кэша скомпилированных `error_regex`; при переполнении кэш сбрасывается целиком.
const REGEX_CACHE_MAX: usize = 1024;

/// Кэш скомпилированных `error_regex` по тексту шаблона (`None` — шаблон не компилируется).
fn regex_cache() -> &'static Mutex<HashMap<String, Option<regex::Regex>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<regex::Regex>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `error_regex` триггера совпал с текстом ошибки. Шаблон компилируется с тем же пределом
/// размера, что при проверке триггера, один раз на процесс (кэш по тексту шаблона, до
/// [`REGEX_CACHE_MAX`]); некорректный шаблон не совпадает ни с чем.
pub fn error_regex_matches(pattern: &str, text: &str) -> bool {
    let re = {
        let mut cache = regex_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(r) = cache.get(pattern) {
            r.clone()
        } else {
            let r = regex::RegexBuilder::new(pattern)
                .size_limit(REGEX_SIZE_LIMIT)
                .dfa_size_limit(REGEX_SIZE_LIMIT)
                .build()
                .ok();
            if cache.len() >= REGEX_CACHE_MAX {
                cache.clear();
            }
            cache.insert(pattern.to_string(), r.clone());
            r
        }
    };
    // сопоставление — вне блокировки (`Regex` дешево клонируется)
    re.is_some_and(|r| r.is_match(text))
}

/// Слова нормализованной ошибки для покрытия: буквенно-цифровые от [`MIN_TERM_CHARS`]
/// символов, в нижнем регистре, без повторов, не больше 32 (как в запросе FTS).
pub fn error_terms(normalized: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in normalized
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= MIN_TERM_CHARS)
    {
        let w = w.to_lowercase();
        if !out.contains(&w) {
            out.push(w);
            if out.len() == MAX_TERMS {
                break;
            }
        }
    }
    out
}

/// Сколько слов `terms` встречается в `text` по границе слова (без учёта регистра): слово
/// текста начинается с терма — как префиксный терм FTS (`file` находит `files`, но не
/// `profile`).
pub fn term_coverage(terms: &[String], text: &str) -> usize {
    let text = text.to_lowercase();
    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    terms
        .iter()
        .filter(|t| words.iter().any(|w| w.starts_with(t.as_str())))
        .count()
}

/// Наибольший порог покрытия: длинная ошибка (stack trace) не должна требовать от короткой
/// записи десятков совпадений.
const COVERAGE_CAP: usize = 4;

/// Покрытие достаточно для подсказки по ослабленному FTS (любое слово): не меньше
/// `min(max(2, ⌈total / 3⌉), 4)` слов и не больше `total` **[решение исполнителя, задача 8
/// M4]** — иначе общие слова ошибки («file», «error») поднимали бы случайные записи.
pub fn coverage_enough(matched: usize, total: usize) -> bool {
    if total == 0 {
        return false;
    }
    let need = total.div_ceil(3).clamp(2, COVERAGE_CAP).min(total);
    matched >= need
}

/// Одна строка без переводов строк и с обрезанными краями (заголовок, путь).
pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Строка подсказки (§10.3): `ZeroAIMemory: <lead><title> [<теги через «, »>] — <ссылка>`;
/// пустые теги опускаются вместе со скобками.
pub fn hint_line(lead: &str, title: &str, tags: &[&str], reference: &str) -> String {
    let tags: Vec<&str> = tags.iter().copied().filter(|t| !t.is_empty()).collect();
    let mut s = format!("{HINT_PREFIX} {lead}{}", one_line(title));
    if !tags.is_empty() {
        s.push_str(&format!(" [{}]", tags.join(", ")));
    }
    s.push_str(&format!(" — {reference}"));
    s
}

/// Раздел дайджеста: заголовок (строка `### …`, ставится, только если в раздел вошёл хоть
/// один блок) и блоки — по одному на подсказку (строка и, возможно, строки с отступом).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub title: Option<String>,
    pub blocks: Vec<String>,
}

/// Упаковка разделов в `budget` токенов по счётчику `count`: разделы и блоки — по порядку
/// (приоритет), блок, который не влезает, пропускается, следующие короткие ещё пробуются.
/// Итог — текст (блоки через перевод строки, перед заголовком раздела — пустая строка) и
/// вошедшие блоки `(раздел, блок)`. Гарантия: `count(text) <= budget` (при расхождении суммы
/// частей с подсчётом целого с конца снимаются блоки).
pub fn pack_sections(
    sections: &[Section],
    budget: usize,
    count: &dyn Fn(&str) -> usize,
) -> (String, Vec<(usize, usize)>) {
    let mut used = 0usize;
    let mut taken: Vec<(usize, usize)> = Vec::new();
    for (si, sec) in sections.iter().enumerate() {
        let mut opened = false;
        for (bi, block) in sec.blocks.iter().enumerate() {
            let head = match (&sec.title, opened) {
                (Some(t), false) => count(&format!("\n{t}\n")),
                _ => 0,
            };
            let cost = head + count(&format!("{block}\n"));
            if used + cost <= budget {
                used += cost;
                opened = true;
                taken.push((si, bi));
            }
        }
    }
    loop {
        let text = render(sections, &taken);
        if count(&text) <= budget || taken.is_empty() {
            return (text, taken);
        }
        taken.pop();
    }
}

/// Текст вошедших блоков (см. [`pack_sections`]).
fn render(sections: &[Section], taken: &[(usize, usize)]) -> String {
    let mut out = String::new();
    let mut last: Option<usize> = None;
    for &(si, bi) in taken {
        if last != Some(si) {
            if let Some(t) = &sections[si].title {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
                out.push('\n');
            }
            last = Some(si);
        }
        out.push_str(&sections[si].blocks[bi]);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_error_strips_paths_numbers_hashes() {
        let raw = "/bin/bash: ./deploy.sh: /bin/bash^M: bad interpreter: No such file or \
                   directory\nerror at C:\\work\\a.rs:12:5 commit 3f2a9c1d0b request \
                   123e4567-e89b-12d3-a456-426614174000 ptr 0xDEADBEEF line 42 (exit 127)";
        let n = normalize_error(raw);
        assert_eq!(
            n,
            "bad interpreter: No such file or directory error at commit request ptr line (exit"
        );
        for gone in [
            "/",
            "\\",
            "12",
            "3f2a9c1d0b",
            "123e4567",
            "DEADBEEF",
            "42",
            "127",
        ] {
            assert!(!n.contains(gone), "{gone}: {n}");
        }
        // слова без цифр и обычные слова-«hex» (deadbeef без цифр) остаются
        assert_eq!(normalize_error("cafe added"), "cafe added");
        // токены с буквой — целиком, числа и хвосты позиции — прочь
        assert_eq!(normalize_error("E0425 unresolved"), "E0425 unresolved");
        assert_eq!(
            normalize_error("error[E0425]: at main.rs:12:5: LNK1181 TS2307 sha256 x86_64 42 1.5"),
            "error[E0425]: at main.rs LNK1181 TS2307 sha256 x86_64"
        );
        assert_eq!(normalize_error("  \n 123 456 \t"), "");
        // предел 512 символов, по границе символа
        let long = "ошибка ".repeat(200);
        let n = normalize_error(&long);
        assert_eq!(n.chars().count(), ERROR_MAX_CHARS);
        assert!(n.starts_with("ошибка ошибка"));
    }

    #[test]
    fn error_regex_and_coverage() {
        assert!(error_regex_matches(
            r"bad interpreter",
            "x: bad interpreter: y"
        ));
        assert!(!error_regex_matches(r"^ok$", "bad"));
        // некорректный шаблон не совпадает
        assert!(!error_regex_matches(r"(", "("));
        let t = error_terms("Bad interpreter: No such file or directory, bad");
        assert_eq!(t, ["bad", "interpreter", "such", "file", "directory"]);
        assert_eq!(term_coverage(&t, "BAD interpreter и CRLF"), 2);
        // по границе слова: `file` — в `files`, но не в `profile`
        let f = vec!["file".to_string()];
        assert_eq!(term_coverage(&f, "no files here"), 1);
        assert_eq!(term_coverage(&f, "profile"), 0);
        assert!(coverage_enough(2, 5));
        assert!(!coverage_enough(1, 5));
        assert!(coverage_enough(1, 1));
        assert!(!coverage_enough(0, 0));
        assert!(!coverage_enough(3, 10));
        assert!(coverage_enough(4, 10));
        // длинная ошибка: порог не выше 4
        assert!(coverage_enough(4, 32));
        assert!(!coverage_enough(3, 32));
        // кэш шаблонов: повтор и некорректный шаблон
        assert!(error_regex_matches(r"bad interpreter", "bad interpreter"));
        assert!(!error_regex_matches(r"(", "("));
    }

    #[test]
    fn hint_line_format() {
        assert_eq!(
            hint_line("", "LF в\nскриптах", &["gotcha", "hypothesis"], "i:1"),
            "ZeroAIMemory: LF в скриптах [gotcha, hypothesis] — i:1"
        );
        assert_eq!(
            hint_line("эта ошибка уже разбиралась: ", "x", &["", ""], "i:2"),
            "ZeroAIMemory: эта ошибка уже разбиралась: x — i:2"
        );
    }

    #[test]
    fn pack_sections_respects_budget_and_skips_big_blocks() {
        let words = |s: &str| s.split_whitespace().count();
        let secs = vec![
            Section {
                title: Some("### A".into()),
                blocks: vec!["a1 a1".into(), "big ".repeat(50)],
            },
            Section {
                title: Some("### B".into()),
                blocks: vec!["b1".into()],
            },
            Section {
                title: None,
                blocks: vec!["c1 c1 c1".into()],
            },
        ];
        let (text, taken) = pack_sections(&secs, 10, &words);
        assert_eq!(taken, [(0, 0), (1, 0), (2, 0)]);
        assert_eq!(text, "### A\na1 a1\n\n### B\nb1\nc1 c1 c1\n");
        assert!(words(&text) <= 10);
        // раздел без вошедших блоков — без заголовка
        let (text, taken) = pack_sections(&secs, 4, &words);
        assert_eq!(taken, [(0, 0)]);
        assert_eq!(text, "### A\na1 a1\n");
        let (text, taken) = pack_sections(&secs, 0, &words);
        assert!(taken.is_empty() && text.is_empty());
    }
}
