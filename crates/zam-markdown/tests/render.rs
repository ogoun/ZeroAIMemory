//! HTML-рендер: заголовки, sourcepos, подсветка, mermaid, формулы, callout,
//! вики-ссылки, вложения, CSS подсветки и сверка `id` заголовков с comrak.

use zam_markdown::{
    LinkKind, LinkResolution, NoResolve, RENDER_VERSION, Rendered, Resolver, comrak_options,
    highlight_css, outline, parse, render_html,
};

const SHA: &str = "0123456789abcdef";

/// Тестовый резолвер: существует только заметка «Есть», вложения — только с `SHA`.
struct TestResolver;

impl Resolver for TestResolver {
    fn wiki(&self, target: &str, anchor: Option<&str>) -> Option<LinkResolution> {
        match target {
            "Есть" => Some(LinkResolution {
                href: match anchor {
                    Some(a) => format!("notes/Есть#h-{a}"),
                    None => "notes/Есть".into(),
                },
                exists: true,
            }),
            "Будет" => Some(LinkResolution {
                href: "notes/Будет".into(),
                exists: false,
            }),
            _ => None,
        }
    }

    fn attachment(&self, sha16: &str, filename: &str) -> Option<String> {
        (sha16 == SHA).then(|| format!("api/v1/blobs/{sha16}/{filename}"))
    }
}

fn html(src: &str) -> String {
    render_html(src, &TestResolver).html
}

// ---------- Шаг 1: рендер ----------

#[test]
fn render_version_is_three() {
    assert_eq!(RENDER_VERSION, 3);
}

