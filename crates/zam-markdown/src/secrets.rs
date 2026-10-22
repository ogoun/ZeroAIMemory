//! Сканер секретов перед индексацией (ТЗ §7.6).
//!
//! [`scan_secrets`] находит в тексте значения, похожие на ключи, токены, пароли и закрытые
//! ключи; [`mask_secrets`] заменяет их на [`SECRET_MARK`]. Нарезка заметок и записей строит
//! чанки из замаскированного текста: значения не попадают ни в FTS, ни в эмбеддинги, ни в
//! Qdrant. Исходник заметки не меняется.
//!
//! Принцип правил — «лучше пропустить, чем сломать поиск»: идентификаторы (git SHA, sha256,
//! UUID, base32-id `Id`, длинные URL с query, хэши в логах) не маскируются никогда, поэтому
//! правило высокой энтропии узкое: нужны строчные, заглавные и цифры одновременно, а строки
//! только из hex-символов и дефисов (хэши, UUID) отбрасываются.
//!
//! Диапазоны результата отсортированы, не пересекаются и лежат на границах символов UTF-8.
//! Бюджет — не больше 2 мс на 100 КБ (release).

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

/// Замена секрета в тексте чанка.
pub const SECRET_MARK: &str = "‹secret›";

/// Тип сработавшего правила.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretKind {
    /// Известный префикс ключа (метка — семейство: `aws`, `github`, …).
    KnownPrefix(&'static str),
    /// Блок `-----BEGIN … PRIVATE KEY-----`.
    PrivateKey,
    /// `password = значение`, `api_key: значение` и подобное; маскируется только значение.
    KeyValue,
    /// Пароль в URL: `scheme://user:пароль@host`.
    UrlPassword,
    /// JWT.
    Jwt,
    /// Длинная случайная строка с высокой энтропией.
    HighEntropy,
}

impl SecretKind {
    /// Короткое имя вида для ответа API (без значения секрета).
    pub fn as_str(self) -> &'static str {
        match self {
            SecretKind::KnownPrefix(_) => "known_prefix",
            SecretKind::PrivateKey => "private_key",
            SecretKind::KeyValue => "key_value",
            SecretKind::UrlPassword => "url_password",
            SecretKind::Jwt => "jwt",
            SecretKind::HighEntropy => "high_entropy",
        }
    }
}

/// Найденный секрет: диапазон байтов значения и строка (с 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretHit {
    pub kind: SecretKind,
    pub range: Range<usize>,
    pub line: usize,
}

struct Prefix {
    family: &'static str,
    re: Regex,
}

fn rx(p: &str) -> Regex {
    Regex::new(p).expect("шаблон сканера секретов")
}

// ASCII-границы слова `(?-u:\b)`: быстрее юникодных и не срабатывают на кириллице.
static PREFIXES: LazyLock<Vec<Prefix>> = LazyLock::new(|| {
    [
        ("aws", r"(?-u:\b)(?:AKIA|ASIA)[0-9A-Z]{16}"),
        ("github", r"(?-u:\b)gh[pousr]_[A-Za-z0-9]{36}"),
        ("github", r"(?-u:\b)github_pat_[A-Za-z0-9_]{60,}"),
        ("gitlab", r"(?-u:\b)glpat-[A-Za-z0-9_-]{20,}"),
        ("slack", r"(?-u:\b)xox[abprs]-[A-Za-z0-9-]{10,}"),
        ("api", r"(?-u:\b)sk-(?:ant-)?[A-Za-z0-9_-]{20,}"),
        ("google", r"(?-u:\b)AIza[0-9A-Za-z_-]{35}"),
        // формат zam_core::token: `zam_` + 40 символов base62
        ("zeroaimemory", r"(?-u:\b)zam_[A-Za-z0-9]{40}"),
    ]
    .into_iter()
    .map(|(family, p)| Prefix { family, re: rx(p) })
    .collect()
});

