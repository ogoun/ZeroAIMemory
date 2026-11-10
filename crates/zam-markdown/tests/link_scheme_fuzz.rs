//! Дифференциальная проверка `zam_core::link::url_scheme` против разборщика URL (`url`, тот же,
//! которым схемы проверяет ammonia) на случайных строках с фиксированным зерном (ревью 15a,
//! Minor 6). Расхождение значило бы, что наша проверка схемы и санитайзер читают один адрес
//! по-разному. Тест WebUI (`web/test/reportLinks.test.ts`) тем же генератором сверяет функцию на
//! TypeScript с `new URL()`.

use ammonia::Url;
use zam_core::link::{report_link_clickable, url_scheme};

/// Куски, из которых собираются адреса: буквы схем, разделители, пробельные и управляющие
/// символы, невидимые и похожие на ASCII знаки.
const PARTS: &[&str] = &[
    "j", "a", "v", "s", "c", "r", "i", "p", "t", "J", "A", "S", "h", "f", "l", "e", "d", "o", ":",
    ":", "/", "//", "\\", " ", "\t", "\n", "\r", "\u{0}", "\u{1}", "\u{b}", "\u{c}", "\u{1f}", "%",
    "3A", "+", "-", ".", "1", "#", "?", "@", "\u{a0}", "\u{200b}", "\u{feff}", "\u{17f}",
    "\u{ff1a}", "\u{430}", "&",
];

/// xorshift32 — тот же генератор, что в тесте WebUI.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
}

fn check(s: &str) {
    let ours = url_scheme(s);
    match Url::parse(s) {
        Ok(u) => assert_eq!(ours.as_deref(), Some(u.scheme()), "{s:?}"),
        Err(e) if e.to_string() == "relative URL without a base" => {
            assert_eq!(ours, None, "{s:?}");
        }
        // схема прочитана, но адрес недействителен (пустой хост, порт …)
        Err(e) => assert!(ours.is_some(), "{s:?}: {e}"),
    }
    // кликабельное со схемой — только четыре вида адресов
    if let Some(scheme) = ours
        && report_link_clickable(s)
    {
        assert!(
            matches!(scheme.as_str(), "http" | "https" | "mailto" | "vscode"),
            "{s:?}"
        );
    }
}

#[test]
fn url_scheme_agrees_with_url_parser_on_random_strings() {
    let mut rng = Rng(0x15A_2026);
    let mut with_scheme = 0u32;
    for _ in 0..40_000 {
        let n = rng.next() % 10;
        let mut s = String::new();
        for _ in 0..n {
            s.push_str(PARTS[(rng.next() % PARTS.len() as u32) as usize]);
        }
        with_scheme += u32::from(url_scheme(&s).is_some());
        check(&s);
    }
    // выборка содержательна: заметная доля строк — со схемой
    assert!(with_scheme > 500, "{with_scheme}");
    // готовые схемы с шумом вокруг
    for scheme in ["javascript", "file", "vscode", "http", "data", "mailto"] {
        for _ in 0..2_000 {
            let mut s = String::new();
            for part in [scheme, ":"] {
                if rng.next().is_multiple_of(3) {
                    s.push_str(PARTS[(rng.next() % PARTS.len() as u32) as usize]);
                }
                s.push_str(part);
            }
            s.push_str("//file/x");
            check(&s);
        }
    }
}
