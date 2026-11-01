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
use syntect::html::{ClassStyle, css_for_theme_with_class_style, line_tokens_to_classed_spans};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

/// Стиль классов: атомы области видимости через пробел, каждый с `hl-`.
const CLASS_STYLE: ClassStyle = ClassStyle::SpacedPrefixed { prefix: "hl-" };

/// Светлая тема.
const LIGHT_THEME: &str = "InspiredGitHub";
/// Тёмная тема, включается через `prefers-color-scheme: dark`.
const DARK_THEME: &str = "base16-ocean.dark";

/// Сколько байт исходного кода подсвечивается на документ суммарно.
pub const HIGHLIGHT_BUDGET_BYTES: usize = 256 * 1024;
/// Сколько байт HTML подсветки строится на документ суммарно (ревью задачи 8 M5, раунд 2): без
/// него надбавка «+64 КиБ» на каждый блок давала 1000 блокам `[` по 250 Б 25 МБ HTML и 16 с.
/// Блок, разметка которого не помещается в остаток, — экранированный текст (`hl-skipped`).
pub const HIGHLIGHT_HTML_BUDGET_BYTES: usize = 4 * 1024 * 1024;
/// Строка длиннее не подсвечивается (блок целиком — экранированный текст): предел проверяется
/// после строки, и одна длинная строка временно раздувала бы разметку до десятков МБ.
pub const HIGHLIGHT_LINE_MAX: usize = 16 * 1024;

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

/// Остаток бюджетов подсветки документа: байт исходника и байт HTML.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Budget {
    pub source: usize,
    pub html: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            source: HIGHLIGHT_BUDGET_BYTES,
            html: HIGHLIGHT_HTML_BUDGET_BYTES,
        }
    }
}

/// Подсвечивает `code` для языка `lang` (первое слово info-строки), списывая размер блока с
/// бюджета исходника и фактический HTML — с бюджета HTML документа. Предел HTML блока —
/// `min(40 × исходник + 64 КиБ, остаток бюджета HTML)`.
pub(crate) fn highlight(lang: &str, code: &str, budget: &mut Budget) -> Highlighted {
    let Some(syntax) = syntax_for(lang) else {
        return Highlighted::Plain;
    };
    if code.len() > budget.source {
        return Highlighted::Skipped;
    }
    budget.source -= code.len();
    let max = code
        .len()
        .saturating_mul(HIGHLIGHT_RATIO_MAX)
        .saturating_add(64 * 1024)
        .min(budget.html);
    match highlight_with(syntax, code, max) {
        Ok(html) => {
            budget.html -= html.len();
            Highlighted::Html(html)
        }
        // разметка раздулась сверх предела — как блок вне бюджета
        Err(Fail::TooLarge) => Highlighted::Skipped,
        Err(Fail::Syntect) => Highlighted::Plain,
    }
}

fn syntax_for(lang: &str) -> Option<&'static SyntaxReference> {
    if lang.is_empty() {
        return None;
    }
    SYNTAXES.find_syntax_by_token(lang)
}

/// Во сколько раз HTML подсветки может быть больше исходника. Замер на файлах проекта (классы
/// `hl-*`): Rust 11–15×, JSON 25×, CSS 28×; враждебные `[`/`'''`/`&a` — 80–100×. Предел 8×
/// из ревью отключил бы подсветку обычного кода, 40× отделяет одно от другого.
pub const HIGHLIGHT_RATIO_MAX: usize = 40;

/// Наибольший HTML подсветки файла ([`highlight_source`]), байт: не больше
/// [`HIGHLIGHT_RATIO_MAX`] × исходник + 64 КиБ и не больше 4 МиБ. Один символ (`[` в JSON,
/// тройная кавычка в Python) даёт до сотни байт разметки — без предела 256 КиБ раздувались до
/// 25 МБ (ревью задачи 8 M5, I-1). Блок кода заметки — без предела 4 МиБ (исходник ограничен
/// бюджетом документа 256 КиБ, прежнее поведение рендера сохраняется).
pub fn highlight_html_max(source_len: usize) -> usize {
    source_len
        .saturating_mul(HIGHLIGHT_RATIO_MAX)
        .saturating_add(64 * 1024)
        .min(4 * 1024 * 1024)
}

/// Почему подсветка не получилась.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fail {
    /// HTML больше [`highlight_html_max`].
    TooLarge,
    /// Ошибка syntect.
    Syntect,
}

/// HTML подсветки `code` построчно (как `ClassedHTMLGenerator`); рост проверяется после каждой
/// строки — сверх `max` байт разбор прекращается сразу.
fn highlight_with(syntax: &SyntaxReference, code: &str, max: usize) -> Result<String, Fail> {
    let ss = &*SYNTAXES;
    if code.split('\n').any(|l| l.len() > HIGHLIGHT_LINE_MAX) {
        return Err(Fail::TooLarge);
    }
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut open: usize = 0;
    let mut out = String::with_capacity(code.len().saturating_mul(2).min(max));
    for line in LinesWithEndings::from(code) {
        let ops = state.parse_line(line, ss).map_err(|_| Fail::Syntect)?;
        let (html, delta) = line_tokens_to_classed_spans(line, &ops, CLASS_STYLE, &mut stack)
            .map_err(|_| Fail::Syntect)?;
        open = open.saturating_add_signed(delta);
        // с закрывающими `</span>` всех открытых областей
        if out.len() + html.len() + 7 * open > max {
            return Err(Fail::TooLarge);
        }
        out.push_str(&html);
    }
    for _ in 0..open {
        out.push_str("</span>");
    }
    Ok(out)
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
    /// Подсветка отброшена: `too_large` — HTML больше [`highlight_html_max`].
    pub skipped: Option<&'static str>,
}