/// JWT: заголовок `eyJ…` (base64 от `{"`) и ещё две части, каждая от 10 символов
/// [решение исполнителя: требование `eyJ` отсекает имена вида `a.b.c`].
static JWT: LazyLock<Regex> =
    LazyLock::new(|| rx(r"(?-u:\b)eyJ[A-Za-z0-9_-]{7,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}"));

/// Блок закрытого ключа целиком; без `END` — заголовок и следующие строки base64.
static PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    rx(
        r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----(?:.*?-----END [A-Z ]*PRIVATE KEY-----|(?:\s*[A-Za-z0-9+/=]{20,})*)",
    )
});

/// Ключ-значение; необязательная кавычка перед разделителем покрывает JSON (`"password": "…"`)
/// [решение исполнителя]. Значение — группа 2.
static KEY_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    rx(
        r#"(?i)(password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key|client[_-]?secret)["']?\s*[:=]\s*["']?([^\s"'`]{6,})"#,
    )
});

/// Пароль в URL; пароль — группа 1.
static URL_PASSWORD: LazyLock<Regex> =
    LazyLock::new(|| rx(r"[A-Za-z][A-Za-z0-9+.-]*://[^\s/:@]+:([^\s/@]+)@"));

static ENTROPY_CANDIDATE: LazyLock<Regex> = LazyLock::new(|| rx(r"[A-Za-z0-9+/_=-]{32,}"));

/// Порог энтропии Шеннона, бит на символ.
const ENTROPY_MIN: f64 = 4.5;

/// Значение — заведомая заглушка, а не секрет.
fn is_placeholder(v: &str) -> bool {
    let l = v.to_ascii_lowercase();
    v.starts_with('<')
        || v.starts_with(SECRET_MARK) // уже замаскировано: повторный проход идемпотентен
        || v.starts_with("${")
        || v.starts_with("{{")
        // ссылка на переменную окружения: $TOKEN, $(cmd), %TOKEN%
        || v.starts_with('$')
        || (v.starts_with('%') && v.ends_with('%'))
        || v.bytes().all(|b| b == b'*' || b == b'x' || b == b'X')
        || l == "changeme"
        || l == "example"
}

fn shannon(s: &str) -> f64 {
    let mut counts = [0u32; 256];
    for b in s.bytes() {
        counts[b as usize] += 1;
    }
    let n = s.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = f64::from(c) / n;
            -p * p.log2()
        })
        .sum()
}

/// Кандидат правила энтропии — настоящий секрет, а не идентификатор.
fn is_high_entropy_secret(s: &str, word: &str) -> bool {
    let b = s.as_bytes();
    // хэши, UUID: только hex-символы и дефисы
    if b.iter().all(|c| c.is_ascii_hexdigit() || *c == b'-') {
        return false;
    }
    if !(b.iter().any(u8::is_ascii_lowercase)
        && b.iter().any(u8::is_ascii_uppercase)
        && b.iter().any(u8::is_ascii_digit))
    {
        return false;
    }
    // SRI-хэши (`sha512-…`) — не секреты
    if ["sha256-", "sha384-", "sha512-"]
        .iter()
        .any(|p| s.starts_with(p))
    {
        return false;
    }
    // слово целиком — URL: пути и query-значения идентификаторами не считаются;
    // секреты в query ловит правило «ключ-значение»
    if word.contains("://") {
        return false;
    }
    // путь файловой системы: сегменты-слова, а не случайная строка
    if s.matches('/').count() >= 2 && s.split('/').filter(|p| is_wordlike(p)).count() >= 2 {
        return false;
    }
    shannon(s) >= ENTROPY_MIN
}

/// Сегмент пути из букв, `_` и `-`, без цифр (имя каталога или файла).
fn is_wordlike(p: &str) -> bool {
    !p.is_empty()
        && p.bytes()
            .all(|b| b.is_ascii_alphabetic() || b == b'_' || b == b'-')
}