#[test]
fn heading_gets_prefixed_id() {
    let h = html("# Установка\n\nтекст\n");
    assert!(h.contains(r#"id="h-установка""#), "{h}");
    // ссылки-якоря comrak с неверным href в вывод не попадают
    assert!(!h.contains("class=\"anchor\""), "{h}");
}

#[test]
fn sourcepos_present() {
    let h = html("# A\n\nабзац\n");
    assert!(h.contains("data-sourcepos=\""), "{h}");
}

#[test]
fn rust_code_highlighted_with_hl_classes() {
    let h = html("```rust\nfn main() { let x = 1; }\n```\n");
    assert!(h.contains("class=\"hl-"), "{h}");
    assert!(h.contains("<pre"), "{h}");
    assert!(h.contains("main"), "{h}");
}

#[test]
fn unknown_language_is_plain_but_escaped() {
    let h = html("```nonexistent-lang\n<b>x</b>\n```\n");
    assert!(h.contains("&lt;b&gt;x&lt;/b&gt;"), "{h}");
    assert!(!h.contains("<b>"), "{h}");
}

#[test]
fn mermaid_not_highlighted() {
    let h = html("```mermaid\ngraph TD; A-->B;\n```\n");
    assert!(h.contains(r#"<pre class="mermaid-src""#), "{h}");
    assert!(h.contains("graph TD; A--&gt;B;"), "{h}");
    let start = h.find("mermaid-src").unwrap();
    let end = h[start..].find("</pre>").unwrap() + start;
    assert!(!h[start..end].contains("hl-"), "{h}");
}

#[test]
fn math_inline_display_and_block() {
    let h = html("$x$ и $$y$$\n\n```math\nz^2\n```\n");
    assert!(h.contains("class=\"math\""), "{h}");
    assert!(h.contains(r#"data-math-style="inline""#), "{h}");
    assert!(h.contains(r#"data-math-style="display""#), "{h}");
    assert!(h.contains("z^2"), "{h}");
    assert!(!h.contains("language-math"), "{h}");
}

#[test]
fn obsidian_callout_folded() {
    let h = html("> [!info]- Подробности\n> скрыто\n");
    assert!(h.contains("<details"), "{h}");
    assert!(h.contains(r#"data-callout="info""#), "{h}");
    assert!(h.contains(r#"data-fold="closed""#), "{h}");
    assert!(h.contains("<summary class=\"callout-title\""), "{h}");
    assert!(h.contains("Подробности"), "{h}");
    assert!(h.contains("скрыто"), "{h}");
    // тело — отдельный абзац со своей позицией, заголовок — только первая строка
    assert!(
        h.contains(r#"<p data-sourcepos="2:3-2:14">скрыто</p>"#),
        "{h}"
    );
    assert!(h.contains(r#"data-sourcepos="1:3-1:"#), "{h}");
    assert!(!h.contains("[!info]"), "{h}");
    assert!(!h.contains("<blockquote"), "{h}");
}

#[test]
fn obsidian_callout_open_and_plain() {
    let h = html("> [!Tip]+ Совет\n> текст\n");
    assert!(h.contains("<details"), "{h}");
    assert!(h.contains(" open"), "{h}");
    assert!(h.contains(r#"data-callout="tip""#), "{h}");
    assert!(h.contains(r#"data-fold="open""#), "{h}");

    let h = html("> [!my-type]\n> тело\n");
    assert!(
        h.contains(r#"<div class="callout" data-callout="my-type""#),
        "{h}"
    );
    assert!(h.contains(r#"data-fold="none""#), "{h}");
    assert!(h.contains("<div class=\"callout-title\""), "{h}");
    // без заголовка — тип как заголовок
    assert!(h.contains("My-type"), "{h}");
    assert!(!h.contains("<details"), "{h}");
}

#[test]
fn callout_title_keeps_inline_markup() {
    let h = html("> [!info] Заголовок **жирный**\n> тело\n");
    let t = h.find("callout-title").unwrap();
    let rest = &h[t..];
    let title_end = rest.find("</div>").unwrap();
    assert!(rest[..title_end].contains("<strong"), "{h}");
    assert!(!rest[..title_end].contains("тело"), "{h}");
}

#[test]
fn github_alert_same_markup() {
    let h = html("> [!WARNING]\n> осторожно\n");
    assert!(
        h.contains(r#"<div class="callout" data-callout="warning""#),
        "{h}"
    );
    assert!(h.contains(r#"data-fold="none""#), "{h}");
    assert!(h.contains("callout-title"), "{h}");
    assert!(h.contains("Warning"), "{h}");
    assert!(!h.contains("markdown-alert"), "{h}");

    // заголовок alert comrak разбирает как текст: разметка экранируется
    let h = html("> [!NOTE] Мой <b>заголовок</b>\n> текст\n");
    assert!(h.contains("Мой &lt;b&gt;заголовок&lt;/b&gt;"), "{h}");

    let h = html("> [!note]- Свернуто\n> текст\n");
    assert!(h.contains("<details"), "{h}");
    assert!(h.contains(r#"data-callout="note""#), "{h}");
    assert!(h.contains("Свернуто"), "{h}");
}

#[test]
fn plain_blockquote_untouched() {
    let h = html("> просто цитата [!x]\n");
    assert!(h.contains("<blockquote"), "{h}");
    assert!(!h.contains("callout"), "{h}");
}

#[test]
fn wikilink_resolved_and_broken() {
    let r = render_html(
        "См. [[Есть#Раздел|тут]], [[Есть]] и [[Нет]].\n",
        &TestResolver,
    );
    let h = &r.html;
    assert!(
        h.contains(r#"class="wikilink" href="notes/%D0%95%D1%81%D1%82%D1%8C#h-%D0%A0%D0%B0%D0%B7%D0%B4%D0%B5%D0%BB""#)
            || h.contains(r#"class="wikilink" href="notes/Есть#h-Раздел""#),
        "{h}"
    );
    assert!(h.contains(">тут</a>"), "{h}");
    assert!(h.contains(r#"class="wikilink broken""#), "{h}");
    assert!(h.contains(r#"data-target="Нет""#), "{h}");
    // у битой ссылки нет href
    let b = h.find("wikilink broken").unwrap();
    let tag_end = h[b..].find('>').unwrap() + b;
    assert!(!h[b..tag_end].contains("href"), "{h}");
    assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
    assert!(r.warnings[0].contains("Нет"), "{:?}", r.warnings);
}

#[test]
fn wikilink_resolved_but_missing_is_broken_with_href() {
    let r = render_html("[[Будет]]\n", &TestResolver);
    assert!(r.html.contains(r#"class="wikilink broken""#), "{}", r.html);
    assert!(r.html.contains(r#"href="notes/"#), "{}", r.html);
    assert_eq!(r.warnings.len(), 1);
}

#[test]
fn attachments_by_mime() {
    let r = render_html(
        &format!(
            "![видео](_attachments/{SHA}/clip.mp4)\n\n![кот](_attachments/{SHA}/cat.png)\n\n\
             ![отчёт](_attachments/{SHA}/report.pdf)\n\n![звук](_attachments/{SHA}/a.mp3)\n"
        ),
        &TestResolver,
    );
    let h = &r.html;
    assert!(h.contains("<video"), "{h}");
    assert!(h.contains("controls"), "{h}");
    assert!(h.contains(r#"preload="metadata""#), "{h}");
    assert!(h.contains(&format!("api/v1/blobs/{SHA}/clip.mp4")), "{h}");
    assert!(h.contains("<img"), "{h}");
    assert!(
        h.contains(&format!(r#"src="api/v1/blobs/{SHA}/cat.png""#)),
        "{h}"
    );
    assert!(h.contains(r#"alt="кот""#), "{h}");
    assert!(h.contains(r#"<a class="attachment""#), "{h}");
    assert!(h.contains("download"), "{h}");
    assert!(h.contains("<audio"), "{h}");
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
}

#[test]
fn attachment_filename_percent_decoded() {
    struct Spy;
    impl Resolver for Spy {
        fn wiki(&self, _: &str, _: Option<&str>) -> Option<LinkResolution> {
            None
        }
        fn attachment(&self, sha16: &str, filename: &str) -> Option<String> {
            (filename == "мой файл.png").then(|| format!("blob/{sha16}"))
        }
    }
    let r = render_html(
        &format!("![x](_attachments/{SHA}/%D0%BC%D0%BE%D0%B9%20%D1%84%D0%B0%D0%B9%D0%BB.png)\n"),
        &Spy,
    );
    assert!(
        r.html.contains(&format!(r#"src="blob/{SHA}""#)),
        "{}",
        r.html
    );
}

#[test]
fn attachment_ref_normalizes_sha_and_strips_query_and_fragment() {
    let up = SHA.to_ascii_uppercase();
    let r = render_html(
        &format!(
            "![a](_attachments/{up}/cat.png?v=2)\n\n![b](_attachments/{SHA}/clip.mp4#t=10)\n\n\
             ![c](_attachments/{SHA}/a%20b.png?x#y)\n"
        ),
        &TestResolver,
    );
    let h = &r.html;
    assert!(
        h.contains(&format!(r#"src="api/v1/blobs/{SHA}/cat.png""#)),
        "{h}"
    );
    // MIME — по имени без `#…`: видео, а не файл
    assert!(h.contains("<video"), "{h}");
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert_eq!(
        zam_markdown::attachment_ref(&format!("./_attachments/{up}/a%20b.png?x#y")),
        Some((SHA.to_string(), Some("a b.png".to_string())))
    );
    assert_eq!(zam_markdown::attachment_ref("_attachments/xyz/a.png"), None);
}

#[test]
fn attachment_unresolved_is_text_and_warning() {
    let r = render_html(
        "![подпись](_attachments/ffffffffffffffff/x.png)\n",
        &TestResolver,
    );
    assert!(!r.html.contains("<img"), "{}", r.html);
    assert!(r.html.contains("подпись"), "{}", r.html);
    assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
}

#[test]
fn attachment_markdown_link_resolved() {
    let r = render_html(
        &format!("[файл](_attachments/{SHA}/doc.docx)\n"),
        &TestResolver,
    );
    assert!(r.html.contains(r#"class="attachment""#), "{}", r.html);
    assert!(
        r.html.contains(&format!("api/v1/blobs/{SHA}/doc.docx")),
        "{}",
        r.html
    );
}

#[test]
fn ordinary_image_and_external_link() {
    let h = html("![a](img/a.png) [сайт](https://example.com) [внутр](other.md)\n");
    assert!(h.contains(r#"src="img/a.png""#), "{h}");
    let ext = h.find("https://example.com").unwrap();
    let tag_start = h[..ext].rfind('<').unwrap();
    let tag_end = h[ext..].find('>').unwrap() + ext;
    assert!(
        h[tag_start..tag_end].contains(r#"rel="noopener noreferrer""#),
        "{h}"
    );
    let int = h.find("other.md").unwrap();
    let tag_start = h[..int].rfind('<').unwrap();
    let tag_end = h[int..].find('>').unwrap() + int;
    assert!(!h[tag_start..tag_end].contains("rel="), "{h}");
}

#[test]
fn gfm_tasks_tables_footnotes() {
    let h = html(
        "- [x] сделано\n- [ ] нет\n\n| a | b |\n|:-|-:|\n| 1 | 2 |\n\nтекст[^1]\n\n[^1]: сноска\n\n~~зач~~\n",
    );
    assert!(h.contains(r#"type="checkbox""#), "{h}");
    assert!(h.contains("checked"), "{h}");
    assert!(h.contains("disabled"), "{h}");
    assert!(h.contains("task-list-item"), "{h}");
    assert!(h.contains("<table"), "{h}");
    assert!(h.contains("<td"), "{h}");
    assert!(h.contains("<section"), "{h}");
    assert!(h.contains(r#"class="footnotes""#), "{h}");
    assert!(h.contains("footnote-ref"), "{h}");
    assert!(h.contains(r##"href="#fn-1""##), "{h}");
    assert!(h.contains(r#"id="fn-1""#), "{h}");
    assert!(h.contains("<del"), "{h}");
}

#[test]
fn frontmatter_not_rendered() {
    let h = html("---\ntags: [секрет]\n---\n# A\n");
    assert!(!h.contains("секрет"), "{h}");
    assert!(!h.contains("tags"), "{h}");
}

#[test]
fn toc_and_links_from_parse() {
    let src = "# A\n## B\n[[Есть]] [x](y.md)\n";
    let r: Rendered = render_html(src, &NoResolve);
    let doc = parse(src);
    assert_eq!(r.toc, outline(&doc));
    assert_eq!(r.links, doc.links);
    assert_eq!(r.links[0].kind, LinkKind::Wiki);
}

#[test]
fn no_resolve_everything_broken() {
    let r = render_html(
        &format!("[[A]] ![x](_attachments/{SHA}/a.png)\n"),
        &NoResolve,
    );
    assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
    assert!(r.html.contains("wikilink broken"), "{}", r.html);
}

#[test]
fn render_never_panics_on_odd_input() {
    for src in [
        "",
        "\u{FEFF}",
        "> [!",
        "> [!]",
        "> [!x",
        ">\n> [!info]",
        "> [!info]\n",
        "- > [!info]- в списке\n  > текст",
        "> > [!tip] вложенный",
        "> [!info]\\\n> после жёсткого переноса",
        "[[]]",
        "[[#якорь]]",
        "![](_attachments/0123456789abcdef/)",
        "![](_attachments/0123456789abcdef/%FF%FE.png)",
        "```\n```",
        "$$",
    ] {
        let _ = render_html(src, &TestResolver);
    }
}

// ---------- Шаг 3: CSS подсветки ----------

#[test]
fn highlight_css_has_light_and_dark() {
    let css = highlight_css();
    assert!(css.contains(".hl-"), "{css}");
    assert!(css.contains("@media (prefers-color-scheme: dark)"), "{css}");
}

// ---------- Сверка якорей заголовков с comrak ----------

/// `id` заголовков вида `id="h-…"` в порядке появления.
fn heading_ids(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find(" id=\"h-") {
        let v = &rest[i + 5..];
        let end = v.find('"').unwrap();
        out.push(v[..end].to_string());
        rest = &v[end..];
    }
    out
}

#[test]
fn heading_anchor_matches_rendered_id() {
    let src = "\
# Установка
# Установка
## Код `x` и *курсив* и [ссылка](u.md)
### Привет, мир! — 2024
> # В цитате
- ## В списке
# Установка
Setext заголовок
----------------
## Ёлка  Пробелы
#
# Установка-1
";
    let doc = parse(src);
    let rendered = render_html(src, &NoResolve);
    let ours = heading_ids(&rendered.html);
    let theirs = heading_ids(&comrak::markdown_to_html(src, &comrak_options()));
    // наш форматтер даёт те же id, что и comrak
    assert_eq!(ours, theirs);
    // «h-» + Heading.anchor — подпоследовательность id документа (вложенные
    // заголовки тоже участвуют в нумерации дублей)
    let mut it = ours.iter();
    for h in &doc.headings {
        let want = format!("h-{}", h.anchor);
        assert!(
            it.any(|id| *id == want),
            "нет id {want} для {:?} в {ours:?}",
            h.text
        );
    }
    // дубли пронумерованы
    assert!(ours.contains(&"h-установка-1".to_string()), "{ours:?}");
    assert!(ours.contains(&"h-установка-2".to_string()), "{ours:?}");
}

#[test]
fn cyrillic_callout_type() {
    let h = html("> [!Заметка]\n> текст\n");
    assert!(
        h.contains(r#"<div class="callout" data-callout="заметка""#),
        "{h}"
    );
    // без заголовка — тип с заглавной буквы
    assert!(h.contains(">Заметка<"), "{h}");
    assert!(!h.contains("[!"), "{h}");

    let h = html("> [!важно_2]- Свёрнуто\n> тело\n");
    assert!(h.contains(r#"data-callout="важно_2""#), "{h}");
    assert!(h.contains(r#"data-fold="closed""#), "{h}");
}

#[test]
fn callout_type_with_punctuation_is_plain_quote() {
    for src in [
        "> [!a\"b]\n> x\n",
        "> [!a b]\n> x\n",
        "> [!a<b>]\n> x\n",
        "> [!тип.точка]\n> x\n",
    ] {
        let h = html(src);
        assert!(h.contains("<blockquote"), "{src:?}: {h}");
        assert!(!h.contains("data-callout"), "{src:?}: {h}");
    }
}

// ---------- Бюджет подсветки (ТЗ §17) ----------

/// Блок Rust примерно на `bytes` байт.
fn rust_block(bytes: usize) -> String {
    let line = "fn f(x: u32) -> u32 { let y = x * 2; y + 1 } // комментарий\n";
    let mut s = String::from("```rust\n");
    while s.len() < bytes {
        s.push_str(line);
    }
    s.push_str("```\n\n");
    s
}

#[test]
fn highlight_budget_per_document() {
    // два блока по 150 КБ: первый подсвечен, второй уже не помещается в 256 КиБ
    let src = format!("{}{}", rust_block(150 * 1024), rust_block(150 * 1024));
    let h = html(&src);
    assert_eq!(
        h.matches("<pre class=\"hl-code\"").count(),
        1,
        "подсвечен один блок"
    );
    assert_eq!(
        h.matches("<pre class=\"hl-skipped\"").count(),
        1,
        "второй — без подсветки"
    );
    // пропущенный блок — экранированный текст без span-ов
    let skipped = &h[h.find("hl-skipped").unwrap()..];
    assert!(
        !skipped.contains("<span"),
        "в пропущенном блоке нет подсветки"
    );
    assert!(skipped.contains("-&gt; u32"), "текст экранирован");
}

#[test]
fn block_is_highlighted_only_if_it_fits_remaining_budget() {
    // блок больше бюджета целиком не подсвечивается и бюджет не тратит:
    // следующий малый блок подсвечен
    let src = format!("{}```rust\nfn main() {{}}\n```\n", rust_block(260 * 1024));
    let h = html(&src);
    assert_eq!(h.matches("<pre class=\"hl-skipped\"").count(), 1);
    assert_eq!(h.matches("<pre class=\"hl-code\"").count(), 1);

    // 200 КБ + 60 КБ: второй не помещается в остаток, третий (малый) помещается
    let src = format!(
        "{}{}```rust\nfn main() {{}}\n```\n",
        rust_block(200 * 1024),
        rust_block(60 * 1024)
    );
    let h = html(&src);
    assert_eq!(h.matches("<pre class=\"hl-code\"").count(), 2);
    assert_eq!(h.matches("<pre class=\"hl-skipped\"").count(), 1);

    // блоки без языка и с неизвестным языком — обычный `<pre>`, не `hl-skipped`
    let h = html("```\nx\n```\n\n```нет-такого\ny\n```\n");
    assert!(!h.contains("hl-skipped"), "{h}");
}

#[test]
fn megabyte_note_with_large_code_blocks_renders_bounded() {
    // 1 МБ: четыре блока по ~250 КБ кода Rust; первый — настоящий код (исходник
    // рендера), остальные — плотный синтетический
    let mut real = String::from("```rust\n");
    for line in include_str!("../src/render.rs").lines().cycle() {
        if real.len() + line.len() + 1 > 250 * 1024 {
            break;
        }
        real.push_str(line);
        real.push('\n');
    }
    real.push_str("```\n\n");
    let src = format!("{real}{}", rust_block(250 * 1024).repeat(3));
    assert!(src.len() >= 1_000_000);
    let started = std::time::Instant::now();
    let h = html(&src);
    let took = started.elapsed();
    eprintln!("рендер 1 МБ: {took:?}, HTML {} байт", h.len());
    assert_eq!(h.matches("<pre class=\"hl-code\"").count(), 1);
    assert_eq!(h.matches("<pre class=\"hl-skipped\"").count(), 3);
    // подсветка раздувает HTML; без бюджета вывод был бы в разы больше
    assert!(h.len() < 8 * src.len(), "HTML {} байт", h.len());
    if !cfg!(debug_assertions) {
        assert!(took < std::time::Duration::from_secs(1), "{took:?}");
    }
}

#[test]
fn highlight_css_is_cached() {
    assert!(std::ptr::eq(highlight_css(), highlight_css()));
}
