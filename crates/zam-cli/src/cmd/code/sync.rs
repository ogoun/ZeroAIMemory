//! Синхронизация карты кода (спецификация M5.6 §5, §13; решения составителя плана 6–9):
//! отпечатки → хэши (из `code-state.json` при том же отпечатке, иначе blake3) →
//! `POST /code/sync` → разбор `need` по одному в одном потоке под журналом падений →
//! `PUT /code/files` пачками (≤ 500 файлов и ≤ 8 МиБ JSON) → `code-state.json` и
//! `code-sync.json`.
//!
//! - **Журнал падений** `cache/code-parsing.json` ([`CrashLog`]): `{path, hash}` пишется перед
//!   разбором файла без fsync (обрыв процесса запись сохраняет) и удаляется после. Незакрытая
//!   запись при следующем запуске сначала атомарно дописывается в `crashed` `code-state.json`
//!   и только потом снимается (ревью задачи 5, I1: иначе второй обрыв подряд терял первую
//!   запись); файл из `crashed` уходит `skipped`/`parser_crash`, пока его хэш тот же.
//! - **Замок** `cache/code-sync.lock` ([`SyncLock`]) — блокировка файла ОС
//!   (`File::try_lock`): занят — вторая синхронизация не идёт; ОС снимает её при любом
//!   завершении процесса (обрыв тоже), поэтому брошенных замков нет. Файл остаётся.
//! - **Предохранитель** (ревью задачи 5, I3): манифест меньше половины файлов прошлого
//!   состояния (если их ≥ [`PRUNE_GUARD_MIN`]) не отправляется без `--prune` — сервер удалил
//!   бы из карты всё недостающее. Нет локального состояния этого решения (вторая машина, кэш
//!   удалён) — сверка с числом файлов карты на сервере (`GET /code/status`, ревью задачи 5).
//! - На сервер — только метаданные §4: без `code.send_signatures` поле `sig` не отправляется;
//!   секреты сигнатуры клиент маскирует до обрезки ([`mask_sig`]); файл, чей JSON больше
//!   предела запроса, уходит `skipped`/`limit` без символов.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zam_code::{FileUpload, Limits, ParseState, SkipReason};

use super::config::CodeConfig;
use super::scan::{self, Fingerprint};
use super::state::{CodeState, FileState, SyncMark, cache_dir};
use crate::client::{CliError, read_json};
use crate::cmd::hook::cache;
use crate::cmd::{Ctx, refs};

/// Предел тела `PUT /code/files`, байт (сервер: `code.max_request_bytes` 8 МиБ).
pub const BATCH_BYTES_MAX: usize = 8 * 1024 * 1024;
/// Запас на конверт запроса (`solution`, `commit`, `branch`, `machine`, скобки).
const ENVELOPE_RESERVE: usize = 4096;
/// Предохранитель массового удаления действует от этого числа файлов прошлого состояния.
pub const PRUNE_GUARD_MIN: usize = 20;
/// Пределы символов и ссылок файла (как умолчания сервера `[code]`).
const MAX_SYMBOLS: usize = 5000;
const MAX_REFS: usize = 20_000;

/// Итог синхронизации.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SyncReport {
    /// Файлов в манифесте.
    pub files: usize,
    /// Путей в `need` (с `--full` — все).
    pub need: usize,
    /// Принято сервером.
    pub uploaded: usize,
    /// Удалено из карты (нет в манифесте).
    pub removed: u64,
    /// Причина пропуска или неполного разбора → число файлов среди разобранных.
    pub skipped: BTreeMap<String, usize>,
    /// Ссылок памяти признано устаревшими.
    pub stale_refs: u32,
    pub ms: u128,
    /// Предупреждения сервера и клиента (stderr; в фоне — в строке итога `hooks.log`).
    pub warnings: Vec<String>,
}

impl SyncReport {
    /// Одна строка итога (stdout).
    pub fn line(&self) -> String {
        let mut s = format!(
            "карта кода: файлов {}, к загрузке {}, загружено {}, удалено {}",
            self.files, self.need, self.uploaded, self.removed
        );
        if !self.skipped.is_empty() {
            let parts: Vec<String> = self
                .skipped
                .iter()
                .map(|(r, n)| format!("{r} {n}"))
                .collect();
            s.push_str(&format!(", пропущено: {}", parts.join(", ")));
        }
        if self.stale_refs > 0 {
            s.push_str(&format!(", устаревших ссылок {}", self.stale_refs));
        }
        s.push_str(&format!(" ({} мс)", self.ms));
        s
    }

