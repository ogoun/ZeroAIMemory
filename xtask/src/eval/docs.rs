//! `cargo xtask eval --docs` — eval документов M5 (спецификация §6, задача 10).
//!
//! Набор `eval/docs/` (`cargo xtask docs-corpus`) и запросы `eval/docs-queries.toml` (факт →
//! файл, `locator`, дословное `evidence`). Прогон:
//!
//! 1. **A/B извлечения PDF** ([`extract`]): каждый PDF набора разбирается выбранным бэкендом
//!    (`--pdf-backend pdfium|pdf_oxide|pdf_oxide_plain`) в отдельном процессе — доля строк
//!    эталона в тексте страницы, доля пар соседних строк в верном порядке (двухколоночные
//!    страницы, таблицы, CJK), факт на своей странице, время (книга на 500 страниц — 3 повтора)
//!    и пиковая память; враждебные PDF — исход (`ok`/`error`/`crash`/`timeout`).
//! 2. **Сервер** (`--ab-only` — без него): заметки корпуса `eval/corpus` (кроме `--docs-only`)
//!    → латентность поиска основного набора **без вложений**; заметки-владельцы и загрузка
//!    файлов набора `POST /blobs` (у `pdf_oxide` — кэш извлечения `blob_extract` PDF заполняется
//!    его текстом до загрузки: нарезка, индексация и поиск — те же) → ожидание извлечения и
//!    индексации → запросы набора: recall@5 по вложению, MRR@10, точность `locator`
//!    (`/read` по `locator` карточки содержит `evidence`), совпадение `locator` с эталоном;
//!    латентность основного набора **с вложениями**; исходы враждебных файлов и «скана»;
//!    превью листов XLSX (первый экран, холодный и из кэша); с `--large` — сроки разбора
//!    больших DOCX и XLSX сервером (по одному файлу).
//!
//! Результат — `eval/results/<дата>-<префикс>-docs-<fts|модель>-<бэкенд>[-метка].{json,md}`;
//! A/B — `<дата>-<префикс>-pdf-ab-<бэкенд>.{json,md}`. Доверительные интервалы — бутстреп по
//! запросам (как у eval M2–M4.5).

pub mod extract;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use extract::{OrderScore, Outcome, PdfBackend, extract_isolated, norm, order_score};

use super::corpus::EVAL_SOLUTION;
use super::metrics::{Estimate, estimate, quantile};
use super::report::{est, write_pair};
use super::run::{RunSpec, Session, default_model, git_rev, parse_sets, server_dir, today};
use super::{EvalArgs, Mode, Paths, load_set};
use crate::docs_corpus::{Fact, Manifest};
use crate::smoke::agent_with;

/// Глубина выдачи.
const TOP_K: usize = 10;
/// Предел ожидания извлечения и индексации вложений.
const EXTRACT_WAIT: Duration = Duration::from_secs(3 * 3600);
/// Предел запроса поиска и чтения.
const REQ_TIMEOUT: Duration = Duration::from_secs(300);
/// Предел разбора файла в A/B.
const AB_LIMIT: Duration = Duration::from_secs(180);
/// Повторов разбора книги на 500 страниц (время — медиана).
const BOOK_REPEAT: usize = 3;
const BOUNDARY: &str = "zam-eval-docs-boundary-5b1f";

/// Запрос набора документов.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DocsQuery {
    pub id: String,
    pub text: String,
    pub lang: String,
    pub kind: String,
    pub fact: String,
}

#[derive(Deserialize)]
struct QueryFile {
    query: Vec<DocsQuery>,
}

/// Виды запросов.
pub const KINDS: [&str; 4] = ["exact", "paraphrase", "cross-lang", "date"];
/// Не меньше запросов (спецификация §6).
pub const MIN_QUERIES: usize = 30;

pub fn load_queries(path: &Path) -> Result<Vec<DocsQuery>> {
    let t = fs::read_to_string(path).with_context(|| path.display().to_string())?;
    let f: QueryFile = toml::from_str(&t).with_context(|| path.display().to_string())?;
    Ok(f.query)
}

/// Запросы ссылаются на факты манифеста, id уникальны, виды известны, запросов ≥ 30.
pub fn validate(m: &Manifest, qs: &[DocsQuery]) -> Result<()> {
    if qs.len() < MIN_QUERIES {
        bail!("запросов {} < {MIN_QUERIES}", qs.len());
    }
    let mut ids = std::collections::BTreeSet::new();
    for q in qs {
        if !ids.insert(q.id.as_str()) {
            bail!("повтор id запроса {}", q.id);
        }
        if !KINDS.contains(&q.kind.as_str()) {
            bail!("{}: вид {} (ожидается {KINDS:?})", q.id, q.kind);
        }
        if m.fact(&q.fact).is_none() {
            bail!("{}: нет факта {} в манифесте", q.id, q.fact);
        }
        if q.text.trim().is_empty() {
            bail!("{}: пустой текст", q.id);
        }
    }
    Ok(())
}

/// Формат файла факта: `docx` | `xlsx` | `pptx` | `pdf`.
fn format_of(file: &str) -> &str {
    file.rsplit('.').next().unwrap_or("?")
}

// ------------------------------------------------------------------------- A/B

/// Итог A/B по файлу.
#[derive(Debug, Clone, Serialize)]
pub struct AbFile {
    pub file: String,
    pub role: String,
    pub status: String,
    pub detail: Option<String>,
    pub ms_median: Option<f64>,
    pub peak_mb: Option<f64>,
    pub page_count: u32,
    pub text_pages: usize,
    pub ocr_needed: bool,
    pub warnings: Vec<String>,
    /// Страницы эталона: (страница, раскладка, метрики).
    pub order: Vec<(u32, String, OrderScore)>,
    /// Факты файла: (id, найден на своей странице).
    pub facts: Vec<(String, bool)>,
}

/// Сводка A/B по раскладке.
#[derive(Debug, Clone, Serialize)]
pub struct AbLayout {
    pub layout: String,
    pub pages: usize,
    /// Доля строк эталона в тексте (бутстреп по страницам).
    pub line_recall: Estimate,
    /// Доля пар соседних строк в верном порядке (по страницам).
    pub order: Estimate,
    /// Таблицы: строк эталона и целых строк (все ячейки в одной строке текста).
    pub table_rows: usize,
    pub table_rows_intact: usize,
}

/// Отчёт A/B бэкенда.
#[derive(Debug, Clone, Serialize)]
pub struct AbReport {
    pub date: String,
    pub commit: String,
    pub backend: PdfBackend,
    pub corpus_seed: u64,
    pub files: Vec<AbFile>,
    pub layouts: Vec<AbLayout>,
    pub facts_ok: usize,
    pub facts: usize,
    pub book_ms: Option<f64>,
    pub book_pages_per_s: Option<f64>,
}

fn median(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    Some(s[s.len() / 2])
}

