//! Схемы URL ссылок отчёта (M6 §16 п. 15, долг M5.5): одно правило у клиента `zam report`
//! (проверка до отправки), сервера (ссылки тела и рендер страницы) и WebUI
//! (`web/src/lib/reportLinks.ts` — та же функция на TypeScript). Согласие трёх мест держит общий
//! набор примеров `tests/fixtures/link_schemes.json`: его читают тест этого модуля и тест WebUI.
//!
//! Тело отчёта пишет агент — содержимое недоверенное. Разрешён только список
//! [`REPORT_LINK_SCHEMES`]; всё прочее (`javascript:`, `data:`, `ftp:` …) — отказ.

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
        assert!(cases.len() >= 50, "{}", cases.len());
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
