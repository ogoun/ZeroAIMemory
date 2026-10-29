//! Локальный кэш хуков `.zeroaimemory/cache/` (§10.3).
//!
//! - `triggers.json` — `{etag, fetched_at, server, solution, triggers}` из `GET /triggers`
//!   (`If-None-Match` → 304 — кэш остаётся, обновляется `fetched_at`); кэш другого сервера или
//!   решения не используется; битый — игнорируется и переписывается при следующем обновлении;
//! - `hooks.json` — `{fetched_at, server, hooks}`: настройки типов подсказок с сервера из
//!   ответа `/recall(session_start)` (R5);
//! - `shown-<сессия>.json` — `{shown, reminded}`: записи, уже показанные `pre-tool` в этой
//!   сессии, и признак напоминания `checkpoint`;
//! - `edits-<сессия>` — маркер правок Write/Edit/`apply_patch` (для `checkpoint`, R1): файл есть —
//!   правки были; создаётся без чтения и перезаписи (fix round 1, m-2);
//! - `file-refs.json` — `{etag, fetched_at, server, solution, refs}` из `GET /file-refs`
//!   (M4.5 §6.3; как `triggers.json`): знание о файле для `post-tool` и `pre-tool` без сети;
//! - `changed-<сессия>.jsonl` — очередь `{path, hash}` правленых файлов (M4.5 §6.2): пишет
//!   `post-tool`, отправляют `POST /files/changed` `prompt`, `session-start` и `checkpoint`
//!   ([`flush_changed`]);
//! - `events-<сессия>.jsonl` — журнал событий сессии ([`super::journal`]);
//! - `<сессия>` в именах — очищенный id, `-` и 8 hex blake3 исходного id (fix round 1, m-9);
//!   `shown-*`/`edits-*`/`events-*`/`changed-*` старше [`SHOWN_TTL`] и временные `.*.tmp`
//!   старше [`TMP_TTL`] удаляет `session-start` ([`gc_cache`]);
//! - `session.json` — `{id, agent}`: текущая сессия агента в проекте; команды `zam` берут из
//!   него `X-Zam-Session` (не старше [`SESSION_TTL`]), чтобы запись в память через CLI попала
//!   в `memory_writes` сессии хуков **[решение исполнителя]**.
//!
//! Запись — временный файл в том же каталоге и rename (атомарно); на unix файлы кэша — с правами
//! 0600 (fix round 1, m-8).
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Env, HookFlags};
use crate::client::{CliError, Client, EXIT_UNREACHABLE, read_json};

/// Срок хранения `shown-<сессия>.json` (по времени изменения).
pub const SHOWN_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// Срок, в который `session.json` считается текущей сессией (как окно MCP, R13).
pub const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);
/// Срок, после которого брошенный временный файл записи удаляется.
pub const TMP_TTL: Duration = Duration::from_secs(3600);
/// Предел файла кэша, байт.
const CACHE_MAX: u64 = 16 * 1024 * 1024;

/// Триггер из `GET /triggers`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Trigger {
    pub id: String,
    pub entry_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub pattern: String,
    pub tools: Option<Vec<String>>,
    pub title: String,
    pub insight_kind: String,
    pub certainty: Option<String>,
    pub space: String,
}

impl Trigger {
    /// Триггер применим к инструменту (`tools` не задан — к любому).
    pub fn for_tool(&self, tool: Option<&str>) -> bool {
        match (&self.tools, tool) {
            (Some(ts), Some(t)) => ts.iter().any(|x| x.eq_ignore_ascii_case(t)),
            _ => true,
        }
    }

    /// Строка подсказки §10.3: `ZeroAIMemory: <lead><title> [<вид>, <certainty>] — i:<id>`.
    pub fn hint(&self, lead: &str) -> String {
        let title = self.title.split_whitespace().collect::<Vec<_>>().join(" ");
        let tags: Vec<&str> = [
            self.insight_kind.as_str(),
            self.certainty.as_deref().unwrap_or(""),
        ]
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect();
        let mut s = format!("ZeroAIMemory: {lead}{title}");
        if !tags.is_empty() {
            s.push_str(&format!(" [{}]", tags.join(", ")));
        }
        s.push_str(&format!(" — i:{}", self.entry_id));
        s
    }
}

