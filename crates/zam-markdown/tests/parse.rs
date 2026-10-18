//! Разбор Markdown: позиции, golden-файлы, сверка с pulldown-cmark и property-тесты.

use std::fs;
use std::path::{Path, PathBuf};

use proptest::prelude::*;
use pulldown_cmark::{Event, Options as PdOptions, Parser, Tag};
use serde::Serialize;
use zam_markdown::{
    Doc, Frontmatter, LineIndex, Link, LinkKind, Section, first_paragraph, outline, parse,
    section_by_path, slug,
};

// ---------- Шаг 1: позиции ----------

#[test]
fn line_index_lf() {
    let src = "ab\ncd\n\nef";
    let li = LineIndex::new(src);
    assert_eq!(li.offset(1, 1), 0);
    assert_eq!(li.offset(1, 2), 1);
    assert_eq!(li.offset(2, 1), 3);
    assert_eq!(li.offset(3, 1), 6);
    assert_eq!(li.offset(4, 2), 8);
    assert_eq!(li.line_count(), 4);
}

#[test]
fn line_index_crlf_and_lone_cr() {
    let src = "ab\r\ncd\r\n\r\nef\rgh";
    let li = LineIndex::new(src);
    assert_eq!(li.offset(2, 1), 4);
    assert_eq!(li.offset(3, 1), 8);
    assert_eq!(li.offset(4, 1), 10);
    // одиночный \r — тоже конец строки (как в CommonMark и comrak)
    assert_eq!(li.offset(5, 1), 13);
    assert!(src[li.offset(5, 1)..].starts_with("gh"));
}

#[test]
fn line_index_bom_and_cyrillic_bytes() {
    let src = "\u{FEFF}# Привет\nмир";
    let li = LineIndex::new(src);
    // колонки — в байтах от начала строки, BOM входит в первую строку
    assert!(src[li.offset(1, 4)..].starts_with("# Привет"));
    assert!(src[li.offset(1, 6)..].starts_with("Привет"));
    // «П» занимает 2 байта: колонка 8 — начало «р»
    assert!(src[li.offset(1, 8)..].starts_with("ривет"));
    assert!(src[li.offset(2, 1)..].starts_with("мир"));
}

#[test]
fn line_index_clamps_out_of_range() {
    let src = "a\nb";
    let li = LineIndex::new(src);
    assert_eq!(li.offset(10, 1), src.len());
    assert_eq!(li.offset(2, 100), src.len());
    assert_eq!(li.offset(0, 0), 0);
}

#[test]
fn heading_offsets_point_at_heading_text() {
    let cases: [(&str, &[&str]); 4] = [
        ("# A\n\ntext\n\n## B\n", &["# A", "## B"]),
        ("# A\r\n\r\ntext\r\n## B\r\n", &["# A", "## B"]),
        ("\u{FEFF}# Первый\n## Второй\n", &["# Первый", "## Второй"]),
        (
            "Абзац\n\n# Заголовок раз\n\nСетекст\n---\n",
            &["# Заголовок раз", "Сетекст\n---"],
        ),
    ];
    for (src, expected) in cases {
        let doc = parse(src);
        assert_eq!(doc.headings.len(), expected.len(), "{src:?}");
        for (h, want) in doc.headings.iter().zip(expected) {
            assert!(
                src[h.range.start..].starts_with(want),
                "{src:?}: {:?} не начинается с {want:?}",
                &src[h.range.start..]
            );
        }
    }
}

#[test]
fn heading_range_covers_whole_lines() {
    let src = "# A\r\nbody\r\n\r\nB\r\n===\r\nend";
    let doc = parse(src);
    assert_eq!(&src[doc.headings[0].range.clone()], "# A\r\n");
    assert_eq!(&src[doc.headings[1].range.clone()], "B\r\n===\r\n");
    assert_eq!(&src[doc.sections[0].body.clone()], "body\r\n\r\n");
    // отступ до трёх пробелов: диапазон начинается с `#`, но берёт строку до конца
    let src = "   ## B ##\nx\n";
    let h = &parse(src).headings[0];
    assert_eq!(&src[h.range.clone()], "## B ##\n");
}

// ---------- API ----------