    /// Строка итога с предупреждениями (фоновый режим: stderr не пишется).
    pub fn line_with_warnings(&self) -> String {
        let mut s = self.line();
        if !self.warnings.is_empty() {
            s.push_str(&format!("; предупреждения: {}", self.warnings.join("; ")));
        }
        s
    }
}

/// Запись журнала падений.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CrashEntry {
    path: String,
    hash: String,
}

/// Журнал падений разбора `cache/code-parsing.json`.
pub struct CrashLog {
    path: PathBuf,
}

impl CrashLog {
    pub fn new(root: &Path) -> CrashLog {
        CrashLog {
            path: cache_dir(root).join("code-parsing.json"),
        }
    }

    /// Перед разбором: перезапись малого файла без fsync (сбой записи разбор не останавливает).
    pub fn begin(&self, rel: &str, hash: &str) {
        let e = CrashEntry {
            path: rel.to_string(),
            hash: hash.to_string(),
        };
        if let Ok(bytes) = serde_json::to_vec(&e)
            && let Ok(mut f) = cache::private_options().open(&self.path)
        {
            let _ = f.write_all(&bytes);
        }
    }

    /// После разбора: запись снимается.
    pub fn end(&self) {
        let _ = std::fs::remove_file(&self.path);
    }

    /// Незакрытая запись прошлого запуска; не снимается — снять ([`Self::end`]) после того, как
    /// она сохранена в `code-state.json`. Битая запись — `None`.
    pub fn unfinished(&self) -> Option<(String, String)> {
        let bytes = std::fs::read(&self.path).ok()?;
        let e: CrashEntry = serde_json::from_slice(&bytes).ok()?;
        Some((e.path, e.hash))
    }
}

/// Незакрытая запись журнала → `crashed` состояния, состояние — на диск, затем запись
/// снимается (обрыв между шагами запись не теряет: она ещё в журнале).
fn absorb_crash(root: &Path, crash: &CrashLog, state: &mut CodeState) -> Result<(), CliError> {
    if let Some(c) = crash.unfinished()
        && !state.crashed.contains(&c)
    {
        state.crashed.push(c);
        state
            .save(root)
            .map_err(|e| CliError::config(format!("кэш карты кода: {e}")))?;
    }
    crash.end();
    Ok(())
}

/// Замок синхронизации проекта: блокировка файла ОС; снимается при `Drop` (и ОС — при
/// завершении процесса).
#[derive(Debug)]
pub struct SyncLock {
    _file: std::fs::File,
}