/// `triggers.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TriggersCache {
    pub etag: String,
    pub fetched_at: String,
    pub server: String,
    pub solution: Option<String>,
    pub triggers: Vec<Trigger>,
}

/// `hooks.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct HooksCache {
    fetched_at: String,
    server: String,
    hooks: HookFlags,
}

/// `shown-<сессия>.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Shown {
    /// Id записей, показанных `pre-tool` в этой сессии.
    pub shown: Vec<String>,
    /// `checkpoint` уже напомнил.
    pub reminded: bool,
}

/// `session.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CurrentSession {
    id: String,
    agent: String,
}

/// Атомарная запись: временный файл в том же каталоге и rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("cache");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    {
        use std::io::Write;
        let mut f = private_options().open(&tmp)?;
        f.write_all(bytes)?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Создание файла кэша: на unix — права 0600 (в кэше — id сессий и заголовки записей).
pub fn private_options() -> std::fs::OpenOptions {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, String> {
    match std::fs::metadata(path) {
        Ok(m) if m.len() > CACHE_MAX => return Err("файл слишком велик".into()),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| e.to_string())
}

fn write_json_file<T: Serialize>(path: &Path, v: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(v).map_err(std::io::Error::other)?;
    write_atomic(path, &bytes)
}

pub fn triggers_path(root: &Path) -> PathBuf {
    root.join(".zeroaimemory")
        .join("cache")
        .join("triggers.json")
}

/// Кэш триггеров; битый — `None` и строка в журнал.
pub fn read_triggers(env: &Env) -> Option<TriggersCache> {
    match read_json_file(&triggers_path(&env.root)) {
        Ok(c) => c,
        Err(e) => {
            env.log(&format!("кэш триггеров битый, пропущен: {e}"));
            None
        }
    }
}

/// Кэш триггеров, пригодный для сервера `server` (если известен) и решения проекта.
pub fn usable_triggers(env: &Env, server: Option<&str>) -> Option<TriggersCache> {
    let c = read_triggers(env)?;
    let sol = env.solution.as_ref().map(|s| s.id.as_str());
    if c.solution.as_deref() != sol || server.is_some_and(|s| s != c.server) {
        return None;
    }
    Some(c)
}

/// Итог обновления кэша триггеров.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// 200: новый состав.
    Updated(usize),
    /// 304: состав прежний.
    NotModified(usize),
}

/// `GET /triggers` и запись `triggers.json`; `force` — без `If-None-Match`. Предупреждения
/// ответа (`solution access …`) — в журнал хуков.
pub fn refresh_triggers(
    env: &Env,
    client: &Client,
    force: bool,
) -> Result<(Refresh, Vec<String>), CliError> {
    let path = triggers_path(&env.root);
    let sol = env.solution.as_ref().map(|s| s.id.clone());
    let old = if force {
        None
    } else {
        read_json_file::<TriggersCache>(&path)
            .ok()
            .flatten()
            .filter(|c| c.server == client.base() && c.solution == sol && !c.etag.is_empty())
    };
    let inm = old.as_ref().map(|c| format!("\"{}\"", c.etag));
    let mut headers = Vec::new();
    if let Some(v) = &inm {
        headers.push(("If-None-Match", v.as_str()));
    }
    let query: Vec<(&str, &str)> = sol.iter().map(|g| ("solution", g.as_str())).collect();
    let r = client.get_with("/triggers", &query, &headers)?;
    let write = |c: &TriggersCache| {
        write_json_file(&path, c)
            .map_err(|e| CliError::new(EXIT_UNREACHABLE, format!("запись {}: {e}", path.display())))
    };
    if r.status().as_u16() == 304
        && let Some(mut c) = old
    {
        c.fetched_at = now_rfc3339();
        write(&c)?;
        return Ok((Refresh::NotModified(c.triggers.len()), Vec::new()));
    }
    let v = read_json(r)?;
    let triggers: Vec<Trigger> = serde_json::from_value(v["triggers"].clone())
        .map_err(|e| CliError::new(EXIT_UNREACHABLE, format!("ответ /triggers: {e}")))?;
    let warnings = strings(&v["warnings"]);
    let c = TriggersCache {
        etag: v["etag"].as_str().unwrap_or_default().to_string(),
        fetched_at: now_rfc3339(),
        server: client.base().to_string(),
        solution: sol,
        triggers,
    };
    write(&c)?;
    Ok((Refresh::Updated(c.triggers.len()), warnings))
}

