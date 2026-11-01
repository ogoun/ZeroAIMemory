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
pub const HIGHLIGHT_BUDGET_BYTES: usize = 256 * 1024;

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

/// Подсвеченный исходный файл (превью вложения, M5 задача 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceHtml {
    /// HTML: только `<span class="hl-…">` и экранированный текст (без атрибутов, кроме
    /// `class`, без стилей и скриптов) — вставляется в `<pre><code>` под CSP без `unsafe-inline`.
    pub html: String,
    /// Имя синтаксиса syntect; `None` — простой текст.
    pub syntax: Option<String>,
    /// Байт исходника подсвечено (остаток — экранированный текст без подсветки).
    pub highlighted: usize,
}

/// Подсвечивает файл `name` (синтаксис — по расширению, затем по первой строке) с классами
/// `hl-*`: первые [`HIGHLIGHT_BUDGET_BYTES`] байт по границе строки, дальше — экранированный
/// текст. Неизвестный синтаксис и ошибка syntect — экранированный текст целиком.
pub fn highlight_source(name: &str, code: &str) -> SourceHtml {
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or(name);
    let ss = &*SYNTAXES;
    let syntax = ss
        .find_syntax_by_extension(ext)
        .or_else(|| ss.find_syntax_by_extension(&ext.to_ascii_lowercase()))
        .or_else(|| ss.find_syntax_by_first_line(code.lines().next().unwrap_or("")))
        .filter(|s| s.name != "Plain Text");
    let Some(syntax) = syntax else {
        return SourceHtml {
            html: escape_html(code),
            syntax: None,
            highlighted: 0,
        };
    };
    // граница бюджета — конец строки, не дальше бюджета
    let mut cut = code.len().min(HIGHLIGHT_BUDGET_BYTES);
    if cut < code.len() {
        while !code.is_char_boundary(cut) {
            cut -= 1;
        }
        cut = code[..cut].rfind('\n').map_or(0, |i| i + 1);
    }
    let (head, tail) = code.split_at(cut);
    match highlight_with(syntax, head) {
        Some(mut html) => {
            html.push_str(&escape_html(tail));
            SourceHtml {
                html,
                syntax: Some(syntax.name.clone()),
                highlighted: head.len(),
            }
        }
        None => SourceHtml {
            html: escape_html(code),
            syntax: None,
            highlighted: 0,
        },
    }
}

/// Экранирование текста для HTML (`&`, `<`, `>`, `"`, `'`).
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 16);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Исходник в HTML: только классы и экранированный текст; разметка из файла не проходит.
    #[test]
    fn source_escaped_classes_only_and_budget() {
        let code = "fn main() { let s = \"<script>alert('x')</script>\"; }\n";
        let h = highlight_source("x.rs", code);
        assert_eq!(h.syntax.as_deref(), Some("Rust"));
        assert_eq!(h.highlighted, code.len());
        assert!(h.html.contains("hl-"), "{}", h.html);
        assert!(!h.html.contains("<script"), "{}", h.html);
        assert!(h.html.contains("&lt;script&gt;"), "{}", h.html);
        assert!(!h.html.contains("style="), "{}", h.html);
        // единственные теги — span с классом
        let tags: Vec<&str> = h
            .html
            .split('<')
            .skip(1)
            .map(|t| t.split('>').next().unwrap())
            .collect();
        assert!(
            tags.iter()
                .all(|t| *t == "/span" || t.starts_with("span class=\"")),
            "{tags:?}"
        );
        // неизвестный вид — простой экранированный текст
        let p = highlight_source("data.zzz", "a < b & c");
        assert_eq!((p.syntax, p.html.as_str()), (None, "a &lt; b &amp; c"));
        // сверх бюджета — по границе строки, хвост экранирован
        let big = "let x = 1; // <b>\n".repeat(HIGHLIGHT_BUDGET_BYTES / 10);
        let b = highlight_source("big.rs", &big);
        assert!(b.highlighted <= HIGHLIGHT_BUDGET_BYTES && b.highlighted > 0);
        assert_eq!(big.as_bytes()[b.highlighted - 1], b'\n');
        assert!(
            b.html.ends_with("// &lt;b&gt;\n"),
            "{}",
            &b.html[b.html.len() - 40..]
        );
        // JSON и YAML узнаются по расширению
        assert_eq!(
            highlight_source("a.json", "{\"a\": 1}").syntax.as_deref(),
            Some("JSON")
        );
        assert!(highlight_source("a.yaml", "a: 1\n").syntax.is_some());
    }
}
