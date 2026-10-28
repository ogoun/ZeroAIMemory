//! PATCH секций байтовым splice: операции, переводы строк, ошибки и инвариант
//! «байты вне изменённого диапазона совпадают с исходными».

mod common;

use std::ops::Range;

use common::md_line;
use proptest::prelude::*;
use zam_markdown::{
    Applied, PatchError, PatchOp, apply_patch, parse, section_by_path, section_bytes,
};

/// Документ с BOM, frontmatter, H1/H2/H3 и CRLF; последняя строка без перевода.
const SRC: &str = "\u{FEFF}---\r\ntags: [x]\r\n---\r\n# A\r\nintro\r\n## B\r\nb\r\n### C\r\nc\r\n## D\r\nd\r\n# E\r\ne";

fn replace(path: &str, content: &str) -> PatchOp {
    PatchOp::ReplaceSection {
        section: path.into(),
        content: content.into(),
    }
}

fn append_to(path: &str, content: &str) -> PatchOp {
    PatchOp::AppendToSection {
        section: path.into(),
        content: content.into(),
    }
}

fn after_heading(path: &str, content: &str) -> PatchOp {
    PatchOp::InsertAfterHeading {
        section: path.into(),
        content: content.into(),
    }
}

fn apply(src: &str, op: &PatchOp) -> Applied {
    apply_patch(src, op).expect("патч применяется")
}

// ---------- Шаг 1: операции ----------

#[test]
fn fixture_has_frontmatter_after_bom() {
    let doc = parse(SRC);
    assert!(doc.frontmatter.is_some());
    assert!(SRC[doc.body_start..].starts_with("# A\r\n"));
}

#[test]
fn replace_section_keeps_heading_and_converts_newlines() {
    let a = apply(SRC, &replace("A > B", "new\nlines"));
    assert_eq!(
        a.text,
        "\u{FEFF}---\r\ntags: [x]\r\n---\r\n# A\r\nintro\r\n## B\r\nnew\r\nlines\r\n## D\r\nd\r\n# E\r\ne"
    );
    assert_eq!(&a.text[a.changed.clone()], "new\r\nlines\r\n");
    assert_eq!(a.touched_section.as_deref(), Some("A > B"));
}

#[test]
fn append_to_section_goes_after_subsections() {
    let a = apply(SRC, &append_to("A > B", "tail"));
    assert_eq!(
        a.text,
        "\u{FEFF}---\r\ntags: [x]\r\n---\r\n# A\r\nintro\r\n## B\r\nb\r\n### C\r\nc\r\n\r\ntail\r\n## D\r\nd\r\n# E\r\ne"
    );
    // пустая строка: иначе «tail» стал бы ленивым продолжением абзаца «c»
    assert_eq!(&a.text[a.changed.clone()], "\r\ntail\r\n");
}

#[test]
fn insert_after_heading_lines() {
    let a = apply(SRC, &after_heading("A > B", "first\r\n"));
    assert_eq!(
        a.text,
        "\u{FEFF}---\r\ntags: [x]\r\n---\r\n# A\r\nintro\r\n## B\r\nfirst\r\n\r\nb\r\n### C\r\nc\r\n## D\r\nd\r\n# E\r\ne"
    );
    let a = apply(SRC, &after_heading("A > B > C", "x"));
    assert_eq!(
        a.text,
        "\u{FEFF}---\r\ntags: [x]\r\n---\r\n# A\r\nintro\r\n## B\r\nb\r\n### C\r\nx\r\n\r\nc\r\n## D\r\nd\r\n# E\r\ne"
    );
    assert_eq!(a.touched_section.as_deref(), Some("A > B > C"));
}

#[test]
fn append_adds_missing_newline_before() {
    let a = apply(
        SRC,
        &PatchOp::Append {
            content: "end".into(),
        },
    );
    assert_eq!(
        a.text,
        "\u{FEFF}---\r\ntags: [x]\r\n---\r\n# A\r\nintro\r\n## B\r\nb\r\n### C\r\nc\r\n## D\r\nd\r\n# E\r\ne\r\n\r\nend\r\n"
    );
    assert_eq!(&a.text[a.changed.clone()], "\r\n\r\nend\r\n");
    assert_eq!(a.touched_section, None);
}

