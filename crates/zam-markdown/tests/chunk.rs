//! Чанкер §8.2, сегментация на предложения и уровни L1: golden-файлы,
//! случаи предложений, свойства и точечные проверки правил.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::md_doc;
use proptest::prelude::*;
use serde::Serialize;
use zam_markdown::{
    ApproxCounter, ChunkDraft, ChunkLevel, ChunkParams, L1_MAX_TOKENS, TokenCounter, chunk_note,
    note_l1, parse, section_l1, split_sentences,
};

fn chunks(src: &str) -> Vec<ChunkDraft> {
    chunk_note(src, &parse(src), &ChunkParams::default(), &ApproxCounter)
}

fn leaves(drafts: &[ChunkDraft]) -> Vec<&ChunkDraft> {
    drafts
        .iter()
        .filter(|d| d.level == ChunkLevel::Chunk)
        .collect()
}

fn sentences(text: &str) -> Vec<&str> {
    split_sentences(text)
        .into_iter()
        .map(|r| &text[r])
        .collect()
}

/// Префикс `text` перед `src[range]` — пусто или повторённый заголовок блока.
fn header_of<'a>(src: &str, d: &'a ChunkDraft) -> &'a str {
    let r = d.range.clone().expect("у листового чанка есть диапазон");
    let body = &src[r];
    assert!(
        d.text.ends_with(body),
        "text не кончается на src[range]: {:?}",
        d.text
    );
    &d.text[..d.text.len() - body.len()]
}

// ---------- Счётчик и параметры ----------

#[test]
fn approx_counter_is_ceil_chars_div_3_5() {
    let tc = ApproxCounter;
    assert_eq!(tc.count(""), 0);
    assert_eq!(tc.count("a"), 1);
    assert_eq!(tc.count("abc"), 1);
    assert_eq!(tc.count("abcd"), 2);
    assert_eq!(tc.count("abcdefg"), 2);
    // символы, а не байты
    assert_eq!(tc.count("привет!"), 2);
    assert!(!tc.is_exact());
}

#[test]
fn params_default_match_spec() {
    let p = ChunkParams::default();
    assert_eq!(
        (
            p.target_min,
            p.target_max,
            p.hard_max,
            p.merge_below,
            p.overlap_sentences
        ),
        (300, 500, 800, 100, 1)
    );
    assert_eq!(ChunkLevel::Chunk.as_str(), "chunk");
    assert_eq!(ChunkLevel::Section.as_str(), "section");
    assert_eq!(ChunkLevel::Note.as_str(), "note");
}

// ---------- Предложения ----------