/// Слово (по ASCII-пробелам) вокруг диапазона.
fn word_around<'a>(text: &'a str, r: &Range<usize>) -> &'a str {
    let b = text.as_bytes();
    let mut s = r.start;
    while s > 0 && !b[s - 1].is_ascii_whitespace() {
        s -= 1;
    }
    let mut e = r.end;
    while e < b.len() && !b[e].is_ascii_whitespace() {
        e += 1;
    }
    // ASCII-пробелы не бывают продолжением многобайтового символа: границы корректны
    text.get(s..e).unwrap_or("")
}

/// Находит секреты. Результат отсортирован по началу, диапазоны не пересекаются
/// (пересёкшиеся объединяются, вид берётся у первого), `line` считается с 1.
pub fn scan_secrets(text: &str) -> Vec<SecretHit> {
    let mut raw: Vec<(Range<usize>, SecretKind)> = Vec::new();
    for p in PREFIXES.iter() {
        for m in p.re.find_iter(text) {
            raw.push((m.range(), SecretKind::KnownPrefix(p.family)));
        }
    }
    for m in JWT.find_iter(text) {
        raw.push((m.range(), SecretKind::Jwt));
    }
    for m in PRIVATE_KEY.find_iter(text) {
        raw.push((m.range(), SecretKind::PrivateKey));
    }
    for c in KEY_VALUE.captures_iter(text) {
        if let Some(v) = c.get(2)
            && !is_placeholder(v.as_str())
        {
            raw.push((v.range(), SecretKind::KeyValue));
        }
    }
    for c in URL_PASSWORD.captures_iter(text) {
        if let Some(v) = c.get(1)
            && !is_placeholder(v.as_str())
        {
            raw.push((v.range(), SecretKind::UrlPassword));
        }
    }
    for m in ENTROPY_CANDIDATE.find_iter(text) {
        let mut r = m.range();
        // `имя=значение`: `=` внутри строки — разделитель, а не base64-дополнение (оно в конце)
        if let Some(i) = m.as_str().trim_end_matches('=').rfind('=') {
            r.start += i + 1;
            if r.len() < 32 {
                continue;
            }
        }
        if is_high_entropy_secret(&text[r.clone()], word_around(text, &r)) {
            raw.push((r, SecretKind::HighEntropy));
        }
    }
    raw.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(b.0.end.cmp(&a.0.end)));
    let mut merged: Vec<(Range<usize>, SecretKind)> = Vec::with_capacity(raw.len());
    for (r, k) in raw {
        match merged.last_mut() {
            Some((last, _)) if r.start < last.end => last.end = last.end.max(r.end),
            _ => merged.push((r, k)),
        }
    }
    // номера строк одним проходом
    let (mut line, mut pos) = (1usize, 0usize);
    merged
        .into_iter()
        .map(|(range, kind)| {
            line += text.as_bytes()[pos..range.start]
                .iter()
                .filter(|&&b| b == b'\n')
                .count();
            pos = range.start;
            SecretHit { kind, range, line }
        })
        .collect()
}

/// Заменяет диапазоны `hits` на [`SECRET_MARK`]; остальные байты не меняются.
/// Некорректные диапазоны (не по порядку, за пределами, не на границах символов)
/// пропускаются.
pub fn mask_secrets(text: &str, hits: &[SecretHit]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for h in hits {
        let r = &h.range;
        if r.start < pos
            || r.end < r.start
            || r.end > text.len()
            || !text.is_char_boundary(r.start)
            || !text.is_char_boundary(r.end)
        {
            continue;
        }
        out.push_str(&text[pos..r.start]);
        out.push_str(SECRET_MARK);
        pos = r.end;
    }
    out.push_str(&text[pos..]);
    out
}

/// Сканирует и маскирует за один вызов (текст без секретов возвращается копией).
pub fn mask_text(text: &str) -> String {
    let hits = scan_secrets(text);
    if hits.is_empty() {
        text.to_string()
    } else {
        mask_secrets(text, &hits)
    }
}
