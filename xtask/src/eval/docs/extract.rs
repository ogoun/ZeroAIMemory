//! A/B извлечения PDF (спецификация M5 §6): pdfium (`zam_docs::pdf`, тот же код, что у рабочего
//! процесса `doc-worker`) против `pdf_oxide` (фича xtask `pdf-oxide`) — порядок строк, полнота
//! строк, факты на своей странице, время и пиковая память.
//!
//! Каждый файл разбирается в отдельном процессе (`xtask docs-extract`, скрытая команда): сбой
//! или зависание разборщика не роняет прогон и классифицируется (`crash`, `timeout`, `error`),
//! а пик памяти процесса — собственный, без чужих выделений.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Бэкенд разбора PDF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
pub enum PdfBackend {
    /// pdfium + раскладка XY-cut `zam-docs` (продукт)
    #[value(name = "pdfium")]
    #[serde(rename = "pdfium")]
    Pdfium,
    /// `pdf_oxide::PdfDocument::extract_text` (сборка строк по глифам)
    #[value(name = "pdf_oxide")]
    #[serde(rename = "pdf_oxide")]
    PdfOxide,
    /// `pdf_oxide::PdfDocument::to_plain_text` (полный конвейер порядка чтения, XY-cut)
    #[value(name = "pdf_oxide_plain")]
    #[serde(rename = "pdf_oxide_plain")]
    PdfOxidePlain,
}

impl PdfBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            PdfBackend::Pdfium => "pdfium",
            PdfBackend::PdfOxide => "pdf_oxide",
            PdfBackend::PdfOxidePlain => "pdf_oxide_plain",
        }
    }

    pub fn file_tag(self) -> &'static str {
        match self {
            PdfBackend::Pdfium => "pdfium",
            PdfBackend::PdfOxide => "pdf-oxide",
            PdfBackend::PdfOxidePlain => "pdf-oxide-plain",
        }
    }
}

/// Аргументы скрытой команды `xtask docs-extract`.
#[derive(Debug, clap::Args)]
pub struct ExtractArgs {
    #[arg(long, value_enum)]
    pub backend: PdfBackend,
    #[arg(long)]
    pub file: PathBuf,
    /// Повторов разбора (время — по каждому)
    #[arg(long, default_value_t = 1)]
    pub repeat: usize,
}

/// Результат разбора одного файла в процессе-исполнителе.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Extraction {
    pub ok: bool,
    pub error: Option<String>,
    /// Страницы с текстом: (номер с 1, текст).
    pub pages: Vec<(u32, String)>,
    pub page_count: u32,
    pub ms: Vec<f64>,
    pub peak_mb: f64,
    pub ocr_needed: bool,
    pub warnings: Vec<String>,
}

/// Исход разбора в процессе: `ok` | `error` | `crash` | `timeout`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub status: String,
    pub detail: Option<String>,
    pub wall_ms: f64,
    pub result: Option<Extraction>,
}

/// pdfium из `cargo xtask fetch-deps`.
pub fn pdfium_lib(repo: &Path) -> PathBuf {
    let (plat, name) = if cfg!(windows) {
        ("windows-x64", "pdfium.dll")
    } else {
        ("linux-x64", "libpdfium.so")
    };
    repo.join("target")
        .join("deps")
        .join(plat)
        .join("pdfium")
        .join(name)
}

/// Пиковая память процесса, МиБ.
fn peak_mb() -> f64 {
    #[cfg(windows)]
    {
        use windows::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows::Win32::System::Threading::GetCurrentProcess;
        let mut c = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        // SAFETY: псевдодескриптор текущего процесса и структура нужного размера
        let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
        if ok.is_ok() {
            return c.PeakWorkingSetSize as f64 / (1024.0 * 1024.0);
        }
        0.0
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmHWM:"))
                    .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
            })
            .map_or(0.0, |kb| kb / 1024.0)
    }
}