#[test]
fn prepend_goes_after_frontmatter() {
    let a = apply(
        SRC,
        &PatchOp::Prepend {
            content: "pre".into(),
        },
    );
    assert_eq!(
        a.text,
        "\u{FEFF}---\r\ntags: [x]\r\n---\r\npre\r\n# A\r\nintro\r\n## B\r\nb\r\n### C\r\nc\r\n## D\r\nd\r\n# E\r\ne"
    );
    assert_eq!(a.touched_section, None);
    // без frontmatter — сразу после BOM, без лишнего перевода строки
    let a = apply(
        "\u{FEFF}# A\n",
        &PatchOp::Prepend {
            content: "pre".into(),
        },
    );
    assert_eq!(a.text, "\u{FEFF}pre\n# A\n");
    let a = apply(
        "",
        &PatchOp::Prepend {
            content: "x".into(),
        },
    );
    assert_eq!(a.text, "x\n");
}

#[test]
fn lf_document_normalizes_crlf_and_lone_cr() {
    let src = "# A\ntext\n## B\nb\n";
    let a = apply(src, &replace("A", "x\r\ny\rz"));
    // тело H1 включает подсекции — заменяется целиком
    assert_eq!(a.text, "# A\nx\ny\nz\n");
    assert_eq!(a.changed, 4..10);
}

#[test]
fn heading_at_eof_without_newline() {
    let a = apply("# A", &after_heading("A", "x"));
    assert_eq!(a.text, "# A\nx\n");
    assert_eq!(a.changed, 3..6);
    let a = apply("Сетекст\n===", &replace("Сетекст", "тело"));
    assert_eq!(a.text, "Сетекст\n===\nтело\n");
}

#[test]
fn indented_next_heading_line_belongs_to_next_section() {
    let src = "# A
a
## B
b
   # C
c
";
    // отступ строки заголовка — часть следующей секции, а не предыдущей
    assert_eq!(
        section_bytes(src, "A > B"),
        Some(
            "## B
b
"
        )
    );
    assert_eq!(
        section_bytes(src, "C"),
        Some(
            "   # C
c
"
        )
    );
    let a = apply(src, &append_to("A > B", "t"));
    assert_eq!(
        a.text,
        "# A
a
## B
b

t
   # C
c
"
    );
    assert_eq!(
        &a.text[a.changed.clone()],
        "
t
"
    );
    // смена отступа C не меняет байты B (нет ложного конфликта)
    let moved = "# A
a
## B
b
 # C
c
";
    assert_eq!(section_bytes(moved, "A > B"), section_bytes(src, "A > B"));
    assert_eq!(
        section_by_path(&parse(&a.text), "C").map(|s| s.level),
        Some(1)
    );
}

#[test]
fn empty_content_inserts_nothing() {
    let a = apply(SRC, &replace("A > B", ""));
    assert_eq!(
        a.text,
        "\u{FEFF}---\r\ntags: [x]\r\n---\r\n# A\r\nintro\r\n## B\r\n## D\r\nd\r\n# E\r\ne"
    );
    assert!(a.changed.is_empty());
    let a = apply(
        SRC,
        &PatchOp::Append {
            content: String::new(),
        },
    );
    assert_eq!(a.text, SRC);
}

#[test]
fn path_spacing_is_lenient_touched_is_canonical() {
    let a = apply(SRC, &append_to(" A >B ", "t"));
    assert_eq!(a.touched_section.as_deref(), Some("A > B"));
}

#[test]
fn section_bytes_returns_whole_range() {
    assert_eq!(
        section_bytes(SRC, "A > B"),
        Some("## B\r\nb\r\n### C\r\nc\r\n")
    );
    assert_eq!(section_bytes(SRC, "E"), Some("# E\r\ne"));
    assert_eq!(section_bytes(SRC, "Нет"), None);
    // PATCH соседней секции не меняет байты этой
    let a = apply(SRC, &replace("A > D", "новое"));
    assert_eq!(section_bytes(&a.text, "A > B"), section_bytes(SRC, "A > B"));
    assert_ne!(section_bytes(&a.text, "A > D"), section_bytes(SRC, "A > D"));
}

