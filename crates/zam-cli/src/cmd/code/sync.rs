//! Синхронизация карты кода (спецификация M5.6 §5, §13; решения составителя плана 6–9):
//! отпечатки → хэши (из `code-state.json` при том же отпечатке, иначе blake3) →
//! `POST /code/sync` → разбор `need` по одному в одном потоке под журналом падений →
//! `PUT /code/files` пачками (≤ 500 файлов и ≤ 8 МиБ JSON) → `code-state.json` и
//! `code-sync.json`.
//!
//! - **Журнал падений** `cache/code-parsing.json` ([`CrashLog`]): `{path, hash}` пишется перед
//!   разбором файла без fsync (обрыв процесса запись сохраняет) и удаляется после; незакрытая
//!   запись при следующем запуске попадает в `crashed` состояния — файл уходит `skipped`/
//!   `parser_crash`, пока его хэш тот же.
//! - **Замок** `cache/code-sync.lock` ([`SyncLock`], `create_new`, внутри `{pid, at}`): живой —
//!   вторая синхронизация не идёт; брошенный (старше [`LOCK_STALE`] или процесс `pid` завершён)
//!   снимается; свой замок снимается в конце и при ошибке (`Drop`).
//! - На сервер — только метаданные §4: без `code.send_signatures` поле `sig` не отправляется.
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
/// Возраст замка, после которого он считается брошенным.
pub const LOCK_STALE: Duration = Duration::from_secs(30 * 60);
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
}

impl SyncReport {
    /// Одна строка итога (stdout и `hooks.log`).
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

    /// Незакрытая запись прошлого запуска (и её снятие).
    pub fn take_unfinished(&self) -> Option<(String, String)> {
        let bytes = std::fs::read(&self.path).ok()?;
        let _ = std::fs::remove_file(&self.path);
        let e: CrashEntry = serde_json::from_slice(&bytes).ok()?;
        Some((e.path, e.hash))
    }
}