impl SyncLock {
    /// Захват: `None` — идёт другая синхронизация.
    pub fn acquire(root: &Path) -> Result<Option<SyncLock>, CliError> {
        let dir = cache_dir(root);
        let path = dir.join("code-sync.lock");
        std::fs::create_dir_all(&dir)
            .map_err(|e| CliError::config(format!("{}: {e}", dir.display())))?;
        let mut o = std::fs::OpenOptions::new();
        o.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let file = o
            .open(&path)
            .map_err(|e| CliError::config(format!("замок {}: {e}", path.display())))?;
        match file.try_lock() {
            Ok(()) => {
                // `{pid, at}` — для хука (задача 6) и человека; замок держит ОС, не содержимое
                let at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                let mut f = &file;
                let _ = f.set_len(0);
                let _ = f.write_all(
                    json!({"pid": std::process::id(), "at": at})
                        .to_string()
                        .as_bytes(),
                );
                Ok(Some(SyncLock { _file: file }))
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => {
                Err(CliError::config(format!("замок {}: {e}", path.display())))
            }
        }
    }
}

/// Тестовый крючок debug-сборки (решение составителя плана 16): обрыв процесса после записи
/// журнала падений на файле `ZAM_DEBUG_CODE_ABORT`.
#[cfg(debug_assertions)]
fn debug_abort(rel: &str) {
    if std::env::var("ZAM_DEBUG_CODE_ABORT").is_ok_and(|p| p == rel) {
        std::process::abort();
    }
}

#[cfg(not(debug_assertions))]
fn debug_abort(_rel: &str) {}

/// Хэш файла: из состояния при том же отпечатке, иначе blake3 содержимого.
fn hash_of(root: &Path, fp: &Fingerprint, state: &CodeState) -> Option<String> {
    if let Some(s) = state.files.get(&fp.path)
        && s.fp == fp.fp
        && s.size == fp.size
    {
        return Some(s.hash.clone());
    }
    refs::hash_file(&root.join(&fp.path)).ok()
}

/// `git rev-parse <args>` одной строкой (без необязательных замков); не git — `None`.
fn rev_parse(root: &Path, args: &[&str]) -> Option<String> {
    if !refs::in_git(root) {
        return None;
    }
    let mut a = vec!["rev-parse"];
    a.extend_from_slice(args);
    let out = scan::git_run(root, &a).ok()?;
    let s = String::from_utf8(out).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Коммит `HEAD` (hex) или `None`.
fn git_head(root: &Path) -> Option<String> {
    let s = rev_parse(root, &["HEAD"])?.to_ascii_lowercase();
    ((4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())).then_some(s)
}

/// Ветка git (`None` — не git или отсоединённый `HEAD`).
fn git_branch(root: &Path) -> Option<String> {
    let b = rev_parse(root, &["--abbrev-ref", "HEAD"])?;
    (b != "HEAD" && b.chars().count() <= 256).then_some(b)
}

/// Снимок: коммит, ветка, машина.
struct Meta {
    commit: Option<String>,
    branch: Option<String>,
    machine: Option<String>,
}

impl Meta {
    fn put_into(&self, body: &mut serde_json::Map<String, Value>) {
        for (k, v) in [
            ("commit", &self.commit),
            ("branch", &self.branch),
            ("machine", &self.machine),
        ] {
            if let Some(v) = v {
                body.insert(k.into(), json!(v));
            }
        }
    }
}

/// Разбор одного файла под журналом падений → загрузка.
fn parse_file(
    root: &Path,
    fp: &Fingerprint,
    hash: &str,
    state: &CodeState,
    cfg: &CodeConfig,
    crash: &CrashLog,
) -> Option<FileUpload> {
    let lang = zam_code::lang_of(&fp.path)?;
    let mut up = FileUpload {
        path: fp.path.clone(),
        lang,
        hash: hash.to_string(),
        git_oid: fp.git_oid.clone(),
        size: fp.size,
        parse_state: ParseState::Skipped,
        skip_reason: None,
        symbols: vec![],
        refs: vec![],
    };
    if state.is_crashed(&fp.path, hash) {
        up.skip_reason = Some(SkipReason::ParserCrash);
        return Some(up);
    }
    if fp.size > cfg.max_file_bytes {
        up.skip_reason = Some(SkipReason::TooLarge);
        return Some(up);
    }
    let bytes = std::fs::read(root.join(&fp.path)).ok()?;
    let now = blake3::hash(&bytes).to_hex().to_string();
    if now != hash {
        // файл изменился после отпечатка: хэш — разобранного содержимого
        up.hash = now;
        if state.is_crashed(&fp.path, &up.hash) {
            up.skip_reason = Some(SkipReason::ParserCrash);
            return Some(up);
        }
    }
    up.size = bytes.len() as u64;
    let lim = Limits {
        max_file_bytes: cfg.max_file_bytes,
        parse_timeout: Duration::from_millis(cfg.parse_timeout_ms),
        max_symbols: MAX_SYMBOLS,
        max_refs: MAX_REFS,
    };
    crash.begin(&fp.path, &up.hash);
    debug_abort(&fp.path);
    let ex = zam_code::extract(lang, &bytes, &lim);
    crash.end();
    up.parse_state = ex.state;
    up.skip_reason = ex.skip;
    up.symbols = ex.symbols;
    up.refs = ex.refs;
    if cfg.send_signatures {
        let src = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        for s in &mut up.symbols {
            if let Some(sig) = s.sig.take() {
                let tail = (s.end_line > s.line)
                    .then(|| line_of(src, s.line))
                    .flatten();
                s.sig = Some(mask_sig(&sig, tail));
            }
        }
    } else {
        for s in &mut up.symbols {
            s.sig = None;
        }
    }
    Some(up)
}

/// Строка `n` (с 1) исходника без перевода строки (невалидный UTF-8 — с заменой).
fn line_of(src: &[u8], n: u32) -> Option<String> {
    let l = src.split(|&b| b == b'\n').nth(n.checked_sub(1)? as usize)?;
    Some(String::from_utf8_lossy(l).trim_end().to_string())
}

/// Символ токена секрета (base64, hex, `-_.`).
fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_' | '.')
}

/// Префиксы известных ключей (GitHub, GitLab, Slack, AWS, OpenAI/Anthropic, Stripe, Google):
/// префикс и ≥ 16 символов токена.
const KEY_PREFIXES: [&str; 12] = [
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xox",
    "AKIA",
    "sk-",
    "sk_live_",
    "AIza",
];

/// Ключи точного формата (ревью задачи 5, M8; как у `zam_markdown::secrets`): AWS `AKIA`/`ASIA`
/// и 16 `[0-9A-Z]`, Hugging Face `hf_` и ≥ 34 `[A-Za-z0-9]`, npm `npm_` и 36, SendGrid
/// `SG.<22>.<43>`. Точный формат, а не префикс: `ASIAN_REGION_CODES`, `hf_model_loader` — имена.
static EXACT_KEYS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"^(?:(?:AKIA|ASIA)[0-9A-Z]{16}|hf_[A-Za-z0-9]{34,}|npm_[A-Za-z0-9]{36}|SG\.[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43})$",
    )
    .expect("шаблон ключей")
});