/// Синтаксис файла `name`: по расширению, затем по первой строке `code`; простой текст — `None`.
fn source_syntax(name: &str, code: &str) -> Option<&'static SyntaxReference> {
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or(name);
    let ss = &*SYNTAXES;
    ss.find_syntax_by_extension(ext)
        .or_else(|| ss.find_syntax_by_extension(&ext.to_ascii_lowercase()))
        .or_else(|| ss.find_syntax_by_first_line(code.lines().next().unwrap_or("")))
        .filter(|s| s.name != "Plain Text")
}

/// Имя синтаксиса, который [`highlight_source`] выберет для файла `name` (ключ кэша превью).
pub fn source_syntax_name(name: &str, code: &str) -> &'static str {
    source_syntax(name, code).map_or("Plain Text", |s| s.name.as_str())
}

fn plain(code: &str, skipped: Option<&'static str>) -> SourceHtml {
    SourceHtml {
        html: escape_html(code),
        syntax: None,
        highlighted: 0,
        skipped,
    }
}

/// Подсвечивает файл `name` (синтаксис — по расширению, затем по первой строке) с классами
/// `hl-*`: первые `budget` байт по границе строки, дальше — экранированный текст. Неизвестный
/// синтаксис и ошибка syntect — экранированный текст целиком; HTML подсветки больше
/// [`highlight_html_max`] — тоже, с `skipped = Some("too_large")`.
pub fn highlight_source(name: &str, code: &str, budget: usize) -> SourceHtml {
    let Some(syntax) = source_syntax(name, code) else {
        return plain(code, None);
    };
    // граница бюджета — конец строки, не дальше бюджета
    let mut cut = code.len().min(budget);
    if cut < code.len() {
        while !code.is_char_boundary(cut) {
            cut -= 1;
        }
        cut = code[..cut].rfind('\n').map_or(0, |i| i + 1);
    }
    let (head, tail) = code.split_at(cut);
    match highlight_with(syntax, head, highlight_html_max(head.len())) {
        Ok(mut html) => {
            html.push_str(&escape_html(tail));
            SourceHtml {
                html,
                syntax: Some(syntax.name.clone()),
                highlighted: head.len(),
                skipped: None,
            }
        }
        Err(Fail::TooLarge) => plain(code, Some("too_large")),
        Err(Fail::Syntect) => plain(code, None),
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
        let h = highlight_source("x.rs", code, HIGHLIGHT_BUDGET_BYTES);
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
        let p = highlight_source("data.zzz", "a < b & c", HIGHLIGHT_BUDGET_BYTES);
        assert_eq!((p.syntax, p.html.as_str()), (None, "a &lt; b &amp; c"));
        // сверх бюджета — по границе строки, хвост экранирован
        // бюджет превью — 64 КиБ: при 256 КиБ плотный код упирается в общий предел 4 МиБ
        let budget = 64 * 1024;
        let big = "    let total = items.iter().map(|item| item.len()).sum::<usize>(); // <b>\n"
            .repeat(budget / 20);
        let b = highlight_source("big.rs", &big, budget);
        assert_eq!(b.skipped, None);
        assert!(b.highlighted <= budget && b.highlighted > 0);
        assert_eq!(big.as_bytes()[b.highlighted - 1], b'\n');
        assert!(
            b.html.ends_with("// &lt;b&gt;\n"),
            "{}",
            &b.html[b.html.len() - 40..]
        );
        // JSON и YAML узнаются по расширению
        assert_eq!(
            highlight_source("a.json", "{\"a\": 1}", 1024)
                .syntax
                .as_deref(),
            Some("JSON")
        );
        assert!(highlight_source("a.yaml", "a: 1\n", 1024).syntax.is_some());
        assert_eq!(p.skipped, None);
    }

    /// Раздувание разметки (ревью задачи 8, I-1): `[` в JSON, тройные кавычки в Python, якоря
    /// YAML — HTML не больше предела; сверх него — экранированный текст и `too_large`.
    #[test]
    fn source_blowup_capped() {
        for (name, code) in [
            ("a.json", "[".repeat(256 * 1024)),
            ("a.py", "\'\'\'\n".repeat(64 * 1024)),
            ("a.yaml", "&a \n".repeat(64 * 1024)),
        ] {
            let t = std::time::Instant::now();
            let h = highlight_source(name, &code, HIGHLIGHT_BUDGET_BYTES);
            eprintln!(
                "{name}: {} Б → {} Б, {:?}, {:?}",
                code.len(),
                h.html.len(),
                h.skipped,
                t.elapsed()
            );
            match h.skipped {
                Some(s) => {
                    assert_eq!((s, h.syntax.as_deref()), ("too_large", None));
                    assert!(h.html.len() <= 6 * code.len(), "{name}");
                }
                None => assert!(h.html.len() <= highlight_html_max(code.len()), "{name}"),
            }
        }
        let j = highlight_source("a.json", &"[".repeat(256 * 1024), HIGHLIGHT_BUDGET_BYTES);
        assert_eq!(j.skipped, Some("too_large"));
        // обычный код предел не задевает
        let own = highlight_source("h.rs", include_str!("highlight.rs"), 64 * 1024);
        assert_eq!((own.skipped, own.syntax.as_deref()), (None, Some("Rust")));
        assert_eq!(j.html.len(), 256 * 1024);
    }
}
