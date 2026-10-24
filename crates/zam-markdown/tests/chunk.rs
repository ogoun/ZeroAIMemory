//! Чанкер §8.2, сегментация на предложения и уровни L1: golden-файлы,
//! случаи предложений, свойства и точечные проверки правил.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{md_doc, md_line};
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

fn leaf_paths(src: &str) -> Vec<String> {
    leaves(&chunks(src))
        .iter()
        .map(|d| d.heading_path.clone())
        .collect()
}

#[test]
fn short_section_not_merged_beyond_target_max() {
    // короткая A и длинный сосед B: склейка длиннее target_max — A остаётся отдельной
    let long = "Длинный текст соседней секции для проверки. ".repeat(60);
    let src = format!("## A\n\nКоротко A.\n\n## B\n\n{long}\n");
    let all = chunks(&src);
    let ls = leaves(&all);
    assert_eq!(
        (ls[0].heading_path.as_str(), ls[0].text.as_str()),
        ("A", "Коротко A.")
    );
    assert!(ls.len() > 1 && ls[1..].iter().all(|d| d.heading_path == "B"));
    // склейка в пределах target_max — под путём первой секции
    let mid = "Средний текст соседа. ".repeat(10);
    let src = format!("## A\n\nКоротко A.\n\n## B\n\n{mid}\n");
    assert_eq!(leaf_paths(&src), ["A"]);
}

#[test]
fn short_section_merges_through_empty_sibling_and_backwards() {
    // пустой сосед (только заголовок) цепочку не рвёт
    let src = "## A\n\nКоротко A.\n\n## Пусто\n\n## C\n\nКоротко C.\n";
    let all = chunks(src);
    let ls = leaves(&all);
    assert_eq!(ls.len(), 1);
    assert!(ls[0].text.contains("## Пусто") && ls[0].text.ends_with("Коротко C."));
    // последняя короткая секция без следующего соседа — к предыдущему листу того же родителя
    let body = "Обычный текст секции средней длины. ".repeat(20);
    let src = format!(
        "# P\n\n## A\n\n{body}\n\n## B\n\n{body}\n\n## Хвост\n\nКоротко.\n\n# Q\n\nТекст Q.\n"
    );
    let all = chunks(&src);
    let ls = leaves(&all);
    let paths: Vec<_> = ls.iter().map(|d| d.heading_path.as_str()).collect();
    assert_eq!(paths, ["P > A", "P > B", "Q"], "{ls:?}");
    assert!(ls[1].text.ends_with("## Хвост\n\nКоротко."));
    // без предыдущего соседа того же родителя хвост остаётся отдельным
    let src = "# P\n\nТекст P.\n\n## Один\n\nКоротко.\n";
    assert_eq!(leaf_paths(src), ["P", "P > Один"]);
}

#[test]
fn obsidian_callouts_repeat_marker_line() {
    for marker in ["> [!info]- Заголовок callout", "> [!заметка]", "> [!tip]+"] {
        for nl in ["\n", "\r\n"] {
            let mut src = format!("# Callout{nl}{nl}{marker}{nl}");
            for i in 0..400 {
                src.push_str(&format!("> строка {i} длинного callout с пояснением{nl}"));
                if i % 20 == 19 {
                    src.push_str(&format!(">{nl}"));
                }
            }
            let all = chunks(&src);
            let ls = leaves(&all);
            assert!(ls.len() > 2, "{marker}");
            assert!(ls[0].text.starts_with(marker));
            for d in &ls[1..] {
                assert_eq!(header_of(&src, d), format!("{marker}{nl}"), "{marker}");
                assert!(d.token_count <= 800);
            }
        }
    }
}