/// Разбор в этом процессе.
pub fn extract_here(repo: &Path, backend: PdfBackend, bytes: &[u8], repeat: usize) -> Extraction {
    let mut out = Extraction::default();
    let r = match backend {
        _ if !bytes.starts_with(b"%PDF") => office_units(bytes, repeat.max(1), &mut out),
        PdfBackend::Pdfium => pdfium_pages(repo, bytes, repeat.max(1), &mut out),
        PdfBackend::PdfOxide | PdfBackend::PdfOxidePlain => {
            oxide_pages(backend, bytes, repeat.max(1), &mut out)
        }
    };
    match r {
        Ok(()) => out.ok = true,
        Err(e) => out.error = Some(format!("{e:#}")),
    }
    out.peak_mb = peak_mb();
    out
}

/// Не PDF (DOCX, XLSX, PPTX…): `zam_docs::extract` при умолчаниях `[docs]` и сроке рабочего
/// процесса (80 % от 60 с) — время разбора в процессе без кадров `doc-worker`; «страницы» —
/// единицы по порядку.
fn office_units(bytes: &[u8], repeat: usize, out: &mut Extraction) -> Result<()> {
    let kind = zam_docs::sniff(bytes, "").context("вид документа не определён")?;
    let limits = zam_docs::Limits::default();
    for _ in 0..repeat {
        let t0 = Instant::now();
        let x = zam_docs::extract(kind, bytes, &limits, t0 + Duration::from_millis(48_000))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        out.ms.push(t0.elapsed().as_secs_f64() * 1000.0);
        out.page_count = x.units.len() as u32;
        out.pages = x
            .units
            .iter()
            .enumerate()
            .map(|(i, u)| (i as u32 + 1, u.text.clone()))
            .collect();
        out.warnings = x.warnings;
    }
    Ok(())
}

fn pdfium_pages(repo: &Path, bytes: &[u8], repeat: usize, out: &mut Extraction) -> Result<()> {
    // pdfium привязывается один раз на процесс (ограничение `pdfium-render`)
    static LIB: std::sync::OnceLock<Result<zam_docs::pdf::Pdfium, String>> =
        std::sync::OnceLock::new();
    let lib_path = pdfium_lib(repo);
    let lib = LIB
        .get_or_init(|| zam_docs::pdf::bind(&lib_path))
        .as_ref()
        .map_err(|e| anyhow::anyhow!("pdfium {}: {e}", lib_path.display()))?;
    let limits = zam_docs::Limits::default();
    for _ in 0..repeat {
        let t0 = Instant::now();
        // срок — как у рабочего процесса при умолчаниях: 80 % от `docs.timeout_ms` 60 с
        let deadline = t0 + Duration::from_millis(48_000);
        let x = zam_docs::pdf::extract_pdf(lib, bytes, &limits, deadline)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        out.ms.push(t0.elapsed().as_secs_f64() * 1000.0);
        out.pages = x
            .units
            .iter()
            .filter_map(|u| match u.locator {
                zam_docs::Locator::Page(n) => Some((n, u.text.clone())),
                _ => None,
            })
            .collect();
        out.ocr_needed = x.ocr_needed;
        out.warnings = x.warnings;
    }
    out.page_count = out.pages.last().map_or(0, |p| p.0);
    if let Ok(doc) = lib.load_pdf_from_byte_slice(bytes, None) {
        out.page_count = u32::try_from(doc.pages().len()).unwrap_or(0);
    }
    Ok(())
}

#[cfg(feature = "pdf-oxide")]
fn oxide_pages(
    backend: PdfBackend,
    bytes: &[u8],
    repeat: usize,
    out: &mut Extraction,
) -> Result<()> {
    use pdf_oxide::document::PdfDocument;
    for _ in 0..repeat {
        let t0 = Instant::now();
        let doc = PdfDocument::from_bytes(bytes.to_vec()).map_err(|e| anyhow::anyhow!("{e}"))?;
        let n = doc.page_count().map_err(|e| anyhow::anyhow!("{e}"))?;
        let opts = pdf_oxide::converters::ConversionOptions::default();
        let mut pages = Vec::with_capacity(n);
        let mut failed = 0u32;
        for i in 0..n {
            let t = match backend {
                PdfBackend::PdfOxidePlain => doc.to_plain_text(i, &opts),
                _ => doc.extract_text(i),
            };
            match t {
                Ok(t) if !t.trim().is_empty() => pages.push((i as u32 + 1, t)),
                Ok(_) => {}
                Err(_) => failed += 1,
            }
        }
        out.ms.push(t0.elapsed().as_secs_f64() * 1000.0);
        // колонтитулы — тем же правилом, что у pdfium (`zam-docs`): строки в 8 % у краёв
        // страницы здесь не известны, поэтому — строки, повторяющиеся на ≥ 60 % страниц
        out.pages = strip_repeated(pages);
        out.page_count = n as u32;
        out.warnings = if failed > 0 {
            vec![format!("skipped: {failed} pages")]
        } else {
            Vec::new()
        };
    }
    Ok(())
}