#[test]
fn sentence_cases() {
    let cases: &[(&str, &[&str])] = &[
        // 1–11: русский, сокращения и инициалы
        ("Привет. Как дела?", &["Привет.", "Как дела?"]),
        (
            "Это, т. е. пример, работает. Второе.",
            &["Это, т. е. пример, работает.", "Второе."],
        ),
        (
            "Яблоки, груши и т. д. Потом мы ушли.",
            &["Яблоки, груши и т. д.", "Потом мы ушли."],
        ),
        (
            "Книги, журналы и т. п. и многое другое.",
            &["Книги, журналы и т. п. и многое другое."],
        ),
        (
            "Он живёт в г. Москва. Это столица.",
            &["Он живёт в г. Москва.", "Это столица."],
        ),
        (
            "Это было в 2020 г. Потом всё изменилось.",
            &["Это было в 2020 г.", "Потом всё изменилось."],
        ),
        (
            "Адрес: ул. Ленина, дом пять. Вход со двора.",
            &["Адрес: ул. Ленина, дом пять.", "Вход со двора."],
        ),
        (
            "Подробнее см. Главу 2. Там всё.",
            &["Подробнее см. Главу 2.", "Там всё."],
        ),
        (
            "На рис. 5 показана схема. Далее текст.",
            &["На рис. 5 показана схема.", "Далее текст."],
        ),
        (
            "Поэт А. С. Пушкин родился в Москве. Это известно.",
            &["Поэт А. С. Пушкин родился в Москве.", "Это известно."],
        ),
        (
            "Итак, т.к. идёт дождь, сидим дома. Ок.",
            &["Итак, т.к. идёт дождь, сидим дома.", "Ок."],
        ),
        // 12–18: числа, версии, пути, URL, код
        (
            "Число 1.5 больше единицы. Да.",
            &["Число 1.5 больше единицы.", "Да."],
        ),
        (
            "Версия v1.2.3 вышла. Обновитесь.",
            &["Версия v1.2.3 вышла.", "Обновитесь."],
        ),
        (
            "Файл лежит в ./src/main.rs. Откройте его.",
            &["Файл лежит в ./src/main.rs.", "Откройте его."],
        ),
        (
            "Откройте https://example.com/a?b=1&c=2. Затем войдите.",
            &["Откройте https://example.com/a?b=1&c=2.", "Затем войдите."],
        ),
        (
            "Сайт www.example.org. Ещё.",
            &["Сайт www.example.org.", "Ещё."],
        ),
        (
            "Код `let x = 1. Y` внутри. Конец.",
            &["Код `let x = 1. Y` внутри.", "Конец."],
        ),
        (
            "Пример:\n```\nfn main() { a. B }\nДругое. Предложение.\n```\nКонец.",
            &[
                "Пример:",
                "```\nfn main() { a. B }\nДругое. Предложение.\n```",
                "Конец.",
            ],
        ),
        // 19–29: английский
        (
            "Hello world. This is a test.",
            &["Hello world.", "This is a test."],
        ),
        (
            "Use tools, e.g. Python or Rust. Then stop.",
            &["Use tools, e.g. Python or Rust.", "Then stop."],
        ),
        (
            "One name, i.e. The Thing, wins. Next.",
            &["One name, i.e. The Thing, wins.", "Next."],
        ),
        (
            "We bought apples, pears, etc. and went home.",
            &["We bought apples, pears, etc. and went home."],
        ),
        (
            "We bought apples, pears, etc. Then we left.",
            &["We bought apples, pears, etc.", "Then we left."],
        ),
        (
            "Dr. Smith arrived. He was late.",
            &["Dr. Smith arrived.", "He was late."],
        ),
        (
            "Apples vs. Oranges is a debate. Yes.",
            &["Apples vs. Oranges is a debate.", "Yes."],
        ),
        (
            "Mr. J. R. R. Tolkien wrote books. Indeed.",
            &["Mr. J. R. R. Tolkien wrote books.", "Indeed."],
        ),
        (
            "We need plan B. Then we go.",
            &["We need plan B.", "Then we go."],
        ),
        (
            "Pi is about 3.14. Right? Yes!",
            &["Pi is about 3.14.", "Right?", "Yes!"],
        ),
        (
            "Visit https://docs.rs/crate/1.0.0/. It helps.",
            &["Visit https://docs.rs/crate/1.0.0/.", "It helps."],
        ),
        (
            "Path C:\\Users\\Me\\file.TXT is there. OK.",
            &["Path C:\\Users\\Me\\file.TXT is there.", "OK."],
        ),
        // 31–38: строки, абзацы, списки, кавычки, крайние случаи
        (
            "Line one without dot\nstill same sentence. Next one.",
            &["Line one without dot\nstill same sentence.", "Next one."],
        ),
        (
            "Абзац один.\n\nАбзац два без точки",
            &["Абзац один.", "Абзац два без точки"],
        ),
        (
            "- пункт один\n- пункт два",
            &["- пункт один", "- пункт два"],
        ),
        ("Что? Да! Хорошо.", &["Что?", "Да!", "Хорошо."]),
        (
            "Он сказал: «Привет.» Потом ушёл.",
            &["Он сказал: «Привет.»", "Потом ушёл."],
        ),
        ("", &[]),
        ("   \n\n  ", &[]),
        ("Без точки вообще", &["Без точки вообще"]),
        (
            "Сравни Rust vs. Go. Итог.\r\nНовая строка.",
            &["Сравни Rust vs. Go.", "Итог.", "Новая строка."],
        ),
        (
            "Метод foo.Bar вызывается. Готово.",
            &["Метод foo.Bar вызывается.", "Готово."],
        ),
    ];
    assert!(cases.len() >= 30);
    let mut failed = Vec::new();
    for (text, want) in cases {
        let got = sentences(text);
        if got != *want {
            failed.push(format!("{text:?}\n  ждали {want:?}\n  вышло {got:?}"));
        }
    }
    assert!(failed.is_empty(), "расхождения:\n{}", failed.join("\n"));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// Диапазоны предложений упорядочены, не пересекаются, лежат на границах
    /// символов, без краевых пробелов и покрывают весь непробельный текст.
    #[test]
    fn sentences_are_ordered_trimmed_and_cover(src in md_doc()) {
        let rs = split_sentences(&src);
        let mut covered = vec![false; src.len()];
        let mut last = 0;
        for r in &rs {
            prop_assert!(r.start >= last && r.start < r.end, "{rs:?}");
            prop_assert!(src.is_char_boundary(r.start) && src.is_char_boundary(r.end));
            let s = &src[r.clone()];
            prop_assert_eq!(s.trim(), s);
            covered[r.clone()].iter_mut().for_each(|c| *c = true);
            last = r.end;
        }
        for (i, ch) in src.char_indices() {
            prop_assert!(ch.is_whitespace() || covered[i], "не покрыт байт {i} в {src:?}");
        }
    }
}

