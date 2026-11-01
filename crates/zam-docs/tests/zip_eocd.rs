//! Строгая проверка конца центрального каталога zip (ревью задачи 4 M5, раунды 2–4): поддельная EOCD
//! в хвосте или в комментарии, неверная EOCD64, каталог сверх `max_central_dir` — отказ и в
//! `sniff`/`sniff_file`, и в разборе OOXML; настоящие DOCX/XLSX/ODS (в том числе в форме zip64)
//! по-прежнему распознаются и разбираются. Пик памяти на пробе ревьюера (zip64 на 2 млн записей
//! за поддельной EOCD) — счётчик аллокаций этого тестового бинарника.
mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

use common::{W, zip_of};
use zam_docs::{DocError, DocKind, Limits, extract, sniff, sniff_file};

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

/// Счётчик общий для бинарника: тесты идут по одному.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn u16le(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn u32le(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn u64le(v: &mut Vec<u8>, x: u64) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn rd16(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}
fn rd32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}

/// Поддельная EOCD: 1 запись, каталог 46 байт по смещению `0xFFFF_FFF0`, без комментария.
fn fake_eocd() -> Vec<u8> {
    let mut v = Vec::new();
    u32le(&mut v, 0x0605_4b50);
    u16le(&mut v, 0);
    u16le(&mut v, 0);
    u16le(&mut v, 1);
    u16le(&mut v, 1);
    u32le(&mut v, 46);
    u32le(&mut v, 0xFFFF_FFF0);
    u16le(&mut v, 0);
    v
}

/// Позиция EOCD архива без комментария (последние 22 байта).
fn eocd_at(b: &[u8]) -> usize {
    let p = b.len() - 22;
    assert_eq!(rd32(b, p), 0x0605_4b50);
    p
}

/// Тот же архив в форме zip64: EOCD64 (с `ext` байтами расширяемых данных) + локатор + EOCD с
/// полями `0xFFFF…`; `sig64` — сигнатура записи EOCD64.
fn to_zip64(b: &[u8], ext: usize, sig64: u32) -> Vec<u8> {
    let p = eocd_at(b);
    let (n, size, off) = (rd16(b, p + 10), rd32(b, p + 12), rd32(b, p + 16));
    let mut v = b[..p].to_vec();
    let e64 = v.len() as u64;
    u32le(&mut v, sig64);
    u64le(&mut v, 44 + ext as u64);
    u16le(&mut v, 45);
    u16le(&mut v, 45);
    u32le(&mut v, 0);
    u32le(&mut v, 0);
    u64le(&mut v, u64::from(n));
    u64le(&mut v, u64::from(n));
    u64le(&mut v, u64::from(size));
    u64le(&mut v, u64::from(off));
    v.extend(std::iter::repeat_n(0u8, ext));
    u32le(&mut v, 0x0706_4b50);
    u32le(&mut v, 0);
    u64le(&mut v, e64);
    u32le(&mut v, 1);
    u32le(&mut v, 0x0605_4b50);
    u16le(&mut v, 0);
    u16le(&mut v, 0);
    u16le(&mut v, 0xFFFF);
    u16le(&mut v, 0xFFFF);
    u32le(&mut v, 0xFFFF_FFFF);
    u32le(&mut v, 0xFFFF_FFFF);
    u16le(&mut v, 0);
    v
}

/// Архив с комментарием `comment` (длина EOCD исправляется).
fn with_comment(b: &[u8], comment: &[u8]) -> Vec<u8> {
    let p = eocd_at(b);
    let mut v = b.to_vec();
    v[p + 20..p + 22].copy_from_slice(&(comment.len() as u16).to_le_bytes());
    v.extend_from_slice(comment);
    v
}

fn content_types(part: &str, ct: &str) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="{part}" ContentType="{ct}"/></Types>"#
    )
    .into_bytes()
}