#[cfg(not(feature = "pdf-oxide"))]
fn oxide_pages(_: PdfBackend, _: &[u8], _: usize, _: &mut Extraction) -> Result<()> {
    anyhow::bail!(
        "pdf_oxide не собран: cargo run -p xtask --release --features pdf-oxide -- eval --docs …"
    )
}

/// Строка без цифр (номер страницы колонтитула не мешает повтору).
#[cfg_attr(not(feature = "pdf-oxide"), allow(dead_code))]
fn edge_key(l: &str) -> String {
    l.trim()
        .chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect()
}

/// Убирает первую и последнюю строку страницы, если такая строка (без цифр) есть на ≥ 60 %
/// страниц — колонтитулы, как `pdf_layout::repeated_edges` у pdfium.
#[cfg_attr(not(feature = "pdf-oxide"), allow(dead_code))]
pub fn strip_repeated(pages: Vec<(u32, String)>) -> Vec<(u32, String)> {
    use std::collections::BTreeMap;
    let n = pages.len();
    if n < 3 {
        return pages;
    }
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    for (_, t) in &pages {
        let lines: Vec<&str> = t.lines().filter(|l| !l.trim().is_empty()).collect();
        let mut keys: Vec<String> = Vec::new();
        keys.extend(lines.first().map(|l| edge_key(l)));
        keys.extend(lines.last().map(|l| edge_key(l)));
        keys.dedup();
        for k in keys {
            *count.entry(k).or_default() += 1;
        }
    }
    let repeated = |l: &str| count.get(&edge_key(l)).is_some_and(|&c| c * 10 >= n * 6);
    pages
        .into_iter()
        .map(|(no, t)| {
            let mut lines: Vec<&str> = t.lines().filter(|l| !l.trim().is_empty()).collect();
            if lines.first().is_some_and(|l| repeated(l)) {
                lines.remove(0);
            }
            if lines.last().is_some_and(|l| repeated(l)) {
                lines.pop();
            }
            (no, lines.join("\n"))
        })
        .collect()
}

/// Точка входа скрытой команды: JSON [`Extraction`] в stdout.
pub fn child_main(a: ExtractArgs, repo: &Path) -> Result<()> {
    let bytes = std::fs::read(&a.file).with_context(|| a.file.display().to_string())?;
    let x = extract_here(repo, a.backend, &bytes, a.repeat);
    println!("{}", serde_json::to_string(&x)?);
    Ok(())
}

/// Разбор файла в отдельном процессе со сроком `limit`.
pub fn extract_isolated(
    backend: PdfBackend,
    file: &Path,
    repeat: usize,
    limit: Duration,
) -> Result<Outcome> {
    let exe = std::env::current_exe().context("путь xtask")?;
    let out_path = file.with_extension("extract.json");
    let _ = std::fs::remove_file(&out_path);
    let stdout = std::fs::File::create(&out_path)?;
    let t0 = Instant::now();
    let mut child = Command::new(exe)
        .args(["docs-extract", "--backend", backend.as_str(), "--repeat"])
        .arg(repeat.to_string())
        .arg("--file")
        .arg(file)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::null())
        .spawn()
        .context("запуск docs-extract")?;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break Some(st);
        }
        if t0.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let wall_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let text = std::fs::read_to_string(&out_path).unwrap_or_default();
    let _ = std::fs::remove_file(&out_path);
    let parsed: Option<Extraction> = text
        .lines()
        .last()
        .and_then(|l| serde_json::from_str(l).ok());
    Ok(match (status, parsed) {
        (None, _) => Outcome {
            status: "timeout".into(),
            detail: Some(format!("> {} с", limit.as_secs())),
            wall_ms,
            result: None,
        },
        (Some(st), Some(x)) if st.success() => Outcome {
            status: if x.ok { "ok" } else { "error" }.into(),
            detail: x.error.clone(),
            wall_ms,
            result: Some(x),
        },
        (Some(st), _) => Outcome {
            status: "crash".into(),
            detail: Some(format!("{st}")),
            wall_ms,
            result: None,
        },
    })
}