/// A/B бэкенда на PDF набора.
pub fn run_ab(repo: &Path, dir: &Path, m: &Manifest, backend: PdfBackend) -> Result<AbReport> {
    let mut files = Vec::new();
    // PDF — бэкендом A/B; не PDF набора (DOCX, XLSX, PPTX; большие — если сгенерированы) —
    // `zam-docs` в процессе: время разбора без кадров рабочего процесса
    let office = |f: &&crate::docs_corpus::FileEntry| {
        backend == PdfBackend::Pdfium
            && !f.path.ends_with(".pdf")
            && matches!(f.role.as_str(), "content" | "large")
            && dir.join(&f.path).is_file()
    };
    for f in m
        .files
        .iter()
        .filter(|f| f.path.ends_with(".pdf") || office(f))
    {
        let repeat = if f.path.contains("book-") || f.role == "content" && !f.path.ends_with(".pdf")
        {
            BOOK_REPEAT
        } else {
            1
        };
        let path = dir.join(&f.path);
        let o: Outcome = extract_isolated(backend, &path, repeat, AB_LIMIT)?;
        let x = o.result.clone().unwrap_or_default();
        let page_text = |n: u32| -> &str {
            x.pages
                .iter()
                .find(|p| p.0 == n)
                .map_or("", |p| p.1.as_str())
        };
        let order: Vec<(u32, String, OrderScore)> = m
            .order
            .iter()
            .filter(|op| op.file == f.path)
            .map(|op| {
                let mut sc = order_score(&op.lines, page_text(op.page));
                if op.cols > 0 {
                    (sc.rows, sc.rows_intact) =
                        extract::rows_intact(&op.lines[1..], op.cols, page_text(op.page));
                }
                (op.page, op.layout.clone(), sc)
            })
            .collect();
        let facts: Vec<(String, bool)> = m
            .facts
            .iter()
            .filter(|ft| ft.file == f.path && f.path.ends_with(".pdf"))
            .map(|ft| {
                let page = ft
                    .locator
                    .strip_prefix("page:")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                (
                    ft.id.clone(),
                    norm(page_text(page)).contains(&norm(&ft.evidence)),
                )
            })
            .collect();
        println!(
            "  {} [{}]: {} {}, {:.0} мс, пик {:.0} МиБ, страниц с текстом {}/{}{}",
            f.path,
            backend.as_str(),
            o.status,
            o.detail.as_deref().unwrap_or(""),
            median(&x.ms).unwrap_or(o.wall_ms),
            x.peak_mb,
            x.pages.len(),
            x.page_count,
            if x.ocr_needed { ", ocr_needed" } else { "" }
        );
        files.push(AbFile {
            file: f.path.clone(),
            role: f.role.clone(),
            status: o.status,
            detail: o.detail,
            ms_median: median(&x.ms),
            peak_mb: o.result.as_ref().map(|r| r.peak_mb),
            page_count: x.page_count,
            text_pages: x.pages.len(),
            ocr_needed: x.ocr_needed,
            warnings: x.warnings.clone(),
            order,
            facts,
        });
    }
    let mut by: BTreeMap<String, Vec<&OrderScore>> = BTreeMap::new();
    for f in &files {
        for (_, layout, s) in &f.order {
            by.entry(layout.clone()).or_default().push(s);
            by.entry("all".into()).or_default().push(s);
        }
    }
    let layouts = by
        .into_iter()
        .map(|(layout, ss)| {
            let rec: Vec<f64> = ss
                .iter()
                .map(|s| s.found as f64 / s.lines.max(1) as f64)
                .collect();
            let ord: Vec<f64> = ss
                .iter()
                .filter(|s| s.pairs > 0)
                .map(|s| s.ordered as f64 / s.pairs as f64)
                .collect();
            AbLayout {
                layout,
                pages: ss.len(),
                line_recall: estimate(&rec),
                order: estimate(&ord),
                table_rows: ss.iter().map(|s| s.rows).sum(),
                table_rows_intact: ss.iter().map(|s| s.rows_intact).sum(),
            }
        })
        .collect();
    let facts: Vec<&(String, bool)> = files.iter().flat_map(|f| &f.facts).collect();
    let book = files.iter().find(|f| f.file.contains("book-"));
    let book_ms = book.and_then(|b| b.ms_median);
    Ok(AbReport {
        date: today(),
        commit: git_rev(repo),
        backend,
        corpus_seed: m.seed,
        facts_ok: facts.iter().filter(|f| f.1).count(),
        facts: facts.len(),
        book_pages_per_s: book
            .zip(book_ms)
            .map(|(b, ms)| b.page_count as f64 / (ms / 1000.0)),
        book_ms,
        files,
        layouts,
    })
}

pub fn ab_md(r: &AbReport) -> String {
    let mut s = format!(
        "# A/B извлечения PDF: {} ({})\n\nКод — `{}`, набор `eval/docs/` (сид {:#x}). Каждый файл — \
         в отдельном процессе `xtask docs-extract`; время — release, медиана повторов.\n\n",
        r.backend.as_str(),
        r.date,
        r.commit,
        r.corpus_seed
    );
    s.push_str(
        "| Раскладка | Страниц | Доля строк | Доля верного порядка | Целые строки таблиц |\n\
         |---|---:|---|---|---|\n",
    );
    for l in &r.layouts {
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            l.layout,
            l.pages,
            est(&l.line_recall),
            est(&l.order),
            if l.table_rows > 0 {
                format!("{} из {}", l.table_rows_intact, l.table_rows)
            } else {
                "—".into()
            }
        ));
    }
    s.push_str(&format!(
        "\nФакты на своей странице: **{} из {}**. Книга на 500 страниц: {}.\n\n",
        r.facts_ok,
        r.facts,
        match (r.book_ms, r.book_pages_per_s) {
            (Some(ms), Some(pps)) => format!("{:.0} мс ({pps:.0} стр./с)", ms),
            _ => "—".into(),
        }
    ));
    s.push_str("| Файл | Роль | Исход | мс | Пик, МиБ | Стр. с текстом | ocr_needed | Предупреждения |\n|---|---|---|---:|---:|---:|---|---|\n");
    for f in &r.files {
        s.push_str(&format!(
            "| `{}` | {} | {}{} | {} | {} | {}/{} | {} | {} |\n",
            f.file,
            f.role,
            f.status,
            f.detail
                .as_ref()
                .map(|d| format!(" ({})", d.chars().take(60).collect::<String>()))
                .unwrap_or_default(),
            f.ms_median.map_or("—".into(), |m| format!("{m:.0}")),
            f.peak_mb.map_or("—".into(), |m| format!("{m:.0}")),
            f.text_pages,
            f.page_count,
            f.ocr_needed,
            f.warnings.join("; ")
        ));
    }
    let bad: Vec<String> = r
        .files
        .iter()
        .flat_map(|f| f.facts.iter().filter(|x| !x.1).map(|x| x.0.clone()))
        .collect();
    if !bad.is_empty() {
        s.push_str(&format!(
            "\nФакты не на своей странице: {}.\n",
            bad.join(", ")
        ));
    }
    s
}

// ------------------------------------------------------------------------- сервер

/// Итог запроса.
#[derive(Debug, Clone, Serialize)]
pub struct QueryResult {
    pub id: String,
    /// Текст запроса (`--compare` сверяет его у пары прогонов).
    pub text: String,
    pub kind: String,
    pub format: String,
    pub fact: String,
    /// Ранг первой карточки файла факта (с 1) в первых 10.
    pub rank: Option<usize>,
    /// Ранг среди одних вложений (`filters.kinds = ["attachment"]`).
    pub rank_attach: Option<usize>,
    /// Первая карточка файла: `locator`.
    pub locator: Option<String>,
    /// `/read` по `locator` первой карточки содержит `evidence`.
    pub loc_ok: Option<bool>,
    /// `locator` первой карточки совпадает с эталоном (страница, слайд, ячейка в диапазоне;
    /// абзац — единица начинается не позже абзаца факта и `loc_ok`).
    pub loc_exact: Option<bool>,
    /// В первых 5 есть карточка файла, `/read` по `locator` которой содержит `evidence`.
    pub loc5: bool,
    pub ms: f64,
    /// Первые 5 выдачи: `файл · locator` или путь заметки.
    pub top5: Vec<String>,
    /// Выдача целиком (первые 10): сверка прогонов побайтово (A/A, `w_attach = 1,0`, M6 §11).
    #[serde(default)]
    pub hits: Vec<DocHit>,
    /// То же при `filters.kinds = ["attachment"]`.
    #[serde(default)]
    pub hits_attach: Vec<DocHit>,
}

/// Попадание выдачи для побайтовой сверки: вид, путь заметки (у вложения — владельца), файл и
/// `locator` вложения, балл.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocHit {
    pub kind: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    pub score: f64,
}

fn doc_hits(v: &Value) -> Vec<DocHit> {
    v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .take(TOP_K)
        .map(|it| DocHit {
            kind: it["kind"].as_str().unwrap_or_default().to_string(),
            path: it["path"].as_str().unwrap_or_default().to_string(),
            file: it["file"].as_str().map(String::from),
            locator: it["locator"].as_str().map(String::from),
            score: it["score"].as_f64().unwrap_or(f64::NAN),
        })
        .collect()
}

/// Основной набор `eval/queries.toml` на сервере с вложениями (M6 §11: «основной набор без
/// регрессии» при `search.w_attach`): метрики и выдача каждого запроса.
#[derive(Debug, Clone, Serialize)]
pub struct MainQuality {
    pub n: usize,
    pub ndcg10: Estimate,
    pub recall5: Estimate,
    pub queries: Vec<MainQuery>,
}

/// Запрос основного набора: nDCG@10, recall@5 и выдача (путь, балл).
#[derive(Debug, Clone, Serialize)]
pub struct MainQuery {
    pub id: String,
    pub text: String,
    pub ndcg10: f64,
    pub recall5: f64,
    pub hits: Vec<(String, f64)>,
}

fn main_quality(runs: &[super::run::QueryRun]) -> MainQuality {
    let n: Vec<f64> = runs.iter().map(|q| q.metrics.ndcg10).collect();
    let r: Vec<f64> = runs.iter().map(|q| q.metrics.recall5).collect();
    MainQuality {
        n: runs.len(),
        ndcg10: estimate(&n),
        recall5: estimate(&r),
        queries: runs
            .iter()
            .map(|q| MainQuery {
                id: q.id.clone(),
                text: q.text.clone(),
                ndcg10: q.metrics.ndcg10,
                recall5: q.metrics.recall5,
                hits: q.hits.iter().map(|h| (h.path.clone(), h.score)).collect(),
            })
            .collect(),
    }
}

/// Метрики подмножества запросов.
#[derive(Debug, Clone, Serialize)]
pub struct DocsAggregate {
    pub n: usize,
    pub recall5: Estimate,
    pub mrr10: Estimate,
    pub loc5: Estimate,
    /// Точность `locator` среди найденных в первых 5.
    pub loc_ok: Option<Estimate>,
    pub loc_exact: Option<Estimate>,
    pub found5: usize,
    /// recall@5 среди одних вложений.
    pub recall5_attach: Estimate,
}