/// Пароль в URL `scheme://user:пароль@host` (пароль — группа 2), как у `zam_markdown::secrets`.
static URL_PASSWORD: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"([A-Za-z][A-Za-z0-9+.-]*://[^\s/:@]*:)([^\s/@]+)@").expect("шаблон URL")
});

/// Высокая энтропия в сигнатуре — по правилу сервера (`zam_markdown::secrets`): ≥ 32 символов,
/// строчные, прописные и цифры, не путь из слов, ≥ 4,5 бита на символ. Порог журнала хуков
/// (3,5 бита, без требования классов) здесь не годится: длинные имена
/// (`handle_incoming_websocket_message_v2` — 4,0) он принимает за секреты (ревью задачи 5, M8).
fn sig_high_entropy(t: &str) -> bool {
    let b = t.as_bytes();
    if b.len() < 32
        || !b.iter().any(u8::is_ascii_lowercase)
        || !b.iter().any(u8::is_ascii_uppercase)
        || !b.iter().any(u8::is_ascii_digit)
    {
        return false;
    }
    let wordlike = |p: &str| {
        !p.is_empty()
            && p.bytes()
                .all(|c| c.is_ascii_alphabetic() || c == b'_' || c == b'-')
    };
    if t.matches('/').count() >= 2 && t.split('/').filter(|p| wordlike(p)).count() >= 2 {
        return false;
    }
    let mut counts = [0u32; 256];
    for &c in b {
        counts[c as usize] += 1;
    }
    let n = b.len() as f64;
    let h: f64 = counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = f64::from(c) / n;
            -p * p.log2()
        })
        .sum();
    h >= 4.5
}

/// Токен похож на секрет: известный префикс и ≥ 16 символов, точный формат ключа или высокая
/// энтропия ([`sig_high_entropy`]).
fn secret_token(t: &str) -> bool {
    (t.chars().count() >= 16 && KEY_PREFIXES.iter().any(|p| t.starts_with(p)))
        || EXACT_KEYS.is_match(t)
        || sig_high_entropy(t)
}

/// Секреты в тексте → `‹secret›`: пароль в URL, затем токены из [`token_char`].
fn mask_tokens(text: &str) -> String {
    let text = URL_PASSWORD.replace_all(text, |c: &regex::Captures| {
        format!("{}{}@", &c[1], crate::cmd::hook::journal::SECRET)
    });
    let mut out = String::with_capacity(text.len());
    let mut tok = String::new();
    let flush = |tok: &mut String, out: &mut String| {
        if secret_token(tok) {
            out.push_str(crate::cmd::hook::journal::SECRET);
        } else {
            out.push_str(tok);
        }
        tok.clear();
    };
    for c in text.chars() {
        if token_char(c) {
            tok.push(c);
        } else {
            flush(&mut tok, &mut out);
            out.push(c);
        }
    }
    flush(&mut tok, &mut out);
    out
}

/// Сигнатура для сервера (ревью задач 4 и 5): секреты маскируются клиентом **до** обрезки
/// до 160 символов — обрезанный экстрактором токен шаблон сервера может не узнать.
/// Необрезанная сигнатура (она не длиннее узла) маскируется как есть. Обрезанная: `tail` —
/// строка имени, если узел продолжается на следующих строках (тогда весь остаток строки —
/// внутри узла); полный текст — от начала сигнатуры до конца строки. Нет `tail` (однострочный
/// узел: конец узла на строке неизвестен, дальше может быть соседний код) — оборванный
/// последний токен снимается. Сервер маскирует повторно.
fn mask_sig(sig: &str, tail: Option<String>) -> String {
    let Some(head) = sig.strip_suffix('…') else {
        return mask_tokens(sig);
    };
    let full = tail
        .as_deref()
        .and_then(|l| l.find(head).map(|i| l[i..].to_string()))
        .unwrap_or_else(|| {
            let keep = head.trim_end_matches(token_char).trim_end();
            format!("{keep}…")
        });
    let masked = mask_tokens(&full);
    if masked.chars().count() <= zam_code::SIG_MAX_CHARS {
        return masked;
    }
    let mut s: String = masked.chars().take(zam_code::SIG_MAX_CHARS - 1).collect();
    s.push('…');
    s
}

