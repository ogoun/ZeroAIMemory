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
//! - [`corpus_fingerprint`] — всё, что грузит ТС-1: `eval/corpus`, `eval/docs` (без `large/`) и
//!   `eval/insights.toml`.
//!
//! Отпечаток — blake3 (32 шестнадцатеричных знака); `\r\n` текстовых файлов сводится к `\n`,
//! чтобы отпечаток не зависел от `core.autocrlf`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

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

/// Файл источника нагета: заметка `<corpus>/<путь>.md`, `insight:` — файл insight набора
/// (`--insights`); файла нет — ошибка (источник документа появится вместе со сценариями на
/// вложения, задача 1b: пока источник — только заметка или insight).
fn source_file(corpus: &Path, insights: &Path, source: &str) -> Result<PathBuf> {
    let f = if source.starts_with(INSIGHT_REF) {
        insights.to_path_buf()
    } else {
        corpus.join(format!("{source}.md"))
    };
    if !f.is_file() {
        bail!("источник нагета {source}: нет файла {}", f.display());
    }
    Ok(f)
}

/// Отпечаток сценариев, нагетов и текстов источников нагетов; нет файла сценариев или
/// источника — ошибка.
pub fn nuggets_fingerprint(scenarios: &Path, corpus: &Path, insights: &Path) -> Result<String> {
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
        let file = source_file(corpus, insights, s)?;
        let b = file_bytes(&file).with_context(|| format!("чтение {}", file.display()))?;
        put(&mut h, s, Some(&b));
    }
    Ok(hex(h.finalize()))
}

/// Файлы каталога рекурсивно (`/` в относительных путях), по порядку путей; `large/` верхнего
/// уровня (большие файлы `docs-corpus --large`) пропускается.
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
            if dir == root && rel == "large" {
                continue;
            }
            walk(root, &p, out);
        } else {
            out.push((rel, p));
        }
    }
}

/// Отпечаток корпуса ТС-1: каталоги (рекурсивно) и отдельные файлы в порядке аргументов;
/// отсутствующий путь входит пометкой.
pub fn corpus_fingerprint(dirs: &[&Path]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"zam-m6-corpus-v1\0");
    for (i, d) in dirs.iter().enumerate() {
        let label = format!("#{i}");
        if d.is_file() {
            put(&mut h, &label, file_bytes(d).as_deref());
            continue;
        }
        if !d.is_dir() {
            put(&mut h, &label, None);
            continue;
        }
        put(&mut h, &label, Some(b"dir"));
        let mut files = Vec::new();
        walk(d, d, &mut files);
        files.sort();
        for (rel, p) in files {
            put(&mut h, &rel, file_bytes(&p).as_deref());
        }
    }
    hex(h.finalize())
}

/// Замок отпечатков `nuggets.lock`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    pub nuggets_fingerprint: String,
    pub corpus_fingerprint: String,
}

/// Путь замка для файла сценариев.
pub fn lock_path(scenarios: &Path) -> PathBuf {
    scenarios.parent().unwrap_or(Path::new(".")).join(LOCK_FILE)
}