#[test]
fn patch_op_json_shape() {
    let op: PatchOp = serde_json::from_str(
        r#"{"op":"replace_section","section":"Установка > Linux","content":"x"}"#,
    )
    .expect("json");
    assert!(
        matches!(op, PatchOp::ReplaceSection { ref section, .. } if section == "Установка > Linux")
    );
    for tag in ["append_to_section", "insert_after_heading"] {
        let json = format!(r#"{{"op":"{tag}","section":"A","content":"x"}}"#);
        serde_json::from_str::<PatchOp>(&json).expect("json");
    }
    let op: PatchOp = serde_json::from_str(r#"{"op":"prepend","content":"x"}"#).expect("json");
    assert!(matches!(op, PatchOp::Prepend { .. }));
    let back = serde_json::to_string(&PatchOp::Append {
        content: "y".into(),
    })
    .expect("json");
    assert_eq!(back, r#"{"op":"append","content":"y"}"#);
}

// ---------- Шаг 3: ошибки ----------

#[test]
fn missing_and_ambiguous_sections() {
    let src = "# H\n## A\nx\n## A\ny\n";
    assert!(matches!(
        apply_patch(src, &replace("H > Z", "t")),
        Err(PatchError::SectionNotFound(p)) if p == "H > Z"
    ));
    assert!(matches!(
        apply_patch(src, &append_to("H > A", "t")),
        Err(PatchError::Ambiguous(p)) if p == "H > A"
    ));
    assert!(matches!(
        apply_patch(src, &after_heading("", "t")),
        Err(PatchError::SectionNotFound(_))
    ));
    assert_eq!(section_bytes(src, "H > A"), None);
    // стенд Debian 13: хвост пути правка не принимает, но подсказывает полный путь
    let src = "# Сборка\nx\n## Тесты\ny\n";
    let e = apply_patch(src, &replace("Тесты", "t")).unwrap_err();
    assert_eq!(
        e,
        PatchError::SectionNotFoundTail {
            section: "Тесты".into(),
            full: "Сборка > Тесты".into()
        }
    );
    assert!(e.to_string().contains("«Сборка > Тесты»"), "{e}");
    // неоднозначный хвост — без подсказки
    let src = "# A\n## T\n# B\n## T\n";
    assert!(matches!(
        apply_patch(src, &replace("T", "t")),
        Err(PatchError::SectionNotFound(_))
    ));
    assert_eq!(
        PatchError::SectionNotFound("X".into()).to_string(),
        "секция не найдена: X"
    );
    assert_eq!(
        PatchError::Ambiguous("X".into()).to_string(),
        "путь секции неоднозначен: X"
    );
}

#[test]
fn section_lost_after_splice_is_broken() {
    // `---` в начале без закрытия: вставка `---` делает из начала frontmatter
    assert_eq!(
        apply_patch(
            "---
# d",
            &append_to("d", "---")
        ),
        Err(PatchError::Broken("d".into()))
    );
    // вставка дубля пути делает его неоднозначным
    let src = "# A
## B
x
";
    assert_eq!(
        apply_patch(
            src,
            &append_to(
                "A > B", "## B
"
            )
        ),
        Err(PatchError::Broken("A > B".into()))
    );
    assert_eq!(
        PatchError::Broken("X".into()).to_string(),
        "после правки секция не находится однозначно: X"
    );
}

// ---------- Шаг 2: property-инвариант ----------

/// Строки документа: общие из `md_line` и заголовки с короткими разными названиями,
/// чтобы пути чаще были уникальными.
fn patch_line() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => md_line(),
        1 => "(#|##|###) [a-e]{1,2}",
    ]
}

/// Документ: необязательные BOM и frontmatter, LF или CRLF, с переводом строки в конце или без.
fn patch_doc() -> impl Strategy<Value = String> {
    (
        prop::collection::vec(patch_line(), 0..24),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(lines, crlf, bom, fm, tail)| {
            let nl = if crlf { "\r\n" } else { "\n" };
            let mut s = String::new();
            if bom {
                s.push('\u{FEFF}');
            }
            if fm {
                s.push_str(&format!("---{nl}tags: [t]{nl}---{nl}"));
            }
            s.push_str(&lines.join(nl));
            if tail {
                s.push_str(nl);
            }
            s
        })
}

fn patch_content() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => (prop::collection::vec(patch_line(), 0..6), prop_oneof![Just("\n"), Just("\r\n"), Just("\r")], any::<bool>())
            .prop_map(|(lines, nl, tail)| {
                let mut s = lines.join(nl);
                if tail {
                    s.push_str(nl);
                }
                s
            }),
        1 => any::<String>(),
    ]
}