/// Файл, чей JSON не помещается в запрос (ревью задачи 5, M7): `skipped`/`limit` без символов
/// и ссылок — иначе сервер отвечал бы 413 на каждой синхронизации.
fn over_limit(mut f: FileUpload) -> FileUpload {
    f.parse_state = ParseState::Skipped;
    f.skip_reason = Some(SkipReason::Limit);
    f.symbols.clear();
    f.refs.clear();
    f
}

/// JSON файла помещается в один запрос.
fn fits_request(n: usize) -> bool {
    n + 1 + ENVELOPE_RESERVE <= BATCH_BYTES_MAX
}

/// Пачка `PUT /code/files`: файлы в виде готового JSON.
struct Batch {
    files: Vec<Vec<u8>>,
    paths: Vec<String>,
    bytes: usize,
}

impl Batch {
    fn new() -> Batch {
        Batch {
            files: Vec::new(),
            paths: Vec::new(),
            bytes: 0,
        }
    }

    fn fits(&self, n: usize) -> bool {
        self.files.is_empty()
            || (self.files.len() < zam_code::UPLOAD_FILES_MAX
                && self.bytes + n + 1 + ENVELOPE_RESERVE <= BATCH_BYTES_MAX)
    }
}

/// Отправитель пачек.
struct Uploader<'a> {
    ctx: &'a Ctx,
    head: Vec<u8>,
    batch: Batch,
    accepted: usize,
    stale_refs: u32,
    sent: Vec<String>,
    warnings: Vec<String>,
}

impl Uploader<'_> {
    fn push(&mut self, bytes: Vec<u8>, path: &str) -> Result<(), CliError> {
        if !self.batch.fits(bytes.len()) {
            self.flush()?;
        }
        self.batch.bytes += bytes.len() + 1;
        self.batch.files.push(bytes);
        self.batch.paths.push(path.to_string());
        Ok(())
    }

    fn flush(&mut self) -> Result<(), CliError> {
        if self.batch.files.is_empty() {
            return Ok(());
        }
        let b = std::mem::replace(&mut self.batch, Batch::new());
        let mut body = self.head.clone();
        for (i, f) in b.files.iter().enumerate() {
            if i > 0 {
                body.push(b',');
            }
            body.extend_from_slice(f);
        }
        body.extend_from_slice(b"]}");
        let v = self
            .ctx
            .client
            .put_json_body("/code/files", body)
            .and_then(read_json)
            .map_err(upload_error)?;
        self.accepted += v["accepted"].as_u64().unwrap_or(0) as usize;
        self.stale_refs += v["stale_refs"].as_u64().unwrap_or(0) as u32;
        for w in v["warnings"].as_array().into_iter().flatten() {
            if let Some(w) = w.as_str() {
                self.warnings.push(format!("сервер: {w}"));
            }
        }
        self.sent.extend(b.paths);
        Ok(())
    }
}

/// Ошибка загрузки пачки: предел сервера (413/422 — `max_files` карты, размер запроса,
/// символы файла) — с подсказкой, что сузить.
fn upload_error(e: CliError) -> CliError {
    let hint = match e.http_status() {
        Some(413 | 422) => {
            "; предел карты сервера ([code] max_files, max_request_bytes, max_symbols_per_file) \
             — сузьте code.exclude или code.languages"
        }
        _ => "",
    };
    let mut e = e.context("загрузка файлов карты");
    e.msg.push_str(hint);
    e
}

/// Режим синхронизации.
#[derive(Debug, Clone, Copy, Default)]
pub struct SyncOpts {
    /// Разобрать и загрузить все файлы.
    pub full: bool,
    /// Разрешить манифест меньше половины прошлого состояния ([`PRUNE_GUARD_MIN`]).
    pub prune: bool,
}

