//! Подсветка кода syntect: CSS-классы с префиксом `hl-` (ТЗ §15.2).
//!
//! HTML несёт только классы; цвета задаёт [`highlight_css`] для светлой
//! (InspiredGitHub) и тёмной (base16-ocean.dark) темы.

use std::sync::LazyLock;

use syntect::highlighting::ThemeSet;
use syntect::html::{ClassStyle, ClassedHTMLGenerator, css_for_theme_with_class_style};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// Стиль классов: атомы области видимости через пробел, каждый с `hl-`.
const CLASS_STYLE: ClassStyle = ClassStyle::SpacedPrefixed { prefix: "hl-" };

/// Светлая тема.
const LIGHT_THEME: &str = "InspiredGitHub";
/// Тёмная тема, включается через `prefers-color-scheme: dark`.
const DARK_THEME: &str = "base16-ocean.dark";

/// Блоки длиннее не подсвечиваются: подсветка syntect нелинейна по времени,
/// а заметка может быть до 5 МБ.
pub(crate) const MAX_HIGHLIGHT_BYTES: usize = 256 * 1024;

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

/// Подсвечивает `code` для языка `lang` (первое слово info-строки).
/// `None` — язык неизвестен, блок слишком велик или syntect вернул ошибку:
/// тогда вызывающий выводит экранированный текст без подсветки.
pub(crate) fn highlight(lang: &str, code: &str) -> Option<String> {
    if lang.is_empty() || code.len() > MAX_HIGHLIGHT_BYTES {
        return None;
    }
    let ss = &*SYNTAXES;
    let syntax = ss.find_syntax_by_token(lang)?;
    let mut generator = ClassedHTMLGenerator::new_with_class_style(syntax, ss, CLASS_STYLE);
    for line in LinesWithEndings::from(code) {
        generator
            .parse_html_for_line_which_includes_newline(line)
            .ok()?;
    }
    Some(generator.finalize())
}

/// CSS классов `hl-*`: светлая тема по умолчанию, тёмная — внутри
/// `@media (prefers-color-scheme: dark)`.
pub fn highlight_css() -> String {
    let themes = ThemeSet::load_defaults();
    let css = |name: &str| {
        themes
            .themes
            .get(name)
            .and_then(|t| css_for_theme_with_class_style(t, CLASS_STYLE).ok())
            .unwrap_or_default()
    };
    let mut out = css(LIGHT_THEME);
    out.push_str("\n@media (prefers-color-scheme: dark) {\n");
    out.push_str(&css(DARK_THEME));
    out.push_str("}\n");
    out
}
