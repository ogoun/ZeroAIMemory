//! Нагеты сценариев ТС-1 и отпечатки набора (спецификация M6 §3.2).
//!
//! **Нагет** — дословный фрагмент источника (20–200 символов, 1–4 на источник), без которого
//! ответа нет. Проверки `cargo xtask eval --check` ([`validate`]):
//! - фрагмент встречается в источнике ровно один раз (пробельные символы — как один пробел, как у
//!   `evidence` запросов `chunk`);
//! - фрагмент не входит в L0 источника (приближение L0 сервера — [`l0_of`]);
//! - у каждого источника `expect` есть нагет;
//! - ≥ 40 % нагетов набора — с низким лексическим пересечением с вопросом: доля основ Snowball
//!   слов нагета (без служебных), общих с вопросом, ≤ 0,2.
//!
//! Нагет **найден**, если целиком содержится в выданном агенту тексте сценария после снятия
//! отступов и маркеров Markdown ([`normalize_md`], [`found`]).
//!
//! **Заморозка** — `eval/nuggets.lock` рядом с файлом сценариев ([`check_lock`]):
//! - [`nuggets_fingerprint`] — файл сценариев (с нагетами) и тексты источников нагетов;
//! - [`corpus_fingerprint`] — всё, что грузит ТС-1: `eval/corpus`, `eval/insights.toml` и
//!   набор документов `eval/docs` — только его части [`DOCS_PARTS`] (`manifest.json`, `files/`,
//!   `hostile/`): посторонние файлы в корне `eval/docs` и `large/` отпечаток не меняют;
//! - в замке ещё `docs_manifest_sha256` — sha256 `eval/docs/manifest.json`: при его расхождении
//!   ошибка называет причину «набор документов собран другим генератором».
//!
//! Источник нагета — заметка (путь), `insight:<key>` или документ набора `doc:<путь в
//! eval/docs>` ([`DOC_REF`]; только DOCX, PPTX, XLSX — их текст извлекается `zam-docs` в
//! процессе; L0 у вложений нет).
//!
//! Отпечаток — blake3 (32 шестнадцатеричных знака); `\r\n` текстовых файлов сводится к `\n`,
//! чтобы отпечаток не зависел от `core.autocrlf`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::corpus::{Corpus, INSIGHT_REF, content_words, squash_ws, stem};

/// Длина нагета, символов.
pub const NUGGET_CHARS: std::ops::RangeInclusive<usize> = 20..=200;
/// Нагетов на источник.
pub const NUGGETS_PER_SOURCE: std::ops::RangeInclusive<usize> = 1..=4;
/// Порог «низкого» лексического пересечения нагета с вопросом.
pub const LOW_OVERLAP: f64 = 0.2;
/// Не меньше такой доли нагетов набора — с низким пересечением.
pub const MIN_LOW_OVERLAP_SHARE: f64 = 0.4;
/// Предел авто-L0 сервера (`L0_AUTO_MAX_CHARS` в zam-store).
const L0_AUTO_MAX_CHARS: usize = 200;
/// Имя файла замка рядом с файлом сценариев.
pub const LOCK_FILE: &str = "nuggets.lock";
/// Префикс источника-документа: `doc:<путь в eval/docs>` (`doc:files/roadmap.pptx`).
pub const DOC_REF: &str = "doc:";
/// Части набора документов, которые входят в отпечаток корпуса (прочее в корне `eval/docs` —
/// посторонние файлы и `large/` — не входит).
pub const DOCS_PARTS: [&str; 3] = ["manifest.json", "files", "hostile"];
/// Манифест набора документов.
pub const DOCS_MANIFEST: &str = "manifest.json";
/// Предел разбора документа-источника.
const DOC_EXTRACT_LIMIT: Duration = Duration::from_secs(120);

/// Нагет сценария.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Nugget {
    /// Источник из `expect`: путь заметки или `insight:<key>`.
    pub source: String,
    pub text: String,
}

/// Текстовый файл: `\r\n` → `\n`.
fn is_text(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()),
        Some("md" | "toml" | "txt" | "json" | "csv")
    )
}

fn file_bytes(p: &Path) -> Option<Vec<u8>> {
    let b = fs::read(p).ok()?;
    if !is_text(p) {
        return Some(b);
    }
    let mut out = Vec::with_capacity(b.len());
    for (i, c) in b.iter().enumerate() {
        if *c == b'\r' && b.get(i + 1) == Some(&b'\n') {
            continue;
        }
        out.push(*c);
    }
    Some(out)
}

fn hex(h: blake3::Hash) -> String {
    h.to_hex().as_str()[..32].to_string()
}

fn put(h: &mut blake3::Hasher, label: &str, bytes: Option<&[u8]>) {
    h.update(label.as_bytes());
    h.update(&[0]);
    match bytes {
        Some(b) => {
            h.update(&(b.len() as u64).to_le_bytes());
            h.update(b);
        }
        None => {
            h.update(b"<missing>");
        }
    }
}

#[derive(Deserialize)]
struct NuggetsOnly {
    #[serde(default)]
    scenario: Vec<ScenarioNuggets>,
}

#[derive(Deserialize)]
struct ScenarioNuggets {
    #[serde(default)]
    nuggets: Vec<Nugget>,
}

/// Файлы набора ТС-1: корпус заметок, файл insight и каталог набора документов.
#[derive(Debug, Clone, Copy)]
pub struct Sources<'a> {
    pub corpus: &'a Path,
    pub insights: &'a Path,
    pub docs: &'a Path,
}

