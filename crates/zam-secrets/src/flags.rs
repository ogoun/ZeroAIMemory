//! Правила флагов командной строки (задача 3 M7.1, §4.2 п. 5 спецификации M7): значения
//! паролей и токенов в аргументах команд, которых сканер [`crate::scan_secrets`] не знает.
//!
//! Перенесены из клиента `zam` (`snapshot::mask_flags`, задача 17a M6) без изменения поведения:
//! их применяет клиент (поле команд снимка сессии), а с задачи 5 M7.1 — и сервер (выдача агенту,
//! §4.3 спецификации). В
//! [`crate::mask_text`] они не входят: в прозе «basic», «bearer» и `-u` — не пароль, поэтому
//! [`mask_flags`] вызывается только для текста команд и отдельно от сканера;
//! [`crate::RULES_VERSION`] от них не зависит.

use std::sync::LazyLock;

use regex::Regex;

use crate::SECRET_MARK;

/// Шаблоны значений: `--password X`, `-u user:pass`, `Authorization: X`, `-pXXX`.
static FLAGS: LazyLock<[Regex; 4]> = LazyLock::new(|| {
    [
        // --password X, --password=X, --token "a b"
        Regex::new(
            r#"(?i)(?:^|[\s'"])--(?:password|passwd|pass|pwd|passphrase|token|api-key|apikey|api-token|secret|secret-key|client-secret|access-token|auth-token|http-password|proxy-password)(?:=|\s+)("[^"]*"|'[^']*'|[^\s"']+)"#,
        )
        .expect("regex"),
        // curl -u user:pass, --user user:pass, -uuser:pass
        Regex::new(r#"(?:^|\s)(?:-u\s*|--user(?:=|\s+))["']?([^\s:"']+):([^\s"'@]+)"#)
            .expect("regex"),
        // Authorization: Bearer X | Basic X | Token X | X; X-Api-Key: X
        Regex::new(
            r#"(?i)(?:authorization\s*:\s*(?:(?:bearer|basic|token|digest)\s+)?|x-api-key\s*:\s*)([^\s"']{6,})"#,
        )
        .expect("regex"),
        // -pXXX, -p'a b' (значение слитно с флагом)
        Regex::new(r#"(?:^|\s)-p("[^"]*"|'[^']*'|[^\s"']+)"#).expect("regex"),
    ]
});

/// Команды, у которых флаги `-p` и `-u` значат пароль: mysql-подобные и `sshpass`; HTTP-клиенты.
static GATES: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        Regex::new(r"(?i)\b(?:mysql\w*|mariadb\w*|sshpass)\b").expect("regex"),
        Regex::new(r"(?i)\b(?:curl|wget|wget2|http|https|httpie|xh|lftp|aria2c)\b").expect("regex"),
    ]
});

/// Маскирует значения флагов командной строки с паролем или токеном (текст команды; в прозе
/// «basic»/«bearer» и `-u` не пароль — там не применять):
/// - `-pXXX`/`-p'a b'` у mysql-подобных команд и `sshpass`;
/// - `--password X`/`--password=X` (и `--passphrase`, `--secret-key`, `--client-secret`, `--token`,
///   `--api-key`, `--secret`, …);
/// - `-u user:pass`/`--user user:pass` у curl, wget и подобных (числовые `uid:gid` — нет);
/// - значение заголовка `Authorization:` (`Bearer X`, `Basic X`, `Token X`) и `X-Api-Key:`;
///   ссылка на переменную (`Bearer $TOKEN`) и сама схема не маскируются.
///
/// Значение заменяется меткой [`SECRET_MARK`]; ссылки на переменные (`$X`, `%X%`), заглушки
/// (`<…>`) и уже замаскированные значения остаются — повторный вызов ничего не меняет.
pub fn mask_flags(text: &str) -> String {
    let [mysql, curl] = &*GATES;
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for (i, r) in FLAGS.iter().enumerate() {
        if (i == 3 && !mysql.is_match(text)) || (i == 1 && !curl.is_match(text)) {
            continue;
        }
        for c in r.captures_iter(text) {
            // `-u user:pass` — значение во второй группе; числовые `uid:gid` — не пароль
            let m = if i == 1 {
                let (Some(u), Some(p)) = (c.get(1), c.get(2)) else {
                    continue;
                };
                let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
                if digits(u.as_str()) && digits(p.as_str()) {
                    continue;
                }
                p
            } else {
                let Some(m) = c.get(1) else { continue };
                m
            };
            let v = m.as_str();
            let (mut b, mut e) = (m.start(), m.end());
            if (v.starts_with('"') || v.starts_with('\'')) && v.len() >= 2 {
                b += 1;
                e -= 1;
            }
            let inner = &text[b..e];
            // значение целиком — ссылка на переменную (`$NAME`, `${NAME}`, `%NAME%`) или заглушка
            // `<…>` ([`is_reference`]); у заголовка без значения длиной ≥ 6 регулярка берёт
            // слово-схему — его не трогаем
            let scheme = i == 2
                && ["bearer", "basic", "token", "digest"]
                    .iter()
                    .any(|w| inner.eq_ignore_ascii_case(w));
            if inner.is_empty() || scheme || inner.contains(SECRET_MARK) || is_reference(inner) {
                continue;
            }
            ranges.push((b, e));
        }
    }
    if ranges.is_empty() {
        return text.to_string();
    }
    ranges.sort();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for (b, e) in ranges {
        if b < pos {
            continue;
        }
        out.push_str(&text[pos..b]);
        out.push_str(SECRET_MARK);
        pos = e;
    }
    out.push_str(&text[pos..]);
    out
}

/// Значение — целиком ссылка на переменную или заглушка, а не секрет: `$NAME`, `${NAME}`,
/// `%NAME%` (имя — `[A-Za-z_][A-Za-z0-9_]*`) или `<…>`. `%2Fabc…`, `$2b$10$…` (хэш bcrypt) —
/// не ссылки.
fn is_reference(v: &str) -> bool {
    let ident = |s: &str| {
        let mut c = s.chars();
        c.next()
            .is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
            && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
    };
    if let Some(r) = v.strip_prefix("${") {
        return r.strip_suffix('}').is_some_and(ident);
    }
    if let Some(r) = v.strip_prefix('$') {
        return ident(r);
    }
    if let Some(r) = v.strip_prefix('%') {
        return r.strip_suffix('%').is_some_and(ident);
    }
    v.len() >= 2 && v.starts_with('<') && v.ends_with('>')
}