// ---------- Чанкер: точечные правила ----------

#[test]
fn empty_body_yields_no_chunks() {
    for src in [
        "",
        "   \n\n",
        "---\ntags: [a]\n---\n",
        "# Только заголовок\n",
        "# A\n## B\n",
    ] {
        assert!(chunks(src).is_empty(), "{src:?}: {:?}", chunks(src));
    }
}

#[test]
fn heading_lines_and_frontmatter_are_not_in_leaves() {
    let src = "---\ntags: [секрет]\n---\n# Тема\n\nТекст темы.\n\n## Под\n\nТекст подсекции.\n";
    let all = chunks(src);
    let ls = leaves(&all);
    for d in &ls {
        assert!(!d.text.contains("tags:"), "{:?}", d.text);
        assert!(!d.text.contains("# Тема"), "{:?}", d.text);
    }
    let texts: Vec<_> = ls
        .iter()
        .map(|d| (d.heading_path.as_str(), d.text.as_str()))
        .collect();
    assert_eq!(
        texts,
        [("Тема", "Текст темы."), ("Тема > Под", "Текст подсекции.")]
    );
}

#[test]
fn subsection_text_is_not_duplicated_in_parent() {
    let mut src = String::from("# Родитель\n\nСвой текст родителя.\n\n");
    for i in 0..30 {
        src.push_str(&format!(
            "## Ребёнок {i}\n\nТекст ребёнка {i} достаточно длинный, чтобы не склеиваться с соседом по правилу коротких секций. {}\n\n",
            "Ещё немного слов для объёма текста. ".repeat(6)
        ));
    }
    let all = chunks(&src);
    for i in 0..30 {
        let needle = format!("Текст ребёнка {i} ");
        let n = leaves(&all)
            .iter()
            .filter(|d| d.text.contains(&needle))
            .count();
        assert_eq!(n, 1, "{needle}");
    }
}

#[test]
fn leaves_respect_budgets_and_overlap_one_sentence() {
    let para: String = (0..12)
        .map(|i| format!("Предложение номер {i} рассказывает о работе индексатора подробно. "))
        .collect();
    let src = format!("# Длинная\n\n{}\n", [para.as_str(); 6].join("\n\n"));
    let all = chunks(&src);
    let ls = leaves(&all);
    assert!(ls.len() >= 3, "{}", ls.len());
    for (i, d) in ls.iter().enumerate() {
        assert!(d.token_count <= 800, "{}", d.token_count);
        assert_eq!(d.token_count, ApproxCounter.count(&d.text));
        if i + 1 < ls.len() {
            assert!(d.token_count >= 300, "лист {i}: {}", d.token_count);
        }
        assert_eq!(header_of(&src, d), "");
    }
    // перекрытие: следующий лист начинается с последнего предложения предыдущего
    for w in ls.windows(2) {
        let prev = sentences(&w[0].text);
        let last = prev.last().expect("предложение");
        assert!(
            w[1].text.starts_with(last),
            "{last:?} / {:?}",
            &w[1].text[..80]
        );
        let a = w[0].range.clone().unwrap();
        let b = w[1].range.clone().unwrap();
        assert!(b.start < a.end && b.start > a.start);
    }
}