/// Синхронизация карты решения `guid` с корнем `root` (замок берёт вызывающий).
pub fn sync(
    ctx: &Ctx,
    guid: &str,
    root: &Path,
    cfg: &CodeConfig,
    opts: SyncOpts,
) -> Result<SyncReport, CliError> {
    let started = Instant::now();
    let mut state = CodeState::load(root);
    let crash = CrashLog::new(root);
    absorb_crash(root, &crash, &mut state)?;
    let fps = scan::fingerprints(root, cfg)?;
    let mut hashes: HashMap<String, String> = HashMap::with_capacity(fps.len());
    let mut manifest: Vec<Value> = Vec::with_capacity(fps.len());
    let mut live: Vec<&Fingerprint> = Vec::with_capacity(fps.len());
    for fp in &fps {
        let Some(h) = hash_of(root, fp, &state) else {
            continue; // исчез между отпечатком и чтением
        };
        manifest.push(json!({"path": fp.path, "hash": h}));
        hashes.insert(fp.path.clone(), h);
        live.push(fp);
    }
    let known = if state.solution == guid && !state.files.is_empty() {
        state.files.len()
    } else if opts.prune {
        0
    } else {
        server_files(ctx, guid)
    };
    if !opts.prune && known >= PRUNE_GUARD_MIN && live.len() * 2 < known {
        return Err(CliError::config(format!(
            "файлов кода {} — меньше половины файлов карты ({known}): сервер удалил бы \
             из карты остальные. Проверьте корень, code.exclude и code.languages; удаление \
             намеренное — zam code sync --prune",
            live.len()
        )));
    }
    let meta = Meta {
        commit: git_head(root),
        branch: git_branch(root),
        machine: ctx.client.machine().map(str::to_string),
    };
    // решение регистрируется (идемпотентно): карта есть только у известного серверу решения
    let r = ctx
        .client
        .post_json(
            "/solutions",
            &json!({"agent_solution_id": guid, "name": solution_name(ctx, guid)}),
        )
        .map_err(|e| e.context("регистрация решения"))?;
    let mut warnings = Vec::new();
    if r.status().as_u16() == 202 {
        warnings.push(format!(
            "решение {guid} принадлежит другому пользователю: запрос доступа ждёт подтверждения владельца"
        ));
    }
    let mut body = serde_json::Map::new();
    body.insert("solution".into(), json!(guid));
    meta.put_into(&mut body);
    body.insert("manifest".into(), Value::Array(manifest));
    let resp = read_json(ctx.client.post_json("/code/sync", &Value::Object(body))?)
        .map_err(|e| e.context("синхронизация карты"))?;
    let mut report = SyncReport {
        files: live.len(),
        removed: resp["removed"].as_u64().unwrap_or(0),
        stale_refs: resp["stale_refs"].as_u64().unwrap_or(0) as u32,
        warnings,
        ..SyncReport::default()
    };
    // разбор сменился (`EXTRACT_VERSION`) — карта этого клиента перезагружается целиком
    let reparse = state.solution == guid
        && !state.files.is_empty()
        && state.extractor != zam_code::EXTRACT_VERSION;
    if reparse {
        report.warnings.push(format!(
            "версия разбора сменилась ({} → {}): файлы карты загружаются заново",
            state.extractor,
            zam_code::EXTRACT_VERSION
        ));
    }
    let need: Vec<&Fingerprint> = if opts.full || reparse {
        live.clone()
    } else {
        let set: HashSet<&str> = resp["need"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        live.iter()
            .copied()
            .filter(|f| set.contains(f.path.as_str()))
            .collect()
    };
    report.need = need.len();
    let mut head = serde_json::Map::new();
    head.insert("solution".into(), json!(guid));
    meta.put_into(&mut head);
    let mut head = serde_json::to_vec(&Value::Object(head)).unwrap_or_default();
    head.pop(); // `}`
    head.extend_from_slice(if head.len() > 1 {
        b",\"files\":["
    } else {
        b"\"files\":["
    });
    let mut up = Uploader {
        ctx,
        head,
        batch: Batch::new(),
        accepted: 0,
        stale_refs: 0,
        sent: Vec::new(),
        warnings: Vec::new(),
    };
    for fp in need {
        let h = &hashes[&fp.path];
        let Some(mut file) = parse_file(root, fp, h, &state, cfg, &crash) else {
            continue;
        };
        let ser = |f: &FileUpload| {
            serde_json::to_vec(f).map_err(|e| CliError::config(format!("сериализация файла: {e}")))
        };
        let mut bytes = ser(&file)?;
        if !fits_request(bytes.len()) {
            file = over_limit(file);
            bytes = ser(&file)?;
        }
        if let Some(r) = file.skip_reason {
            *report.skipped.entry(r.as_str().to_string()).or_default() += 1;
        }
        if file.hash != *h {
            hashes.insert(fp.path.clone(), file.hash.clone());
        }
        up.push(bytes, &file.path)?;
    }
    up.flush()?;
    report.uploaded = up.accepted;
    report.stale_refs += up.stale_refs;
    report.warnings.append(&mut up.warnings);
    // состояние: текущие файлы; журнал падений — только для неизменившихся хэшей
    let now = cache::now_rfc3339();
    let sent: HashSet<&str> = up.sent.iter().map(String::as_str).collect();
    let mut files = BTreeMap::new();
    for fp in &live {
        let hash = hashes[&fp.path].clone();
        let sent_at = if sent.contains(fp.path.as_str()) {
            Some(now.clone())
        } else {
            state.files.get(&fp.path).and_then(|s| s.sent_at.clone())
        };
        files.insert(
            fp.path.clone(),
            FileState {
                fp: fp.fp.clone(),
                size: fp.size,
                hash,
                sent_at,
            },
        );
    }
    let crashed: Vec<(String, String)> = state
        .crashed
        .iter()
        .filter(|(p, h)| hashes.get(p) == Some(h))
        .cloned()
        .collect();
    let new_state = CodeState {
        server: ctx.client.base().to_string(),
        solution: guid.to_string(),
        files,
        crashed,
        extractor: zam_code::EXTRACT_VERSION,
    };
    let io = |e: std::io::Error| CliError::config(format!("кэш карты кода: {e}"));
    new_state.save(root).map_err(io)?;
    SyncMark {
        server: ctx.client.base().to_string(),
        solution: guid.to_string(),
        last_ok_at: now,
        head: meta.commit.clone(),
    }
    .save(root)
    .map_err(io)?;
    report.ms = started.elapsed().as_millis();
    Ok(report)
}

/// Файлов в карте решения на сервере (`GET /code/status`) — для предохранителя без локального
/// состояния; карты нет или ответ не получен — 0 (ошибку сети покажет сама синхронизация).
fn server_files(ctx: &Ctx, guid: &str) -> usize {
    ctx.client
        .get("/code/status", &[("solution", guid)])
        .and_then(read_json)
        .ok()
        .and_then(|v| v["files"].as_u64())
        .map_or(0, |n| usize::try_from(n).unwrap_or(usize::MAX))
}

/// Имя решения для регистрации: из `solution.toml`, иначе GUID.
fn solution_name(ctx: &Ctx, guid: &str) -> String {
    ctx.solution
        .as_ref()
        .filter(|s| s.id == guid)
        .map_or_else(|| guid.to_string(), |s| s.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp/zam-cli-code-sync")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(cache_dir(&d)).unwrap();
        d
    }

    #[test]
    fn crash_log_round_trip() {
        let r = root("crash");
        let c = CrashLog::new(&r);
        assert_eq!(c.unfinished(), None);
        c.begin("src/a.rs", "h1");
        c.end();
        assert_eq!(c.unfinished(), None);
        // две незакрытые записи подряд: каждая сохраняется в состоянии до снятия (I1)
        let mut st = CodeState::default();
        c.begin("src/b.rs", "h2");
        absorb_crash(&r, &c, &mut st).unwrap();
        assert_eq!(c.unfinished(), None);
        c.begin("src/c.rs", "h3");
        let mut st2 = CodeState::load(&r);
        absorb_crash(&r, &c, &mut st2).unwrap();
        let saved = CodeState::load(&r);
        assert_eq!(
            saved.crashed,
            [
                ("src/b.rs".into(), "h2".into()),
                ("src/c.rs".into(), "h3".into())
            ]
        );
        drop(st);
    }

    #[test]
    fn lock_held_then_released() {
        let r = root("lock");
        let a = SyncLock::acquire(&r).unwrap().expect("первый");
        assert!(SyncLock::acquire(&r).unwrap().is_none(), "занят");
        drop(a);
        assert!(SyncLock::acquire(&r).unwrap().is_some(), "снят при Drop");
    }

    #[test]
    fn oversized_file_goes_limit() {
        assert!(fits_request(BATCH_BYTES_MAX - ENVELOPE_RESERVE - 1));
        assert!(!fits_request(BATCH_BYTES_MAX - ENVELOPE_RESERVE));
        let f = FileUpload {
            path: "a.rs".into(),
            lang: zam_code::Lang::Rust,
            hash: "0".repeat(64),
            git_oid: None,
            size: 1,
            parse_state: ParseState::Ok,
            skip_reason: None,
            symbols: vec![zam_code::Symbol {
                name: "a".into(),
                qual: "a".into(),
                kind: zam_code::SymbolKind::Function,
                line: 1,
                end_line: 1,
                sig: None,
                body_hash: "0".repeat(16),
                exported: None,
            }],
            refs: vec![],
        };
        let g = over_limit(f);
        assert_eq!(
            (g.parse_state, g.skip_reason, g.symbols.len()),
            (ParseState::Skipped, Some(SkipReason::Limit), 0)
        );
    }

    #[test]
    fn sig_masked_before_cut() {
        let tok = "ghp_Ab3dEf6hIj9kLm2nOp5qRs8tUv1wXy4zAb7c";
        let line = format!("export function f({}, key = \"{tok}\") {{", "a".repeat(120));
        let cut: String = line.chars().take(159).collect::<String>() + "…";
        let s = mask_sig(&cut, Some(line.clone()));
        assert!(!s.contains("ghp_"), "{s}");
        assert!(s.chars().count() <= 160 && s.contains("‹secret›"), "{s}");
        // строки нет — оборванный токен снимается
        let s = mask_sig(&cut, None);
        assert!(!s.contains("ghp_") && s.ends_with('…'), "{s}");
        // короткая — как есть, секрет внутри — маскируется
        assert_eq!(mask_sig("fn open() -> Store", None), "fn open() -> Store");
        // необрезанная — соседний код строки не подхватывается (I2)
        let line = "pub fn a() -> u32 { 1 } static DB_PASSWORD: &str = \"hunter2-prod\";";
        assert_eq!(
            mask_sig("pub fn a() -> u32 { 1 }", Some(line.into())),
            "pub fn a() -> u32 { 1 }"
        );
        assert_eq!(
            mask_sig(&format!("const K = \"{tok}\";"), None),
            "const K = \"‹secret›\";"
        );
    }

    /// Ревью задачи 5, M8: `ASIA…`, `hf_`, `npm_`, `SG.`, пароль в URL — маскируются; длинные
    /// имена и пути в сигнатуре — нет.
    #[test]
    fn sig_masks_more_prefixes_not_identifiers() {
        let rnd = |n: usize, seed: u64| -> String {
            const A: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
            let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (0..n)
                .map(|_| {
                    x = x
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    A[((x >> 33) % 62) as usize] as char
                })
                .collect()
        };
        let up = rnd(16, 1).to_ascii_uppercase();
        for secret in [
            format!("AS{}{up}", "IA"),
            format!("hf_{}", rnd(34, 2)),
            format!("npm_{}", rnd(36, 3)),
            format!("SG.{}.{}", rnd(22, 4), rnd(43, 5)),
        ] {
            let s = mask_sig(&format!("const K = \"{secret}\";"), None);
            assert_eq!(s, "const K = \"‹secret›\";", "{secret}");
        }
        let s = mask_sig("const DSN = \"postgres://admin:s3cr3tpw@db/app\";", None);
        assert!(
            !s.contains("s3cr3tpw") && s.contains("admin:‹secret›@"),
            "{s}"
        );
        for sig in [
            "pub fn handle_incoming_websocket_message_v2(&self) -> Result<(), Error>",
            "export function useQueryClientInvalidationOnMount2x(): void",
            "const VIEW = \"src/components/Button2/index_view.tsx\";",
            "fn hf_model_loader_from_pretrained_v2(npm_package_version: &str)",
        ] {
            assert_eq!(mask_sig(sig, None), sig);
        }
    }

    #[test]
    fn upload_limit_error_explained() {
        let mut e = CliError::input("не больше 50000 файлов в карте (code.max_files) (HTTP 422)");
        e.problem = Some(json!({"status": 422}));
        let e = upload_error(e);
        assert_eq!(e.code, crate::client::EXIT_INPUT);
        assert!(
            e.msg.starts_with("загрузка файлов карты: не больше"),
            "{}",
            e.msg
        );
        assert!(e.msg.contains("code.exclude"), "{}", e.msg);
        let e = upload_error(CliError::new(2, "сервер недоступен"));
        assert_eq!(e.msg, "загрузка файлов карты: сервер недоступен");
    }

    #[test]
    fn batch_limits() {
        let mut b = Batch::new();
        assert!(b.fits(BATCH_BYTES_MAX * 2), "пустая пачка берёт любой файл");
        b.files.push(vec![]);
        b.bytes = BATCH_BYTES_MAX - ENVELOPE_RESERVE - 10;
        assert!(b.fits(9) && !b.fits(10));
        b.bytes = 0;
        b.files = vec![vec![]; zam_code::UPLOAD_FILES_MAX];
        assert!(!b.fits(1));
    }
}
