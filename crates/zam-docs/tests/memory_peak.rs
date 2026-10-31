//! Пик памяти разбора на враждебных zip (ревью задачи 1 M5, C-1): счётчик аллокаций —
//! глобальный аллокатор этого тестового бинарника (один тест, чтобы замеры не смешивались).
//!
//! Фикстура — абзац 450 МиБ (~1,5 ГиБ памяти на время теста); debug — ~3 с, release — ~1 с.
//! Цифры: `cargo test -p zam-docs --release --test memory_peak -- --nocapture`.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

use common::{W, drop_central, forge_compressed, zip_with};
use zam_docs::{DocKind, Extracted, Limits, extract};

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
fn measured(bytes: &[u8], limits: &Limits) -> (Extracted, usize) {
    let base = CUR.load(Relaxed);
    PEAK.store(base, Relaxed);
    let deadline = Instant::now() + Duration::from_secs(600);
    let x = extract(DocKind::Docx, bytes, limits, deadline).unwrap();
    (x, PEAK.load(Relaxed).saturating_sub(base))
}

#[test]
fn forged_450mib_paragraph_peak_within_limits() {
    // один абзац 450 МиБ, compressed_size раздут на 5 МиБ «дыры» (без перекрытия записей)
    let pad = 5 * MIB;
    let bytes = {
        let mut doc = format!("<w:document {W}><w:body><w:p><w:r><w:t>");
        doc.push_str(&"a".repeat(450 * MIB));
        doc.push_str("</w:t></w:r></w:p></w:body></w:document>");
        let z = zip_with(
            &[
                ("word/document.xml", doc.into_bytes()),
                ("pad.bin", vec![7u8; pad]),
            ],
            &["pad.bin"],
        );
        let mut b = drop_central(&z, "pad.bin");
        forge_compressed(&mut b, "word/document.xml", pad as u32);
        b
    };
    let defaults = Limits::default();
    let bound = defaults.max_unpacked as usize + 64 * MIB;

    // умолчания: степень сжатия по фактическим сжатым байтам обрывает чтение рано
    let t = Instant::now();
    let (x, peak) = measured(&bytes, &defaults);
    println!(
        "defaults: file={} KiB peak={} MiB warnings={:?} t={:?}",
        bytes.len() / 1024,
        peak / MIB,
        x.warnings,
        t.elapsed()
    );
    assert!(x.warnings.contains(&"truncated: max_ratio".to_string()));
    assert!(peak <= bound, "{} MiB", peak / MIB);

    // без степени сжатия: читается вся запись 450 МиБ — пик в пределах max_unpacked + 64 МиБ
    let open = Limits {
        max_ratio: u32::MAX,
        ..Limits::default()
    };
    let t = Instant::now();
    let (x, peak) = measured(&bytes, &open);
    println!(
        "max_ratio=MAX: peak={} MiB warnings={:?} chars={} t={:?}",
        peak / MIB,
        x.warnings,
        x.units.iter().map(|u| u.text.len()).sum::<usize>(),
        t.elapsed()
    );
    assert_eq!(x.warnings, ["truncated: max_chars"]);
    assert!(peak <= bound, "{} MiB > {} MiB", peak / MIB, bound / MIB);
}