#[test]
fn no_overlap_across_sections() {
    let para = "Короткое предложение про секцию. ".repeat(40);
    let src = format!("## A\n\n{para}\n\n## B\n\n{para}\n");
    let all = chunks(&src);
    let ls = leaves(&all);
    let first_b = ls.iter().find(|d| d.heading_path == "B").expect("B");
    assert!(src[..first_b.range.clone().unwrap().start].ends_with("## B\n\n"));
}

#[test]
fn short_sections_merge_with_next_sibling() {
    let src = "# Корень\n\n## A\n\nКоротко A.\n\n## B\n\nКоротко B.\n\n## C\n\nКоротко C.\n";
    let all = chunks(src);
    let ls = leaves(&all);
    assert_eq!(ls.len(), 1, "{ls:?}");
    assert_eq!(ls[0].heading_path, "Корень > A");
    assert!(ls[0].text.starts_with("Коротко A."));
    assert!(ls[0].text.contains("## B") && ls[0].text.ends_with("Коротко C."));
    // ребёнок не склеивается с родителем, секции разных родителей — тоже
    let src = "# X\n\nКоротко X.\n\n## Y\n\nКоротко Y.\n\n# Z\n\nКоротко Z.\n";
    let paths: Vec<_> = leaves(&chunks(src))
        .iter()
        .map(|d| d.heading_path.clone())
        .collect();
    assert_eq!(paths, ["X", "X > Y", "Z"]);
}

#[test]
fn big_code_block_repeats_fence_header() {
    let mut src = String::from("# Код\n\n```python title=\"x.py\"\n");
    for i in 0..3000 {
        src.push_str(&format!("print({i})  # строка\n"));
    }
    src.push_str("```\n");
    let all = chunks(&src);
    let ls = leaves(&all);
    assert!(ls.len() > 3);
    assert_eq!(header_of(&src, ls[0]), "");
    assert!(ls[0].text.starts_with("```python title=\"x.py\"\n"));
    for d in &ls[1..] {
        assert_eq!(header_of(&src, d), "```python title=\"x.py\"\n");
        assert!(d.token_count <= 800);
        // режется по строкам
        let r = d.range.clone().unwrap();
        assert!(src[..r.start].ends_with('\n'));
        assert!(d.text.ends_with(')') || d.text.ends_with("строка") || d.text.ends_with("```"));
    }
    assert!(ls.last().unwrap().text.ends_with("```"));
}

#[test]
fn big_table_repeats_header_and_delimiter() {
    let mut src = String::from("| a | b |\r\n|---|---|\r\n");
    for i in 0..1500 {
        src.push_str(&format!("| {i} | значение {i} |\r\n"));
    }
    let all = chunks(&src);
    let ls = leaves(&all);
    assert!(ls.len() > 3);
    for d in &ls[1..] {
        assert_eq!(header_of(&src, d), "| a | b |\r\n|---|---|\r\n");
        assert!(d.text.lines().skip(2).all(|l| l.starts_with("| ")));
    }
}

#[test]
fn big_list_splits_by_items_and_callout_repeats_marker() {
    let mut src = String::from("Список:\n\n");
    for i in 0..500 {
        src.push_str(&format!("- пункт {i}\n  продолжение пункта {i}\n"));
    }
    let all = chunks(&src);
    for d in leaves(&all).iter().skip(1) {
        assert!(d.text.starts_with("- пункт "), "{:?}", &d.text[..30]);
        assert!(d.text.ends_with(|c: char| c.is_ascii_digit()));
    }

    let mut src = String::from("> [!NOTE]\n");
    for i in 0..400 {
        src.push_str(&format!(
            "> строка {i} длинного callout-блока с пояснением\n"
        ));
    }
    let all = chunks(&src);
    let ls = leaves(&all);
    assert!(ls.len() > 2);
    for d in &ls[1..] {
        assert_eq!(header_of(&src, d), "> [!NOTE]\n");
    }
}