/// Файл источника нагета: заметка `<corpus>/<путь>.md`, `insight:` — файл insight набора
/// (`--insights`), `doc:` — файл набора документов; файла нет — ошибка.
fn source_file(src: Sources, source: &str) -> Result<PathBuf> {
    let f = if source.starts_with(INSIGHT_REF) {
        src.insights.to_path_buf()
    } else if let Some(rel) = source.strip_prefix(DOC_REF) {
        src.docs.join(rel)
    } else {
        src.corpus.join(format!("{source}.md"))
    };
    if !f.is_file() {
        bail!("источник нагета {source}: нет файла {}", f.display());
    }
    Ok(f)
}

/// Отпечаток сценариев, нагетов и текстов источников нагетов; нет файла сценариев или
/// источника — ошибка.
pub fn nuggets_fingerprint(scenarios: &Path, src: Sources) -> Result<String> {
    let mut h = blake3::Hasher::new();
    h.update(b"zam-m6-nuggets-v1\0");
    let bytes = file_bytes(scenarios).with_context(|| format!("чтение {}", scenarios.display()))?;
    put(&mut h, "scenarios", Some(&bytes));
    let text = std::str::from_utf8(&bytes)
        .with_context(|| format!("{} — не UTF-8", scenarios.display()))?;
    let f: NuggetsOnly =
        toml::from_str(text).with_context(|| format!("разбор {}", scenarios.display()))?;
    let sources: BTreeSet<String> = f
        .scenario
        .into_iter()
        .flat_map(|s| s.nuggets.into_iter().map(|n| n.source))
        .collect();
    for s in &sources {
        let file = source_file(src, s)?;
        let b = file_bytes(&file).with_context(|| format!("чтение {}", file.display()))?;
        put(&mut h, s, Some(&b));
    }
    Ok(hex(h.finalize()))
}

/// Файлы каталога рекурсивно (`/` в относительных путях).
fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let rel = p
            .strip_prefix(root)
            .unwrap_or(&p)
            .to_string_lossy()
            .replace('\\', "/");
        if p.is_dir() {
            walk(root, &p, out);
        } else {
            out.push((rel, p));
        }
    }
}

/// Файлы каталога `dir` по порядку путей; `parts` — только эти имена верхнего уровня.
fn put_dir(h: &mut blake3::Hasher, label: &str, dir: &Path, parts: Option<&[&str]>) {
    if !dir.is_dir() {
        put(h, label, None);
        return;
    }
    put(h, label, Some(b"dir"));
    let mut files = Vec::new();
    walk(dir, dir, &mut files);
    if let Some(keep) = parts {
        files.retain(|(rel, _)| {
            keep.iter()
                .any(|k| rel == k || rel.strip_prefix(k).is_some_and(|r| r.starts_with('/')))
        });
    }
    files.sort();
    for (rel, p) in files {
        put(h, &rel, file_bytes(&p).as_deref());
    }
}

/// Отпечаток корпуса ТС-1: каталог заметок целиком, файл insight и части [`DOCS_PARTS`]
/// набора документов; отсутствующий путь входит пометкой.
pub fn corpus_fingerprint(src: Sources) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"zam-m6-corpus-v2\0");
    put_dir(&mut h, "corpus", src.corpus, None);
    put(&mut h, "insights", file_bytes(src.insights).as_deref());
    put_dir(&mut h, "docs", src.docs, Some(&DOCS_PARTS));
    hex(h.finalize())
}

/// sha256 `manifest.json` набора документов (байты файла, `\r\n` → `\n`); нет файла — `None`.
pub fn docs_manifest_sha256(docs: &Path) -> Option<String> {
    let b = file_bytes(&docs.join(DOCS_MANIFEST))?;
    Some(sha256_hex(&b))
}

/// sha256 байтов шестнадцатеричной строкой.
pub fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// Замок отпечатков `nuggets.lock`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    pub nuggets_fingerprint: String,
    pub corpus_fingerprint: String,
    /// sha256 `eval/docs/manifest.json` полного набора (`cargo xtask docs-corpus` с сидом по
    /// умолчанию, без `--large`).
    pub docs_manifest_sha256: String,
    /// Сценариев и нагетов набора (справочно: видно в диффе замка).
    pub scenarios: usize,
    pub nuggets: usize,
}

/// Путь замка для файла сценариев.
pub fn lock_path(scenarios: &Path) -> PathBuf {
    scenarios.parent().unwrap_or(Path::new(".")).join(LOCK_FILE)
}

/// Читает замок; нет файла — `Ok(None)` (до заморозки, задача 1b).
pub fn read_lock(lock: &Path) -> Result<Option<Lock>> {
    if !lock.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(lock).with_context(|| format!("чтение {}", lock.display()))?;
    let l: Lock = toml::from_str(&text).with_context(|| format!("разбор {}", lock.display()))?;
    Ok(Some(l))
}

