//! Пик памяти разбора на враждебных zip (ревью задачи 1 M5, раунды 1–3): счётчик аллокаций —
//! глобальный аллокатор этого тестового бинарника; замеры идут последовательно в одном тесте.
//!
//! Граница: `max_unpacked` (буфер части) + 2 × 4 × `max_chars` байт + 1 МиБ. Всё выданное —
//! текст единиц, их пути и служебный вес единицы (64 символа ≥ памяти `Unit` с выделениями) — и
//! ещё не выданный абзац учитываются в одном `max_chars` (≤ 4 байт на символ); второй множитель —
//! временные копии: сноски (абзацы → строка `[id] текст` → единица), строки фигур PPTX и
//! текущая строка таблицы. 1 МиБ — буферы чтения (2 × 64 КиБ), подпись пути (≤ 512 символов),
//! рост `Vec`. Основной тест — `max_unpacked` 32 МиБ, `max_chars` 1 млн, фикстуры ~40 МиБ:
//! константа 16 МиБ ≥ 9 МиБ. Вариант на 480 МиБ с умолчаниями (граница 661 МиБ) — `#[ignore]`:
//! `cargo test -p zam-docs --release --test memory_peak -- --ignored --nocapture`.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

use common::{W, drop_central, forge_compressed, zip_of, zip_with};
use zam_docs::{DocError, DocKind, Extracted, HEADING_PATH_MAX_CHARS, Limits, extract};

struct Counter;
static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn add(n: usize) {
    let c = CUR.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(c, Relaxed);
}