#[test]
fn text_without_dots_still_fits_hard_max() {
    let src = format!("Текст{}", " слово".repeat(3000));
    let all = chunks(&src);
    let ls = leaves(&all);
    assert!(ls.len() >= 2);
    assert!(ls.iter().all(|d| d.token_count <= 800));
    // одно огромное «слово» тоже режется
    let src = "x".repeat(10_000);
    let ls_total: usize = leaves(&chunks(&src))
        .iter()
        .map(|d| d.range.clone().unwrap().len())
        .sum();
    assert!(ls_total >= 10_000);
}

#[test]
fn section_and_note_levels() {
    let para = "Предложение о разделе для проверки уровней. ".repeat(30);
    let src = format!(
        "# Большой\n\nВводная фраза большого раздела. Ещё.\n\n## Первый\n\n{para}\n\n## Второй\n\n{para}\n\n# Малый\n\nОдин абзац малого раздела.\n"
    );
    let all = chunks(&src);
    let sections: Vec<_> = all
        .iter()
        .filter(|d| d.level == ChunkLevel::Section)
        .collect();
    let paths: Vec<_> = sections.iter().map(|d| d.heading_path.as_str()).collect();
    // H1 «Большой» — не меньше двух листов вместе с подсекциями; «Малый» — один лист
    assert!(paths.contains(&"Большой"), "{paths:?}");
    assert!(!paths.contains(&"Малый"), "{paths:?}");
    for d in &sections {
        assert!(d.range.is_none());
        assert!(d.token_count <= 300);
    }
    let big = sections
        .iter()
        .find(|d| d.heading_path == "Большой")
        .unwrap();
    assert!(
        big.text
            .starts_with("Большой — Вводная фраза большого раздела.")
    );
    assert!(big.text.contains("\n  Первый — Предложение о разделе"));
    let doc = parse(&src);
    let s = &doc.sections[0];
    assert_eq!(big.text, section_l1(&src, s, &ApproxCounter, 300));

    let notes: Vec<_> = all.iter().filter(|d| d.level == ChunkLevel::Note).collect();
    assert_eq!(notes.len(), 1);
    let n = notes[0];
    assert!(n.range.is_none() && n.heading_path.is_empty() && n.token_count <= 300);
    assert!(n.text.contains("Малый — Один абзац малого раздела."));
    assert_eq!(n.text, note_l1(&src, &doc, 1200));
}

#[test]
fn note_l1_respects_max_chars_and_section_l1_max_tokens() {
    let mut src = String::new();
    for i in 0..200 {
        src.push_str(&format!(
            "## Раздел {i}\n\nПервое предложение раздела {i}. Второе.\n\n"
        ));
    }
    let doc = parse(&src);
    let l1 = note_l1(&src, &doc, 1200);
    assert!(l1.chars().count() <= 1200);
    assert!(l1.starts_with("Раздел 0 — Первое предложение раздела 0."));
    assert!(!l1.contains("Второе"));
    assert_eq!(note_l1(&src, &doc, 0), "");
    let s = section_l1(&src, &doc.sections[3], &ApproxCounter, 300);
    assert_eq!(s, "Раздел 3 — Первое предложение раздела 3.");
    // одна строка длиннее бюджета обрезается с многоточием
    let long = format!("# T\n\n{}\n", "длинно ".repeat(500));
    let d = parse(&long);
    let s = section_l1(&long, &d.sections[0], &ApproxCounter, 20);
    assert!(ApproxCounter.count(&s) <= 20 && s.ends_with('…'), "{s:?}");
}

// ---------- Golden ----------

