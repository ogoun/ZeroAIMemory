//! PDF через pdfium (M5, задача 3): текст постранично, порядок колонок, колонтитулы, сканы,
//! лимиты и миниатюры.
//!
//! Фикстуры собирает писатель `zam_docs::test_pdf` (Helvetica, бинарных файлов в git нет).
//! Нужен pdfium из `cargo xtask fetch-deps` (`target/deps/<платформа>/pdfium/`); без него тесты
//! пропускаются с сообщением в stderr, как тесты моделей.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use zam_docs::pdf::{Pdfium, bind, extract_pdf, render_thumb};
use zam_docs::test_pdf::{PAGE_H, Page, build, pdfium_path};
use zam_docs::{DocError, Extracted, Limits, Locator};

/// pdfium привязывается один раз на процесс; `None` — библиотеки нет.
fn lib() -> Option<&'static Pdfium> {
    static LIB: OnceLock<Option<Pdfium>> = OnceLock::new();
    LIB.get_or_init(|| {
        let Some(p) = pdfium_path(env!("CARGO_MANIFEST_DIR")) else {
            eprintln!("pdfium не найден (cargo xtask fetch-deps) — тест PDF пропущен");
            return None;
        };
        Some(bind(&p).expect("pdfium загружается"))
    })
    .as_ref()
}

fn later() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

fn run(pdf: &[u8], limits: &Limits) -> Extracted {
    extract_pdf(lib().unwrap(), pdf, limits, later()).unwrap()
}

fn page_texts(x: &Extracted) -> Vec<(u32, &str)> {
    x.units
        .iter()
        .map(|u| match u.locator {
            Locator::Page(n) => (n, u.text.as_str()),
            ref l => panic!("не страница: {l:?}"),
        })
        .collect()
}

#[test]
fn pdf_pages_text() {
    let Some(_) = lib() else { return };
    let pdf = build(&[
        Page::default()
            .line(72.0, 700.0, "First page text")
            .line(72.0, 680.0, "second line"),
        Page::default().line(72.0, 700.0, "Second page"),
        Page::default(),
        Page::default().line(72.0, 700.0, "Fourth (with parens)"),
    ]);
    let x = run(&pdf, &Limits::default());
    assert_eq!(
        page_texts(&x),
        [
            (1, "First page text\nsecond line"),
            (2, "Second page"),
            (4, "Fourth (with parens)"),
        ]
    );
    assert!(x.units.iter().all(|u| u.heading_path.is_empty()));
    assert!(x.warnings.is_empty(), "{:?}", x.warnings);
    assert!(!x.ocr_needed, "пустая страница без картинки — не скан");
}

#[test]
fn pdf_two_columns_order() {
    let Some(_) = lib() else { return };
    // поток содержимого построчно через обе колонки: левая строка, правая строка, …
    let mut p = Page::default().line(72.0, 720.0, "Two column article title spanning the page");
    for k in 0..4 {
        let y = 680.0 - 16.0 * k as f32;
        p = p
            .line(72.0, y, &format!("left{k} alpha"))
            .line(320.0, y, &format!("right{k} beta"));
    }
    let x = run(&build(&[p]), &Limits::default());
    let t = page_texts(&x)[0].1;
    let order: Vec<usize> = [
        "title", "left0", "left1", "left2", "left3", "right0", "right1", "right2", "right3",
    ]
    .iter()
    .map(|w| t.find(w).unwrap_or_else(|| panic!("нет {w} в {t:?}")))
    .collect();
    assert!(order.is_sorted(), "порядок колонок: {t:?}");
}

#[test]
fn pdf_headers_footers_removed() {
    let Some(_) = lib() else { return };
    let pages: Vec<Page> = (1..=5)
        .map(|n| {
            let mut p = Page::default()
                .line(72.0, PAGE_H - 30.0, "ACME Quarterly Report")
                .line(72.0, 400.0, &format!("Body of page {n}"))
                .line(280.0, 20.0, &format!("Page {n} of 5"));
            if n == 3 {
                // строка в зоне колонтитулов, но не повторяется — остаётся
                p = p.line(72.0, PAGE_H - 50.0, "Unique top note");
            }
            p
        })
        .collect();
    let x = run(&build(&pages), &Limits::default());
    let t = page_texts(&x);
    assert_eq!(t.len(), 5);
    for (n, text) in &t {
        assert!(!text.contains("ACME"), "колонтитул остался: {text:?}");
        assert!(!text.contains("of 5"), "номер страницы остался: {text:?}");
        assert!(text.contains(&format!("Body of page {n}")), "{text:?}");
    }
    assert!(t[2].1.contains("Unique top note"), "{:?}", t[2].1);
    // повтор меньше чем на 60 % страниц — не колонтитул
    let mut few: Vec<Page> = (1..=5)
        .map(|n| Page::default().line(72.0, 400.0, &format!("Body {n}")))
        .collect();
    for p in few.iter_mut().take(2) {
        *p = p.clone().line(72.0, PAGE_H - 30.0, "Rare header");
    }
    let x = run(&build(&few), &Limits::default());
    assert!(
        x.units[0].text.contains("Rare header"),
        "{:?}",
        x.units[0].text
    );
}