/// Запись индекса `GET /file-refs`: ссылка (`n:`/`i:`/…), вид, заголовок, состояние.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RefItem {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub state: String,
}

/// Путь индекса `GET /file-refs`: последний записанный хэш и до 5 записей.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PathRefs {
    pub path: String,
    pub hash: Option<String>,
    pub items: Vec<RefItem>,
}

/// `file-refs.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FileRefsCache {
    pub etag: String,
    pub fetched_at: String,
    pub server: String,
    pub solution: Option<String>,
    pub refs: Vec<PathRefs>,
}

impl FileRefsCache {
    /// Путь индекса (на Windows — без учёта регистра).
    pub fn find(&self, path: &str) -> Option<&PathRefs> {
        self.refs
            .iter()
            .find(|r| crate::cmd::refs::same_path(&r.path, path))
    }
}

pub fn file_refs_path(root: &Path) -> PathBuf {
    root.join(".zeroaimemory")
        .join("cache")
        .join("file-refs.json")
}

/// Индекс ссылок для сервера `server` (если известен) и решения проекта; битый — `None` и
/// строка в журнал.
pub fn usable_file_refs(env: &Env, server: Option<&str>) -> Option<FileRefsCache> {
    let c: FileRefsCache = match read_json_file(&file_refs_path(&env.root)) {
        Ok(c) => c?,
        Err(e) => {
            env.log(&format!("кэш file-refs битый, пропущен: {e}"));
            return None;
        }
    };
    let sol = env.solution.as_ref().map(|s| s.id.as_str());
    if sol.is_none() || c.solution.as_deref() != sol || server.is_some_and(|s| s != c.server) {
        return None;
    }
    Some(c)
}

/// `GET /file-refs?solution=` с `If-None-Match` и запись `file-refs.json` (304 — кэш остаётся,
/// обновляется `fetched_at`). Без решения — `None` без запроса.
pub fn refresh_file_refs(env: &Env, client: &Client) -> Result<Option<Refresh>, CliError> {
    let Some(sol) = env.solution.as_ref().map(|s| s.id.clone()) else {
        return Ok(None);
    };
    let path = file_refs_path(&env.root);
    let old = read_json_file::<FileRefsCache>(&path)
        .ok()
        .flatten()
        .filter(|c| {
            c.server == client.base() && c.solution.as_deref() == Some(&sol) && !c.etag.is_empty()
        });
    let inm = old.as_ref().map(|c| format!("\"{}\"", c.etag));
    let mut headers = Vec::new();
    if let Some(v) = &inm {
        headers.push(("If-None-Match", v.as_str()));
    }
    let r = client.get_with("/file-refs", &[("solution", sol.as_str())], &headers)?;
    let write = |c: &FileRefsCache| {
        write_json_file(&path, c)
            .map_err(|e| CliError::new(EXIT_UNREACHABLE, format!("запись {}: {e}", path.display())))
    };
    if r.status().as_u16() == 304
        && let Some(mut c) = old
    {
        c.fetched_at = now_rfc3339();
        write(&c)?;
        return Ok(Some(Refresh::NotModified(c.refs.len())));
    }
    let v = read_json(r)?;
    let refs: Vec<PathRefs> = serde_json::from_value(v["refs"].clone())
        .map_err(|e| CliError::new(EXIT_UNREACHABLE, format!("ответ /file-refs: {e}")))?;
    let c = FileRefsCache {
        etag: v["etag"].as_str().unwrap_or_default().to_string(),
        fetched_at: now_rfc3339(),
        server: client.base().to_string(),
        solution: Some(sol),
        refs,
    };
    write(&c)?;
    Ok(Some(Refresh::Updated(c.refs.len())))
}

/// Строка очереди `changed-<сессия>.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changed {
    pub path: String,
    pub hash: String,
}

pub fn changed_path(env: &Env, session: &str) -> PathBuf {
    env.cache_dir()
        .join(format!("changed-{}.jsonl", session_file_part(session)))
}