/// Заменённый диапазон в старом тексте по известному разбору.
fn old_range(src: &str, op: &PatchOp) -> Range<usize> {
    let doc = parse(src);
    let sec = |p: &str| section_by_path(&doc, p).expect("секция").clone();
    match op {
        PatchOp::ReplaceSection { section, .. } => sec(section).body,
        PatchOp::AppendToSection { section, .. } => {
            let e = sec(section).range.end;
            e..e
        }
        PatchOp::InsertAfterHeading { section, .. } => {
            let s = sec(section).body.start;
            s..s
        }
        PatchOp::Append { .. } => src.len()..src.len(),
        PatchOp::Prepend { .. } => doc.body_start..doc.body_start,
    }
}

fn doc_newline(src: &str) -> &'static str {
    match src.find('\n') {
        Some(i) if i > 0 && src.as_bytes()[i - 1] == b'\r' => "\r\n",
        _ => "\n",
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]
    #[test]
    fn splice_keeps_bytes_outside_changed_range(
        src in patch_doc(),
        content in patch_content(),
        pick in any::<prop::sample::Index>(),
        kind in 0u8..5,
    ) {
        let doc = parse(&src);
        let paths: Vec<String> = doc
            .sections
            .iter()
            .filter(|s| s.level > 0)
            .map(|s| s.path.join(" > "))
            .collect();
        let op = match (kind, paths.is_empty()) {
            (0..=2, true) | (3, _) => PatchOp::Append { content: content.clone() },
            (4.., _) => PatchOp::Prepend { content: content.clone() },
            (k, false) => {
                let path = pick.get(&paths).clone();
                let unique = paths.iter().filter(|p| **p == path).count() == 1;
                let op = match k {
                    0 => replace(&path, &content),
                    1 => append_to(&path, &content),
                    _ => after_heading(&path, &content),
                };
                if !unique {
                    prop_assert!(
                        matches!(apply_patch(&src, &op), Err(PatchError::Ambiguous(_))),
                        "{:?}",
                        src
                    );
                    return Ok(());
                }
                op
            }
        };

        let a = match apply_patch(&src, &op) {
            Ok(a) => a,
            Err(PatchError::Broken(_)) => return Ok(()),
            Err(e) => return Err(TestCaseError::fail(format!("{e}: {src:?}"))),
        };
        let old = old_range(&src, &op);
        prop_assert_eq!(a.changed.start, old.start);
        prop_assert!(a.changed.end <= a.text.len());
        prop_assert_eq!(&a.text[..a.changed.start], &src[..old.start]);
        prop_assert_eq!(&a.text[a.changed.end..], &src[old.end..]);

        // вставка — в переводах строк документа и заканчивается переводом строки
        let ins = &a.text[a.changed.clone()];
        let nl = doc_newline(&src);
        if content.is_empty() {
            prop_assert!(ins.is_empty());
        } else {
            prop_assert!(ins.ends_with(nl), "{:?}", ins);
            let unix = ins.replace("\r\n", "\n");
            prop_assert!(!unix.contains('\r'), "{:?}", ins);
            if nl == "\r\n" {
                prop_assert_eq!(ins.matches("\r\n").count(), ins.matches('\n').count());
            } else {
                prop_assert!(!ins.contains('\r'), "{:?}", ins);
            }
        }

        // Ok — значит, секция по-прежнему находится ровно одна
        if let Some(path) = &a.touched_section {
            let new_doc = parse(&a.text);
            let found = new_doc
                .sections
                .iter()
                .filter(|s| s.level > 0 && s.path.join(" > ") == *path)
                .count();
            prop_assert_eq!(found, 1, "{:?} -> {:?}", src, a.text);
        }
    }
}

