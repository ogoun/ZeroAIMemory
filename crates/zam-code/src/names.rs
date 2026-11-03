//! Нормализация имён символов: слова для FTS (§6 `code_symbols_fts.terms`) и разбор ссылки
//! памяти `путь#qual` (§8).

use crate::proto::{QUAL_MAX_CHARS, REF_MAX_BYTES, lang_of};

/// Слова идентификатора в нижнем регистре: `getUserById` → `[get, user, by, id]`,
/// `HTTPServer` → `[http, server]`, `parse_http` → `[parse, http]`; цифры — часть слова.
pub fn split_ident(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
    {
        let chars: Vec<char> = token.chars().collect();
        let mut word = String::new();
        for (i, &c) in chars.iter().enumerate() {
            if i > 0 && c.is_uppercase() {
                let prev = chars[i - 1];
                let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
                // aB, a1B → граница; ABc (конец аббревиатуры) → граница перед B
                if !prev.is_uppercase() || next_lower {
                    push_word(&mut out, &mut word);
                }
            }
            word.extend(c.to_lowercase());
        }
        push_word(&mut out, &mut word);
    }
    out
}

fn push_word(out: &mut Vec<String>, word: &mut String) {
    if !word.is_empty() {
        out.push(std::mem::take(word));
    }
}

/// Слова для FTS: слова `name`, затем слова частей `qual` (без суффикса `#N`), без повторов,
/// через пробел, в нижнем регистре.
pub fn fts_terms(name: &str, qual: &str) -> String {
    let qual = strip_dup_suffix(qual);
    let mut seen: Vec<String> = Vec::new();
    let parts = std::iter::once(name).chain(qual.split("::"));
    for w in parts.flat_map(split_ident) {
        if !seen.contains(&w) {
            seen.push(w);
        }
    }
    seen.join(" ")
}

/// `foo#2` → `foo`; без суффикса — как есть.
fn strip_dup_suffix(qual: &str) -> &str {
    match qual.rsplit_once('#') {
        Some((head, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => qual,
    }
}

/// Ссылка памяти `путь#qual` → `(путь, Some(qual))`; иначе `(raw, None)`.
///
/// Символ отделяется по последнему `#`, при котором префикс — путь кода (`lang_of`), а суффикс —
/// допустимый `qual`: 1..=512 символов, без `/` и управляющих, `#` — только суффикс одноимённых
/// `#N`, не якорь строки в стиле GitHub (`L10`, `L10-L20`, `L10-20`). Поэтому `a.rs#foo#2` —
/// символ `foo#2`, а `docs/a#b.md`, `a.rs#`, `a.rs#x/y`, `src/a.rs#L10-L20` — пути (как в 0.0.8).
///
/// Разбор линеен по длине строки (вход недоверенный): строка длиннее [`REF_MAX_BYTES`] — путь
/// без разбора (его отвергнет нормализация пути: символов больше `PATH_MAX_CHARS`), а `qual`
/// содержит не больше одного `#`, поэтому кандидатов — только два последних `#`.
pub fn split_ref(raw: &str) -> (&str, Option<&str>) {
    if raw.len() > REF_MAX_BYTES {
        return (raw, None);
    }
    let mut end = raw.len();
    for _ in 0..2 {
        let Some(i) = raw[..end].rfind('#') else {
            break;
        };
        let (path, qual) = (&raw[..i], &raw[i + 1..]);
        if lang_of(path).is_some() && valid_qual(qual) {
            return (path, Some(qual));
        }
        end = i;
    }
    (raw, None)
}

fn valid_qual(qual: &str) -> bool {
    let n = qual.chars().take(QUAL_MAX_CHARS + 1).count();
    if n == 0 || n > QUAL_MAX_CHARS || qual.contains('/') || qual.chars().any(char::is_control) {
        return false;
    }
    let base = strip_dup_suffix(qual);
    !base.is_empty() && !base.contains('#') && !is_line_anchor(qual)
}

/// Якорь строки в стиле GitHub: `L10`, `L10-L20`, `L10-20`.
fn is_line_anchor(qual: &str) -> bool {
    let num = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let Some(rest) = qual.strip_prefix('L') else {
        return false;
    };
    match rest.split_once('-') {
        None => num(rest),
        Some((a, b)) => num(a) && num(b.strip_prefix('L').unwrap_or(b)),
    }
}