/// Сверяет отпечатки с замком: замка нет — `Ok(None)` (до заморозки, задача 1b); расхождение —
/// ошибка с обоими значениями.
pub fn check_lock(lock: &Path, nuggets: &str, corpus: &str) -> Result<Option<Lock>> {
    if !lock.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(lock).with_context(|| format!("чтение {}", lock.display()))?;
    let l: Lock = toml::from_str(&text).with_context(|| format!("разбор {}", lock.display()))?;
    let mut errs = Vec::new();
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

/// Приближение L0 заметки сервером: `l0`/`description` frontmatter, иначе первый абзац текста
/// (не заголовок, не код, не таблица, не список), обрезанный до 200 символов по границе слова.
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
    let s = squash_ws(&para.join(" "));
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

/// Текст источника: заметка или insight (заголовок и тело); L0 — по [`l0_of`] или заголовок.
fn source_text(corpus: &Corpus, source: &str) -> Option<(String, String)> {
    match source.strip_prefix(INSIGHT_REF) {
        Some(k) => corpus
            .insights
            .iter()
            .rev()
            .find(|i| i.key == k)
            .map(|i| (format!("{}\n{}", i.title, i.body), i.title.clone())),
        None => corpus.get(source).map(|n| (n.text.clone(), l0_of(&n.text))),
    }
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
pub fn validate(set: &[Check], corpus: &Corpus) -> Result<Summary> {
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
            let Some((text, l0)) = source_text(corpus, &n.source) else {
                errs.push(format!("{}: нет источника {}", c.id, n.source));
                continue;
            };
            let needle = squash_ws(&n.text);
            let k = occurrences(&squash_ws(&text), &needle);
            if k != 1 {
                errs.push(format!(
                    "{}: нагет «{}» встречается в {} {k} раз, нужно ровно 1",
                    c.id, n.text, n.source
                ));
            }
            if squash_ws(&l0).contains(&needle) {
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

    #[test]
    fn fingerprints_follow_content() {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/xtask/nuggets-fp");
        let _ = fs::remove_dir_all(&d);
        let c = d.join("corpus");
        fs::create_dir_all(c.join("ru")).unwrap();
        fs::write(c.join("ru/a.md"), "текст a\r\n").unwrap();
        fs::write(c.join("ru/b.md"), "текст b\n").unwrap();
        let sc = d.join("scenarios.toml");
        fs::write(
            &sc,
            "[[scenario]]\nid = \"x\"\nnuggets = [{ source = \"ru/a\", text = \"текст a\" }]\n",
        )
        .unwrap();
        let ins = d.join("insights.toml");
        let n1 = nuggets_fingerprint(&sc, &c, &ins).unwrap();
        let c1 = corpus_fingerprint(&[&c]);
        assert_eq!(n1.len(), 32);
        // CRLF не меняет отпечаток
        fs::write(c.join("ru/a.md"), "текст a\n").unwrap();
        assert_eq!(nuggets_fingerprint(&sc, &c, &ins).unwrap(), n1);
        assert_eq!(corpus_fingerprint(&[&c]), c1);
        // заметка без нагетов меняет только отпечаток корпуса
        fs::write(c.join("ru/b.md"), "текст b2\n").unwrap();
        assert_eq!(nuggets_fingerprint(&sc, &c, &ins).unwrap(), n1);
        assert_ne!(corpus_fingerprint(&[&c]), c1);
        // источник нагета — оба
        fs::write(c.join("ru/a.md"), "текст a2\n").unwrap();
        assert_ne!(nuggets_fingerprint(&sc, &c, &ins).unwrap(), n1);
        // нет файла источника (заметки или insight) — ошибка, а не пометка в отпечатке
        let sc2 = d.join("scenarios2.toml");
        for src in ["ru/zzz", "insight:k1"] {
            let toml = format!(
                "[[scenario]]\nid = \"x\"\nnuggets = [{{ source = \"{src}\", text = \"t\" }}]\n"
            );
            fs::write(&sc2, toml).unwrap();
            let e = nuggets_fingerprint(&sc2, &c, &ins).unwrap_err().to_string();
            assert!(e.contains(src), "{e}");
        }
        // замок
        let lock = lock_path(&sc);
        assert!(check_lock(&lock, "a", "b").unwrap().is_none());
        fs::write(
            &lock,
            "nuggets_fingerprint = \"a\"\ncorpus_fingerprint = \"b\"\n",
        )
        .unwrap();
        assert!(check_lock(&lock, "a", "b").unwrap().is_some());
        let e = check_lock(&lock, "a", "z").unwrap_err().to_string();
        assert!(
            e.contains("corpus_fingerprint z") && !e.contains("nuggets_fingerprint a"),
            "{e}"
        );
    }
}