#[test]
fn body_start_skips_bom_and_frontmatter() {
    assert_eq!(parse("# A\n").body_start, 0);
    assert_eq!(parse("\u{FEFF}# A\n").body_start, 3);
    let src = "---\ntitle: x\n---\n\n# A\n";
    let doc = parse(src);
    assert!(src[doc.body_start..].starts_with("# A"));
    let fm = doc.frontmatter.expect("frontmatter");
    assert_eq!(fm.raw, "title: x\n");
}

#[test]
fn frontmatter_fields() {
    let src = "---\ntags: [a, \"b c\"]\ndescription: 'Описание'\nl0: Суть.\nindex: false\n---\n";
    let fm = parse(src).frontmatter.expect("frontmatter");
    assert_eq!(fm.tags, vec!["a".to_string(), "b c".to_string()]);
    assert_eq!(fm.description.as_deref(), Some("Описание"));
    assert_eq!(fm.l0.as_deref(), Some("Суть."));
    assert_eq!(fm.index, Some(false));

    let src = "---\ntags:\n  - x\n  - \"y\"\n---\nтекст\n";
    let fm = parse(src).frontmatter.expect("frontmatter");
    assert_eq!(fm.tags, vec!["x".to_string(), "y".to_string()]);
    assert_eq!(fm.index, None);
}

#[test]
fn slug_matches_comrak_and_keeps_unicode() {
    assert_eq!(slug("Hello, World!"), "hello-world");
    assert_eq!(slug("Установка в Linux"), "установка-в-linux");
    assert_eq!(slug("snake_case и-дефис"), "snake_case-и-дефис");
}

#[test]
fn duplicate_anchors_are_unique() {
    let doc = parse("# Дубль\n\n# Дубль\n");
    assert_eq!(doc.headings[0].anchor, "дубль");
    assert_eq!(doc.headings[1].anchor, "дубль-1");
}

#[test]
fn outline_and_section_by_path() {
    let src = "intro\n\n# Установка\n## Linux\n### Debian\n## Windows\n#### мелкий\n";
    let doc = parse(src);
    let items = outline(&doc);
    let titles: Vec<_> = items.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["Установка", "Linux", "Debian", "Windows"]);
    assert_eq!(items[2].path, ["Установка", "Linux", "Debian"]);
    assert_eq!(items[1].anchor, "linux");

    let s = section_by_path(&doc, "Установка > Linux").expect("секция");
    assert_eq!(&src[s.range.clone()], "## Linux\n### Debian\n");
    let s = section_by_path(&doc, " Установка >Windows ").expect("секция");
    assert_eq!(&src[s.range.clone()], "## Windows\n#### мелкий\n");
    assert!(section_by_path(&doc, "Linux").is_none());
    assert!(section_by_path(&doc, "").is_none());
}

#[test]
fn first_paragraph_plain_text() {
    let src = "---\nl0: x\n---\n# Заголовок\n\nПервый **жирный** и [ссылка](a.md)\nвторая строка.\n\nВторой.\n";
    let doc = parse(src);
    assert_eq!(
        first_paragraph(src, &doc).as_deref(),
        Some("Первый жирный и ссылка вторая строка.")
    );
    let src = "# Только заголовок\n";
    assert_eq!(first_paragraph(src, &parse(src)), None);
}

#[test]
fn preamble_only_when_non_empty() {
    assert_eq!(parse("\n\n# A\n").sections[0].level, 1);
    let doc = parse("текст\n# A\n");
    assert_eq!(doc.sections[0].level, 0);
    assert!(doc.sections[0].title.is_empty());
    assert!(doc.sections[0].path.is_empty());
    assert!(parse("").sections.is_empty());
}

#[test]
fn link_ranges_with_bom_crlf_and_cyrillic() {
    let src = "\u{FEFF}[[Заметка#Раздел|текст]] и ![кот](img/кот.png)\r\n\r\nтекст [ссылка](путь.md#якорь)";
    let doc = parse(src);
    let texts: Vec<_> = doc.links.iter().map(|l| &src[l.range.clone()]).collect();
    assert_eq!(
        texts,
        [
            "[[Заметка#Раздел|текст]]",
            "![кот](img/кот.png)",
            "[ссылка](путь.md#якорь)"
        ]
    );
    let w = &doc.links[0];
    assert_eq!(w.kind, LinkKind::Wiki);
    assert_eq!(w.target, "Заметка");
    assert_eq!(w.anchor.as_deref(), Some("Раздел"));
    assert_eq!(w.label.as_deref(), Some("текст"));
    assert_eq!(doc.links[1].kind, LinkKind::Image);
    assert_eq!(doc.links[1].label.as_deref(), Some("кот"));
    assert_eq!(doc.links[2].target, "путь.md");
    assert_eq!(doc.links[2].anchor.as_deref(), Some("якорь"));
}

