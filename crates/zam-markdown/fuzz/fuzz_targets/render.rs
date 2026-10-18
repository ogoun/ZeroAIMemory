//! Фазз-цель: `render_html` не паникует, а вывод не содержит опасных тегов
//! (allowlist ammonia).

#![no_main]

use libfuzzer_sys::fuzz_target;
use zam_markdown::{LinkResolution, Resolver, render_html};

/// Резолвер, разрешающий всё: проверяются ветки вики-ссылок и вложений.
struct All;

impl Resolver for All {
    fn wiki(&self, target: &str, _anchor: Option<&str>) -> Option<LinkResolution> {
        Some(LinkResolution {
            href: format!("n/{target}"),
            exists: target.len() % 2 == 0,
        })
    }
    fn attachment(&self, sha16: &str, filename: &str) -> Option<String> {
        Some(format!("b/{sha16}/{filename}"))
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    let html = render_html(src, &All).html.to_lowercase();
    // текст экранируется, поэтому `<` в выводе — только начало разрешённого тега
    for bad in [
        "<script", "<iframe", "<object", "<embed", "<style", "<form", "<svg", "<math",
    ] {
        assert!(!html.contains(bad), "{bad} в выводе");
    }
});