/// Сверяет отпечатки с замком: замка нет — `Ok(None)`; расхождение — ошибка с обоими
/// значениями. `docs_manifest` — sha256 `manifest.json` набора документов на диске: если он
/// другой, причина названа — набор собран другим генератором или сидом.
pub fn check_lock(
    lock: &Path,
    nuggets: &str,
    corpus: &str,
    docs_manifest: Option<&str>,
    counts: (usize, usize),
) -> Result<Option<Lock>> {
    let Some(l) = read_lock(lock)? else {
        return Ok(None);
    };
    let mut errs = Vec::new();
    if (l.scenarios, l.nuggets) != counts {
        errs.push(format!(
            "сценариев и нагетов {} и {} ≠ замороженных {} и {}",
            counts.0, counts.1, l.scenarios, l.nuggets
        ));
    }
    if l.nuggets_fingerprint != nuggets {
        errs.push(format!(
            "nuggets_fingerprint {nuggets} ≠ замороженного {}",
            l.nuggets_fingerprint
        ));
    }
    if l.corpus_fingerprint != corpus {
        errs.push(format!(
            "corpus_fingerprint {corpus} ≠ замороженного {}",
            l.corpus_fingerprint
        ));
    }
    if docs_manifest != Some(l.docs_manifest_sha256.as_str()) {
        errs.push(format!(
            "eval/docs/manifest.json sha256 {} ≠ замороженного {} — набор документов собран \
             другим генератором документов или сидом (xtask/src/docs_corpus.rs, \
             CORPUS_VERSION): пересоздайте его — cargo xtask docs-corpus; если изменился сам \
             генератор, это правка корпуса после заморозки",
            docs_manifest.unwrap_or("<нет файла>"),
            l.docs_manifest_sha256
        ));
    }
    if !errs.is_empty() {
        bail!(
            "набор ТС-1 изменён после заморозки ({}): {}. Правка нагетов и корпуса — только при \
             доказанной ошибке разметки, с записью в журнал, новым замком и переснятием всех плеч \
             (спецификация M6 §3.2). eval/docs не в git: если каталог стёрт или собран другой \
             версией, пересоздайте его — cargo xtask docs-corpus",
            lock.display(),
            errs.join("; ")
        );
    }
    Ok(Some(l))
}

/// Текст для поиска нагета: строки без отступов и маркеров Markdown (заголовок, цитата,
/// пункт списка, выделение, код), пробельные символы — один пробел.
pub fn normalize_md(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let mut l = line.trim_start();
        loop {
            let before = l;
            l = l.trim_start_matches('>').trim_start();
            if let Some(r) = l.strip_prefix('#') {
                l = r.trim_start_matches('#');
                if l.starts_with(' ') || l.is_empty() {
                    l = l.trim_start();
                } else {
                    l = before;
                    break;
                }
            }
            for m in ["- ", "* ", "+ "] {
                if let Some(r) = l.strip_prefix(m) {
                    l = r.trim_start();
                }
            }
            let digits = l.chars().take_while(char::is_ascii_digit).count();
            if digits > 0
                && let Some(r) = l[digits..]
                    .strip_prefix(". ")
                    .or_else(|| l[digits..].strip_prefix(") "))
            {
                l = r.trim_start();
            }
            if l == before {
                break;
            }
        }
        out.push_str(l);
        out.push('\n');
    }
    let stripped: String = out
        .replace("**", "")
        .replace("__", "")
        .chars()
        .filter(|c| *c != '`')
        .collect();
    squash_ws(&stripped)
}

/// Нагет целиком содержится в выданном тексте (оба — после [`normalize_md`]).
pub fn found(delivered_norm: &str, nugget: &str) -> bool {
    let n = normalize_md(nugget);
    !n.is_empty() && delivered_norm.contains(&n)
}

/// Приближение L0 заметки сервером **без модели**: `l0`/`description` frontmatter, иначе первый
/// абзац текста (не заголовок, не код, не таблица, не список) без встроенной разметки, обрезанный
/// до 200 символов по границе слова. С моделью сервер заменяет авто-L0 предложением, ближайшим к
/// центроиду эмбеддингов (`zam-index::rollup`): его проверяет прогон ([`in_server_l0`]).
pub fn l0_of(text: &str) -> String {
    let (fm, body) = match text.strip_prefix("---\n") {
        Some(rest) => match rest.split_once("\n---") {
            Some((fm, b)) => (fm, b.split_once('\n').map_or("", |x| x.1)),
            None => ("", text),
        },
        None => ("", text),
    };
    for key in ["l0", "description"] {
        if let Some(v) = fm.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == key).then(|| v.trim().trim_matches('"').trim_matches('\'').to_string())
        }) && !v.is_empty()
        {
            return v;
        }
    }
    let mut para: Vec<&str> = Vec::new();
    let mut in_code = false;
    for l in body.lines() {
        let t = l.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_code = !in_code;
            if !para.is_empty() {
                break;
            }
            continue;
        }
        if in_code {
            continue;
        }
        let block = t.starts_with('#')
            || t.starts_with('|')
            || t.starts_with("- ")
            || t.starts_with("* ")
            || t.starts_with('>');
        if t.is_empty() || block {
            if !para.is_empty() {
                break;
            }
            continue;
        }
        para.push(t);
    }
    // сервер берёт текст абзаца без разметки (comrak `collect_text`)
    let plain = para
        .join(" ")
        .replace("**", "")
        .replace("__", "")
        .replace('`', "");
    let s = squash_ws(&plain);
    match s.char_indices().nth(L0_AUTO_MAX_CHARS) {
        None => s,
        Some((cut, _)) => {
            let head = &s[..cut];
            let head = if s[cut..].starts_with(' ') {
                head
            } else {
                head.rfind(' ').map_or(head, |i| &head[..i])
            };
            head.trim_end().to_string()
        }
    }
}

/// Доля основ слов нагета, общих с вопросом (слов нет — 0).
pub fn overlap(nugget: &str, question: &str) -> f64 {
    let stems =
        |t: &str| -> BTreeSet<String> { content_words(t).iter().map(|w| stem(w)).collect() };
    let n = stems(nugget);
    if n.is_empty() {
        return 0.0;
    }
    let q = stems(question);
    n.intersection(&q).count() as f64 / n.len() as f64
}

/// Сценарий с точки зрения проверки нагетов.
pub struct Check<'a> {
    pub id: &'a str,
    pub question: &'a str,
    pub expect: &'a [String],
    pub nuggets: &'a [Nugget],
}

/// Сводка проверки нагетов набора.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    pub scenarios_with_nuggets: usize,
    pub scenarios_without: usize,
    pub nuggets: usize,
    pub low_overlap: usize,
}