#[test]
fn pdf_scan_ocr_needed() {
    let Some(_) = lib() else { return };
    let x = run(&build(&[Page::default().image()]), &Limits::default());
    assert!(x.ocr_needed);
    assert!(x.units.is_empty());
    // текст с картинкой — не скан
    let x = run(
        &build(&[Page::default().image().line(72.0, 700.0, "caption")]),
        &Limits::default(),
    );
    assert!(!x.ocr_needed);
    assert_eq!(page_texts(&x), [(1, "caption")]);
}

#[test]
fn pdf_limits_pages_chars_size_deadline() {
    let Some(lib) = lib() else { return };
    let pages: Vec<Page> = (0..6)
        .map(|n| Page::default().line(72.0, 700.0, &format!("page {n} with some text")))
        .collect();
    let pdf = build(&pages);
    let l = Limits {
        max_pages: 2,
        ..Limits::default()
    };
    let x = run(&pdf, &l);
    assert_eq!(x.units.len(), 2);
    assert!(x.warnings.contains(&"truncated: max_pages".to_string()));

    let l = Limits {
        max_chars: 150,
        ..Limits::default()
    };
    let x = run(&pdf, &l);
    let weight: usize = x.units.iter().map(|u| u.text.chars().count() + 64).sum();
    assert!(weight <= 150, "выдача {weight} > max_chars");
    assert!(!x.units.is_empty());
    assert!(x.warnings.contains(&"truncated: max_chars".to_string()));

    let l = Limits {
        max_bytes: 100,
        ..Limits::default()
    };
    assert_eq!(
        extract_pdf(lib, &pdf, &l, later()).unwrap_err(),
        DocError::TooLarge
    );

    let x = extract_pdf(lib, &pdf, &Limits::default(), Instant::now()).unwrap();
    assert!(x.units.is_empty());
    assert!(x.warnings.contains(&"truncated: timeout_ms".to_string()));

    for junk in [&b"%PDF-1.4\nnot a pdf"[..], b"", &pdf[..pdf.len() / 3]] {
        match extract_pdf(lib, junk, &Limits::default(), later()) {
            Err(DocError::Malformed(_)) => {}
            // обрезанный файл pdfium иногда чинит сам — тогда частичный результат
            Ok(_) => {}
            Err(e) => panic!("{e:?}"),
        }
    }
}

#[test]
fn pdf_cyrillic_and_cjk_do_not_crash() {
    let Some(_) = lib() else { return };
    // встроенных шрифтов для кириллицы и CJK нет: байты UTF-8 в WinAnsi — pdfium не должен
    // падать; качество — задача 10 (eval/docs)
    let pdf = build(&[Page::default().line(72.0, 700.0, "Привет, мир").line(
        72.0,
        680.0,
        "日本語のテキスト",
    )]);
    let x = run(&pdf, &Limits::default());
    assert_eq!(x.units.len(), 1);
}

#[test]
fn pdf_thumb_png() {
    let Some(lib) = lib() else { return };
    let pdf = build(&[Page::default().line(72.0, 700.0, "thumb").image()]);
    let png = render_thumb(lib, &pdf, 1, 200).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert_eq!(h, 200, "портрет: высота = max_px");
    assert!(w < h && w > 100, "{w}×{h}");
    assert!(matches!(
        render_thumb(lib, &pdf, 2, 200),
        Err(DocError::Malformed(_))
    ));
    assert!(matches!(
        render_thumb(lib, &pdf, 0, 200),
        Err(DocError::Malformed(_))
    ));
}

#[test]
fn pdf_xycut_staircase_within_soft_deadline() {
    // ревью задачи 3 (I-2): лестница из 130 тыс. полос на странице 210000×210000 пт (как
    // `xycut_staircase` из генератора ревью) — частичный или полный результат в пределах срока,
    // а не убийство процесса по жёсткому сроку
    let Some(lib) = lib() else { return };
    let mut p = Page {
        media: Some((210_000.0, 210_000.0)),
        ..Page::default()
    };
    for k in 0..130_000 {
        let (x, y) = (k as f32 * 0.05, 200_000.0 - k as f32 * 1.5);
        for (dx, t) in [(0.0, "a"), (100.0, "b")] {
            p.lines.push(zam_docs::test_pdf::Line {
                x: x + dx,
                y,
                size: 1.0,
                text: t.into(),
            });
        }
    }
    let pdf = build(&[p]);
    let soft = Duration::from_secs(30);
    let t = Instant::now();
    let x = extract_pdf(lib, &pdf, &Limits::default(), t + soft).unwrap();
    let took = t.elapsed();
    let chars: usize = x
        .units
        .iter()
        .map(|u| u.text.matches(['a', 'b']).count())
        .sum();
    eprintln!("лестница PDF: {took:?}, символов {chars}, {:?}", x.warnings);
    assert!(took < soft + Duration::from_secs(5), "{took:?}");
    assert!(chars > 0, "частичный результат не потерян");
}