#[test]
fn level_l1_without_sentences_or_equal_to_note_is_skipped() {
    // таблица без абзацев: L1 из одних заголовков не выдаётся
    let mut src = String::from("# Таблица\n\n| a | b |\n|---|---|\n");
    for i in 0..400 {
        src.push_str(&format!("| {i} | значение {i} |\n"));
    }
    let all = chunks(&src);
    assert!(leaves(&all).len() > 1);
    assert!(
        all.iter().all(|d| d.level == ChunkLevel::Chunk),
        "{:?}",
        all.last()
    );
    // единственная H1 с несколькими листами: L1 секции совпал бы с L1 заметки
    let para = "Предложение о единственном разделе заметки. ".repeat(30);
    let src = format!("# Один\n\nВводная фраза.\n\n## Первый\n\n{para}\n\n## Второй\n\n{para}\n");
    let all = chunks(&src);
    let levels: Vec<_> = all
        .iter()
        .filter(|d| d.level != ChunkLevel::Chunk)
        .map(|d| (d.level, d.heading_path.as_str()))
        .collect();
    assert_eq!(levels, [(ChunkLevel::Note, "")]);
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

// ---------- Сила границ (задача 17 M3) ----------

/// Абзац прозы не короче `tokens` токенов оценки: предложения с меткой `tag`.
fn prose(tokens: usize, tag: &str) -> String {
    let mut s = String::new();
    let mut i = 0;
    while ApproxCounter.count(&s) < tokens {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(&format!("Фраза {tag} номер {i} про нарезку."));
        i += 1;
    }
    s
}

fn with(lambda: f64, cohesion: bool) -> ChunkParams {
    ChunkParams {
        boundary_lambda: lambda,
        cohesion,
        ..ChunkParams::default()
    }
}

fn chunks_with(src: &str, p: &ChunkParams) -> Vec<ChunkDraft> {
    chunk_note(src, &parse(src), p, &ApproxCounter)
}

/// Последняя непустая строка — заголовок ATX уровня из `levels`.
fn ends_with_heading(text: &str, levels: std::ops::RangeInclusive<usize>) -> bool {
    let last = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    let t = last.trim_start();
    let n = t.bytes().take_while(|&b| b == b'#').count();
    levels.contains(&n) && t[n..].starts_with(' ')
}

/// Сценарий исследования (§2): P1 около 310 токенов, H4–H6, P2 = 300. Лист не кончается
/// заголовком, заголовок уходит в лист со своим содержимым.
fn check_heading_never_ends_leaf(marks: &str) {
    let level = marks.len();
    for lambda in [0.0, 1.0, 2.0] {
        for p1 in (280..=330).step_by(3) {
            let src = format!(
                "# Заметка\n\n{}\n\n{marks} Подзаголовок\n\n{}\n",
                prose(p1, "первый"),
                prose(300, "второй")
            );
            let all = chunks_with(&src, &with(lambda, false));
            let ls = leaves(&all);
            for l in &ls {
                assert!(
                    !ends_with_heading(&l.text, level..=level),
                    "λ={lambda}, P1={p1}: лист кончается заголовком: …{:?}",
                    l.text.lines().rev().take(3).collect::<Vec<_>>()
                );
            }
            let head = format!("{marks} Подзаголовок");
            let l = ls
                .iter()
                .find(|l| l.text.contains(&head))
                .expect("лист с заголовком");
            assert!(
                l.text.contains("Фраза второй номер 0 "),
                "λ={lambda}, P1={p1}: заголовок без содержимого"
            );
        }
    }
}

#[test]
fn h4_heading_never_ends_leaf() {
    check_heading_never_ends_leaf("####");
}

#[test]
fn h5_h6_never_end_leaf() {
    check_heading_never_ends_leaf("#####");
    check_heading_never_ends_leaf("######");
}

#[test]
fn colon_intro_stays_with_block() {
    let code: String = (0..40)
        .map(|i| format!("let value_{i} = compute(input_{i}, options);\n"))
        .collect();
    let list: String = (0..40)
        .map(|i| format!("- пункт списка номер {i} с пояснением\n"))
        .collect();
    let table: String = std::iter::once("| ключ | значение |\n|---|---|\n".to_string())
        .chain((0..40).map(|i| format!("| k{i} | значение номер {i} |\n")))
        .collect();
    let blocks = [
        (format!("```rust\n{code}```\n"), "```rust"),
        (list, "- пункт списка номер 0 "),
        (table, "| ключ |"),
    ];
    for (block, start) in &blocks {
        for lambda in [0.0, 1.0] {
            for p1 in (280..=330).step_by(5) {
                let src = format!(
                    "# Заметка\n\n{} Пример настройки:\n\n{block}\nПосле блока.\n",
                    prose(p1, "ввод")
                );
                let all = chunks_with(&src, &with(lambda, false));
                let l = leaves(&all)
                    .into_iter()
                    .find(|l| l.text.contains("Пример настройки:"))
                    .expect("лист с вводной фразой");
                assert!(
                    l.text.contains(start),
                    "λ={lambda}, P1={p1}: вводная фраза оторвана от {start:?}"
                );
            }
        }
    }
}

#[test]
fn heading_path_includes_h4() {
    let src = format!(
        "# A\n\n## B\n\n{}\n\n#### Глубокий\n\n{}\n\n##### Ещё\n\n{}\n\n### C\n\n{}\n",
        prose(400, "один"),
        prose(400, "два"),
        prose(400, "три"),
        prose(400, "четыре")
    );
    let path_of = |all: &[ChunkDraft], needle: &str| -> String {
        leaves(all)
            .into_iter()
            .find(|l| l.text.contains(needle))
            .map(|l| l.heading_path.clone())
            .expect("лист")
    };
    let all = chunks(&src);
    assert_eq!(path_of(&all, "Фраза один номер 0 "), "A > B");
    assert_eq!(path_of(&all, "Фраза два номер 0 "), "A > B > Глубокий");
    assert_eq!(
        path_of(&all, "Фраза три номер 0 "),
        "A > B > Глубокий > Ещё"
    );
    // H3 сбрасывает H4–H6
    assert_eq!(path_of(&all, "Фраза четыре номер 0 "), "A > B > C");
    // L1 секций по-прежнему по H1–H2
    assert!(
        all.iter()
            .filter(|d| d.level == ChunkLevel::Section)
            .all(|d| !d.heading_path.contains("Глубокий"))
    );
    // выключатель: путь только по H1–H3, как в M2
    let p = ChunkParams {
        heading_path_h4: false,
        ..ChunkParams::default()
    };
    let all = chunks_with(&src, &p);
    assert_eq!(path_of(&all, "Фраза три номер 0 "), "A > B");
}

/// Фикстура golden без H4–H6 и без абзацев на «:» — исправления задачи 17 её не касаются.
fn untouched_by_fixes(src: &str) -> bool {
    !src.lines().any(|l| {
        let t = l.trim_start();
        let n = t.bytes().take_while(|&b| b == b'#').count();
        ((4..=6).contains(&n) && t[n..].starts_with(' ')) || l.trim_end().ends_with(':')
    })
}

#[test]
fn dp_matches_greedy_when_lambda_zero() {
    // λ = 0 — жадная упаковка M2: golden-фикстуры M2 без H4–H6 и «:» не меняются,
    // в том числе с включённой связностью (она влияет только на выбор разреза при λ > 0)
    let mut checked = 0;
    let mut sources: Vec<PathBuf> = fs::read_dir(golden_dir())
        .expect("каталог golden_chunks")
        .map(|e| e.expect("запись").path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    sources.sort();
    for path in &sources {
        let src = fs::read_to_string(path).expect("фикстура");
        if !untouched_by_fixes(&src) {
            continue;
        }
        let expected = fs::read_to_string(path.with_extension("json"))
            .expect("golden .json")
            .replace("\r\n", "\n");
        for cohesion in [false, true] {
            let actual = golden_json(&chunks_with(&src, &with(0.0, cohesion)));
            assert_eq!(actual, expected, "{} (cohesion={cohesion})", path.display());
        }
        checked += 1;
    }
    assert!(checked >= 10, "фикстур без H4–H6 и «:» {checked} < 10");
}

#[test]
fn dp_prefers_strong_boundary_when_lambda_positive() {
    let ends = |src: &str, p: &ChunkParams| -> Vec<usize> {
        leaves(&chunks_with(src, p))
            .iter()
            .map(|l| l.range.clone().expect("диапазон").end)
            .collect()
    };
    // Граница абзацев против границы предложений. Q и R — целые абзацы, P длиннее
    // target_max и режется по предложениям. Жадная упаковка дописывает к Q начало P до
    // цели (разрезы — между предложениями), DP с λ > 0 закрывает лист на конце Q (сила 2).
    let (q, p, r) = (prose(300, "кью"), prose(600, "пэ"), prose(300, "эр"));
    let src = format!(
        "# T

{q}

{p}

{r}
"
    );
    let q_end = src.find(&q).expect("Q") + q.len();
    assert!(
        !ends(&src, &with(0.0, false)).contains(&q_end),
        "жадная режет внутри P"
    );
    for lambda in [0.5, 1.0, 2.0] {
        assert!(
            ends(&src, &with(lambda, false)).contains(&q_end),
            "λ={lambda}: разрез не на границе абзацев"
        );
    }
    // Заголовок H4 против границы абзацев: абзацы 190, 150, 150, H4, 150, 150 (цель около 400).
    // Жадная закрывает первый лист после двух абзацев, DP — перед H4 (сила 4 против 2).
    let paras: Vec<String> = [("а", 190), ("б", 150), ("в", 150), ("г", 150), ("д", 150)]
        .iter()
        .map(|&(t, n)| prose(n, t))
        .collect();
    let src = format!(
        "# T

{}

{}

{}

#### Раздел

{}

{}
",
        paras[0], paras[1], paras[2], paras[3], paras[4]
    );
    let c_end = src.find(&paras[2]).expect("абзац в") + paras[2].len();
    assert!(
        !ends(&src, &with(0.0, false)).contains(&c_end),
        "жадная режет перед в"
    );
    for lambda in [0.5, 1.0, 2.0] {
        assert!(
            ends(&src, &with(lambda, false)).contains(&c_end),
            "λ={lambda}: разрез не перед H4"
        );
    }
}

#[test]
fn cohesion_moves_cut_to_topic_change() {
    // Один длинный абзац: две темы с разной лексикой. При λ > 0 и связности разрез
    // ставится на смене темы, а не на ближайшем к цели предложении.
    let topic_a: String = (0..6)
        .map(|i| {
            format!("Репликация базы данных отстаёт на реплике {i}, журнал репликации растёт. ")
        })
        .collect();
    let topic_b: String = (0..9)
        .map(|i| {
            format!("Сертификаты шлюза истекают через {i} дней, обновление сертификатов падает. ")
        })
        .collect();
    let src = format!("# T\n\n{}{}\n", topic_a, topic_b.trim_end());
    let shift = src
        .find("Сертификаты шлюза истекают через 0")
        .expect("смена темы");
    let mut p = ChunkParams {
        target_min: 100,
        target_max: 200,
        hard_max: 400,
        merge_below: 20,
        overlap_sentences: 0,
        boundary_lambda: 1.0,
        cohesion: true,
        heading_path_h4: true,
    };
    let cuts = |p: &ChunkParams| -> Vec<usize> {
        leaves(&chunks_with(&src, p))
            .iter()
            .skip(1)
            .map(|l| l.range.clone().expect("диапазон").start)
            .collect()
    };
    let with_cohesion = cuts(&p);
    assert!(
        with_cohesion.contains(&shift),
        "разрезы {with_cohesion:?}, смена темы {shift}"
    );
    p.cohesion = false;
    let without = cuts(&p);
    assert!(
        !without.contains(&shift),
        "без связности разрез у цели: {without:?}"
    );
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

/// JSON golden-файла по чанкам.
fn golden_json(all: &[ChunkDraft]) -> String {
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
    serde_json::to_string_pretty(&golden).expect("json") + "\n"
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
        let actual = golden_json(&chunks(&src));
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
                header_opens_block(src, head, r.start),
                "заголовок {head:?} не открывает блок части с {}: {src:?}",
                r.start
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

/// Повторённый заголовок `head` — строки исходника, открывающие блок, которому
/// принадлежит часть с началом `start`: `head` стоит в исходнике с начала строки
/// до `start`, это заголовок блока (ограждение кода, шапка таблицы с разделителем,
/// маркер callout), и строки между ним и `start` блок не закрывают.
fn header_opens_block(src: &str, head: &str, start: usize) -> bool {
    let lines: Vec<&str> = head.lines().collect();
    let first = lines.first().copied().unwrap_or("");
    let t = first.trim_start_matches(' ');
    let fence = t
        .bytes()
        .next()
        .filter(|c| *c == b'`' || *c == b'~')
        .map(|c| (c, t.bytes().take_while(|&b| b == c).count()))
        .filter(|&(_, n)| n >= 3);
    let is_delim = |l: &str| {
        let l = l.trim();
        l.contains('-') && l.chars().all(|c| matches!(c, '|' | ':' | '-' | ' '))
    };
    let kind_ok = match (fence, lines.len()) {
        (Some(_), 1) => true,
        (None, 2) => first.contains('|') && is_delim(lines[1]),
        (None, 1) => t.starts_with('>') && t.contains("[!"),
        _ => false,
    };
    if !kind_ok || !head.ends_with(['\n', '\r']) {
        return false;
    }
    let at_line_start = |p: usize| p == 0 || src[..p].ends_with(['\n', '\r']);
    src[..start].match_indices(head).any(|(p, _)| {
        if !at_line_start(p) || p + head.len() > start {
            return false;
        }
        // только целые строки: часть может начинаться внутри строки (резка по словам)
        let between = &src[p + head.len()..start];
        let mut full = between
            .split_inclusive('\n')
            .filter(|l| l.ends_with('\n'))
            .map(str::trim_end);
        match fence {
            // между ограждением и частью нет закрывающего ограждения
            Some((c, n)) => !full.any(|l| {
                let l = l.trim_start_matches(' ');
                l.bytes().take_while(|&b| b == c).count() >= n && l.trim().bytes().all(|b| b == c)
            }),
            // таблица и цитата кончаются на пустой строке
            None => !full.any(|l| l.trim().is_empty()),
        }
    })
}

/// Строка для property-тестов чанкера: строки `md_line` и шапки блоков
/// (таблицы, callout Obsidian и GitHub, ограждение с info).
fn chunk_line() -> impl Strategy<Value = String> {
    prop_oneof![
        12 => md_line(),
        1 => Just("> [!info]- Заголовок callout".to_string()),
        1 => Just("> [!заметка]".to_string()),
        1 => Just("> [!NOTE]".to_string()),
        1 => Just("> строка цитаты с текстом".to_string()),
        1 => Just("| a | b |".to_string()),
        1 => Just("|---|---|".to_string()),
        1 => Just("| 1 | значение |".to_string()),
        1 => Just("```rust title=\"x.rs\"".to_string()),
    ]
}

/// Документ из `chunk_line` с LF или CRLF.
fn chunk_doc() -> impl Strategy<Value = String> {
    (prop::collection::vec(chunk_line(), 0..24), any::<bool>()).prop_map(|(lines, crlf)| {
        let nl = if crlf { "\r\n" } else { "\n" };
        lines.join(nl)
    })
}

/// Документ из `chunk_doc` с длинными кусками: повтор строк даёт блоки больше бюджета.
fn long_doc() -> impl Strategy<Value = String> {
    (chunk_doc(), 1usize..40).prop_map(|(d, k)| {
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
    fn chunk_invariants_default(src in chunk_doc()) {
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
            ..ChunkParams::default()
        };
        check_invariants(&src, &p)?;
    }

    /// Выбор разреза DP (λ > 0) и связность: те же инварианты (`src[range] == text`,
    /// `hard_max`, покрытие тела).
    #[test]
    fn chunk_invariants_boundary_lambda(
        src in chunk_doc(),
        lambda in prop::sample::select(vec![0.0, 1.0, 2.0]),
        cohesion in any::<bool>(),
    ) {
        check_invariants(&src, &with(lambda, cohesion))?;
    }

    #[test]
    fn chunk_invariants_boundary_lambda_small_budgets(
        src in long_doc(),
        hard in 4usize..60,
        lambda in prop::sample::select(vec![0.0, 1.0, 2.0]),
        cohesion in any::<bool>(),
    ) {
        let p = ChunkParams {
            target_min: hard / 3,
            target_max: hard * 2 / 3,
            hard_max: hard,
            merge_below: hard / 6,
            overlap_sentences: 1,
            boundary_lambda: lambda,
            cohesion,
            heading_path_h4: true,
        };
        check_invariants(&src, &p)?;
    }

    /// Фаззинг: произвольный текст и пороги — без паник, листы в пределах `hard_max`,
    /// `text` кончается на `src[range]`.
    #[test]
    fn chunk_note_fuzz_never_panics(
        src in "(\\PC|\n|#|:|`|-|\\|){0,1500}",
        hard in 1usize..200,
        lambda in 0.0f64..3.0,
        cohesion in any::<bool>(),
        h4 in any::<bool>(),
    ) {
        let p = ChunkParams {
            target_min: hard / 2,
            target_max: hard,
            hard_max: hard,
            merge_below: hard / 4,
            overlap_sentences: 2,
            boundary_lambda: lambda,
            cohesion,
            heading_path_h4: h4,
        };
        for d in chunk_note(&src, &parse(&src), &p, &ApproxCounter) {
            if let Some(r) = d.range.clone() {
                prop_assert!(d.token_count <= hard);
                prop_assert!(d.text.ends_with(&src[r]));
            }
        }
    }
}
