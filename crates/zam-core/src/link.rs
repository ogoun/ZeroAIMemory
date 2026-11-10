//! Ссылки отчёта (M6 §16 п. 15, долг M5.5): какие схемы допустимы в теле и по каким адресам
//! разрешён переход. Тело отчёта пишет агент — содержимое недоверенное.
//!
//! Два правила, оба — здесь:
//! - **схема допустима** ([`REPORT_LINK_SCHEMES`], [`report_scheme_allowed`]) — её проверяют
//!   `zam report` до отправки и сервер при записи; прочее (`javascript:`, `data:`, `ftp:` …) —
//!   отказ;
//! - **по ссылке можно перейти** ([`report_link_clickable`]) — `href` в HTML страницы отчёта
//!   сервер пишет только таким ссылкам: `http`, `https`, `mailto` и `vscode://file/<путь>`.
//!   `file:` и прочие адреса `vscode:` (команды расширений редактора, сетевой путь) допустимы в
//!   теле, но в HTML остаются без `href` — адрес уходит в `data-url` (текст, кнопка «копировать
//!   путь» в WebUI). WebUI (`web/src/lib/reportLinks.ts`) повторяет обе проверки второй линией.
//!
//! Согласие Rust и TypeScript держит общий набор `tests/fixtures/link_schemes.json` (схема,
//! допустимость, кликабельность): его читают тест этого модуля и тест WebUI; разбор схемы
//! дополнительно сверяется с разборщиком URL на случайных строках (`zam-markdown`,
//! `tests/link_scheme_fuzz.rs`; в WebUI — с `new URL()`).

/// Схемы внешних ссылок отчёта (ответ владельца по M5.5, п. 1). `vscode:` и `file:` — только
/// у ссылок (не у картинок): WebUI даёт им кнопку «копировать путь», `file:` — без перехода.
pub const REPORT_LINK_SCHEMES: [&str; 5] = ["http", "https", "mailto", "vscode", "file"];

/// Схема URL назначения ссылки в нижнем регистре — так, как её прочтёт браузер (WHATWG URL):
/// ведущие и замыкающие символы до U+0020 включительно (пробел и управляющие C0) отбрасываются,
/// TAB, LF и CR выбрасываются из любого места, схема — буква ASCII, затем буквы, цифры и `+-.`
/// до первого `:`. Поэтому ` javascript:`, `JaVaScRiPt:` и `java\tscript:` — схема `javascript`.
///
/// `None` — схемы нет (относительная ссылка, якорь, `//host`). Percent-кодирование и сущности
/// HTML не раскрываются, как и в браузере: `%6Aavascript:` — относительный путь, а не схема.
/// У пути Windows `C:/…` схема — `c`.
pub fn url_scheme(target: &str) -> Option<String> {
    let t = target.trim_matches(|c: char| c <= ' ');
    let mut scheme = String::new();
    for c in t.chars() {
        match c {
            '\t' | '\n' | '\r' => {}
            ':' => return (!scheme.is_empty()).then_some(scheme),
            c if c.is_ascii_alphabetic() => scheme.push(c.to_ascii_lowercase()),
            c if !scheme.is_empty() && (c.is_ascii_digit() || matches!(c, '+' | '-' | '.')) => {
                scheme.push(c);
            }
            _ => return None,
        }
    }
    None
}

/// Схема `scheme` (в нижнем регистре, из [`url_scheme`]) разрешена ссылке отчёта.
pub fn report_scheme_allowed(scheme: &str) -> bool {
    REPORT_LINK_SCHEMES.contains(&scheme)
}

/// Адрес без того, что отбрасывает браузер: края до U+0020 включительно, TAB/LF/CR — везде.
fn clean(target: &str) -> String {
    target
        .trim_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect()
}

/// По ссылке отчёта разрешён переход (ей пишется `href`):
/// - схемы нет (относительная ссылка, якорь, `//host`) — да;
/// - `http`, `https`, `mailto` — да;
/// - `vscode:` — только `vscode://file/<путь>`: имя схемы без учёта регистра, `//file/` —
///   буквально, путь непуст, не начинается с `/`, `\`, `%2F`, `%5C` (сетевой путь
///   `vscode://file//сервер/ресурс` редактор открыл бы по сети) и не содержит сегментов `.` и
///   `..` ([`vscode_path_plain`]). Прочие адреса `vscode:` —
///   команды расширений (`vscode://vscode.git/clone?url=…`), из недоверенного отчёта не
///   запускаются;
/// - `file:` — нет: браузер с http(s)-страницы его не откроет, а `file://сервер/…` — сетевой путь;
/// - прочие схемы — нет (они и в тело не принимаются).
pub fn report_link_clickable(target: &str) -> bool {
    let t = clean(target);
    match url_scheme(&t).as_deref() {
        None | Some("http" | "https" | "mailto") => true,
        Some("vscode") => {
            // схема — 6 символов ASCII, за ней `:`
            let Some(path) = t[7..].strip_prefix("//file/") else {
                return false;
            };
            vscode_path_plain(path)
        }
        Some(_) => false,
    }
}

