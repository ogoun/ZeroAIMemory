//! Подсветка кода syntect: CSS-классы с префиксом `hl-` (ТЗ §15.2).
//!
//! HTML несёт только классы; цвета задаёт [`highlight_css`] для светлой
//! (InspiredGitHub) и тёмной (base16-ocean.dark) темы.
//!
//! **Бюджет (ТЗ §17).** Подсветка syntect нелинейна по времени, а заметка может
//! быть до 5 МБ, поэтому на документ подсвечивается не больше
//! [`HIGHLIGHT_BUDGET_BYTES`] исходного кода суммарно по всем блокам. Блок
//! подсвечивается, только если целиком помещается в остаток бюджета; иначе
//! рендер выводит его экранированным текстом с классом `hl-skipped`.

use std::sync::LazyLock;

use syntect::highlighting::ThemeSet;
use syntect::html::{ClassStyle, ClassedHTMLGenerator, css_for_theme_with_class_style};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

/// Стиль классов: атомы области видимости через пробел, каждый с `hl-`.
const CLASS_STYLE: ClassStyle = ClassStyle::SpacedPrefixed { prefix: "hl-" };

/// Светлая тема.
const LIGHT_THEME: &str = "InspiredGitHub";
/// Тёмная тема, включается через `prefers-color-scheme: dark`.
const DARK_THEME: &str = "base16-ocean.dark";

/// Сколько байт исходного кода подсвечивается на документ суммарно.
pub(crate) const HIGHLIGHT_BUDGET_BYTES: usize = 256 * 1024;

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

/// CSS обеих тем: строится один раз за процесс.
static CSS: LazyLock<String> = LazyLock::new(build_css);

/// Итог подсветки блока.
pub(crate) enum Highlighted {
    /// HTML с классами `hl-*`.
    Html(String),
    /// Язык известен, но блок не поместился в остаток бюджета.
    Skipped,
    /// Язык не указан или неизвестен, либо syntect вернул ошибку.
    Plain,
}

/// Подсвечивает `code` для языка `lang` (первое слово info-строки), списывая
/// размер блока с `budget` (остаток бюджета документа).
pub(crate) fn highlight(lang: &str, code: &str, budget: &mut usize) -> Highlighted {
    let Some(syntax) = syntax_for(lang) else {
        return Highlighted::Plain;
    };
    if code.len() > *budget {
        return Highlighted::Skipped;
    }
    *budget -= code.len();
    match highlight_with(syntax, code) {
        Some(html) => Highlighted::Html(html),
        None => Highlighted::Plain,
    }
}

fn syntax_for(lang: &str) -> Option<&'static SyntaxReference> {
    if lang.is_empty() {
        return None;
    }
    SYNTAXES.find_syntax_by_token(lang)
}

fn highlight_with(syntax: &SyntaxReference, code: &str) -> Option<String> {
    let ss = &*SYNTAXES;
    let mut generator = ClassedHTMLGenerator::new_with_class_style(syntax, ss, CLASS_STYLE);
    for line in LinesWithEndings::from(code) {
        generator
            .parse_html_for_line_which_includes_newline(line)
            .ok()?;
    }
    Some(generator.finalize())
}

/// CSS классов `hl-*`: светлая тема по умолчанию, тёмная — внутри
/// `@media (prefers-color-scheme: dark)`. Вычисляется один раз.
pub fn highlight_css() -> &'static str {
    &CSS
}

fn build_css() -> String {
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