// ---------- Ленивое продолжение абзаца ----------

/// HTML рендера — для проверки, что вставка стала отдельным блоком.
fn rendered(src: &str) -> String {
    zam_markdown::render_html(src, &zam_markdown::NoResolve).html
}

#[test]
fn inserted_paragraph_is_not_lazy_continuation() {
    // абзац, пункт списка, цитата, строка таблицы перед вставкой
    for (src, expect_text) in [
        ("# A\nабзац\n", "# A\nабзац\n\nновый\n"),
        ("# A\n- пункт\n", "# A\n- пункт\n\nновый\n"),
        ("# A\n> цитата\n", "# A\n> цитата\n\nновый\n"),
        (
            "# A\n| a | b |\n|---|---|\n| 1 | 2 |\n",
            "# A\n| a | b |\n|---|---|\n| 1 | 2 |\n\nновый\n",
        ),
    ] {
        let a = apply(src, &append_to("A", "новый"));
        assert_eq!(a.text, expect_text, "{src:?}");
        assert!(
            rendered(&a.text).contains(">новый</p>"),
            "{}",
            rendered(&a.text)
        );
        let a = apply(
            src,
            &PatchOp::Append {
                content: "новый".into(),
            },
        );
        assert_eq!(a.text, expect_text, "{src:?}");
    }
}

#[test]
fn no_blank_line_when_not_needed() {
    // после заголовка, перед заголовком, после пустой строки
    let a = apply("# A\n", &append_to("A", "текст"));
    assert_eq!(a.text, "# A\nтекст\n");
    let a = apply("# A\nабзац\n\n", &append_to("A", "текст"));
    assert_eq!(a.text, "# A\nабзац\n\nтекст\n");
    let a = apply("# A\n## B\n", &after_heading("A", "текст"));
    assert_eq!(a.text, "# A\nтекст\n## B\n");
    // вставка пункта списка в список продолжает список
    let a = apply("# A\n- один\n", &append_to("A", "- два"));
    assert_eq!(a.text, "# A\n- один\n- два\n");
    // вставка заголовка, цитаты, ограждения — без пустой строки
    let a = apply("# A\nабзац\n", &append_to("A", "> цитата"));
    assert_eq!(a.text, "# A\nабзац\n> цитата\n");
    // ReplaceSection между заголовками пустых строк не добавляет
    let a = apply("# A\nстарое\n# B\n", &replace("A", "новое"));
    assert_eq!(a.text, "# A\nновое\n# B\n");
}

#[test]
fn blank_line_after_insert_before_plain_text() {
    // InsertAfterHeading перед абзацем: абзацы не сливаются
    let a = apply("# A\nстарый\n", &after_heading("A", "новый"));
    assert_eq!(a.text, "# A\nновый\n\nстарый\n");
    assert_eq!(&a.text[a.changed.clone()], "новый\n\n");
    let h = rendered(&a.text);
    assert!(h.contains(">новый</p>") && h.contains(">старый</p>"), "{h}");
    // Prepend перед абзацем, CRLF
    let a = apply(
        "---\r\nt: 1\r\n---\r\nтекст\r\n",
        &PatchOp::Prepend {
            content: "пре".into(),
        },
    );
    assert_eq!(a.text, "---\r\nt: 1\r\n---\r\nпре\r\n\r\nтекст\r\n");
    // перед заголовком, пунктом списка или пустой строкой — без добавки
    let a = apply("# A\n- пункт\n", &after_heading("A", "новый"));
    assert_eq!(a.text, "# A\nновый\n- пункт\n");
    // вставка, сама кончающаяся пустой строкой, не удваивает её
    let a = apply("# A\nстарый\n", &after_heading("A", "новый\n\n"));
    assert_eq!(a.text, "# A\nновый\n\nстарый\n");
}

#[test]
fn setext_underline_content_after_paragraph_stays_break() {
    // `---` сразу после абзаца сделал бы из него заголовок setext
    let a = apply("# A\nабзац\n", &append_to("A", "---"));
    assert_eq!(a.text, "# A\nабзац\n\n---\n");
    assert!(rendered(&a.text).contains("<hr"), "{}", rendered(&a.text));
}