/// Открытие файла очереди на дописывание (на unix — 0600).
fn append_options() -> std::fs::OpenOptions {
    let mut o = std::fs::OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o
}

/// Дописывает файлы в очередь изменённых: строка на файл, все одним `write` с append.
pub fn queue_changed(env: &Env, session: &str, files: &[Changed]) {
    if files.is_empty() {
        return;
    }
    let mut buf = Vec::new();
    for c in files {
        buf.extend(serde_json::to_vec(c).unwrap_or_default());
        buf.push(b'\n');
    }
    let r = std::fs::create_dir_all(env.cache_dir()).and_then(|_| {
        use std::io::Write;
        append_options()
            .open(changed_path(env, session))?
            .write_all(&buf)
    });
    if let Err(e) = r {
        env.log(&format!("очередь изменённых файлов: {e}"));
    }
}

/// Файлы очередей: текущей сессии или (`session = None`) всех сессий проекта.
fn queue_files(env: &Env, session: Option<&str>) -> Vec<PathBuf> {
    if let Some(s) = session {
        let p = changed_path(env, s);
        return if p.is_file() { vec![p] } else { Vec::new() };
    }
    std::fs::read_dir(env.cache_dir())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("changed-") && n.ends_with(".jsonl"))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Запас бюджета на `POST /files/changed`: меньше — очередь не отправляется (останется на
/// следующий хук).
pub const FLUSH_MIN_LEFT: Duration = Duration::from_millis(15);

/// Отправка очередей изменённых файлов (fix round 1, I-2): все очереди (`session` задан —
/// только его) забираются rename во временные файлы и уходят **одним** `POST /files/changed` до
/// 500 путей (повтор пути — последний хэш); перед запросом проверяется остаток бюджета хука
/// ([`FLUSH_MIN_LEFT`]). Успех — забранное удаляется, сверх 500 — возвращается в свою очередь до
/// следующего раза; ошибка сети, 401, 408, 429 и 5xx — всё возвращается; прочие 4xx (запрос
/// не примет никогда) — очередь отбрасывается со строкой в `hooks.log` (fix round 1, M-6).
/// Строки, дописанные во время отправки, не теряются: они в новой очереди. Без решения — ничего.
pub fn flush_changed(env: &Env, client: &Client, session: Option<&str>) {
    let Some(sol) = env.solution.as_ref().map(|s| s.id.clone()) else {
        return;
    };
    if env.http_left().is_none_or(|d| d < FLUSH_MIN_LEFT) {
        return;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    // (очередь, забранный файл)
    let mut taken: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (i, q) in queue_files(env, session).into_iter().enumerate() {
        let name = q.file_name().and_then(|n| n.to_str()).unwrap_or("changed");
        let t = q.with_file_name(format!(".{name}.{}.{nanos}.{i}.tmp", std::process::id()));
        if std::fs::rename(&q, &t).is_ok() {
            taken.push((q, t));
        }
    }
    if taken.is_empty() {
        return;
    }
    // (путь, хэш, номер очереди) — повтор пути: последний хэш
    let mut files: Vec<(Changed, usize)> = Vec::new();
    for (qi, (_, t)) in taken.iter().enumerate() {
        let bytes = std::fs::read(t).unwrap_or_default();
        for l in bytes.split(|b| *b == b'\n') {
            let Ok(c) = serde_json::from_slice::<Changed>(l) else {
                continue;
            };
            files.retain(|(f, _)| !crate::cmd::refs::same_path(&f.path, &c.path));
            files.push((c, qi));
        }
    }
    let n = files.len().min(crate::cmd::refs::FILES_MAX);
    let back: Vec<(Changed, usize)> = if n == 0 {
        Vec::new()
    } else if env.http_left().is_none_or(|d| d < FLUSH_MIN_LEFT) {
        files.clone()
    } else {
        let pairs: Vec<(String, String)> = files[..n]
            .iter()
            .map(|(c, _)| (c.path.clone(), c.hash.clone()))
            .collect();
        match crate::cmd::refs::post_changed(client, &sol, &pairs) {
            Ok(_) => files[n..].to_vec(),
            Err(e) if permanent(&e) => {
                env.log(&format!(
                    "files/changed: очередь ({} путей) отброшена: {e}",
                    files.len()
                ));
                Vec::new()
            }
            Err(e) => {
                env.log(&format!("files/changed: {e}"));
                files.clone()
            }
        }
    };
    for (qi, (q, _)) in taken.iter().enumerate() {
        let mut out = Vec::new();
        for (c, _) in back.iter().filter(|(_, i)| *i == qi) {
            out.extend(serde_json::to_vec(c).unwrap_or_default());
            out.push(b'\n');
        }
        if out.is_empty() {
            continue;
        }
        use std::io::Write;
        if let Err(e) = append_options().open(q).and_then(|mut f| f.write_all(&out)) {
            env.log(&format!("возврат очереди изменённых файлов: {e}"));
        }
    }
    for (_, t) in &taken {
        let _ = std::fs::remove_file(t);
    }
}

/// Ответ, который повтор не исправит: 4xx, кроме 401, 408 и 429.
fn permanent(e: &CliError) -> bool {
    e.http_status()
        .is_some_and(|s| (400..500).contains(&s) && !matches!(s, 401 | 408 | 429))
}

/// Массив строк JSON (остальное пропускается).
pub fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn hooks_path(env: &Env) -> PathBuf {
    env.cache_dir().join("hooks.json")
}

/// Настройки сервера из `hooks.json` (битый или нет — `None`).
pub fn read_server_hooks(env: &Env) -> Option<HookFlags> {
    read_json_file::<HooksCache>(&hooks_path(env))
        .ok()
        .flatten()
        .map(|c| c.hooks)
}

/// Запоминает настройки сервера из ответа `/recall(session_start)`.
pub fn write_server_hooks(env: &Env, server: &str, hooks: HookFlags) {
    let c = HooksCache {
        fetched_at: now_rfc3339(),
        server: server.to_string(),
        hooks,
    };
    if let Err(e) = write_json_file(&hooks_path(env), &c) {
        env.log(&format!("запись hooks.json: {e}"));
    }
}

/// Имя файла сессии: очищенный id (`[A-Za-z0-9_-]`, до 64 символов), `-` и 8 hex blake3
/// исходного id — разные id с одинаковой очисткой не сталкиваются.
pub fn session_file_part(id: &str) -> String {
    let clean: String = id
        .chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let h = blake3::hash(id.as_bytes()).to_hex();
    format!("{clean}-{}", &h[..8])
}

pub fn edits_path(env: &Env, session: &str) -> PathBuf {
    env.cache_dir()
        .join(format!("edits-{}", session_file_part(session)))
}

/// Отмечает правки в сессии: маркер создаётся, если его нет (без чтения и перезаписи).
pub fn mark_edits(env: &Env, session: &str) {
    let path = edits_path(env, session);
    if path.exists() {
        return;
    }
    let r = std::fs::create_dir_all(env.cache_dir()).and_then(|_| private_options().open(&path));
    if let Err(e) = r {
        env.log(&format!("маркер правок сессии: {e}"));
    }
}

/// В сессии были правки.
pub fn had_edits(env: &Env, session: &str) -> bool {
    edits_path(env, session).is_file()
}

pub fn shown_path(env: &Env, session: &str) -> PathBuf {
    env.cache_dir()
        .join(format!("shown-{}.json", session_file_part(session)))
}

pub fn read_shown(env: &Env, session: &str) -> Shown {
    match read_json_file(&shown_path(env, session)) {
        Ok(s) => s.unwrap_or_default(),
        Err(e) => {
            env.log(&format!("shown-файл сессии битый, начат заново: {e}"));
            Shown::default()
        }
    }
}

pub fn write_shown(env: &Env, session: &str, s: &Shown) {
    if let Err(e) = write_json_file(&shown_path(env, session), s) {
        env.log(&format!("запись shown-файла сессии: {e}"));
    }
}

/// Удаляет файлы сессий (`shown-*.json`, `edits-*`, `events-*.jsonl`, `changed-*.jsonl`)
/// старше [`SHOWN_TTL`] и временные `.*.tmp` старше [`TMP_TTL`].
pub fn gc_cache(env: &Env) {
    let Ok(rd) = std::fs::read_dir(env.cache_dir()) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(n) = name.to_str() else { continue };
        let session_file = (n.starts_with("shown-") && n.ends_with(".json"))
            || n.starts_with("edits-")
            || (n.starts_with("events-") && n.ends_with(".jsonl"))
            || (n.starts_with("changed-") && n.ends_with(".jsonl"));
        let ttl = if session_file {
            SHOWN_TTL
        } else if n.starts_with('.')
            && (n.ends_with(".tmp") || n.ends_with(".lock") || n.ends_with(".trimfail"))
        {
            TMP_TTL
        } else {
            continue;
        };
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > ttl);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Запоминает текущую сессию агента проекта (`session.json`).
pub fn write_current_session(env: &Env, id: &str) {
    let c = CurrentSession {
        id: id.to_string(),
        agent: env.agent.name().to_string(),
    };
    if let Err(e) = write_json_file(&env.cache_dir().join("session.json"), &c) {
        env.log(&format!("запись session.json: {e}"));
    }
}

/// Текущая сессия агента проекта с корнем `root`: `session.json` не старше [`SESSION_TTL`].
pub fn current_session(root: &Path) -> Option<String> {
    let path = root
        .join(".zeroaimemory")
        .join("cache")
        .join("session.json");
    let age = std::fs::metadata(&path)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()?;
    if age > SESSION_TTL {
        return None;
    }
    let c: CurrentSession = read_json_file(&path).ok()??;
    let id = c.id.trim();
    (!id.is_empty() && id.chars().count() <= 128 && !id.chars().any(char::is_control))
        .then(|| id.to_string())
}

/// Сейчас в UTC, RFC 3339 с секундами (`2026-10-27T10:00:00Z`).
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339(secs)
}

/// Секунды Unix → RFC 3339 UTC (алгоритм civil-from-days, Howard Hinnant).
fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// `zam triggers sync`: принудительное обновление кэша триггеров проекта.
pub fn sync(ctx: &super::super::Ctx) -> Result<(), CliError> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let env = Env::new(
        super::Kind::SessionStart,
        super::Agent::Claude,
        std::time::Instant::now(),
        cwd,
        super::super::home_dir(),
    );
    let (r, warnings) = refresh_triggers(&env, &ctx.client, true)?;
    let n = match r {
        Refresh::Updated(n) | Refresh::NotModified(n) => n,
    };
    for w in &warnings {
        eprintln!("предупреждение сервера: {w}");
    }
    let path = triggers_path(&env.root);
    if ctx.json {
        return super::super::emit_json(&serde_json::json!({
            "triggers": n,
            "path": path.display().to_string(),
            "warnings": warnings,
        }));
    }
    super::super::emit(format!("триггеров: {n} → {}\n", path.display()).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_known_dates() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_793_102_461), "2026-10-27T12:01:01Z");
    }

    #[test]
    fn hint_line_format() {
        let t = Trigger {
            entry_id: "01J".into(),
            title: "LF  в\nскриптах".into(),
            insight_kind: "gotcha".into(),
            certainty: Some("theorem".into()),
            ..Trigger::default()
        };
        assert_eq!(
            t.hint("для **/*.sh: "),
            "ZeroAIMemory: для **/*.sh: LF в скриптах [gotcha, theorem] — i:01J"
        );
        let bare = Trigger {
            entry_id: "01K".into(),
            title: "x".into(),
            ..Trigger::default()
        };
        assert_eq!(bare.hint(""), "ZeroAIMemory: x — i:01K");
        assert!(bare.for_tool(Some("Bash")));
        let only = Trigger {
            tools: Some(vec!["bash".into()]),
            ..Trigger::default()
        };
        assert!(only.for_tool(Some("Bash")) && !only.for_tool(Some("Write")));
    }

    #[test]
    fn session_file_names_are_safe() {
        let a = session_file_part("a/b\\c:d");
        assert!(
            a.starts_with("a_b_c_d-") && a.len() == "a_b_c_d-".len() + 8,
            "{a}"
        );
        // одинаковая очистка — разные имена
        assert_ne!(session_file_part("a/b"), session_file_part("a:b"));
        assert_eq!(session_file_part(&"x".repeat(300)).len(), 64 + 1 + 8);
    }
}