fn aggregate(rs: &[&QueryResult]) -> DocsAggregate {
    let r5: Vec<f64> = rs
        .iter()
        .map(|r| f64::from(u8::from(r.rank.is_some_and(|k| k <= 5))))
        .collect();
    let mrr: Vec<f64> = rs
        .iter()
        .map(|r| r.rank.map_or(0.0, |k| 1.0 / k as f64))
        .collect();
    let l5: Vec<f64> = rs.iter().map(|r| f64::from(u8::from(r.loc5))).collect();
    let found: Vec<&&QueryResult> = rs
        .iter()
        .filter(|r| r.rank.is_some_and(|k| k <= 5))
        .collect();
    let ok: Vec<f64> = found
        .iter()
        .filter_map(|r| r.loc_ok.map(|b| f64::from(u8::from(b))))
        .collect();
    let ex: Vec<f64> = found
        .iter()
        .filter_map(|r| r.loc_exact.map(|b| f64::from(u8::from(b))))
        .collect();
    let r5a: Vec<f64> = rs
        .iter()
        .map(|r| f64::from(u8::from(r.rank_attach.is_some_and(|k| k <= 5))))
        .collect();
    DocsAggregate {
        recall5_attach: estimate(&r5a),
        n: rs.len(),
        recall5: estimate(&r5),
        mrr10: estimate(&mrr),
        loc5: estimate(&l5),
        loc_ok: (!ok.is_empty()).then(|| estimate(&ok)),
        loc_exact: (!ex.is_empty()).then(|| estimate(&ex)),
        found5: found.len(),
    }
}

/// Латентность набора запросов, мс (клиент).
#[derive(Debug, Clone, Serialize)]
pub struct Latency {
    pub n: usize,
    pub p50: f64,
    pub p95: f64,
    pub mean: f64,
}

fn latency(ms: &[f64]) -> Latency {
    Latency {
        n: ms.len(),
        p50: quantile(ms, 0.5),
        p95: quantile(ms, 0.95),
        mean: ms.iter().sum::<f64>() / ms.len().max(1) as f64,
    }
}

/// Документ после извлечения.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocState {
    pub file: String,
    pub role: String,
    pub expect: String,
    pub state: String,
    pub error: Option<String>,
    pub ocr_needed: bool,
    /// Предупреждения извлечения (`truncated: <лимит>`, `skipped: …`) — из кэша `blob_extract`.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Допустимые исходы манифеста и сверка с ними.
    #[serde(default)]
    pub accept: Vec<String>,
    #[serde(default)]
    pub pass: bool,
    /// От загрузки до конечного состояния, с (точность опроса 0,25 с; файлы идут в очереди
    /// `docs.workers` слотов).
    pub secs: f64,
}

/// Превью листа или миниатюра.
#[derive(Debug, Clone, Serialize)]
pub struct PreviewTime {
    pub file: String,
    pub what: String,
    pub status: u16,
    pub cold_ms: f64,
    pub warm_ms: f64,
    pub detail: Option<String>,
}

/// Кэш извлечения `blob_extract`: строк, байт zstd(JSON) и байт JSON после распаковки.
#[derive(Debug, Clone, Serialize)]
pub struct CacheStats {
    pub rows: usize,
    pub zstd_bytes: u64,
    pub json_bytes: u64,
}

/// Объём кэша извлечения в базе пользователя (проверка выбора zstd(JSON), задача 4 M5).
fn cache_stats(db: &Path) -> Result<CacheStats> {
    let c = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    c.busy_timeout(Duration::from_secs(30))?;
    let mut st = c.prepare("SELECT units FROM blob_extract")?;
    let mut rows = st.query([])?;
    let mut out = CacheStats {
        rows: 0,
        zstd_bytes: 0,
        json_bytes: 0,
    };
    while let Some(r) = rows.next()? {
        let b: Vec<u8> = r.get(0)?;
        out.rows += 1;
        out.zstd_bytes += b.len() as u64;
        out.json_bytes += zstd::stream::decode_all(&b[..])?.len() as u64;
    }
    Ok(out)
}

/// Отчёт прогона на сервере.
#[derive(Debug, Clone, Serialize)]
pub struct DocsReport {
    pub date: String,
    pub commit: String,
    pub mode: Mode,
    pub model: Option<String>,
    pub device: String,
    pub backend: PdfBackend,
    pub sets: Vec<(String, String)>,
    pub label: Option<String>,
    pub corpus_seed: u64,
    pub with_notes: bool,
    pub server_config: Value,
    pub extract_secs: f64,
    pub index_secs: f64,
    pub queries: Vec<QueryResult>,
    pub overall: DocsAggregate,
    pub by_kind: BTreeMap<String, DocsAggregate>,
    pub by_format: BTreeMap<String, DocsAggregate>,
    pub latency_docs: Latency,
    pub latency_before: Option<Latency>,
    pub latency_after: Option<Latency>,
    pub docs: Vec<DocState>,
    pub previews: Vec<PreviewTime>,
    pub cache: Option<CacheStats>,
    pub server_alive: bool,
    /// Основной набор на сервере с вложениями (кроме `--docs-only`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main: Option<MainQuality>,
}

/// Тело `POST /blobs`.
fn multipart(node: &str, file: &str, data: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(data.len() + 512);
    b.extend_from_slice(
        format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"node\"\r\n\r\n{node}\r\n")
            .as_bytes(),
    );
    b.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file}\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    b.extend_from_slice(data);
    b.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    b
}

pub(crate) fn upload(s: &Session, node: &str, file: &str, data: &[u8]) -> Result<Value> {
    let a = agent_with(REQ_TIMEOUT);
    let mut r = a
        .post(format!("{}/api/v1/blobs", s.base))
        .header("Authorization", format!("Bearer {}", s.token()))
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .send(&multipart(node, file, data)[..])?;
    let st = r.status().as_u16();
    let t = r.body_mut().read_to_string()?;
    if st != 201 {
        bail!("POST /blobs {file}: HTTP {st}: {t}");
    }
    Ok(serde_json::from_str(&t)?)
}

/// GET с токеном: статус, время и тело (JSON — разобранный, иначе только размер).
fn get_timed(s: &Session, path: &str) -> Result<(u16, f64, Option<Value>, usize)> {
    let a = agent_with(REQ_TIMEOUT);
    let t0 = Instant::now();
    let mut r = a
        .get(format!("{}{path}", s.base))
        .header("Authorization", format!("Bearer {}", s.token()))
        .call()?;
    let st = r.status().as_u16();
    let bytes = r.body_mut().with_config().limit(64 << 20).read_to_vec()?;
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    let v = serde_json::from_slice(&bytes).ok();
    Ok((st, ms, v, bytes.len()))
}

/// Пользовательская база администратора (`data/users/<id>.db`).
fn user_db(data: &Path) -> Result<PathBuf> {
    let dir = data.join("users");
    let dbs: Vec<PathBuf> = fs::read_dir(&dir)
        .with_context(|| dir.display().to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "db"))
        .collect();
    match dbs.as_slice() {
        [one] => Ok(one.clone()),
        _ => bail!(
            "в {} ожидается одна база, найдено {}",
            dir.display(),
            dbs.len()
        ),
    }
}

/// Кэш извлечения `blob_extract` для PDF набора — текстом бэкенда `backend` (A/B без правки
/// сервера: найдя кэш, конвейер не разбирает файл, а режет и индексирует сохранённые единицы).
fn inject_extracts(
    db: &Path,
    dir: &Path,
    m: &Manifest,
    backend: PdfBackend,
) -> Result<Vec<(String, Vec<u8>)>> {
    let c = rusqlite::Connection::open(db)?;
    c.busy_timeout(Duration::from_secs(30))?;
    let mut out = Vec::new();
    for f in m
        .files
        .iter()
        .filter(|f| f.path.ends_with(".pdf") && f.role != "hostile")
    {
        let o = extract_isolated(backend, &dir.join(&f.path), 1, AB_LIMIT)?;
        let Some(x) = o.result.filter(|x| x.ok) else {
            println!(
                "  {}: {} — без кэша (разберёт pdfium сервера)",
                f.path, o.status
            );
            continue;
        };
        let units: Vec<Value> = x
            .pages
            .iter()
            .map(|(no, t)| json!({"locator": format!("page:{no}"), "heading_path": [], "text": t}))
            .collect();
        let blob = zstd::bulk::compress(&serde_json::to_vec(&units)?, 3)?;
        c.execute(
            "INSERT OR REPLACE INTO blob_extract (blob_sha256, extractor_version, kind, units,
                                                  warnings, ocr_needed, created_at)
             VALUES (?1, ?2, 'pdf', ?3, ?4, ?5, ?6)",
            rusqlite::params![
                f.sha256,
                zam_docs::EXTRACTOR_VERSION,
                blob,
                serde_json::to_string(&x.warnings)?,
                x.ocr_needed,
                jiff::Timestamp::now()
                    .strftime("%Y-%m-%dT%H:%M:%S.%6fZ")
                    .to_string()
            ],
        )?;
        out.push((f.sha256.clone(), blob));
    }
    Ok(out)
}