fn docx() -> Vec<u8> {
    zip_of(&[
        (
            "[Content_Types].xml",
            content_types(
                "/word/document.xml",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
            ),
        ),
        (
            "word/document.xml",
            format!(r#"<w:document {W}><w:body><w:p><w:r><w:t>Привет из DOCX</w:t></w:r></w:p></w:body></w:document>"#)
                .into_bytes(),
        ),
    ])
}

fn xlsx() -> Vec<u8> {
    zip_of(&[
        (
            "[Content_Types].xml",
            content_types(
                "/xl/workbook.xml",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
            ),
        ),
        ("xl/workbook.xml", b"<workbook/>".to_vec()),
    ])
}

fn ods() -> Vec<u8> {
    zip_of(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.spreadsheet".to_vec(),
        ),
        ("content.xml", b"<office:document-content/>".to_vec()),
    ])
}

fn file(b: &[u8], name: &str, l: &Limits) -> Option<DocKind> {
    sniff_file(Cursor::new(b.to_vec()), name, l).unwrap()
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn docx_text(b: &[u8], l: &Limits) -> Result<String, DocError> {
    extract(DocKind::Docx, b, l, deadline()).map(|x| {
        x.units
            .into_iter()
            .map(|u| u.text)
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// Настоящие архивы — как прежде (в том числе в форме zip64 без расширяемых данных).
#[test]
fn genuine_archives_still_recognized_and_parsed() {
    let _g = serial();
    let l = Limits::default();
    for (b, kind, name) in [
        (docx(), DocKind::Docx, "a.docx"),
        (xlsx(), DocKind::Xlsx, "b.xlsx"),
        (ods(), DocKind::Ods, "c.ods"),
    ] {
        assert_eq!(sniff(&b, name), Some(kind), "{name}");
        assert_eq!(file(&b, name, &l), Some(kind), "{name}");
        let z = to_zip64(&b, 0, 0x0606_4b50);
        assert_eq!(sniff(&z, name), Some(kind), "zip64 {name}");
        assert_eq!(file(&z, name, &l), Some(kind), "zip64 {name}");
        // комментарий без сигнатур — допустим
        let c = with_comment(&b, b"just a comment");
        assert_eq!(file(&c, name, &l), Some(kind), "комментарий {name}");
    }
    assert_eq!(docx_text(&docx(), &l).unwrap(), "Привет из DOCX");
    assert_eq!(
        docx_text(&to_zip64(&docx(), 0, 0x0606_4b50), &l).unwrap(),
        "Привет из DOCX"
    );
}

/// Поддельная EOCD в хвосте и в комментарии — отказ в `sniff`, `sniff_file` и разборе.
#[test]
fn forged_eocd_in_tail_or_comment_rejected() {
    let _g = serial();
    let l = Limits::default();
    let d = docx();
    let mut tail = d.clone();
    tail.extend_from_slice(&fake_eocd());
    let comment = with_comment(&d, &fake_eocd());
    let mut tail64 = to_zip64(&d, 0, 0x0606_4b50);
    tail64.extend_from_slice(&fake_eocd());
    // мусор после EOCD (комментарий короче фактического хвоста) — тоже отказ
    let mut garbage = d.clone();
    garbage.extend_from_slice(b"garbage");
    for (label, b) in [
        ("хвост", &tail),
        ("комментарий", &comment),
        ("хвост zip64", &tail64),
        ("мусор", &garbage),
    ] {
        assert_eq!(sniff(b, "a.docx"), None, "{label}");
        assert_eq!(file(b, "a.docx", &l), None, "{label}");
        assert!(
            matches!(docx_text(b, &l), Err(DocError::Malformed(_))),
            "{label}: {:?}",
            docx_text(b, &l)
        );
    }
}

/// zip64 с неверной EOCD64 (сигнатура, расширяемые данные, смещение в локаторе) — отказ.
#[test]
fn zip64_with_bad_eocd64_rejected() {
    let _g = serial();
    let l = Limits::default();
    let d = docx();
    let bad_sig = to_zip64(&d, 0, 0x0606_4b51);
    let ext = to_zip64(&d, 8, 0x0606_4b50);
    let mut bad_loc = to_zip64(&d, 0, 0x0606_4b50);
    let loc = bad_loc.len() - 22 - 20;
    let wrong = rd32(&bad_loc, loc + 8) - 4;
    bad_loc[loc + 8..loc + 12].copy_from_slice(&wrong.to_le_bytes());
    // записей в EOCD64 больше, чем вмещает каталог
    let mut many = to_zip64(&d, 0, 0x0606_4b50);
    let e64 = many.len() - 22 - 20 - 56;
    many[e64 + 24..e64 + 40].copy_from_slice(&[
        0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0,
    ]);
    for (label, b) in [
        ("сигнатура", &bad_sig),
        ("расширяемые данные", &ext),
        ("локатор", &bad_loc),
        ("записей больше каталога", &many),
    ] {
        assert_eq!(sniff(b, "a.docx"), None, "{label}");
        assert_eq!(file(b, "a.docx", &l), None, "{label}");
        assert!(
            matches!(docx_text(b, &l), Err(DocError::Malformed(_))),
            "{label}"
        );
    }
}

/// Каталог больше `max_central_dir` — не документ при определении вида и `truncated` при разборе.
#[test]
fn central_dir_over_limit() {
    let _g = serial();
    let d = docx();
    let small = Limits {
        max_central_dir: 64,
        ..Limits::default()
    };
    assert_eq!(file(&d, "a.docx", &small), None);
    let x = extract(DocKind::Docx, &d, &small, deadline()).unwrap();
    assert!(x.units.is_empty());
    assert!(
        x.warnings.iter().any(|w| w == "truncated: max_central_dir"),
        "{:?}",
        x.warnings
    );
    assert!(Limits::default().max_central_dir >= 16 * MIB as u64);
}

/// Проба ревьюера: локальный заголовок + `n` записей каталога с именем `name_len` байт, zip64
/// (EOCD64 + локатор + EOCD `0xFFFF`) или zip32; `fake_tail` — поддельная EOCD после настоящей.
fn probe(n: u64, name_len: usize, zip64: bool, fake_tail: bool) -> Vec<u8> {
    let mut v = Vec::new();
    let lname = b"[Content_Types].xml";
    u32le(&mut v, 0x0403_4b50);
    u16le(&mut v, 20);
    for _ in 0..4 {
        u16le(&mut v, 0);
    }
    for _ in 0..3 {
        u32le(&mut v, 0);
    }
    u16le(&mut v, lname.len() as u16);
    u16le(&mut v, 0);
    v.extend_from_slice(lname);
    let cd = v.len() as u64;
    for i in 0..n {
        u32le(&mut v, 0x0201_4b50);
        u16le(&mut v, 20);
        u16le(&mut v, 20);
        for _ in 0..4 {
            u16le(&mut v, 0);
        }
        for _ in 0..3 {
            u32le(&mut v, 0);
        }
        u16le(&mut v, name_len as u16);
        for _ in 0..4 {
            u16le(&mut v, 0);
        }
        u32le(&mut v, 0);
        u32le(&mut v, 0);
        if i == 0 && name_len >= lname.len() {
            let mut nm = lname.to_vec();
            nm.resize(name_len, b'x');
            v.extend_from_slice(&nm);
        } else {
            let s = format!("{i:0width$}", width = name_len.max(1));
            v.extend_from_slice(&s.as_bytes()[s.len() - name_len..]);
        }
    }
    let cd_size = v.len() as u64 - cd;
    if zip64 {
        let e64 = v.len() as u64;
        u32le(&mut v, 0x0606_4b50);
        u64le(&mut v, 44);
        u16le(&mut v, 45);
        u16le(&mut v, 45);
        u32le(&mut v, 0);
        u32le(&mut v, 0);
        u64le(&mut v, n);
        u64le(&mut v, n);
        u64le(&mut v, cd_size);
        u64le(&mut v, cd);
        u32le(&mut v, 0x0706_4b50);
        u32le(&mut v, 0);
        u64le(&mut v, e64);
        u32le(&mut v, 1);
        u32le(&mut v, 0x0605_4b50);
        u16le(&mut v, 0);
        u16le(&mut v, 0);
        u16le(&mut v, 0xFFFF);
        u16le(&mut v, 0xFFFF);
        u32le(&mut v, 0xFFFF_FFFF);
        u32le(&mut v, 0xFFFF_FFFF);
    } else {
        u32le(&mut v, 0x0605_4b50);
        u16le(&mut v, 0);
        u16le(&mut v, 0);
        u16le(&mut v, n as u16);
        u16le(&mut v, n as u16);
        u32le(&mut v, cd_size as u32);
        u32le(&mut v, cd as u32);
    }
    u16le(&mut v, 0);
    if fake_tail {
        v.extend_from_slice(&fake_eocd());
    }
    v
}

/// Пик памяти `sniff_file` и разбора сверх входа.
fn peak<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = CUR.load(Relaxed);
    PEAK.store(base, Relaxed);
    let r = f();
    (r, PEAK.load(Relaxed).saturating_sub(base))
}

/// Проба ревьюера: до правки поддельная EOCD пропускала zip64 на 2 млн записей в `ZipArchive`
/// (≈ 650 МиБ на файл 89 МиБ); теперь — отказ без разбора каталога, пик — буфер хвоста.
#[test]
fn reviewer_probe_peak_is_small() {
    let _g = serial();
    let l = Limits::default();
    for (label, b) in [
        ("zip64 2M честный", probe(2_000_000, 1, true, false)),
        (
            "zip64 2M + поддельная EOCD",
            probe(2_000_000, 1, true, true),
        ),
        // каталог 48 МиБ: файл в пределах `max_bytes`, каталог — сверх `max_central_dir`
        ("2500 × имя 20000", probe(2500, 20_000, false, false)),
    ] {
        let t = Instant::now();
        let (r, sniff_peak) = peak(|| sniff_file(Cursor::new(&b[..]), "x.xlsx", &l).unwrap());
        let sniff_time = t.elapsed();
        let (x, extract_peak) = peak(|| extract(DocKind::Xlsx, &b, &l, deadline()));
        eprintln!(
            "{label}: файл {} МиБ, sniff_file {r:?} пик {:.2} МиБ за {sniff_time:?}, \
             extract {} пик {:.2} МиБ",
            b.len() / MIB,
            sniff_peak as f64 / MIB as f64,
            match &x {
                Ok(x) => format!("{:?}", x.warnings),
                Err(e) => format!("{e}"),
            },
            extract_peak as f64 / MIB as f64,
        );
        assert_eq!(r, None, "{label}");
        assert!(sniff_peak < MIB, "{label}: sniff_file пик {sniff_peak}");
        assert!(extract_peak < MIB, "{label}: extract пик {extract_peak}");
        match x {
            Ok(x) => assert!(x.units.is_empty(), "{label}"),
            Err(e) => assert!(matches!(e, DocError::Malformed(_)), "{label}: {e}"),
        }
    }
}

/// Проба ревьюера раунда 2: за каталогом `probe` — маленький каталог из одной записи (`bad`:
/// `name` — длина имени 0xFFFF за концом файла, `lfh` — локальный заголовок за концом, `ok` —
/// согласованная запись) и последняя EOCD на него. До правки `zip` не разбирал каталог последней
/// EOCD и откатывался к ранней (zip64 на 2 млн записей): пик 670 МиБ.
fn fallback(n: u64, zip64: bool, bad: &str) -> Vec<u8> {
    let mut v = probe(n, 1, zip64, false);
    let small = v.len() as u64;
    u32le(&mut v, 0x0201_4b50);
    u16le(&mut v, 20);
    u16le(&mut v, 20);
    for _ in 0..4 {
        u16le(&mut v, 0);
    }
    for _ in 0..3 {
        u32le(&mut v, 0);
    }
    u16le(&mut v, if bad == "name" { 0xFFFF } else { 0 });
    for _ in 0..4 {
        u16le(&mut v, 0);
    }
    u32le(&mut v, 0);
    u32le(&mut v, if bad == "lfh" { 0xFFFF_FF00 } else { 0 });
    u32le(&mut v, 0x0605_4b50);
    u16le(&mut v, 0);
    u16le(&mut v, 0);
    u16le(&mut v, 1);
    u16le(&mut v, 1);
    u32le(&mut v, 46);
    u32le(&mut v, small as u32);
    u16le(&mut v, 0);
    v
}

/// `sniff_file` и разбор: оба отказывают, пик — меньше 2 МиБ.
fn assert_rejected_small(label: &str, b: &[u8]) {
    let l = Limits::default();
    let t = Instant::now();
    let (r, sp) = peak(|| sniff_file(Cursor::new(b), "x.xlsx", &l).unwrap());
    let st = t.elapsed();
    let t = Instant::now();
    let (x, ep) = peak(|| extract(DocKind::Xlsx, b, &l, deadline()));
    let et = t.elapsed();
    eprintln!(
        "{label}: файл {} МиБ, sniff_file {r:?} пик {:.2} МиБ за {st:?}; extract {} пик {:.2} МиБ \
         за {et:?}",
        b.len() / MIB,
        sp as f64 / MIB as f64,
        match &x {
            Ok(x) => format!("units={} {:?}", x.units.len(), x.warnings),
            Err(e) => format!("{e}"),
        },
        ep as f64 / MIB as f64,
    );
    assert_eq!(r, None, "{label}");
    assert!(sp < 2 * MIB, "{label}: sniff_file пик {sp}");
    assert!(ep < 2 * MIB, "{label}: extract пик {ep}");
    if let Ok(x) = x {
        assert!(x.units.is_empty(), "{label}");
    }
}

/// Откат `zip` к ранней EOCD (раунд правок 3, п. 1): с раунда 4 — отказ по второй сигнатуре EOCD
/// в окне поиска `zip`.
#[test]
fn fallback_to_earlier_eocd_rejected() {
    let _g = serial();
    for bad in ["name", "lfh", "ok"] {
        assert_rejected_small(
            &format!("откат zip64 2M [{bad}]"),
            &fallback(2_000_000, true, bad),
        );
    }
}

/// Ранний кандидат в окне поиска `zip` (zip32 на 20 000 или 10 записей перед честным маленьким
/// каталогом) — отказ, хотя последняя EOCD и её каталог согласованы (раунд 4: любая вторая EOCD).
#[test]
fn over_limit_candidate_in_window_rejected() {
    let _g = serial();
    assert_rejected_small("кандидат 20 000 в окне", &fallback(20_000, false, "ok"));
    // ранний кандидат в пределах лимитов, а каталог последней EOCD битый — отказ
    for bad in ["name", "lfh"] {
        let b = fallback(10, false, bad);
        assert_eq!(file(&b, "a.docx", &Limits::default()), None, "{bad}");
        assert!(
            matches!(
                docx_text(&b, &Limits::default()),
                Err(DocError::Malformed(_))
            ),
            "{bad}"
        );
    }
    // раунд 4: и кандидат в пределах лимитов — вторая EOCD в окне, отказ с понятной причиной
    let b = fallback(10, false, "ok");
    let l = Limits::default();
    assert_eq!(file(&b, "a.docx", &l), None);
    let x = extract(DocKind::Docx, &b, &l, deadline());
    assert!(
        matches!(&x, Err(DocError::Malformed(m)) if m.contains("вторая сигнатура конца каталога")),
        "{x:?}"
    );
}

/// Наполнитель из сигнатур записи каталога перед честным маленьким каталогом (п. 2): `zip`
/// открывается с `ArchiveOffset::Known(0)` и файл не сканирует.
#[test]
fn cdfh_signature_flood_does_not_scan() {
    let _g = serial();
    let mut v = probe(0, 1, false, false);
    v.truncate(v.len() - 22);
    for _ in 0..20_000_000usize {
        v.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
    }
    let cd = v.len() as u64;
    let name = b"[Content_Types].xml";
    u32le(&mut v, 0x0201_4b50);
    u16le(&mut v, 20);
    u16le(&mut v, 20);
    for _ in 0..4 {
        u16le(&mut v, 0);
    }
    for _ in 0..3 {
        u32le(&mut v, 0);
    }
    u16le(&mut v, name.len() as u16);
    for _ in 0..4 {
        u16le(&mut v, 0);
    }
    u32le(&mut v, 0);
    u32le(&mut v, 0);
    v.extend_from_slice(name);
    let size = v.len() as u64 - cd;
    u32le(&mut v, 0x0605_4b50);
    u16le(&mut v, 0);
    u16le(&mut v, 0);
    u16le(&mut v, 1);
    u16le(&mut v, 1);
    u32le(&mut v, size as u32);
    u32le(&mut v, cd as u32);
    u16le(&mut v, 0);
    // пустой `[Content_Types].xml` — не документ; важен пик
    assert_rejected_small("наполнитель 80 МБ", &v);
}

/// EOCD64 и локатор при реальных полях EOCD (п. 3): так пишет Python `zipfile` с
/// принудительным zip64-концом; конец каталога — EOCD64, поля EOCD сверяются с ней.
fn eocd64_real_fields(b: &[u8]) -> Vec<u8> {
    let p = eocd_at(b);
    let real = b[p..].to_vec();
    let z = to_zip64(b, 0, 0x0606_4b50);
    let mut v = z[..z.len() - 22].to_vec();
    v.extend_from_slice(&real);
    v
}

/// Архив потоком (дескрипторы данных, бит 3) — второй вариант Python ревьюера.
fn streamed_docx() -> Vec<u8> {
    use std::io::Write;
    let mut z = zip::ZipWriter::new_stream(Vec::new());
    for (name, data) in [
        (
            "[Content_Types].xml",
            content_types(
                "/word/document.xml",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
            ),
        ),
        (
            "word/document.xml",
            format!(r#"<w:document {W}><w:body><w:p><w:r><w:t>Привет из DOCX</w:t></w:r></w:p></w:body></w:document>"#)
                .into_bytes(),
        ),
    ] {
        z.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(&data).unwrap();
    }
    z.finish().unwrap().into_inner()
}

#[test]
fn eocd64_with_real_eocd_fields_accepted() {
    let _g = serial();
    let l = Limits::default();
    for (label, b) in [
        ("принудительный zip64-конец", eocd64_real_fields(&docx())),
        ("поток + zip64-конец", eocd64_real_fields(&streamed_docx())),
        ("поток", streamed_docx()),
    ] {
        assert!(
            zip::ZipArchive::new(Cursor::new(&b[..])).is_ok(),
            "{label}: zip не открывает"
        );
        let (r, sp) = peak(|| file(&b, "a.docx", &l));
        assert_eq!(r, Some(DocKind::Docx), "{label}");
        assert_eq!(sniff(&b, "a.docx"), Some(DocKind::Docx), "{label}");
        assert_eq!(docx_text(&b, &l).unwrap(), "Привет из DOCX", "{label}");
        assert!(sp < 2 * MIB, "{label}: пик {sp}");
    }
    // поля EOCD расходятся с EOCD64 — отказ
    let mut bad = eocd64_real_fields(&docx());
    let p = bad.len() - 22;
    let n = rd16(&bad, p + 10) + 1;
    bad[p + 10..p + 12].copy_from_slice(&n.to_le_bytes());
    bad[p + 8..p + 10].copy_from_slice(&n.to_le_bytes());
    assert_eq!(file(&bad, "a.docx", &l), None);
    assert!(matches!(docx_text(&bad, &l), Err(DocError::Malformed(_))));
}

/// Проба ревьюера раунда 3: большой zip64-каталог на `n` записей, EOCD64 с `ext` байтами
/// расширяемых данных (`zip` с `Known(0)` такую принимает: `record_size + 12` = расстояние до
/// локатора), затем маленький каталог из одной записи с extra `extra` и строго согласованная
/// последняя EOCD. `zip` не разбирает extra последнего каталога и откатывался к раннему
/// (до раунда 4 слой 2 пропускал такого кандидата).
fn r3(n: u64, ext: usize, extra: &[u8]) -> Vec<u8> {
    let mut v = probe(n, 1, true, false);
    if ext > 0 {
        let p = v.len() - 22 - 20 - 56;
        let tail = v.split_off(p);
        let rd64 = |i: usize| u64::from_le_bytes(tail[i..i + 8].try_into().unwrap());
        let (n64, size64, off64) = (rd64(32), rd64(40), rd64(48));
        let e64 = v.len() as u64;
        u32le(&mut v, 0x0606_4b50);
        u64le(&mut v, 44 + ext as u64);
        u16le(&mut v, 45);
        u16le(&mut v, 45);
        u32le(&mut v, 0);
        u32le(&mut v, 0);
        u64le(&mut v, n64);
        u64le(&mut v, n64);
        u64le(&mut v, size64);
        u64le(&mut v, off64);
        v.extend(std::iter::repeat_n(0u8, ext));
        u32le(&mut v, 0x0706_4b50);
        u32le(&mut v, 0);
        u64le(&mut v, e64);
        u32le(&mut v, 1);
        v.extend_from_slice(&tail[56 + 20..]);
    }
    let small = v.len() as u64;
    let name = b"a.xml";
    u32le(&mut v, 0x0201_4b50);
    u16le(&mut v, 20);
    u16le(&mut v, 20);
    for _ in 0..4 {
        u16le(&mut v, 0);
    }
    for _ in 0..3 {
        u32le(&mut v, 0);
    }
    u16le(&mut v, name.len() as u16);
    u16le(&mut v, extra.len() as u16);
    for _ in 0..3 {
        u16le(&mut v, 0);
    }
    u32le(&mut v, 0);
    u32le(&mut v, 0);
    v.extend_from_slice(name);
    v.extend_from_slice(extra);
    let size = v.len() as u64 - small;
    u32le(&mut v, 0x0605_4b50);
    u16le(&mut v, 0);
    u16le(&mut v, 0);
    u16le(&mut v, 1);
    u16le(&mut v, 1);
    u32le(&mut v, size as u32);
    u32le(&mut v, small as u32);
    u16le(&mut v, 0);
    v
}

/// Раунд 4: битое extra в последнем каталоге + ранний zip64-кандидат на 2 млн записей с EOCD64
/// `record_size = 52` — отказ, пик меньше 2 МиБ (до правки — сотни МиБ).
#[test]
fn broken_extra_with_extensible_eocd64_candidate_rejected() {
    let _g = serial();
    let l = Limits::default();
    for (label, extra) in [
        ("extra усечено (3 байта)", vec![1, 0, 8]),
        ("Unicode Path 0x7075 длины 1", vec![0x75, 0x70, 1, 0, 9]),
        ("блок 0x000a длины 0xFFFF", vec![0x0a, 0, 0xff, 0xff]),
    ] {
        let b = r3(2_000_000, 8, &extra);
        assert_rejected_small(&format!("r3 [{label}]"), &b);
        let x = extract(DocKind::Xlsx, &b, &l, deadline());
        assert!(
            matches!(&x, Err(DocError::Malformed(m)) if m.contains("вторая сигнатура конца каталога")),
            "{label}: {x:?}"
        );
    }
}

/// Вторая EOCD в окне поиска `zip` — отказ в `sniff`, `sniff_file` и разборе (с причиной), даже
/// если обе согласованы: в «данных» перед каталогом (копия настоящей EOCD) и в zip64-форме.
#[test]
fn second_eocd_in_window_rejected() {
    let _g = serial();
    let l = Limits::default();
    let d = docx();
    // копия EOCD в конце данных последней записи: каталог сдвигается, поля EOCD правятся
    let p = eocd_at(&d);
    let (size, off) = (rd32(&d, p + 12), rd32(&d, p + 16) as usize);
    let mut v = d[..off].to_vec();
    v.extend_from_slice(&d[p..]);
    let new_off = v.len() as u32;
    v.extend_from_slice(&d[off..]);
    let q = v.len() - 22;
    v[q + 16..q + 20].copy_from_slice(&new_off.to_le_bytes());
    assert_eq!(rd32(&v, q + 12), size);
    let z64 = to_zip64(&v, 0, 0x0606_4b50);
    for (label, b) in [("zip32", &v), ("zip64", &z64)] {
        assert_eq!(sniff(b, "a.docx"), None, "{label}");
        assert_eq!(file(b, "a.docx", &l), None, "{label}");
        let x = docx_text(b, &l);
        assert!(
            matches!(&x, Err(DocError::Malformed(m)) if m.contains("вторая сигнатура конца каталога")),
            "{label}: {x:?}"
        );
    }
    // та же вставка без сигнатуры (первый байт изменён) — обычный документ
    let mut ok = v.clone();
    ok[off] = b'Q';
    assert_eq!(docx_text(&ok, &l).unwrap(), "Привет из DOCX");
}