#[derive(Serialize)]
struct GoldenChunk<'a> {
    level: &'static str,
    heading_path: &'a str,
    token_count: usize,
    range: Option<[usize; 2]>,
    head: String,
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden_chunks")
}

#[test]
fn golden_chunks() {
    let bless = std::env::var("ZAM_BLESS").is_ok_and(|v| v == "1");
    let mut sources: Vec<PathBuf> = fs::read_dir(golden_dir())
        .expect("каталог golden_chunks")
        .map(|e| e.expect("запись каталога").path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    sources.sort();
    assert!(sources.len() >= 10, "нужно не меньше 10 фикстур");
    let mut failed = Vec::new();
    for path in &sources {
        let src = fs::read_to_string(path).expect("чтение фикстуры");
        let all = chunks(&src);
        let golden: Vec<_> = all
            .iter()
            .map(|d| GoldenChunk {
                level: d.level.as_str(),
                heading_path: &d.heading_path,
                token_count: d.token_count,
                range: d.range.clone().map(|r| [r.start, r.end]),
                head: d.text.chars().take(40).collect(),
            })
            .collect();
        let actual = serde_json::to_string_pretty(&golden).expect("json") + "\n";
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

// ---------- Свойства ----------

/// Проверки инвариантов §8.2 для одного документа.
fn check_invariants(src: &str, p: &ChunkParams) -> Result<(), TestCaseError> {
    let doc = parse(src);
    let all = chunk_note(src, &doc, p, &ApproxCounter);
    let mut covered = vec![false; src.len()];
    for d in &all {
        prop_assert_eq!(d.token_count, ApproxCounter.count(&d.text));
        if d.level != ChunkLevel::Chunk {
            prop_assert!(d.range.is_none() && d.token_count <= L1_MAX_TOKENS);
            continue;
        }
        prop_assert!(
            d.token_count <= p.hard_max.max(1),
            "{} > hard_max: {:?}",
            d.token_count,
            d.text
        );
        prop_assert!(!d.text.trim().is_empty());
        let r = d.range.clone().expect("диапазон листа");
        prop_assert!(r.start >= doc.body_start && r.end <= src.len());
        let body = &src[r.clone()];
        if d.text != body {
            // иначе — повторённый заголовок блока: целые строки исходника перед телом
            prop_assert!(d.text.ends_with(body), "{:?} / {:?}", d.text, body);
            let head = &d.text[..d.text.len() - body.len()];
            prop_assert!(
                head.ends_with('\n') && src.contains(head),
                "заголовок {head:?}"
            );
        }
        covered[r].iter_mut().for_each(|c| *c = true);
    }
    // всё непробельное тело вне строк заголовков H1–H3 покрыто листами
    let mut excluded = vec![false; src.len()];
    for h in doc.headings.iter().filter(|h| h.level <= 3) {
        excluded[h.range.clone()].iter_mut().for_each(|c| *c = true);
    }
    for (i, ch) in src.char_indices().filter(|&(i, _)| i >= doc.body_start) {
        if ch.is_whitespace() || ch == '\u{FEFF}' || excluded[i] {
            continue;
        }
        prop_assert!(covered[i], "байт {i} ({ch:?}) не покрыт: {src:?}");
    }
    Ok(())
}

/// Документ из `md_doc` с длинными кусками: повтор строк даёт блоки больше бюджета.
fn long_doc() -> impl Strategy<Value = String> {
    (md_doc(), 1usize..40).prop_map(|(d, k)| {
        let mut s = String::new();
        for _ in 0..k {
            s.push_str(&d);
            s.push('\n');
        }
        s
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn chunk_invariants_default(src in md_doc()) {
        check_invariants(&src, &ChunkParams::default())?;
    }

    /// Малые бюджеты: в каждом случае работают склейка, резка блоков и перекрытие.
    #[test]
    fn chunk_invariants_small_budgets(src in long_doc(), hard in 4usize..60) {
        let p = ChunkParams {
            target_min: hard / 3,
            target_max: hard * 2 / 3,
            hard_max: hard,
            merge_below: hard / 6,
            overlap_sentences: 1,
        };
        check_invariants(&src, &p)?;
    }
}