/// После извлечения строки кэша, заполненные бэкендом A/B, — те же (сервер их не разобрал
/// заново pdfium): иначе прогон `pdf_oxide` молча стал бы прогоном pdfium.
fn verify_injected(db: &Path, injected: &[(String, Vec<u8>)]) -> Result<()> {
    let c = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    c.busy_timeout(Duration::from_secs(30))?;
    for (sha, blob) in injected {
        let got: Option<Vec<u8>> = c
            .query_row(
                "SELECT units FROM blob_extract WHERE blob_sha256 = ?1 AND extractor_version = ?2",
                rusqlite::params![sha, zam_docs::EXTRACTOR_VERSION],
                |r| r.get(0),
            )
            .ok();
        if got.as_deref() != Some(&blob[..]) {
            bail!("кэш извлечения {sha} перезаписан сервером: прогон бэкенда A/B недействителен");
        }
    }
    Ok(())
}

/// Предупреждения извлечения документа по кэшу `blob_extract` (сервер их не отдаёт в API).
fn extract_warnings(db: &Path, sha: &str) -> Vec<String> {
    let Ok(c) =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return Vec::new();
    };
    let _ = c.busy_timeout(Duration::from_secs(30));
    c.query_row(
        "SELECT warnings FROM blob_extract WHERE blob_sha256 = ?1 AND extractor_version = ?2",
        rusqlite::params![sha, zam_docs::EXTRACTOR_VERSION],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .and_then(|w| serde_json::from_str(&w).ok())
    .unwrap_or_default()
}

/// Исход документа подходит под одно из допустимых (`FileEntry::accept`).
pub fn accepted(alts: &[String], d: &DocState) -> bool {
    alts.iter().any(|alt| {
        let (head, warn) = match alt.split_once('+') {
            Some((h, w)) => (h, Some(w)),
            None => (alt.as_str(), None),
        };
        let (state, reason) = match head.split_once(':') {
            Some((st, r)) => (st, Some(r.trim())),
            None => (head, None),
        };
        d.state == state
            && reason.is_none_or(|r| d.error.as_deref().is_some_and(|e| e.starts_with(r)))
            && match warn {
                Some("ocr") => d.ocr_needed,
                Some(w) => d.warnings.iter().any(|x| x == w),
                None => true,
            }
    })
}