impl Summary {
    pub fn low_share(&self) -> f64 {
        if self.nuggets == 0 {
            0.0
        } else {
            self.low_overlap as f64 / self.nuggets as f64
        }
    }
}

/// Вид документа-источника по расширению: только разбираемые в процессе (без pdfium).
fn doc_kind(rel: &str) -> Option<zam_docs::DocKind> {
    match rel.rsplit('.').next() {
        Some("docx") => Some(zam_docs::DocKind::Docx),
        Some("pptx") => Some(zam_docs::DocKind::Pptx),
        Some("xlsx") => Some(zam_docs::DocKind::Xlsx),
        _ => None,
    }
}

/// Текст документа набора `eval/docs/<rel>` — единицы извлечения `zam-docs` через перевод строки.
pub fn doc_text(docs: &Path, rel: &str) -> Result<String> {
    let kind = doc_kind(rel)
        .with_context(|| format!("{DOC_REF}{rel}: источник-документ — только DOCX, PPTX, XLSX"))?;
    let path = docs.join(rel);
    let data = fs::read(&path).with_context(|| {
        format!(
            "{DOC_REF}{rel}: нет файла {} (cargo xtask docs-corpus)",
            path.display()
        )
    })?;
    let x = zam_docs::extract(
        kind,
        &data,
        &zam_docs::Limits::default(),
        Instant::now() + DOC_EXTRACT_LIMIT,
    )
    .map_err(|e| anyhow::anyhow!("{DOC_REF}{rel}: разбор: {e}"))?;
    Ok(x.units
        .iter()
        .map(|u| u.text.as_str())
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Текст источника: заметка, insight (заголовок и тело) или документ; второе — L0 (по
/// [`l0_of`], у insight — заголовок, у документа — пусто).
fn source_text(corpus: &Corpus, docs: &Path, source: &str) -> Result<Option<(String, String)>> {
    if let Some(k) = source.strip_prefix(INSIGHT_REF) {
        return Ok(corpus
            .insights
            .iter()
            .rev()
            .find(|i| i.key == k)
            .map(|i| (format!("{}\n{}", i.title, i.body), i.title.clone())));
    }
    if let Some(rel) = source.strip_prefix(DOC_REF) {
        return doc_text(docs, rel).map(|t| Some((t, String::new())));
    }
    Ok(corpus.get(source).map(|n| (n.text.clone(), l0_of(&n.text))))
}

/// Справочная проверка (`--check`): нагет встречается и в **других** источниках корпуса ТС-1 —
/// заметках, insight, документах `eval/docs` роли `content` (DOCX/PPTX/XLSX — `zam-docs`, PDF —
/// pdfium, если он есть). Такой нагет может «найтись» в выдаче чужого источника.
#[derive(Debug, Default)]
pub struct Foreign {
    /// `<сценарий>: «нагет» (<источник>) — ещё в <другой источник>`.
    pub hits: Vec<String>,
    /// Что не проверено (нет pdfium, документ не разобран).
    pub skipped: Vec<String>,
}

/// Тексты всех источников корпуса ТС-1 (после сведения пробелов): `(источник, текст)`.
fn all_source_texts(
    corpus: &Corpus,
    docs: &Path,
    repo: &Path,
    skipped: &mut Vec<String>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = corpus
        .notes
        .values()
        .map(|n| (n.path.clone(), squash_ws(&n.text)))
        .collect();
    out.extend(corpus.insights.iter().map(|i| {
        (
            format!("{INSIGHT_REF}{}", i.key),
            squash_ws(&format!("{}\n{}", i.title, i.body)),
        )
    }));
    let Ok(m) = crate::docs_corpus::Manifest::load(docs) else {
        skipped.push(format!(
            "{}: нет манифеста набора документов",
            docs.display()
        ));
        return out;
    };
    let pdfium = crate::eval::docs::extract::pdfium_lib(repo).is_file();
    for f in m.files.iter().filter(|f| f.role == "content") {
        if f.path.ends_with(".pdf") && !pdfium {
            skipped.push(format!(
                "{DOC_REF}{}: нет pdfium (cargo xtask fetch-deps)",
                f.path
            ));
            continue;
        }
        let Ok(bytes) = fs::read(docs.join(&f.path)) else {
            skipped.push(format!("{DOC_REF}{}: нет файла", f.path));
            continue;
        };
        let x = crate::eval::docs::extract::extract_here(
            repo,
            crate::eval::docs::extract::PdfBackend::Pdfium,
            &bytes,
            1,
        );
        if !x.ok {
            skipped.push(format!(
                "{DOC_REF}{}: {}",
                f.path,
                x.error.unwrap_or_default()
            ));
            continue;
        }
        let text: Vec<&str> = x.pages.iter().map(|(_, t)| t.as_str()).collect();
        out.push((format!("{DOC_REF}{}", f.path), squash_ws(&text.join("\n"))));
    }
    out
}

/// Нагеты набора, которые встречаются и в других источниках корпуса ([`Foreign`]).
pub fn foreign_occurrences(set: &[Check], corpus: &Corpus, docs: &Path, repo: &Path) -> Foreign {
    let mut f = Foreign::default();
    let texts = all_source_texts(corpus, docs, repo, &mut f.skipped);
    for c in set {
        for n in c.nuggets {
            let needle = squash_ws(&n.text);
            let others: Vec<&str> = texts
                .iter()
                .filter(|(src, t)| *src != n.source && t.contains(&needle))
                .map(|(src, _)| src.as_str())
                .collect();
            if !others.is_empty() {
                f.hits.push(format!(
                    "{}: «{}» ({}) — ещё в {}",
                    c.id,
                    n.text,
                    n.source,
                    others.join(", ")
                ));
            }
        }
    }
    f
}

/// Нагеты, которые входят в L0 источника, выданный сервером (сверка приближения [`l0_of`] с
/// сервером на реальном корпусе): после нормализации пробелов.
///
/// L0 сервера бывает двух видов: без модели — первый абзац без разметки (как [`l0_of`]), с
/// моделью — предложение, ближайшее к центроиду эмбеддингов (`zam-index::rollup`, с разметкой
/// исходника); поэтому обе стороны сравниваются после [`normalize_md`].
pub fn in_server_l0<'a>(nuggets: &[&'a Nugget], server_l0: &str) -> Vec<&'a Nugget> {
    let l0 = normalize_md(server_l0);
    nuggets
        .iter()
        .copied()
        .filter(|n| found(&l0, &n.text))
        .collect()
}