/// Без пробельных символов: переносы строк и пробелы раскладки не влияют на сравнение.
pub fn norm(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Метрики порядка страницы.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OrderScore {
    /// Строк эталона.
    pub lines: usize,
    /// Строк эталона, найденных в тексте страницы (подстрока без пробелов).
    pub found: usize,
    /// Пар соседних строк эталона, обе однозначно найдены (строка — ровно одно вхождение в
    /// эталоне и в тексте).
    pub pairs: usize,
    /// Из них в верном порядке.
    pub ordered: usize,
    /// Таблица: строк эталона.
    #[serde(default)]
    pub rows: usize,
    /// Таблица: строк, все ячейки которых — в одной строке текста и по порядку (строка таблицы
    /// не разорвана чтением по столбцам).
    #[serde(default)]
    pub rows_intact: usize,
}

/// Целость строк таблицы: ячейки строки эталона (`cells`, по `cols`) — в одной строке текста
/// страницы, слева направо.
pub fn rows_intact(cells: &[String], cols: usize, text: &str) -> (usize, usize) {
    if cols == 0 {
        return (0, 0);
    }
    let lines: Vec<String> = text.lines().map(norm).collect();
    let rows: Vec<&[String]> = cells.chunks(cols).filter(|r| r.len() == cols).collect();
    let intact = rows
        .iter()
        .filter(|row| {
            lines.iter().any(|l| {
                let mut at = 0;
                row.iter().all(|c| match l[at..].find(&norm(c)) {
                    Some(i) => {
                        at += i + norm(c).len();
                        true
                    }
                    None => false,
                })
            })
        })
        .count();
    (rows.len(), intact)
}

/// Порядок и полнота строк страницы `text` против эталона `gt`.
pub fn order_score(gt: &[String], text: &str) -> OrderScore {
    let t = norm(text);
    let keys: Vec<String> = gt.iter().map(|l| norm(l)).collect();
    let count_in = |hay: &str, k: &str| hay.matches(k).count();
    let mut s = OrderScore {
        lines: gt.len(),
        ..OrderScore::default()
    };
    let mut pos: Vec<Option<usize>> = Vec::with_capacity(keys.len());
    for k in &keys {
        if k.is_empty() {
            pos.push(None);
            continue;
        }
        if t.contains(k.as_str()) {
            s.found += 1;
        }
        let unique_gt = keys.iter().filter(|x| x.contains(k.as_str())).count() == 1;
        pos.push(
            (unique_gt && count_in(&t, k) == 1)
                .then(|| t.find(k.as_str()))
                .flatten(),
        );
    }
    for w in pos.windows(2) {
        if let (Some(a), Some(b)) = (w[0], w[1]) {
            s.pairs += 1;
            if a < b {
                s.ordered += 1;
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_score_counts_pairs_and_recall() {
        let gt: Vec<String> = ["alpha one", "beta two", "gamma three", "delta four"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let s = order_score(&gt, "alpha one\nbeta two\ngamma three\ndelta four");
        assert_eq!((s.found, s.pairs, s.ordered), (4, 3, 3));
        // колонки переставлены: «gamma, delta, alpha, beta»
        let s = order_score(&gt, "gamma three delta four alpha one beta two");
        assert_eq!((s.found, s.pairs, s.ordered), (4, 3, 2));
        let s = order_score(&gt, "alpha one");
        assert_eq!((s.found, s.pairs), (1, 0));
    }

    #[test]
    fn strip_repeated_removes_running_headers() {
        let pages: Vec<(u32, String)> = (1..=5)
            .map(|i| (i, format!("Compendium 2025\nbody {i} text\nPage {i} of 5")))
            .collect();
        let s = strip_repeated(pages);
        assert!(
            s.iter().all(|(i, t)| t == &format!("body {i} text")),
            "{s:?}"
        );
    }
}