/// Набор на диске совпадает с манифестом (sha256 каждого файла), манифест — с генератором этой
/// сборки при том же сиде: устаревший набор — ошибка, а не тихо неверный прогон.
pub fn verify_corpus(dir: &Path, m: &Manifest) -> Result<()> {
    use sha2::{Digest, Sha256};
    let regen = "перегенерируйте набор: cargo xtask docs-corpus (с --large, если он был)";
    for f in &m.files {
        let data = fs::read(dir.join(&f.path))
            .with_context(|| format!("{}: нет файла; {regen}", f.path))?;
        let sha: String = Sha256::digest(&data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if sha != f.sha256 {
            bail!("{}: sha256 не совпадает с манифестом; {regen}", f.path);
        }
    }
    let profile = crate::docs_corpus::Profile {
        small: m.small,
        large: m.files.iter().any(|f| f.role == "large"),
    };
    if crate::docs_corpus::manifest_in_memory(m.seed, profile) != *m {
        bail!(
            "манифест не совпадает с генератором этой сборки (сид {:#x}); {regen}",
            m.seed
        );
    }
    Ok(())
}

/// `POST` JSON одним агентом с keep-alive: прогон делает тысячи запросов, и новое соединение на
/// каждый (как у `smoke::api_timeout`) исчерпывало эфемерные порты Windows (`os error 10048`,
/// раунд правок 1 задачи 10). Латентность — без установки соединения, одинаково до и после
/// загрузки вложений.
fn post_json(s: &Session, path: &str, body: &Value) -> Result<(u16, Value)> {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    let a = AGENT.get_or_init(|| agent_with(REQ_TIMEOUT));
    let mut r = a
        .post(format!("{}{path}", s.base))
        .header("Authorization", format!("Bearer {}", s.token()))
        .content_type("application/json")
        .send(body.to_string())?;
    let st = r.status().as_u16();
    let t = r.body_mut().read_to_string()?;
    let v =
        serde_json::from_str(&t).with_context(|| format!("{path}: HTTP {st}, ответ не JSON"))?;
    Ok((st, v))
}

/// Тело поиска (как у основного набора: решение eval с Knowledge, без Rules).
fn search_body(text: &str, mode: Mode) -> Value {
    json!({
        "query": text,
        "mode": "notes",
        "top_k": TOP_K,
        "budget_tokens": 1_000_000,
        "scope": { "solution": EVAL_SOLUTION, "include_rules": false },
        "rerank": if mode == Mode::HybridRerank { "on" } else { "off" },
    })
}

fn search(s: &Session, text: &str, mode: Mode) -> Result<(Value, f64)> {
    search_with(s, &search_body(text, mode), text, mode)
}

/// Поиск только по вложениям (`filters.kinds = ["attachment"]`): ранжирование фрагментов без
/// соперничества с заметками корпуса — диагностика.
fn search_attachments(s: &Session, text: &str, mode: Mode) -> Result<Value> {
    let mut body = search_body(text, mode);
    body["filters"] = json!({ "kinds": ["attachment"] });
    Ok(search_with(s, &body, text, mode)?.0)
}

fn search_with(s: &Session, body: &Value, text: &str, mode: Mode) -> Result<(Value, f64)> {
    let t0 = Instant::now();
    let (st, v) = post_json(s, "/api/v1/search", body)?;
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    if st != 200 {
        bail!("поиск «{text}»: HTTP {st}: {v}");
    }
    if mode != Mode::Fts && v["degraded"] == true {
        bail!("поиск деградирован: {}", v["degraded_reason"]);
    }
    Ok((v, ms))
}

/// Латентность основного набора: два прохода, берётся второй (прогретые кэши).
fn main_latency(s: &Session, texts: &[String], mode: Mode) -> Result<Latency> {
    for t in texts {
        search(s, t, mode)?;
    }
    let mut ms = Vec::with_capacity(texts.len());
    for t in texts {
        ms.push(search(s, t, mode)?.1);
    }
    Ok(latency(&ms))
}

/// Текст `/read` вложения по `locator`.
fn read_back(s: &Session, attachment: &str, locator: &str) -> Result<String> {
    let body = json!({
        "items": [{"id": format!("d:{attachment}"), "locator": locator}],
        "budget_tokens": 1_000_000,
    });
    let (st, v) = post_json(s, "/api/v1/read", &body)?;
    if st != 200 {
        bail!("/read d:{attachment} {locator}: HTTP {st}: {v}");
    }
    Ok(v["items"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string())
}

/// Совпадение `locator` карточки с эталоном факта.
fn loc_exact(hit: &str, fact: &Fact, loc_ok: bool) -> Option<bool> {
    let h = zam_docs::Locator::parse(hit).ok()?;
    let f = zam_docs::Locator::parse(&fact.locator).ok()?;
    Some(match (&h, &f) {
        (zam_docs::Locator::Para(a), zam_docs::Locator::Para(b)) => a <= b && loc_ok,
        _ => h.overlaps(&f),
    })
}

/// Каналы карточки выдачи через запятую (`fts,vector`).
fn sources(it: &Value) -> String {
    it["sources"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default()
}

/// Путь заметки-владельца файла набора.
pub(crate) fn owner_path(file: &str) -> String {
    let stem = file.replace(['/', '.'], "-");
    format!("eval-docs/{stem}")
}

/// Ждёт конечного состояния всех документов (`done` | `failed` | `unsupported`).
pub(crate) fn wait_extracted(
    s: &mut Session,
    owners: &[(String, String, Instant)],
) -> Result<BTreeMap<String, (Value, f64)>> {
    let t0 = Instant::now();
    let mut done: BTreeMap<String, (Value, f64)> = BTreeMap::new();
    let mut last = Instant::now();
    while done.len() < owners.len() {
        s.ensure_alive()?;
        for (file, node, at) in owners {
            if done.contains_key(file) {
                continue;
            }
            let (st, v) = s.api(&format!("/api/v1/nodes/{node}/attachments"), None)?;
            if st != 200 {
                bail!("GET attachments {file}: HTTP {st}: {v}");
            }
            if let Some(d) = v.as_array().and_then(|a| a.first())
                && matches!(
                    d["extract_state"].as_str(),
                    Some("done" | "failed" | "unsupported" | "excluded")
                )
            {
                done.insert(file.clone(), (d.clone(), at.elapsed().as_secs_f64()));
            }
        }
        if t0.elapsed() > EXTRACT_WAIT {
            bail!("извлечение не завершилось: {}/{}", done.len(), owners.len());
        }
        if last.elapsed() > Duration::from_secs(30) {
            println!(
                "  извлечение: {}/{} ({:.0} с)",
                done.len(),
                owners.len(),
                t0.elapsed().as_secs_f64()
            );
            last = Instant::now();
        }
        thread::sleep(Duration::from_millis(250));
    }
    Ok(done)
}

/// Ждёт векторной индексации (режимы с моделью): очередь пуста, `pending_count = 0`, векторный
/// канал доступен и поиск не деградирован. Неудачи заданий (враждебные файлы) не ошибка.
fn wait_index_idle(s: &mut Session) -> Result<f64> {
    let t0 = Instant::now();
    let probe = json!({ "query": "eval", "mode": "notes", "top_k": 1 });
    loop {
        s.ensure_alive()?;
        let st = s.index_status()?;
        let q = &st["queue"];
        let idle = q["queued"] == 0 && q["running"] == 0;
        if idle && st["vector"]["available"] == true {
            let (code, v) = s.api("/api/v1/search", Some(&probe))?;
            if code == 200 && v["pending_count"] == 0 && v["degraded"] == false {
                return Ok(t0.elapsed().as_secs_f64());
            }
        }
        if t0.elapsed() > EXTRACT_WAIT {
            bail!("индексация вложений не завершилась: queue = {q}");
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// Прогон набора на сервере в режиме `mode`.
fn run_server(
    a: &EvalArgs,
    p: &Paths,
    dir: &Path,
    m: &Manifest,
    qs: &[DocsQuery],
) -> Result<DocsReport> {
    let mode = *a.modes.first().context("--mode")?;
    if a.modes.len() > 1 {
        bail!("--docs: один --mode за прогон");
    }
    let model = (mode != Mode::Fts).then(|| {
        a.model
            .clone()
            .unwrap_or_else(|| default_model(a.device).to_string())
    });
    let mut sets = parse_sets(&a.sets)?;
    sets.push((
        "docs.pdfium_dylib".into(),
        extract::pdfium_lib(&p.repo).to_string_lossy().into_owned(),
    ));
    let name = format!(
        "docs-{}-{}{}",
        model.as_deref().unwrap_or("fts"),
        a.pdf_backend.file_tag(),
        if a.docs_only { "-only" } else { "" }
    );
    let spec = RunSpec {
        name,
        model: model.clone(),
        device: a.device,
        reranker: a.reranker.clone(),
        sets: sets.clone(),
        keep_cuda_path: a.keep_cuda_path,
        ctrl_break: false,
    };
    let server = server_dir(a, p)?;
    let mut s = Session::start(p, &spec, &server, !a.reuse)?;
    let mut main_queries = Vec::new();
    let main_texts: Vec<String> = if a.docs_only {
        Vec::new()
    } else {
        let (corpus, queries, _) = load_set(p, true)?;
        main_queries = queries.clone();
        if !a.reuse {
            println!("загрузка корпуса заметок eval/corpus…");
            s.load_corpus(&corpus)?;
            // FTS-only: чанки пишутся с заметкой, векторной индексации нет
            if mode != Mode::Fts {
                s.wait_indexed()?;
            }
        }
        queries.iter().map(|q| q.text.clone()).collect()
    };
    if mode != Mode::Fts {
        s.check_model()?;
    }
    let state_file = s
        .data
        .parent()
        .context("каталог прогона")?
        .join("docs-state.json");
    let (states, extract_secs, latency_before) = if a.reuse {
        let t = fs::read_to_string(&state_file).with_context(|| {
            format!(
                "--reuse: нет {} (сначала прогон без --reuse)",
                state_file.display()
            )
        })?;
        let v: Value = serde_json::from_str(&t)?;
        let states: Vec<DocState> = serde_json::from_value(v["docs"].clone())?;
        (states, v["extract_secs"].as_f64().unwrap_or(0.0), None)
    } else {
        let before = if main_texts.is_empty() {
            None
        } else {
            println!(
                "латентность основного набора без вложений ({} запросов)…",
                main_texts.len()
            );
            Some(main_latency(&s, &main_texts, mode)?)
        };
        let t0 = Instant::now();
        s.begin_load();
        // заметки-владельцы (по одной на файл): поиск отличает файлы и по пути заметки
        let mut owners: Vec<(String, String, Instant)> = Vec::new();
        let files: Vec<_> = m
            .files
            .iter()
            .filter(|f| f.role != "large" || a.large)
            .collect();
        let mut node_of: BTreeMap<String, String> = BTreeMap::new();
        for f in &files {
            let body = json!({
                "space": "knowledge",
                "path": owner_path(&f.path),
                "kind": "note",
                "content": format!("# Вложение набора документов\n\nФайл `{}` ({}).\n", f.path, f.role),
            });
            let (st, v) = s.api("/api/v1/nodes", Some(&body))?;
            if st != 201 {
                bail!("POST /nodes {}: HTTP {st}: {v}", f.path);
            }
            node_of.insert(
                f.path.clone(),
                v["id"].as_str().context("id узла")?.to_string(),
            );
        }
        let injected = if a.pdf_backend != PdfBackend::Pdfium {
            let db = user_db(&s.data)?;
            let v = inject_extracts(&db, dir, m, a.pdf_backend)?;
            println!("кэш извлечения {}: {} PDF", a.pdf_backend.as_str(), v.len());
            v
        } else {
            Vec::new()
        };
        // большие файлы — по одному в конце: их время до `done` — время разбора без очереди
        let (big, rest): (
            Vec<&crate::docs_corpus::FileEntry>,
            Vec<&crate::docs_corpus::FileEntry>,
        ) = files.iter().copied().partition(|f| f.role == "large");
        for f in &rest {
            let data = fs::read(dir.join(&f.path))?;
            let fname = Path::new(&f.path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            upload(&s, &node_of[&f.path], &fname, &data)?;
            owners.push((f.path.clone(), node_of[&f.path].clone(), Instant::now()));
        }
        let mut done = wait_extracted(&mut s, &owners)?;
        for f in &big {
            let data = fs::read(dir.join(&f.path))?;
            let fname = Path::new(&f.path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            upload(&s, &node_of[&f.path], &fname, &data)?;
            let one = vec![(f.path.clone(), node_of[&f.path].clone(), Instant::now())];
            done.extend(wait_extracted(&mut s, &one)?);
            owners.extend(one);
        }
        let db = user_db(&s.data)?;
        verify_injected(&db, &injected)?;
        let extract_secs = t0.elapsed().as_secs_f64();
        let states: Vec<DocState> = files
            .iter()
            .map(|f| {
                let (d, secs) = done.get(&f.path).cloned().unwrap_or((Value::Null, 0.0));
                let mut st = DocState {
                    file: f.path.clone(),
                    role: f.role.clone(),
                    expect: f.expect.clone(),
                    state: d["extract_state"].as_str().unwrap_or("?").to_string(),
                    error: d["extract_error"].as_str().map(String::from),
                    ocr_needed: d["ocr_needed"] == true,
                    warnings: extract_warnings(&db, &f.sha256),
                    accept: f.accept.clone(),
                    pass: false,
                    secs,
                };
                st.pass = accepted(&st.accept, &st);
                st
            })
            .collect();
        let doc_ids: BTreeMap<String, (String, String)> = files
            .iter()
            .filter_map(|f| {
                let (d, _) = done.get(&f.path)?;
                Some((
                    f.path.clone(),
                    (
                        d["id"].as_str()?.to_string(),
                        d["sha256"].as_str().unwrap_or_default().to_string(),
                    ),
                ))
            })
            .collect();
        fs::write(
            &state_file,
            serde_json::to_string_pretty(&json!({
                "docs": states, "extract_secs": extract_secs, "node_of": node_of, "doc_ids": doc_ids,
            }))?,
        )?;
        (states, extract_secs, before)
    };
    // FTS-only: чанки вложений пишет конвейер извлечения, точек нет — ждать нечего
    let index_secs = if mode == Mode::Fts {
        0.0
    } else {
        wait_index_idle(&mut s)?
    };
    let saved: Value = serde_json::from_str(&fs::read_to_string(&state_file)?)?;
    // запросы набора
    let mut results = Vec::new();
    let mut lat = Vec::new();
    for q in qs {
        let fact = m.fact(&q.fact).context("факт")?;
        let fname = Path::new(&fact.file)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let owner = owner_path(&fact.file);
        let (v, ms) = search(&s, &q.text, mode)?;
        lat.push(ms);
        let items = v["items"].as_array().cloned().unwrap_or_default();
        let is_fact = |it: &Value| {
            it["kind"] == "attachment"
                && it["file"] == fname.as_str()
                && it["path"] == owner.as_str()
        };
        let rank = items.iter().take(TOP_K).position(is_fact).map(|i| i + 1);
        let only = search_attachments(&s, &q.text, mode)?;
        let hits_attach = doc_hits(&only);
        let rank_attach = only["items"]
            .as_array()
            .and_then(|a| a.iter().take(TOP_K).position(is_fact))
            .map(|i| i + 1);
        let mut loc_ok = None;
        let mut exact = None;
        let mut locator = None;
        let mut loc5 = false;
        for (i, it) in items.iter().take(5).enumerate() {
            if !is_fact(it) {
                continue;
            }
            let Some(loc) = it["locator"].as_str() else {
                continue;
            };
            let att = it["attachment_id"].as_str().unwrap_or_default();
            let text = read_back(&s, att, loc)?;
            let ok = norm(&text).contains(&norm(&fact.evidence));
            loc5 |= ok;
            if Some(i + 1) == rank {
                locator = Some(loc.to_string());
                loc_ok = Some(ok);
                exact = loc_exact(loc, fact, ok);
            }
        }
        let top5: Vec<String> = items
            .iter()
            .take(5)
            .map(|it| match it["kind"].as_str() {
                Some("attachment") => format!(
                    "{} · {} [{}]",
                    it["file"].as_str().unwrap_or("?"),
                    it["locator"].as_str().unwrap_or("—"),
                    sources(it)
                ),
                _ => format!("{} [{}]", it["path"].as_str().unwrap_or("?"), sources(it)),
            })
            .collect();
        results.push(QueryResult {
            id: q.id.clone(),
            text: q.text.clone(),
            kind: q.kind.clone(),
            format: format_of(&fact.file).to_string(),
            fact: q.fact.clone(),
            rank,
            rank_attach,
            locator,
            loc_ok,
            loc_exact: exact,
            loc5,
            ms,
            top5,
            hits: doc_hits(&v),
            hits_attach,
        });
    }
    // качество основного набора с вложениями (выдача и метрики по запросам)
    let main = if main_queries.is_empty() {
        None
    } else {
        println!(
            "основной набор с вложениями: качество ({} запросов)…",
            main_queries.len()
        );
        Some(main_quality(&s.run_queries(&main_queries, mode)?))
    };
    let latency_after = if main_texts.is_empty() {
        None
    } else {
        println!("латентность основного набора с вложениями…");
        Some(main_latency(&s, &main_texts, mode)?)
    };
    // превью листов XLSX и миниатюры враждебных PDF
    let mut previews = Vec::new();
    let doc_ids: BTreeMap<String, (String, String)> =
        serde_json::from_value(saved["doc_ids"].clone()).unwrap_or_default();
    if !a.reuse {
        for (file, (_, sha)) in &doc_ids {
            let what: Vec<(String, String)> =
                if file.ends_with(".xlsx") && !file.starts_with("hostile/") {
                    (1..=3)
                        .map(|n| {
                            (
                                format!("sheet {n}, строки 1–100"),
                                format!("kind=sheet&sheet={n}&from=1&rows=100"),
                            )
                        })
                        .collect()
                } else if file.starts_with("hostile/") && file.ends_with(".pdf")
                    || file.ends_with("book-500.pdf")
                {
                    vec![(
                        "thumb стр. 1, 256 px".into(),
                        "kind=thumb&page=1&px=256".into(),
                    )]
                } else {
                    continue;
                };
            for (label, q) in what {
                let path = format!("/api/v1/previews/{sha}?{q}");
                let (st, cold, v, _) = get_timed(&s, &path)?;
                let (_, warm, _, _) = get_timed(&s, &path)?;
                let detail = v.as_ref().and_then(|v| {
                    v["detail"]
                        .as_str()
                        .map(String::from)
                        .or_else(|| v["total_rows"].as_u64().map(|n| format!("total_rows {n}")))
                });
                previews.push(PreviewTime {
                    file: file.clone(),
                    what: label,
                    status: st,
                    cold_ms: cold,
                    warm_ms: warm,
                    detail,
                });
                s.ensure_alive()?;
            }
        }
    }
    let cache = user_db(&s.data).and_then(|db| cache_stats(&db)).ok();
    let server_alive = s.ensure_alive().is_ok();
    let refs: Vec<&QueryResult> = results.iter().collect();
    let mut by_kind = BTreeMap::new();
    for k in KINDS {
        let sub: Vec<&QueryResult> = results.iter().filter(|r| r.kind == k).collect();
        if !sub.is_empty() {
            by_kind.insert(k.to_string(), aggregate(&sub));
        }
    }
    let mut by_format = BTreeMap::new();
    for f in ["docx", "xlsx", "pptx", "pdf"] {
        let sub: Vec<&QueryResult> = results.iter().filter(|r| r.format == f).collect();
        if !sub.is_empty() {
            by_format.insert(f.to_string(), aggregate(&sub));
        }
    }
    Ok(DocsReport {
        date: today(),
        commit: git_rev(&p.repo),
        mode,
        model,
        device: a.device.as_str().into(),
        backend: a.pdf_backend,
        sets: parse_sets(&a.sets)?,
        label: a.label.clone(),
        corpus_seed: m.seed,
        with_notes: !a.docs_only,
        server_config: s.server_config.clone(),
        extract_secs,
        index_secs,
        overall: aggregate(&refs),
        by_kind,
        by_format,
        latency_docs: latency(&lat),
        latency_before,
        latency_after,
        queries: results,
        docs: states,
        previews,
        cache,
        server_alive,
        main,
    })
}

fn agg_row(name: &str, a: &DocsAggregate) -> String {
    format!(
        "| {name} | {} | {} | {} | {} | {} | {} | {} |\n",
        a.n,
        est(&a.recall5),
        est(&a.recall5_attach),
        est(&a.mrr10),
        est(&a.loc5),
        a.loc_ok.as_ref().map_or("—".into(), est),
        a.loc_exact.as_ref().map_or("—".into(), est),
    )
}

pub fn docs_md(r: &DocsReport) -> String {
    let mut s = format!(
        "# Eval документов: {} {} ({}, PDF — {})\n\nКод — `{}`, {}; набор `eval/docs/` (сид {:#x}), \
         запросов {}; заметки eval/corpus: {}. Извлечение {:.0} с, индексация после него {:.0} с. \
         Сервер жив после набора: {}.\n\n",
        r.mode.as_str(),
        r.model.as_deref().unwrap_or("без модели"),
        r.device,
        r.backend.as_str(),
        r.commit,
        r.date,
        r.corpus_seed,
        r.queries.len(),
        if r.with_notes { "да" } else { "нет" },
        r.extract_secs,
        r.index_secs,
        if r.server_alive { "да" } else { "**нет**" },
    );
    if !r.sets.is_empty() {
        let sets: Vec<String> = r.sets.iter().map(|(k, v)| format!("`{k}={v}`")).collect();
        s.push_str(&format!("Переопределения: {}.\n\n", sets.join(", ")));
    }
    s.push_str("recall@5 — файл факта в первых 5; MRR@10 — по файлу; loc@5 — в первых 5 есть карточка файла, `/read` по `locator` которой содержит `evidence`; точность `locator` и совпадение с эталоном — среди найденных в первых 5 (первая карточка файла).\n\n");
    s.push_str("| Срез | n | recall@5 | recall@5 (одни вложения) | MRR@10 | loc@5 | точность `locator` | `locator` = эталон |\n|---|---:|---|---|---|---|---|---|\n");
    s.push_str(&agg_row("**все**", &r.overall));
    for (k, a) in &r.by_kind {
        s.push_str(&agg_row(k, a));
    }
    for (k, a) in &r.by_format {
        s.push_str(&agg_row(k, a));
    }
    s.push_str(&format!(
        "\nЛатентность запросов набора (клиент): p50 {:.1} мс, p95 {:.1} мс.\n",
        r.latency_docs.p50, r.latency_docs.p95
    ));
    if let Some(m) = &r.main {
        s.push_str(&format!(
            "Основной набор с вложениями ({} запросов): nDCG@10 {}, recall@5 {}.\n",
            m.n,
            est(&m.ndcg10),
            est(&m.recall5)
        ));
    }
    if let (Some(b), Some(a)) = (&r.latency_before, &r.latency_after) {
        s.push_str(&format!(
            "Основной набор ({} запросов, второй проход): без вложений p50 {:.1} / p95 {:.1} мс, \
             с вложениями p50 {:.1} / p95 {:.1} мс.\n",
            b.n, b.p50, b.p95, a.p50, a.p95
        ));
    }
    s.push_str("\n## Запросы\n\n| id | вид | ранг | locator | /read ⊇ evidence | = эталон | первые 5 |\n|---|---|---:|---|---|---|---|\n");
    for q in &r.queries {
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            q.id,
            q.kind,
            q.rank.map_or("—".into(), |k| k.to_string()),
            q.locator.as_deref().unwrap_or("—"),
            q.loc_ok.map_or("—", |b| if b { "да" } else { "нет" }),
            q.loc_exact.map_or("—", |b| if b { "да" } else { "нет" }),
            q.top5.join("; ").replace('|', "\\|"),
        ));
    }
    let passed = r.docs.iter().filter(|d| d.pass).count();
    s.push_str(&format!(
        "\n## Документы\n\nСверка с допустимыми исходами манифеста: **{passed} из {}**.\n\n\
         | Файл | Роль | Допустимо | Состояние | Причина | Предупреждения | ocr_needed | Сверка | с до конца |\n\
         |---|---|---|---|---|---|---|---|---:|\n",
        r.docs.len()
    ));
    for d in &r.docs {
        s.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} | {} | {} | {:.1} |\n",
            d.file,
            d.role,
            d.accept.join(" / "),
            d.state,
            d.error.as_deref().unwrap_or(""),
            d.warnings.join("; "),
            d.ocr_needed,
            if d.pass { "да" } else { "**нет**" },
            d.secs
        ));
    }
    if let Some(c) = &r.cache {
        s.push_str(&format!(
            "
Кэш извлечения `blob_extract`: {} строк, zstd(JSON) {:.2} МБ, JSON {:.2} МБ (сжатие {:.1}×).
",
            c.rows,
            c.zstd_bytes as f64 / 1e6,
            c.json_bytes as f64 / 1e6,
            c.json_bytes as f64 / c.zstd_bytes.max(1) as f64
        ));
    }
    if !r.previews.is_empty() {
        s.push_str("\n## Превью\n\n| Файл | Что | HTTP | холодный, мс | из кэша, мс | Подробности |\n|---|---|---:|---:|---:|---|\n");
        for p in &r.previews {
            s.push_str(&format!(
                "| `{}` | {} | {} | {:.0} | {:.0} | {} |\n",
                p.file,
                p.what,
                p.status,
                p.cold_ms,
                p.warm_ms,
                p.detail.as_deref().unwrap_or("")
            ));
        }
    }
    s
}

/// Выборок бутстрепа парных разностей (как у гейта качества M4).
pub const PAIRED_BOOTSTRAP: usize = 10_000;

/// Значения метрики по запросам прогона (JSON `eval/results/…-docs-…`): id → значение;
/// `format` — только запросы этого формата файла факта.
fn per_query(v: &Value, metric: &str, format: Option<&str>) -> BTreeMap<String, f64> {
    v["queries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|q| format.is_none_or(|f| q["format"] == f))
        .map(|q| {
            let rank = q["rank"].as_u64();
            let x = match metric {
                "recall5" => f64::from(u8::from(rank.is_some_and(|k| k <= 5))),
                "mrr10" => rank.map_or(0.0, |k| 1.0 / k as f64),
                _ => f64::from(u8::from(q["loc5"] == true)),
            };
            (q["id"].as_str().unwrap_or_default().to_string(), x)
        })
        .collect()
}

/// Односторонняя нижняя граница Δ среднего `b − a` (парный бутстреп [`super::metrics::BOOTSTRAP_M6`],
/// перцентиль `alpha`).
fn one_sided_lo(a: &[f64], b: &[f64], alpha: f64) -> f64 {
    let ones = vec![1.0; a.len()];
    super::metrics::paired_ratio_q(
        a,
        &ones,
        b,
        &ones,
        super::metrics::BOOTSTRAP_M6,
        (alpha, 1.0 - alpha),
    )
    .map_or(f64::NAN, |q| q.lo)
}

/// Основной набор прогона (`main` JSON отчёта): id → (nDCG@10, выдача). Нет — `None`.
fn main_per_query(v: &Value) -> Option<BTreeMap<String, (f64, Value)>> {
    let qs = v["main"]["queries"].as_array()?;
    Some(
        qs.iter()
            .map(|q| {
                (
                    q["id"].as_str().unwrap_or_default().to_string(),
                    (q["ndcg10"].as_f64().unwrap_or(0.0), q["hits"].clone()),
                )
            })
            .collect(),
    )
}

/// Общие запросы пары прогонов, у которых разный текст или факт (набор запросов правили между
/// прогонами — разности по `id` тогда сравнивали бы разные вопросы). Текст — с задачи 12 M5;
/// у прогонов раньше его нет, тогда сверяется только факт.
fn query_mismatches(base: &Value, new: &Value) -> Vec<String> {
    let index = |v: &Value| -> BTreeMap<String, (Option<String>, Option<String>)> {
        v["queries"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|q| {
                let s = |k: &str| q[k].as_str().map(String::from);
                (s("id").unwrap_or_default(), (s("text"), s("fact")))
            })
            .collect()
    };
    let (a, b) = (index(base), index(new));
    let differ = |x: &Option<String>, y: &Option<String>| x.is_some() && y.is_some() && x != y;
    a.iter()
        .filter_map(|(id, (ta, fa))| {
            let (tb, fb) = b.get(id)?;
            (differ(ta, tb) || differ(fa, fb)).then(|| id.clone())
        })
        .collect()
}

/// `cargo xtask eval --docs --compare A --compare B [--compare C --compare D …]`: парные
/// разности B − A (D − C, …) recall@5, MRR@10 и loc@5 по общим запросам, бутстреп
/// [`PAIRED_BOOTSTRAP`]; `--docs-format pdf` — только запросы к PDF. Итог — в stdout и
/// `results/<дата>-<префикс>-docs-compare[-метка].md`.
fn compare_runs(a: &EvalArgs, p: &Paths) -> Result<()> {
    if !a.compare.len().is_multiple_of(2) {
        bail!("--compare: пары файлов «база новый»");
    }
    let fmt = a.docs_format.as_deref();
    // поправка Бонферрони — на число пар одного вызова (сравнения с базой)
    let pairs = (a.compare.len() / 2).max(1);
    let mut md = format!(
        "# Парные разности прогонов eval документов ({})\n\nБутстреп по запросам, {} выборок \
         (`metrics::paired_diff_with`); запросы — общие у пары{}.\n\n\
         У Δ документов в скобках — односторонние нижние границы 95 %: без поправки и с поправкой \
         Бонферрони на число пар вызова. Δ nDCG@10 основного — `eval/queries.toml` на сервере \
         с вложениями (односторонняя нижняя \
         граница 95 % — 5-й перцентиль); «≠ выдача» — запросов, у которых первые 10 (вид, путь, \
         файл, `locator`, балл) различаются: документы (смешанная / одни вложения) и основной.\n\n\
         | Новый − база | n | Δ recall@5 | Δ MRR@10 | Δ loc@5 | Δ nDCG@10 основного [lo 5 %] | ≠ выдача док. / влож. / осн. |\n|---|---:|---|---|---|---|---|\n",
        today(),
        PAIRED_BOOTSTRAP,
        fmt.map(|f| format!(", только формат `{f}`"))
            .unwrap_or_default()
    );
    for pair in a.compare.chunks(2) {
        let load = |f: &PathBuf| -> Result<Value> {
            Ok(serde_json::from_str(
                &fs::read_to_string(f).with_context(|| f.display().to_string())?,
            )?)
        };
        let (base, new) = (load(&pair[0])?, load(&pair[1])?);
        let bad = query_mismatches(&base, &new);
        if !bad.is_empty() {
            bail!(
                "--compare {} {}: у запросов {} разный текст или факт — прогоны на разных                  наборах запросов",
                pair[0].display(),
                pair[1].display(),
                bad.join(", ")
            );
        }
        let mut cells = Vec::new();
        let mut n = 0;
        for metric in ["recall5", "mrr10", "loc5"] {
            let (x, y) = (per_query(&base, metric, fmt), per_query(&new, metric, fmt));
            let ids: Vec<&String> = x.keys().filter(|k| y.contains_key(*k)).collect();
            n = ids.len();
            let xa: Vec<f64> = ids.iter().map(|k| x[*k]).collect();
            let ya: Vec<f64> = ids.iter().map(|k| y[*k]).collect();
            let e = super::metrics::paired_diff_with(&xa, &ya, PAIRED_BOOTSTRAP);
            cells.push(format!(
                "{} [{:+.4}; {:+.4}]",
                super::report::diff(&e),
                one_sided_lo(&xa, &ya, 0.05),
                one_sided_lo(&xa, &ya, 0.05 / pairs as f64)
            ));
        }
        let main_cell = match (main_per_query(&base), main_per_query(&new)) {
            (Some(x), Some(y)) => {
                let ids: Vec<&String> = x.keys().filter(|k| y.contains_key(*k)).collect();
                let xa: Vec<f64> = ids.iter().map(|k| x[*k].0).collect();
                let ya: Vec<f64> = ids.iter().map(|k| y[*k].0).collect();
                let e = super::metrics::paired_diff_with(&xa, &ya, PAIRED_BOOTSTRAP);
                format!(
                    "{} [{:+.4}]",
                    super::report::diff(&e),
                    one_sided_lo(&xa, &ya, 0.05)
                )
            }
            _ => "—".into(),
        };
        let differ = |key: &str| -> usize {
            let index = |v: &Value| -> BTreeMap<String, Value> {
                v["queries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|q| {
                        (
                            q["id"].as_str().unwrap_or_default().to_string(),
                            q[key].clone(),
                        )
                    })
                    .collect()
            };
            let (x, y) = (index(&base), index(&new));
            x.iter()
                .filter(|(k, v)| y.get(*k).is_some_and(|w| w != *v))
                .count()
        };
        let main_differ = match (main_per_query(&base), main_per_query(&new)) {
            (Some(x), Some(y)) => x
                .iter()
                .filter(|(k, v)| y.get(*k).is_some_and(|w| w.1 != v.1))
                .count()
                .to_string(),
            _ => "—".into(),
        };
        cells.push(main_cell);
        cells.push(format!(
            "{} / {} / {main_differ}",
            differ("hits"),
            differ("hits_attach")
        ));
        let name = |f: &PathBuf| {
            f.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        md.push_str(&format!(
            "| `{}` − `{}` | {n} | {} |\n",
            name(&pair[1]),
            name(&pair[0]),
            cells.join(" | ")
        ));
    }
    print!("{md}");
    let prefix = a
        .prefix
        .as_deref()
        .map(|x| format!("{x}-"))
        .unwrap_or_default();
    let label = a
        .label
        .as_deref()
        .map(|l| format!("-{l}"))
        .unwrap_or_default();
    fs::create_dir_all(&p.results)?;
    let out = p
        .results
        .join(format!("{}-{prefix}docs-compare{label}.md", today()));
    fs::write(&out, md)?;
    println!("{}", out.display());
    Ok(())
}

/// Точка входа `cargo xtask eval --docs`.
pub fn run(a: &EvalArgs, p: &Paths) -> Result<()> {
    let dir = a
        .docs_dir
        .clone()
        .unwrap_or_else(|| p.repo.join("eval").join("docs"));
    let m = Manifest::load(&dir)?;
    let qpath = a
        .queries
        .clone()
        .unwrap_or_else(|| p.repo.join("eval").join("docs-queries.toml"));
    let qs = load_queries(&qpath)?;
    validate(&m, &qs)?;
    if !a.compare.is_empty() {
        return compare_runs(a, p);
    }
    verify_corpus(&dir, &m)?;
    println!(
        "набор документов: {} файлов, фактов {}, запросов {}",
        m.files.len(),
        m.facts.len(),
        qs.len()
    );
    if a.check {
        println!("EVAL-DOCS-CHECK OK");
        return Ok(());
    }
    fs::create_dir_all(&p.results)?;
    let prefix = a
        .prefix
        .as_deref()
        .map(|x| format!("{x}-"))
        .unwrap_or_default();
    if !a.reuse {
        println!("A/B извлечения PDF: {}…", a.pdf_backend.as_str());
        let ab = run_ab(&p.repo, &dir, &m, a.pdf_backend)?;
        let stem = format!("{}-{prefix}pdf-ab-{}", ab.date, a.pdf_backend.file_tag());
        let (j, md) = write_pair(&p.results, &stem, &ab, &ab_md(&ab))?;
        println!("{}\n{}", j.display(), md.display());
        if a.ab_only {
            return Ok(());
        }
    }
    let r = run_server(a, p, &dir, &m, &qs)?;
    let label = a
        .label
        .as_deref()
        .map(|l| format!("-{l}"))
        .unwrap_or_default();
    let stem = format!(
        "{}-{prefix}docs-{}-{}{label}",
        r.date,
        r.model.as_deref().unwrap_or("fts"),
        a.pdf_backend.file_tag()
    );
    let md = docs_md(&r);
    let (j, mdp) = write_pair(&p.results, &stem, &r, &md)?;
    println!(
        "recall@5 {} · loc@5 {} · точность locator {}\n{}\n{}",
        est(&r.overall.recall5),
        est(&r.overall.loc5),
        r.overall.loc_ok.as_ref().map_or("—".into(), est),
        j.display(),
        mdp.display()
    );
    if !r.server_alive {
        return Err(anyhow!("сервер упал во время прогона набора документов"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docs_corpus::{DEFAULT_SEED, Profile};

    /// `--compare` сверяет текст и факт общих запросов (повторное ревью задачи 10): прогоны без
    /// текста (до задачи 12) сверяются по факту.
    #[test]
    fn compare_checks_query_text() {
        let run = |qs: &[(&str, Option<&str>, &str)]| {
            serde_json::json!({"queries": qs.iter().map(|(id, text, fact)| {
                let mut q = serde_json::json!({"id": id, "fact": fact});
                if let Some(t) = text {
                    q["text"] = serde_json::json!(t);
                }
                q
            }).collect::<Vec<_>>()})
        };
        let a = run(&[("q1", Some("alpha"), "f1"), ("q2", Some("beta"), "f2")]);
        let same = run(&[("q1", Some("alpha"), "f1"), ("q3", Some("x"), "f3")]);
        assert!(query_mismatches(&a, &same).is_empty());
        let text = run(&[("q1", Some("alpha!"), "f1"), ("q2", Some("beta"), "f2")]);
        assert_eq!(query_mismatches(&a, &text), ["q1"]);
        let fact = run(&[("q1", None, "f1"), ("q2", None, "f9")]);
        assert_eq!(query_mismatches(&a, &fact), ["q2"]);
    }

    /// Набор и запросы согласованы; метрики A/B и выдачи считаются на малом наборе: PDF
    /// разбираются pdfium в этом процессе (без pdfium — только проверка запросов), факты PDF —
    /// на своих страницах, порядок двухколоночных страниц верный.
    #[test]
    fn docs_eval_smoke() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let m = crate::docs_corpus::manifest_in_memory(
            DEFAULT_SEED,
            Profile {
                small: true,
                large: false,
            },
        );
        let qs = load_queries(&repo.join("eval").join("docs-queries.toml")).unwrap();
        validate(&m, &qs).unwrap();
        let kinds: std::collections::BTreeSet<&str> = qs.iter().map(|q| q.kind.as_str()).collect();
        assert_eq!(kinds.len(), KINDS.len(), "все виды запросов: {kinds:?}");
        // агрегаты: ранг 1 с верным locator, ранг 7 (вне @5), не найден
        let q = |rank: Option<usize>, ok: Option<bool>| QueryResult {
            rank_attach: rank,
            id: "q".into(),
            text: "query".into(),
            kind: "exact".into(),
            format: "pdf".into(),
            fact: "o1".into(),
            rank,
            locator: None,
            loc_ok: ok,
            loc_exact: ok,
            loc5: ok == Some(true),
            ms: 1.0,
            top5: Vec::new(),
            hits: Vec::new(),
            hits_attach: Vec::new(),
        };
        let rs = [q(Some(1), Some(true)), q(Some(7), None), q(None, None)];
        let agg = aggregate(&rs.iter().collect::<Vec<_>>());
        assert!((agg.recall5.mean - 1.0 / 3.0).abs() < 1e-9);
        assert!((agg.mrr10.mean - (1.0 + 1.0 / 7.0) / 3.0).abs() < 1e-9);
        assert_eq!(agg.found5, 1);
        assert_eq!(agg.loc_ok.unwrap().mean, 1.0);
        let fact = m.fact("d1").unwrap();
        assert_eq!(loc_exact("para:1", fact, true), Some(true));
        assert_eq!(loc_exact("para:999", fact, true), Some(false));
        // pdfium в этом процессе (как рабочий процесс сервера)
        if !extract::pdfium_lib(repo).is_file() {
            eprintln!("pdfium не найден (cargo xtask fetch-deps) — разбор PDF пропущен");
            return;
        }
        let o = crate::docs_corpus::generate_files(
            DEFAULT_SEED,
            Profile {
                small: true,
                large: false,
            },
        );
        for want in [
            "files/onecol-en.pdf",
            "files/twocol-en.pdf",
            "files/cyrillic-ru.pdf",
        ] {
            let data = &o.iter().find(|(p, _)| p == want).unwrap().1;
            let x = extract::extract_here(repo, PdfBackend::Pdfium, data, 1);
            assert!(x.ok, "{want}: {:?}", x.error);
            for f in m.facts.iter().filter(|f| f.file == want) {
                let n: u32 = f.locator.strip_prefix("page:").unwrap().parse().unwrap();
                let t = x
                    .pages
                    .iter()
                    .find(|p| p.0 == n)
                    .map_or("", |p| p.1.as_str());
                assert!(
                    norm(t).contains(&norm(&f.evidence)),
                    "{}: нет на стр. {n}",
                    f.id
                );
            }
            for op in m
                .order
                .iter()
                .filter(|op| op.file == want && op.layout != "table")
            {
                let t = x
                    .pages
                    .iter()
                    .find(|p| p.0 == op.page)
                    .map_or("", |p| p.1.as_str());
                let s = order_score(&op.lines, t);
                assert_eq!(s.found, s.lines, "{want} стр. {}: строки", op.page);
                assert_eq!(s.ordered, s.pairs, "{want} стр. {}: порядок", op.page);
            }
        }
    }
}