fn occurrences(hay: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut n = 0;
    let mut from = 0;
    while let Some(i) = hay[from..].find(needle) {
        n += 1;
        from += i + needle.len().max(1);
    }
    n
}

/// Проверки §3.2. Набор без единого нагета (до разметки, задача 1b) проходит со сводкой; если
/// нагеты есть хоть у одного сценария — размечены должны быть все.
pub fn validate(set: &[Check], corpus: &Corpus, docs: &Path) -> Result<Summary> {
    let mut texts: std::collections::BTreeMap<String, Option<(String, String)>> =
        std::collections::BTreeMap::new();
    let mut s = Summary::default();
    let mut errs = Vec::new();
    for c in set {
        if c.nuggets.is_empty() {
            s.scenarios_without += 1;
            continue;
        }
        s.scenarios_with_nuggets += 1;
        for e in c.expect {
            let k = c.nuggets.iter().filter(|n| &n.source == e).count();
            if !NUGGETS_PER_SOURCE.contains(&k) {
                errs.push(format!(
                    "{}: у источника {e} нагетов {k}, нужно {}–{}",
                    c.id,
                    NUGGETS_PER_SOURCE.start(),
                    NUGGETS_PER_SOURCE.end()
                ));
            }
        }
        for n in c.nuggets {
            s.nuggets += 1;
            let len = n.text.chars().count();
            if !NUGGET_CHARS.contains(&len) {
                errs.push(format!(
                    "{}: нагет «{}» — {len} символов, нужно {}–{}",
                    c.id,
                    n.text,
                    NUGGET_CHARS.start(),
                    NUGGET_CHARS.end()
                ));
            }
            if !c.expect.contains(&n.source) {
                errs.push(format!(
                    "{}: источник нагета {} не в expect",
                    c.id, n.source
                ));
                continue;
            }
            if !texts.contains_key(&n.source) {
                let t = match source_text(corpus, docs, &n.source) {
                    Ok(t) => t,
                    Err(e) => {
                        errs.push(format!("{}: {e:#}", c.id));
                        None
                    }
                };
                texts.insert(n.source.clone(), t);
            }
            let Some((text, l0)) = &texts[&n.source] else {
                errs.push(format!("{}: нет источника {}", c.id, n.source));
                continue;
            };
            let needle = squash_ws(&n.text);
            let k = occurrences(&squash_ws(text), &needle);
            if k != 1 {
                errs.push(format!(
                    "{}: нагет «{}» встречается в {} {k} раз, нужно ровно 1",
                    c.id, n.text, n.source
                ));
            }
            if !l0.is_empty() && squash_ws(l0).contains(&needle) {
                errs.push(format!(
                    "{}: нагет «{}» входит в L0 источника {}",
                    c.id, n.text, n.source
                ));
            }
            if overlap(&n.text, c.question) <= LOW_OVERLAP {
                s.low_overlap += 1;
            }
        }
    }
    if s.scenarios_with_nuggets > 0 {
        if s.scenarios_without > 0 {
            errs.push(format!(
                "нагетов нет у {} сценариев из {}",
                s.scenarios_without,
                set.len()
            ));
        }
        if s.low_share() < MIN_LOW_OVERLAP_SHARE {
            errs.push(format!(
                "нагетов с низким пересечением с вопросом (≤ {LOW_OVERLAP}) {:.0} % < {:.0} %",
                s.low_share() * 100.0,
                MIN_LOW_OVERLAP_SHARE * 100.0
            ));
        }
    }
    if !errs.is_empty() {
        bail!("нагеты сценариев:\n  {}", errs.join("\n  "));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_markdown_markers() {
        let md = "### n:x path  rev1 · ~10 tok\n  - **Флаг** `--hard`\n    выбрасывает правки\n> 1. пункт";
        let n = normalize_md(md);
        assert!(n.contains("Флаг --hard выбрасывает правки пункт"), "{n}");
        assert!(found(&n, "Флаг --hard выбрасывает"));
        assert!(!found(&n, "Флаг --soft"));
        // хеш-тег без пробела — не заголовок
        assert_eq!(normalize_md("#tag текст"), "#tag текст");
    }

    #[test]
    fn l0_is_frontmatter_or_first_paragraph() {
        assert_eq!(
            l0_of("# Заголовок\n\nПервый абзац\nпродолжение.\n\nВторой."),
            "Первый абзац продолжение."
        );
        assert_eq!(l0_of("---\nl0: Кратко\n---\n# З\n\nАбзац"), "Кратко");
        let long = format!("# З\n\n{}", "слово ".repeat(60));
        assert!(l0_of(&long).chars().count() <= L0_AUTO_MAX_CHARS);
    }

    #[test]
    fn overlap_counts_shared_stems() {
        assert_eq!(overlap("история коммитов", "истории коммита"), 1.0);
        assert_eq!(
            overlap("история коммитов, веток и тегов", "истории коммита"),
            0.5
        );
        assert_eq!(
            overlap("рабочий каталог очищается", "как отменить коммит"),
            0.0
        );
    }

    /// Свежий каталог теста в `target/test-tmp`.
    fn tmp(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/test-tmp/xtask")
            .join(name);
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn sources<'a>(c: &'a Path, ins: &'a Path, docs: &'a Path) -> Sources<'a> {
        Sources {
            corpus: c,
            insights: ins,
            docs,
        }
    }

    /// Синтетический корпус: заметки по путям.
    fn corpus_of(notes: &[(&str, &str)]) -> Corpus {
        let mut c = Corpus::default();
        for (path, text) in notes {
            c.notes.insert(
                path.to_string(),
                super::super::corpus::Note {
                    path: path.to_string(),
                    lang: super::super::corpus::Lang::Ru,
                    text: text.to_string(),
                },
            );
        }
        c
    }

    fn nug(source: &str, text: &str) -> Nugget {
        Nugget {
            source: source.into(),
            text: text.into(),
        }
    }

    fn check<'a>(question: &'a str, expect: &'a [String], nuggets: &'a [Nugget]) -> Vec<Check<'a>> {
        vec![Check {
            id: "t-01",
            question,
            expect,
            nuggets,
        }]
    }

    const NOTE: &str = "# Сброс\n\nКороткая сводка о сбросе рабочей копии.\n\n## Подробно\n\n\
                        Флаг --hard выбрасывает незакоммиченные правки\nрабочего каталога \
                        безвозвратно.\n\nВторой абзац: reflog хранит прежние положения HEAD \
                        девяносто дней.\n";

    #[test]
    fn nugget_occurs_once() {
        let c = corpus_of(&[("ru/git/reset", NOTE)]);
        let docs = tmp("nuggets-once");
        let expect = vec!["ru/git/reset".to_string()];
        // перевод строки внутри фрагмента в источнике — как один пробел
        let ok = [nug(
            "ru/git/reset",
            "Флаг --hard выбрасывает незакоммиченные правки рабочего каталога",
        )];
        validate(&check("как откатить", &expect, &ok), &c, &docs).unwrap();
        // фрагмента нет в источнике
        let none = [nug(
            "ru/git/reset",
            "Флаг --soft оставляет правки в индексе целиком",
        )];
        let e = validate(&check("как откатить", &expect, &none), &c, &docs)
            .unwrap_err()
            .to_string();
        assert!(e.contains("0 раз, нужно ровно 1"), "{e}");
        // встречается дважды
        let twice = NOTE.replace(
            "Второй абзац",
            "Флаг --hard выбрасывает незакоммиченные правки",
        );
        let c2 = corpus_of(&[("ru/git/reset", &twice)]);
        let dup = [nug(
            "ru/git/reset",
            "Флаг --hard выбрасывает незакоммиченные правки",
        )];
        let e = validate(&check("как откатить", &expect, &dup), &c2, &docs)
            .unwrap_err()
            .to_string();
        assert!(e.contains("2 раз, нужно ровно 1"), "{e}");
    }

    #[test]
    fn nugget_not_in_l0() {
        let c = corpus_of(&[("ru/git/reset", NOTE)]);
        let docs = tmp("nuggets-l0");
        let expect = vec!["ru/git/reset".to_string()];
        let l0 = [nug(
            "ru/git/reset",
            "Короткая сводка о сбросе рабочей копии",
        )];
        let e = validate(&check("как откатить", &expect, &l0), &c, &docs)
            .unwrap_err()
            .to_string();
        assert!(e.contains("входит в L0"), "{e}");
        // L0 сервера — та же проверка по тексту сервера
        let n = [
            nug("ru/git/reset", "Короткая сводка о сбросе"),
            nug("ru/git/reset", "reflog хранит"),
        ];
        let refs: Vec<&Nugget> = n.iter().collect();
        let hit = in_server_l0(&refs, "  Короткая   сводка о сбросе рабочей копии.");
        assert_eq!(hit, vec![&n[0]]);
        // L0 модели — предложение исходника с разметкой: сравнение после снятия разметки
        let m = [nug("ru/git/reset", "Флаг --hard выбрасывает правки")];
        let refs: Vec<&Nugget> = m.iter().collect();
        assert_eq!(
            in_server_l0(&refs, "Флаг `--hard` выбрасывает правки.").len(),
            1
        );
        // авто-L0 без модели — текст абзаца без встроенной разметки
        assert_eq!(
            l0_of(
                "# З

Флаг `--hard` и **жирный**.
"
            ),
            "Флаг --hard и жирный."
        );
    }

    #[test]
    fn every_expect_has_nugget() {
        let c = corpus_of(&[("ru/git/reset", NOTE), ("ru/git/revert", NOTE)]);
        let docs = tmp("nuggets-expect");
        let expect = vec!["ru/git/reset".to_string(), "ru/git/revert".to_string()];
        let only_one = [nug(
            "ru/git/reset",
            "reflog хранит прежние положения HEAD девяносто дней",
        )];
        let e = validate(&check("как откатить", &expect, &only_one), &c, &docs)
            .unwrap_err()
            .to_string();
        assert!(e.contains("у источника ru/git/revert нагетов 0"), "{e}");
        // больше четырёх на источник — тоже ошибка
        let five: Vec<Nugget> = (0..5)
            .map(|_| nug("ru/git/reset", "reflog хранит прежние положения HEAD"))
            .collect();
        let e = validate(&check("как откатить", &expect[..1], &five), &c, &docs)
            .unwrap_err()
            .to_string();
        assert!(e.contains("нагетов 5, нужно 1–4"), "{e}");
    }

    #[test]
    fn low_overlap_share_at_least_40pct() {
        let c = corpus_of(&[("ru/git/reset", NOTE)]);
        let docs = tmp("nuggets-overlap");
        let expect = vec!["ru/git/reset".to_string()];
        let q = "флаг hard выбрасывает незакоммиченные правки";
        let high1 = nug(
            "ru/git/reset",
            "Флаг --hard выбрасывает незакоммиченные правки",
        );
        let high2 = nug(
            "ru/git/reset",
            "выбрасывает незакоммиченные правки рабочего",
        );
        let low1 = nug(
            "ru/git/reset",
            "reflog хранит прежние положения HEAD девяносто дней",
        );
        let low2 = nug("ru/git/reset", "рабочего каталога безвозвратно");
        assert!(overlap(&high1.text, q) > LOW_OVERLAP && overlap(&high2.text, q) > LOW_OVERLAP);
        assert!(overlap(&low1.text, q) <= LOW_OVERLAP && overlap(&low2.text, q) <= LOW_OVERLAP);
        // низкое пересечение у 1 из 3 (33 %) — ошибка
        let three = [high1.clone(), high2.clone(), low1.clone()];
        let e = validate(&check(q, &expect, &three), &c, &docs)
            .unwrap_err()
            .to_string();
        assert!(e.contains("33 % < 40 %"), "{e}");
        // 2 из 4 (50 %) — проходит
        let four = [high1, high2, low1, low2];
        let s = validate(&check(q, &expect, &four), &c, &docs).unwrap();
        assert_eq!((s.nuggets, s.low_overlap), (4, 2));
    }

    #[test]
    fn doc_source_nuggets_checked_in_extracted_text() {
        let d = tmp("nuggets-doc");
        let p = crate::docs_corpus::Profile {
            small: true,
            large: false,
        };
        let m = crate::docs_corpus::write(&d, 7, p).unwrap();
        let fact = m
            .facts
            .iter()
            .find(|f| {
                f.file.ends_with(".pptx") && NUGGET_CHARS.contains(&f.evidence.chars().count())
            })
            .unwrap();
        let src = format!("{DOC_REF}{}", fact.file);
        let expect = vec![src.clone()];
        let c = Corpus::default();
        let ok = [nug(&src, &fact.evidence)];
        let s = validate(&check("что на слайде", &expect, &ok), &c, &d).unwrap();
        assert_eq!(s.nuggets, 1);
        let bad = [nug(&src, "этого фрагмента нет в презентации набора")];
        let e = validate(&check("что на слайде", &expect, &bad), &c, &d)
            .unwrap_err()
            .to_string();
        assert!(e.contains("0 раз"), "{e}");
        // PDF источником нагета быть не может (текст — только pdfium сервера)
        let pdf = format!("{DOC_REF}files/onecol-en.pdf");
        let e = validate(
            &check(
                "что в pdf",
                std::slice::from_ref(&pdf),
                &[nug(&pdf, "seal replaced after leak и ещё")],
            ),
            &c,
            &d,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("только DOCX, PPTX, XLSX"), "{e}");
        // нет файла — ошибка
        let gone = format!("{DOC_REF}files/nope.docx");
        let e = validate(
            &check(
                "что в файле",
                std::slice::from_ref(&gone),
                &[nug(&gone, "двадцать символов тут есть")],
            ),
            &c,
            &d,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("нет файла"), "{e}");
    }

    #[test]
    fn nugget_in_other_source_is_reported() {
        let other = "# Другая\n\nСводка.\n\nТам тоже: reflog хранит прежние положения HEAD девяносто дней.\n";
        let c = corpus_of(&[("ru/git/reset", NOTE), ("ru/git/other", other)]);
        let docs = tmp("nuggets-foreign");
        let expect = vec!["ru/git/reset".to_string()];
        let n = [
            nug(
                "ru/git/reset",
                "reflog хранит прежние положения HEAD девяносто дней",
            ),
            nug(
                "ru/git/reset",
                "Флаг --hard выбрасывает незакоммиченные правки",
            ),
        ];
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let f = foreign_occurrences(&check("q", &expect, &n), &c, &docs, &repo);
        assert_eq!(f.hits.len(), 1, "{:?}", f.hits);
        assert!(f.hits[0].contains("ещё в ru/git/other"), "{:?}", f.hits);
        // нет набора документов — пометка, а не ошибка
        assert!(
            f.skipped.iter().any(|s| s.contains("нет манифеста")),
            "{:?}",
            f.skipped
        );
    }

    #[test]
    fn fingerprints_follow_content() {
        let d = tmp("nuggets-fp");
        let c = d.join("corpus");
        fs::create_dir_all(c.join("ru")).unwrap();
        fs::write(c.join("ru/a.md"), "текст a\r\n").unwrap();
        fs::write(c.join("ru/b.md"), "текст b\n").unwrap();
        let docs = d.join("docs");
        fs::create_dir_all(docs.join("files")).unwrap();
        fs::write(docs.join("files/x.docx"), b"PK\x03\x04docx").unwrap();
        fs::write(docs.join(DOCS_MANIFEST), "{\"version\": 3}\n").unwrap();
        let sc = d.join("scenarios.toml");
        fs::write(
            &sc,
            "[[scenario]]\nid = \"x\"\nnuggets = [{ source = \"ru/a\", text = \"текст a\" }, \
             { source = \"doc:files/x.docx\", text = \"t\" }]\n",
        )
        .unwrap();
        let ins = d.join("insights.toml");
        let src = sources(&c, &ins, &docs);
        let n1 = nuggets_fingerprint(&sc, src).unwrap();
        let c1 = corpus_fingerprint(src);
        assert_eq!(n1.len(), 32);
        // CRLF не меняет отпечаток
        fs::write(c.join("ru/a.md"), "текст a\n").unwrap();
        assert_eq!(nuggets_fingerprint(&sc, src).unwrap(), n1);
        assert_eq!(corpus_fingerprint(src), c1);
        // заметка без нагетов меняет только отпечаток корпуса
        fs::write(c.join("ru/b.md"), "текст b2\n").unwrap();
        assert_eq!(nuggets_fingerprint(&sc, src).unwrap(), n1);
        let c2 = corpus_fingerprint(src);
        assert_ne!(c2, c1);
        // источник нагета — оба
        fs::write(c.join("ru/a.md"), "текст a2\n").unwrap();
        assert_ne!(nuggets_fingerprint(&sc, src).unwrap(), n1);
        let c3 = corpus_fingerprint(src);
        // документ-источник — оба
        let n3 = nuggets_fingerprint(&sc, src).unwrap();
        fs::write(docs.join("files/x.docx"), b"PK\x03\x04docx2").unwrap();
        assert_ne!(nuggets_fingerprint(&sc, src).unwrap(), n3);
        let c4 = corpus_fingerprint(src);
        assert_ne!(c4, c3);
        // посторонние файлы в корне eval/docs и large/ отпечаток не меняют; hostile/ — меняет
        fs::write(docs.join("README.txt"), "заметки").unwrap();
        fs::create_dir_all(docs.join("large")).unwrap();
        fs::write(docs.join("large/big.docx"), b"big").unwrap();
        fs::create_dir_all(docs.join("filesx")).unwrap();
        fs::write(docs.join("filesx/y"), b"y").unwrap();
        assert_eq!(corpus_fingerprint(src), c4);
        fs::create_dir_all(docs.join("hostile")).unwrap();
        fs::write(docs.join("hostile/z.pdf"), b"z").unwrap();
        assert_ne!(corpus_fingerprint(src), c4);
        // нет файла источника (заметки, insight, документа) — ошибка, а не пометка в отпечатке
        let sc2 = d.join("scenarios2.toml");
        for s in ["ru/zzz", "insight:k1", "doc:files/nope.pptx"] {
            let toml = format!(
                "[[scenario]]\nid = \"x\"\nnuggets = [{{ source = \"{s}\", text = \"t\" }}]\n"
            );
            fs::write(&sc2, toml).unwrap();
            let e = nuggets_fingerprint(&sc2, src).unwrap_err().to_string();
            assert!(e.contains(s), "{e}");
        }
    }

    #[test]
    fn lock_names_docs_generator() {
        let d = tmp("nuggets-lock");
        let lock = d.join(LOCK_FILE);
        assert!(
            check_lock(&lock, "a", "b", Some("m"), (1, 2))
                .unwrap()
                .is_none()
        );
        fs::write(
            &lock,
            "nuggets_fingerprint = \"a\"\ncorpus_fingerprint = \"b\"\n\
             docs_manifest_sha256 = \"m\"\nscenarios = 1\nnuggets = 2\n",
        )
        .unwrap();
        let l = check_lock(&lock, "a", "b", Some("m"), (1, 2))
            .unwrap()
            .unwrap();
        assert_eq!((l.scenarios, l.nuggets), (1, 2));
        // число сценариев и нагетов сверяется
        let e = check_lock(&lock, "a", "b", Some("m"), (1, 3))
            .unwrap_err()
            .to_string();
        assert!(e.contains("1 и 3 ≠ замороженных 1 и 2"), "{e}");
        let e = check_lock(&lock, "a", "z", Some("m"), (1, 2))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("corpus_fingerprint z") && !e.contains("nuggets_fingerprint a"),
            "{e}"
        );
        assert!(!e.contains("генератором"), "{e}");
        // другой манифест набора документов — причина названа
        let e = check_lock(&lock, "a", "z", Some("m2"), (1, 2))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("другим генератором документов") && e.contains("m2"),
            "{e}"
        );
        let e = check_lock(&lock, "a", "b", None, (1, 2))
            .unwrap_err()
            .to_string();
        assert!(e.contains("<нет файла>"), "{e}");
        // старый формат замка (без полей задачи 1b) — ошибка разбора
        fs::write(
            &lock,
            "nuggets_fingerprint = \"a\"\ncorpus_fingerprint = \"b\"\n",
        )
        .unwrap();
        assert!(check_lock(&lock, "a", "b", Some("m"), (1, 2)).is_err());
    }

    /// Замороженный sha256 манифеста — тот, что даёт генератор документов сейчас (полный набор,
    /// сид по умолчанию): правка генератора после заморозки падает здесь с названной причиной,
    /// без прогона. Замка ещё нет (до заморозки) — проверять нечего.
    #[test]
    fn docs_generator_matches_frozen_manifest() {
        let lock = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../eval")
            .join(LOCK_FILE);
        let Some(l) = read_lock(&lock).unwrap() else {
            eprintln!("{} нет — набор не заморожен", lock.display());
            return;
        };
        let m = crate::docs_corpus::manifest_in_memory(
            crate::docs_corpus::DEFAULT_SEED,
            crate::docs_corpus::Profile {
                small: false,
                large: false,
            },
        );
        let bytes = serde_json::to_string_pretty(&m).unwrap() + "\n";
        assert_eq!(
            sha256_hex(bytes.as_bytes()),
            l.docs_manifest_sha256,
            "генератор документов (xtask/src/docs_corpus.rs) даёт другой manifest.json, чем при \
             заморозке ТС-1: правка корпуса после заморозки — только с записью в журнал, новым \
             замком и переснятием всех плеч (спецификация M6 §3.2)"
        );
    }
}