// ---------- Шаг 2: golden ----------

#[derive(Serialize)]
struct GoldenSection<'a> {
    level: u8,
    title: &'a str,
    path: &'a [String],
    anchor: &'a str,
    range: [usize; 2],
    body: [usize; 2],
    text: &'a str,
}

#[derive(Serialize)]
struct GoldenLink<'a> {
    kind: &'static str,
    target: &'a str,
    anchor: Option<&'a str>,
    label: Option<&'a str>,
    range: [usize; 2],
    text: &'a str,
}

#[derive(Serialize)]
struct Golden<'a> {
    body_start: usize,
    frontmatter: Option<&'a Frontmatter>,
    sections: Vec<GoldenSection<'a>>,
    links: Vec<GoldenLink<'a>>,
}

fn golden_of<'a>(src: &'a str, doc: &'a Doc) -> Golden<'a> {
    let sec = |s: &'a Section| GoldenSection {
        level: s.level,
        title: &s.title,
        path: &s.path,
        anchor: &s.anchor,
        range: [s.range.start, s.range.end],
        body: [s.body.start, s.body.end],
        text: &src[s.range.clone()],
    };
    let link = |l: &'a Link| GoldenLink {
        kind: match l.kind {
            LinkKind::Wiki => "wiki",
            LinkKind::Markdown => "markdown",
            LinkKind::Image => "image",
        },
        target: &l.target,
        anchor: l.anchor.as_deref(),
        label: l.label.as_deref(),
        range: [l.range.start, l.range.end],
        text: &src[l.range.clone()],
    };
    Golden {
        body_start: doc.body_start,
        frontmatter: doc.frontmatter.as_ref(),
        sections: doc.sections.iter().map(sec).collect(),
        links: doc.links.iter().map(link).collect(),
    }
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
}