/// Путь адреса `vscode://file/<путь>` прост: непуст, первый сегмент непуст и ни один сегмент не
/// равен `.` или `..`. Запрос и якорь (`?…`, `#…`) — не путь.
///
/// Нормализация пути не моделируется — запрещён её повод. Адрес читают двое, и по-разному:
/// разборщик URL (браузер, WHATWG) делит путь только по `/` и снимает сегменты `.`, `..`, `%2E`,
/// `%2E%2E`; редактор после этого раскрывает `%XX` и на Windows читает `\` как `/`. Поэтому
/// `x%2F/..//сервер/y` разборщик превращает в `//сервер/y` (для него `x%2F` — один сегмент), а
/// правило «сначала раскрыть, потом снять `..`» этого не видит. Здесь сегменты считаются по самому
/// мелкому делению (`/`, `\`, `%2F`, `%5C`; точка — и `%2E`, регистр любой): сегмент `.` или `..`
/// при любом из прочтений — сегмент `.` или `..` и при этом, значит, без них путь не меняет
/// никто. Тогда начало пути — то, что написано, и достаточно, чтобы первый сегмент был непуст:
/// пустые сегменты внутри (`a//b`) в начало пути попасть не могут и не запрещаются.
fn vscode_path_plain(path: &str) -> bool {
    let path = path.split(['?', '#']).next().unwrap_or("");
    let mut plain = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(c) = rest.chars().next() {
        let coded = rest.get(..3).map(str::to_ascii_lowercase);
        match (c, coded.as_deref()) {
            (_, Some("%2f" | "%5c")) => plain.push('/'),
            (_, Some("%2e")) => plain.push('.'),
            ('\\', _) => plain.push('/'),
            _ => {
                plain.push(c);
                rest = &rest[c.len_utf8()..];
                continue;
            }
        }
        rest = &rest[if c == '\\' { 1 } else { 3 }..];
    }
    !plain.starts_with('/') && !plain.is_empty() && !plain.split('/').any(|s| s == "." || s == "..")
}

/// Перечень разрешённых схем для сообщений об ошибке: `http, https, mailto, vscode, file`.
pub fn report_schemes_text() -> String {
    REPORT_LINK_SCHEMES.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Общий набор примеров с WebUI (`web/test/reportLinks.test.ts`).
    const VECTORS: &str = include_str!("../tests/fixtures/link_schemes.json");

    #[test]
    fn report_link_schemes_const() {
        assert_eq!(
            REPORT_LINK_SCHEMES,
            ["http", "https", "mailto", "vscode", "file"]
        );
        assert_eq!(report_schemes_text(), "http, https, mailto, vscode, file");
        for s in REPORT_LINK_SCHEMES {
            assert!(report_scheme_allowed(s), "{s}");
        }
        for s in ["ftp", "javascript", "data", "vbscript", "HTTP", "", "c"] {
            assert!(!report_scheme_allowed(s), "{s}");
        }
    }

    #[test]
    fn url_scheme_shared_vectors() {
        let v: serde_json::Value = serde_json::from_str(VECTORS).unwrap();
        let cases = v.as_array().unwrap();
        assert!(cases.len() >= 80, "{}", cases.len());
        let mut allowed = 0;
        for c in cases {
            let t = c["t"].as_str().unwrap();
            let got = url_scheme(t);
            assert_eq!(got.as_deref(), c["scheme"].as_str(), "{t:?}");
            let ok = got.as_deref().is_some_and(report_scheme_allowed);
            assert_eq!(ok, c["report"].as_bool().unwrap(), "{t:?}");
            allowed += usize::from(ok);
        }
        assert!(allowed >= 10, "{allowed}");
    }

    /// Кликабельность — по общему набору; всё кликабельное со схемой — допустимая схема.
    #[test]
    fn report_link_clickable_shared_vectors() {
        let v: serde_json::Value = serde_json::from_str(VECTORS).unwrap();
        let (mut yes, mut no) = (0, 0);
        for c in v.as_array().unwrap() {
            let t = c["t"].as_str().unwrap();
            let click = report_link_clickable(t);
            assert_eq!(click, c["click"].as_bool().unwrap(), "{t:?}");
            if let Some(s) = url_scheme(t) {
                assert!(!click || report_scheme_allowed(&s), "{t:?}");
                assert!(!click || s != "file", "{t:?}");
            }
            if click { yes += 1 } else { no += 1 }
        }
        assert!(yes >= 30 && no >= 30, "{yes}/{no}");
        // адрес в сотню килобайт — линейно и без паники
        let long = format!("http://example.org/{}", "a".repeat(100_000));
        assert!(report_link_clickable(&long));
        let long = format!("{}:x", "a".repeat(100_000));
        assert_eq!(url_scheme(&long).map(|s| s.len()), Some(100_000));
        assert!(!report_link_clickable(&long));
        let long = format!("vscode://file/{}", "a/".repeat(50_000));
        assert!(report_link_clickable(&long));
    }

    /// Обходы регистром, пробелами и управляющими символами дают схему `javascript`, а не
    /// «относительную ссылку».
    #[test]
    fn url_scheme_bypass_forms() {
        for t in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            " javascript:alert(1)",
            "\u{0}\u{1f}javascript:alert(1)",
            "java\tscript:alert(1)",
            "j\na\rv\ta\nscript:alert(1)",
        ] {
            assert_eq!(url_scheme(t).as_deref(), Some("javascript"), "{t:?}");
        }
    }
}