// SAFETY: делегирует системному аллокатору, только считает байты
unsafe impl GlobalAlloc for Counter {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            add(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        CUR.fetch_sub(l.size(), Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, n) };
        if !q.is_null() {
            if n > l.size() {
                add(n - l.size());
            } else {
                CUR.fetch_sub(l.size() - n, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static GLOBAL: Counter = Counter;

const MIB: usize = 1024 * 1024;

/// Пик памяти сверх уже занятой на время `extract`.
fn measured(bytes: &[u8], limits: &Limits) -> (Result<Extracted, DocError>, usize) {
    let base = CUR.load(Relaxed);
    PEAK.store(base, Relaxed);
    let deadline = Instant::now() + Duration::from_secs(600);
    let r = extract(DocKind::Docx, bytes, limits, deadline);
    (r, PEAK.load(Relaxed).saturating_sub(base))
}

/// `n` байт: повторы `fill` с `noise` псевдослучайными байтами из `alpha` каждые `every` байт
/// (степень сжатия ниже 100 — проверяется память, а не `max_ratio`).
fn noisy(n: usize, fill: &[u8], alpha: &[u8], every: usize, noise: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n + every + noise);
    let mut s: u64 = 0x1234_5678;
    while v.len() < n {
        for _ in 0..every / fill.len() {
            v.extend_from_slice(fill);
        }
        for _ in 0..noise {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            v.push(alpha[(s >> 33) as usize % alpha.len()]);
        }
    }
    v.truncate(n);
    v
}

fn doc_zip(doc: Vec<u8>) -> Vec<u8> {
    zip_of(&[("word/document.xml", doc)])
}

/// Сценарии раундов 1–2 ревью: (имя, zip).
fn scenarios(size: usize) -> Vec<(&'static str, Vec<u8>)> {
    let letters = b"abcdefghijklmnopqrstuvwxyz";
    let open = format!("<w:document {W}><w:body><w:p><w:r><w:t>").into_bytes();
    let close = b"</w:t></w:r></w:p></w:body></w:document>";
    // A: один текстовый узел с `\r` — нормализация концов строк не копирует узел
    let mut a = open.clone();
    a.push(b'\r');
    a.extend(noisy(size, b"a", letters, 300, 4));
    a.extend_from_slice(close);
    // B: неверный UTF-8 — ошибка, а не замена с ростом в 3 раза
    let hi: Vec<u8> = (0x80u8..=0xBF).collect();
    let b = noisy(size, &[0xFF], &hi, 300, 4);
    // C: UTF-16LE с BOM, CJK — раскодируется по ходу, объём учитывается в `max_unpacked`
    let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(u16::to_le_bytes).collect() };
    let mut c = vec![0xFF, 0xFE];
    c.extend(utf16(&format!("<w:document {W}><w:body><w:p><w:r><w:t>")));
    let alpha: Vec<u8> = (0x4Eu8..0x9F).collect();
    let mut body = noisy(size, &utf16("\u{4E00}"), &alpha, 300, 2);
    body.truncate(size & !1);
    c.extend(body);
    c.extend(utf16("</w:t></w:r></w:p></w:body></w:document>"));
    // D: абзац из одной буквы, compressed_size раздут над «дырой» без перекрытия (раунд 1)
    let pad = 5 * MIB;
    let mut d = open;
    d.extend(std::iter::repeat_n(b'a', size));
    d.extend_from_slice(close);
    let z = zip_with(
        &[("word/document.xml", d), ("pad.bin", vec![7u8; pad])],
        &["pad.bin"],
    );
    let mut d = drop_central(&z, "pad.bin");
    forge_compressed(&mut d, "word/document.xml", pad as u32);
    // E (зонд D раунда 3): заголовок 500 тыс. символов и 1000 абзацев по 1000 символов —
    // путь заголовков каждой единицы не должен копировать заголовок целиком
    let mut e = format!(
        r#"<w:document {W}><w:body><w:p><w:pPr><w:outlineLvl w:val="0"/></w:pPr><w:r><w:t>"#
    )
    .into_bytes();
    e.extend(noisy(500_000, b"h", letters, 300, 4));
    e.extend_from_slice(b"</w:t></w:r></w:p>");
    let para = noisy(1000, b"p", letters, 50, 4);
    for _ in 0..1000 {
        e.extend_from_slice(b"<w:p><w:r><w:t>");
        e.extend_from_slice(&para);
        e.extend_from_slice(b"</w:t></w:r></w:p>");
    }
    e.extend_from_slice(b"</w:body></w:document>");
    // F: множество заголовков из одной буквы — единица на каждый (служебный вес в max_chars)
    let one = br#"<w:p><w:pPr><w:outlineLvl w:val="0"/></w:pPr><w:r><w:t>x</w:t></w:r></w:p>"#;
    let mut f = format!("<w:document {W}><w:body>").into_bytes();
    for _ in 0..size / one.len() {
        f.extend_from_slice(one);
    }
    f.extend_from_slice(b"</w:body></w:document>");
    vec![
        ("A text node with CR", doc_zip(a)),
        ("B invalid UTF-8", doc_zip(b)),
        ("C UTF-16 CJK", doc_zip(c)),
        ("D forged size, one letter", d),
        ("E long heading x 1000 units", doc_zip(e)),
        ("F tiny headings", doc_zip(f)),
    ]
}

fn check(limits: &Limits, size: usize, bound: usize) {
    for (name, bytes) in scenarios(size) {
        let t = Instant::now();
        let (r, peak) = measured(&bytes, limits);
        let outcome = match &r {
            Ok(x) => format!("units={} warnings={:?}", x.units.len(), x.warnings),
            Err(e) => format!("err={e:?}"),
        };
        println!(
            "{name}: file={} KiB peak={} MiB (граница {} MiB) {outcome} t={:?}",
            bytes.len() / 1024,
            peak / MIB,
            bound / MIB,
            t.elapsed()
        );
        assert!(
            peak <= bound,
            "{name}: {} MiB > {} MiB",
            peak / MIB,
            bound / MIB
        );
        match name.chars().next() {
            Some('B') => assert!(
                matches!(&r, Err(DocError::Malformed(m)) if m.contains("invalid utf-8")),
                "{r:?}"
            ),
            Some('E' | 'F') => {
                let x = r.unwrap();
                assert!(x.units.len() > 1, "{name}");
                for u in &x.units {
                    for h in &u.heading_path {
                        assert!(h.chars().count() <= HEADING_PATH_MAX_CHARS + 1, "{name}");
                    }
                }
            }
            _ => {
                let x = r.unwrap();
                assert!(
                    x.warnings.iter().any(|w| w.starts_with("truncated: max_")),
                    "{name}: {:?}",
                    x.warnings
                );
                assert_eq!(x.units.len(), 1, "{name}");
            }
        }
    }
}

#[test]
fn peak_within_max_unpacked_plus_constant() {
    let limits = Limits {
        max_unpacked: 32 * MIB as u64,
        max_chars: 1_000_000,
        max_ratio: u32::MAX,
        ..Limits::default()
    };
    check(&limits, 40 * MIB, limits.max_unpacked as usize + 16 * MIB);
}

#[test]
#[ignore = "480 МиБ на сценарий, ~3 ГиБ памяти: вручную в release (doc-комментарий модуля)"]
fn peak_480mib_defaults() {
    let limits = Limits {
        max_ratio: u32::MAX,
        ..Limits::default()
    };
    let bound = limits.max_unpacked as usize + 2 * 4 * limits.max_chars as usize + MIB;
    check(&limits, 480 * MIB, bound);
}