fn golden_sources() -> Vec<(PathBuf, String)> {
    let mut out: Vec<_> = fs::read_dir(golden_dir())
        .expect("каталог golden")
        .map(|e| e.expect("запись каталога").path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .map(|p| {
            let src = fs::read_to_string(&p).expect("чтение фикстуры");
            (p, src)
        })
        .collect();
    out.sort();
    out
}

#[test]
fn golden_files() {
    let bless = std::env::var("ZAM_BLESS").is_ok_and(|v| v == "1");
    let sources = golden_sources();
    assert!(sources.len() >= 8, "нужно не меньше 8 фикстур");
    let mut failed = Vec::new();
    for (path, src) in &sources {
        let doc = parse(src);
        let actual = serde_json::to_string_pretty(&golden_of(src, &doc)).expect("json") + "\n";
        let json_path = path.with_extension("json");
        if bless {
            fs::write(&json_path, &actual).expect("запись golden");
            continue;
        }
        match fs::read_to_string(&json_path) {
            Ok(expected) if expected.replace("\r\n", "\n") == actual => {}
            Ok(_) => failed.push(format!("{}: расхождение", path.display())),
            Err(_) => failed.push(format!("{}: нет .json", path.display())),
        }
    }
    assert!(
        failed.is_empty(),
        "golden расходятся (ZAM_BLESS=1 для обновления):\n{}",
        failed.join("\n")
    );
}

// ---------- Шаг 3: сверка с pulldown-cmark ----------

/// Заголовки верхнего уровня по pulldown-cmark: (начало, номер строки последнего символа).
fn pulldown_headings(src: &str) -> Vec<(usize, usize)> {
    // pulldown-cmark не снимает BOM, а YAML-блоки ищет не только в начале файла:
    // разбираем тело после BOM и frontmatter (их границу проверяют отдельные тесты)
    let bom = parse(src).body_start;
    let body = &src[bom..];
    let mut opts = PdOptions::empty();
    opts.insert(PdOptions::ENABLE_TABLES);
    opts.insert(PdOptions::ENABLE_STRIKETHROUGH);
    opts.insert(PdOptions::ENABLE_TASKLISTS);
    opts.insert(PdOptions::ENABLE_FOOTNOTES);
    let li = LineIndex::new(src);
    let mut depth = 0usize;
    let mut out = Vec::new();
    for (ev, range) in Parser::new_ext(body, opts).into_offset_iter() {
        match ev {
            Event::Start(Tag::Heading { .. }) if depth == 0 => {
                let end = trim_line_end(src, bom + range.end);
                out.push((bom + range.start, li.line_of(end.saturating_sub(1))));
                depth += 1;
            }
            Event::Start(_) => depth += 1,
            Event::End(_) => depth -= 1,
            _ => {}
        }
    }
    out
}

fn trim_line_end(src: &str, mut end: usize) -> usize {
    while end > 0 && matches!(src.as_bytes()[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    end
}

fn zam_headings(src: &str) -> Vec<(usize, usize)> {
    let doc = parse(src);
    let li = LineIndex::new(src);
    doc.headings
        .iter()
        .map(|h| {
            let end = trim_line_end(src, h.range.end);
            (h.range.start, li.line_of(end.saturating_sub(1)))
        })
        .collect()
}

#[test]
fn headings_match_pulldown_on_golden() {
    for (path, src) in golden_sources() {
        assert_eq!(
            zam_headings(&src),
            pulldown_headings(&src),
            "{}",
            path.display()
        );
    }
}

fn md_line() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("# Заголовок".to_string()),
        Just("## Второй".to_string()),
        Just("### Третий #".to_string()),
        Just("#### Четвёртый".to_string()),
        Just("#без пробела".to_string()),
        Just("текст абзаца".to_string()),
        Just("Setext".to_string()),
        Just("===".to_string()),
        Just("---".to_string()),
        Just("".to_string()),
        Just("- пункт".to_string()),
        Just("- # заголовок в списке".to_string()),
        Just("  продолжение".to_string()),
        Just("1. нумерованный".to_string()),
        Just("> цитата".to_string()),
        Just("> ## в цитате".to_string()),
        Just("```".to_string()),
        Just("~~~".to_string()),
        Just("[[ссылка#якорь|текст]]".to_string()),
        "[a-zа-я ]{0,12}",
    ]
}

fn md_doc() -> impl Strategy<Value = String> {
    (prop::collection::vec(md_line(), 0..24), any::<bool>()).prop_map(|(lines, crlf)| {
        let nl = if crlf { "\r\n" } else { "\n" };
        lines.join(nl)
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]
    #[test]
    fn headings_match_pulldown_generated(src in md_doc()) {
        prop_assert_eq!(zam_headings(&src), pulldown_headings(&src), "{:?}", src);
    }
}

// ---------- Шаг 4: без паники ----------

fn assert_range(src: &str, r: &std::ops::Range<usize>) -> Result<(), TestCaseError> {
    prop_assert!(
        r.start <= r.end && r.end <= src.len(),
        "{:?} вне 0..{}",
        r,
        src.len()
    );
    prop_assert!(
        src.is_char_boundary(r.start) && src.is_char_boundary(r.end),
        "{:?}",
        r
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]
    #[test]
    fn parse_never_panics(src in any::<String>()) {
        let doc = parse(&src);
        prop_assert!(doc.body_start <= src.len() && src.is_char_boundary(doc.body_start));
        for s in &doc.sections {
            assert_range(&src, &s.range)?;
            assert_range(&src, &s.body)?;
        }
        for h in &doc.headings {
            assert_range(&src, &h.range)?;
        }
        for l in &doc.links {
            assert_range(&src, &l.range)?;
        }
        let _ = first_paragraph(&src, &doc);
    }

    #[test]
    fn parse_never_panics_markdownish(src in md_doc()) {
        let doc = parse(&src);
        // корневые секции (преамбула и секции без родителя) идут подряд и покрывают
        // тело; до первой из них — только пробельные символы
        let mut roots = doc.sections.iter().filter(|s| s.path.len() <= 1);
        let mut prev_end = doc.body_start;
        if let Some(first) = roots.next() {
            prop_assert!(src[doc.body_start..first.range.start].trim().is_empty());
            prev_end = first.range.end;
        }
        for s in roots {
            assert_range(&src, &s.range)?;
            prop_assert_eq!(s.range.start, prev_end);
            prev_end = s.range.end;
        }
        if !doc.sections.is_empty() {
            prop_assert_eq!(prev_end, src.len());
        }
    }
}