/// Содержимое замка.
#[derive(Debug, Serialize, Deserialize)]
struct LockInfo {
    pid: u32,
    /// Секунды Unix.
    at: u64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Замок синхронизации проекта; снимается при `Drop`.
#[derive(Debug)]
pub struct SyncLock {
    path: PathBuf,
}

impl SyncLock {
    /// Захват: `None` — идёт другая синхронизация (живой замок).
    pub fn acquire(root: &Path) -> Result<Option<SyncLock>, CliError> {
        let path = cache_dir(root).join("code-sync.lock");
        std::fs::create_dir_all(cache_dir(root))
            .map_err(|e| CliError::config(format!("{}: {e}", cache_dir(root).display())))?;
        for _ in 0..2 {
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                o.mode(0o600);
            }
            match o.open(&path) {
                Ok(mut f) => {
                    let info = LockInfo {
                        pid: std::process::id(),
                        at: now_secs(),
                    };
                    let _ = f.write_all(&serde_json::to_vec(&info).unwrap_or_default());
                    return Ok(Some(SyncLock { path }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let seen = std::fs::read(&path).unwrap_or_default();
                    if !stale(&path, &seen) {
                        return Ok(None);
                    }
                    // брошенный: снимается, только если его не успели заменить
                    if std::fs::read(&path).unwrap_or_default() == seen {
                        let _ = std::fs::remove_file(&path);
                    }
                }
                Err(e) => {
                    return Err(CliError::config(format!("замок {}: {e}", path.display())));
                }
            }
        }
        Ok(None)
    }
}

impl Drop for SyncLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Замок брошен: старше [`LOCK_STALE`] или процесс завершён. Нечитаемое содержимое — по
/// времени изменения файла.
fn stale(path: &Path, bytes: &[u8]) -> bool {
    let age = |at: u64| Duration::from_secs(now_secs().saturating_sub(at));
    match serde_json::from_slice::<LockInfo>(bytes) {
        Ok(i) => age(i.at) > LOCK_STALE || !pid_alive(i.pid),
        Err(_) => std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_none_or(|a| a > LOCK_STALE),
    }
}

/// Процесс `pid` существует (Linux — `/proc`, Windows — `OpenProcess`; иначе считается живым).
#[cfg(target_os = "linux")]
fn pid_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use std::ffi::c_void;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    const ERROR_ACCESS_DENIED: u32 = 5;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn GetExitCodeProcess(h: *mut c_void, code: *mut u32) -> i32;
        fn CloseHandle(h: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }
    // SAFETY: вызовы kernel32 с проверкой результата; дескриптор закрывается в той же ветке,
    // указатель `code` — на локальную переменную.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            // нет такого процесса — ERROR_INVALID_PARAMETER; чужой защищённый — жив
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        !ok || code == STILL_ACTIVE
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn pid_alive(_pid: u32) -> bool {
    true
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

/// Ветка git (`None` — не git или отсоединённый `HEAD`).
fn git_branch(root: &Path) -> Option<String> {
    if !refs::in_git(root) {
        return None;
    }
    let out = refs::git(root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let b = String::from_utf8(out).ok()?.trim().to_string();
    (!b.is_empty() && b != "HEAD" && b.chars().count() <= 256).then_some(b)
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
                s.sig = Some(mask_sig(&sig, line_of(src, s.line)));
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

/// Префиксы известных ключей (GitHub, GitLab, Slack, AWS, OpenAI/Anthropic, Stripe, Google).
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

/// Токен похож на секрет: известный префикс и ≥ 16 символов либо высокая энтропия.
fn secret_token(t: &str) -> bool {
    let n = t.chars().count();
    (n >= 16 && KEY_PREFIXES.iter().any(|p| t.starts_with(p)))
        || crate::cmd::hook::journal::high_entropy(t)
}

/// Секреты в тексте → `‹secret›` (по токенам из [`token_char`]).
fn mask_tokens(text: &str) -> String {
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

/// Сигнатура для сервера (ревью задачи 4): маскировка секретов клиентом **до** обрезки до
/// 160 символов — обрезанный экстрактором токен шаблон сервера может не узнать. Полный текст —
/// строка определения с начала сигнатуры; не нашлась — у обрезанной сигнатуры снимается
/// оборванный последний токен. Сервер маскирует повторно.
fn mask_sig(sig: &str, line: Option<String>) -> String {
    let cut = sig.strip_suffix('…');
    let head = cut.unwrap_or(sig);
    let full = line
        .as_deref()
        .and_then(|l| l.find(head).map(|i| l[i..].to_string()))
        .unwrap_or_else(|| match cut {
            // оборванный токен в конце — прочь (его продолжения не видно)
            Some(h) => {
                let keep = h.trim_end_matches(token_char).trim_end();
                format!("{keep}…")
            }
            None => sig.to_string(),
        });
    let masked = mask_tokens(&full);
    if masked.chars().count() <= zam_code::SIG_MAX_CHARS {
        return masked;
    }
    let mut s: String = masked.chars().take(zam_code::SIG_MAX_CHARS - 1).collect();
    s.push('…');
    s
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
}

impl Uploader<'_> {
    fn push(&mut self, up: &FileUpload) -> Result<(), CliError> {
        let bytes = serde_json::to_vec(up)
            .map_err(|e| CliError::config(format!("сериализация файла: {e}")))?;
        if !self.batch.fits(bytes.len()) {
            self.flush()?;
        }
        self.batch.bytes += bytes.len() + 1;
        self.batch.files.push(bytes);
        self.batch.paths.push(up.path.clone());
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
                eprintln!("предупреждение сервера: {w}");
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
            "; предел карты сервера ([code] max_files, max_request_bytes, max_symbols_per_file) —              сузьте code.exclude или code.languages"
        }
        _ => "",
    };
    let mut e = e.context("загрузка файлов карты");
    e.msg.push_str(hint);
    e
}

/// Синхронизация карты решения `guid` с корнем `root` (замок берёт вызывающий).
pub fn sync(
    ctx: &Ctx,
    guid: &str,
    root: &Path,
    cfg: &CodeConfig,
    full: bool,
) -> Result<SyncReport, CliError> {
    let started = Instant::now();
    let mut state = CodeState::load(root);
    let crash = CrashLog::new(root);
    if let Some(c) = crash.take_unfinished()
        && !state.crashed.contains(&c)
    {
        state.crashed.push(c);
    }
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
    let meta = Meta {
        commit: refs::git_head(root),
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
    if r.status().as_u16() == 202 {
        eprintln!(
            "решение {guid} принадлежит другому пользователю: запрос доступа ждёт подтверждения владельца"
        );
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
        ..SyncReport::default()
    };
    let need: Vec<&Fingerprint> = if full {
        live.clone()
    } else {
        let set: std::collections::HashSet<&str> = resp["need"]
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
    };
    for fp in need {
        let h = &hashes[&fp.path];
        let Some(file) = parse_file(root, fp, h, &state, cfg, &crash) else {
            continue;
        };
        if let Some(r) = file.skip_reason {
            *report.skipped.entry(r.as_str().to_string()).or_default() += 1;
        }
        if file.hash != *h {
            hashes.insert(fp.path.clone(), file.hash.clone());
        }
        up.push(&file)?;
    }
    up.flush()?;
    report.uploaded = up.accepted;
    report.stale_refs += up.stale_refs;
    // состояние: текущие файлы; журнал падений — только для неизменившихся хэшей
    let now = cache::now_rfc3339();
    let sent: std::collections::HashSet<&str> = up.sent.iter().map(String::as_str).collect();
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
        assert_eq!(c.take_unfinished(), None);
        c.begin("src/a.rs", "h1");
        c.end();
        assert_eq!(c.take_unfinished(), None);
        c.begin("src/b.rs", "h2");
        assert_eq!(c.take_unfinished(), Some(("src/b.rs".into(), "h2".into())));
        assert_eq!(c.take_unfinished(), None);
    }

    #[test]
    fn lock_live_stale_dead() {
        let r = root("lock");
        let a = SyncLock::acquire(&r).unwrap().expect("первый");
        assert!(SyncLock::acquire(&r).unwrap().is_none(), "живой замок");
        drop(a);
        let p = cache_dir(&r).join("code-sync.lock");
        assert!(!p.exists());
        let old = LockInfo {
            pid: std::process::id(),
            at: now_secs() - 31 * 60,
        };
        std::fs::write(&p, serde_json::to_vec(&old).unwrap()).unwrap();
        assert!(
            SyncLock::acquire(&r).unwrap().is_some(),
            "брошенный по времени"
        );
        if cfg!(any(windows, target_os = "linux")) {
            let dead = LockInfo {
                pid: u32::MAX - 1,
                at: now_secs(),
            };
            std::fs::write(&p, serde_json::to_vec(&dead).unwrap()).unwrap();
            assert!(SyncLock::acquire(&r).unwrap().is_some(), "процесс завершён");
        }
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
        assert_eq!(
            mask_sig(&format!("const K = \"{tok}\";"), None),
            "const K = \"‹secret›\";"
        );
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
