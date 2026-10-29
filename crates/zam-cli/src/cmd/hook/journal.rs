//! Локальный журнал событий сессии `.zeroaimemory/cache/events-<сессия>.jsonl` (M4.5 §9.1).
//!
//! - Только метаданные: строка JSON `{ts, ev, …}` (`ts` — мс Unix), события [`Ev`]. Текста
//!   промпта (только длина), аргументов команд (только `exe` — первое слово без пути) и
//!   содержимого файлов здесь нет; ошибка — до [`ERROR_TEXT_MAX`] символов нормализованного
//!   текста ([`normalize_error`]) и отпечаток [`err_fingerprint`].
//! - **Запись** ([`append`]): одна строка ≤ [`LINE_MAX`] байт одним `write` в файл, открытый
//!   с append, — параллельные процессы `zam hook` не перемешивают строки. Файл больше
//!   [`FILE_MAX`] — голова отбрасывается до ≤ [`TRIM_TO`] через временный файл и rename;
//!   усекает один процесс (замок `.events-….lock`, брошенный старше [`LOCK_TTL`] снимается).
//!   Строка, дописанная другим процессом между чтением и rename, может потеряться — это цена
//!   записи без блокировки на горячем пути **[решение исполнителя, задача 8]**.
//! - **Чтение** ([`read`]): битые строки (обрыв, чужой формат) пропускаются.
//! - Права 0600 на unix; удаляется через 7 дней, как `shown-*` ([`super::cache::gc_cache`]).
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Предел строки журнала, байт (с переводом строки).
pub const LINE_MAX: usize = 4096;
/// Предел файла журнала, байт: больше — усечение головы.
pub const FILE_MAX: u64 = 1024 * 1024;
/// Размер файла после усечения, байт (не больше).
pub const TRIM_TO: u64 = 512 * 1024;
/// Предел текста ошибки в событии, символов.
pub const ERROR_TEXT_MAX: usize = 200;
/// Брошенный замок усечения старше — снимается.
pub const LOCK_TTL: Duration = Duration::from_secs(10);
/// После неудачного усечения следующая попытка — не раньше.
pub const TRIM_RETRY: Duration = Duration::from_secs(60);
/// Предел чтения журнала, байт (файл не должен быть больше; лишнее с головы не читается).
const READ_MAX: u64 = 2 * FILE_MAX;

/// Момент события: миллисекунды Unix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub u64);

impl Timestamp {
    pub fn now() -> Timestamp {
        Timestamp(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        )
    }
}

/// Событие сессии (§9.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum Ev {
    /// Промпт пользователя: только длина в символах.
    Prompt { len: usize },
    /// Чтение файла (путь относительно корня).
    Read { path: String },
    /// Правка файла (путь относительно корня).
    Edit { path: String },
    /// Команда: первое слово без пути; `err_fp` — отпечаток ошибки.
    Cmd {
        exe: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        err_fp: Option<String>,
    },
    /// Ошибка инструмента: нормализованный текст до [`ERROR_TEXT_MAX`] символов.
    Error {
        tool: String,
        err_fp: String,
        text: String,
    },
    /// Запуск субагента.
    Subagent {
        agent_type: String,
        agent_id: String,
    },
    /// Сжатие контекста (`manual` | `auto`).
    Compact { trigger: String },
    /// Запись в память командой `zam` (вид записи).
    MemoryWrite { kind: String },
}

impl Ev {
    /// Строковые поля, которые можно укоротить, чтобы строка уложилась в [`LINE_MAX`].
    fn shrink(&mut self, max: usize) {
        let cut = |s: &mut String| {
            if s.len() > max {
                *s = super::head_bytes(s, max).to_string();
            }
        };
        match self {
            Ev::Prompt { .. } => {}
            Ev::Read { path } | Ev::Edit { path } => cut(path),
            Ev::Cmd { exe, .. } => cut(exe),
            Ev::Error { tool, text, .. } => {
                cut(tool);
                cut(text);
            }
            Ev::Subagent {
                agent_type,
                agent_id,
            } => {
                cut(agent_type);
                cut(agent_id);
            }
            Ev::Compact { trigger } => cut(trigger),
            Ev::MemoryWrite { kind } => cut(kind),
        }
    }
}

/// Строка журнала.
#[derive(Serialize, Deserialize)]
struct Line {
    ts: Timestamp,
    #[serde(flatten)]
    ev: Ev,
}

/// Путь журнала сессии под корнем проекта `root`.
pub fn path(root: &Path, session: &str) -> PathBuf {
    root.join(".zeroaimemory").join("cache").join(format!(
        "events-{}.jsonl",
        super::cache::session_file_part(session)
    ))
}

/// Строка JSON события с переводом строки, не длиннее [`LINE_MAX`].
fn encode(ts: Timestamp, ev: &Ev) -> Vec<u8> {
    let mut ev = ev.clone();
    for max in [usize::MAX, 1024, 256, 64] {
        if max != usize::MAX {
            ev.shrink(max);
        }
        let mut b = serde_json::to_vec(&Line { ts, ev: ev.clone() }).unwrap_or_default();
        b.push(b'\n');
        if b.len() <= LINE_MAX {
            return b;
        }
    }
    Vec::new()
}

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

/// Дописывает событие одной строкой (один `write` с append); файл больше [`FILE_MAX`] —
/// усекается с головы.
pub fn append(root: &Path, session: &str, ev: &Ev) -> std::io::Result<()> {
    append_at(root, session, Timestamp::now(), ev)
}

/// [`append`] с заданным моментом (тесты, перенос событий).
pub fn append_at(root: &Path, session: &str, ts: Timestamp, ev: &Ev) -> std::io::Result<()> {
    let line = encode(ts, ev);
    if line.is_empty() {
        return Ok(());
    }
    let p = path(root, session);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let len = {
        let mut f = append_options().open(&p)?;
        f.write_all(&line)?;
        f.metadata().map(|m| m.len()).unwrap_or(0)
    };
    if len > FILE_MAX {
        trim(&p)?;
    }
    Ok(())
}

/// Событие хука в журнал сессии под корнем проекта; сбой — строка в `hooks.log`. Ключ
/// `session_journal` проверяет вызывающий.
pub fn note(env: &super::Env, session: &str, ev: &Ev) {
    if let Err(e) = append(&env.root, session, ev) {
        env.log(&format!("журнал сессии: {e}"));
    }
}

/// Усечение головы до ≤ [`TRIM_TO`] по границе строки: временный файл и rename под замком
/// (0600 на unix). Читается только хвост — последние [`TRIM_TO`] байт, не весь растущий файл
/// (fix round 1, M-11). Неудачный rename (файл держит другой процесс) оставляет метку
/// `.events-….trimfail`: следующие [`TRIM_RETRY`] попытки не делаются, `append` не перечитывает
/// файл на каждой строке. Окно потери: строка, дописанная другим процессом между чтением хвоста и
/// rename, теряется — принятая цена записи без блокировки на горячем пути (журнал этапа).
fn trim(p: &Path) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("events");
    let dir = p.parent().unwrap_or(Path::new("."));
    let fail = dir.join(format!(".{name}.trimfail"));
    if age(&fail).is_some_and(|a| a < TRIM_RETRY) {
        return Ok(());
    }
    let lock = dir.join(format!(".{name}.lock"));
    if age(&lock).is_some_and(|a| a > LOCK_TTL) {
        let _ = std::fs::remove_file(&lock);
    }
    if lock_options().open(&lock).is_err() {
        // усекает другой процесс
        return Ok(());
    }
    let r = (|| {
        let mut f = std::fs::File::open(p)?;
        let len = f.metadata()?.len();
        if len <= FILE_MAX {
            return Ok(());
        }
        f.seek(SeekFrom::Start(len - TRIM_TO))?;
        let mut tail = Vec::with_capacity(TRIM_TO as usize);
        f.take(TRIM_TO).read_to_end(&mut tail)?;
        // первая целая строка после точки отреза
        let cut = tail
            .iter()
            .position(|b| *b == b'\n')
            .map_or(tail.len(), |i| i + 1);
        let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
        {
            let mut t = super::cache::private_options().open(&tmp)?;
            t.write_all(&tail[cut..])?;
        }
        std::fs::rename(&tmp, p).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
            let _ = super::cache::private_options().open(&fail);
        })
    })();
    let _ = std::fs::remove_file(&lock);
    if r.is_ok() {
        let _ = std::fs::remove_file(&fail);
    }
    r
}

/// Возраст файла по времени изменения; нет файла — `None`.
fn age(p: &Path) -> Option<Duration> {
    std::fs::metadata(p).ok()?.modified().ok()?.elapsed().ok()
}

/// Создание файла замка: только новый, на unix — 0600.
fn lock_options() -> std::fs::OpenOptions {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o
}

/// События журнала по порядку записи; битые строки пропускаются. Читатели — сводка
/// восстановления ([`summarize`]), пары «ошибка → исправление» ([`fixes`]) и петли повторов.
pub fn read(root: &Path, session: &str) -> Vec<(Timestamp, Ev)> {
    let p = path(root, session);
    let Ok(meta) = std::fs::metadata(&p) else {
        return Vec::new();
    };
    let Ok(bytes) = std::fs::read(&p) else {
        return Vec::new();
    };
    let skip = (meta.len().saturating_sub(READ_MAX) as usize).min(bytes.len());
    bytes[skip..]
        .split(|b| *b == b'\n')
        .filter_map(|l| serde_json::from_slice::<Line>(l).ok())
        .map(|l| (l.ts, l.ev))
        .collect()
}

/// Файлов в сводке журнала.
pub const SUMMARY_FILES_MAX: usize = 10;
/// Предел текста сводки журнала, символов (§9.2).
pub const SUMMARY_MAX_CHARS: usize = 1500;
/// Файлов в паре «ошибка → исправление».
pub const FIX_FILES_MAX: usize = 5;
/// Пар в сводке журнала (последние).
const SUMMARY_FIXES_MAX: usize = 3;
/// Предел текста ошибки в строках сводки и напоминания, символов.
const FIX_ERROR_CHARS: usize = 120;

/// Пара «ошибка → исправление» (§9.3): ошибка команды и файлы, правленые до успеха той же
/// команды.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    /// Нормализованный текст ошибки (или её отпечаток, если текста в журнале нет).
    pub error: String,
    /// Правленые файлы (до [`FIX_FILES_MAX`], по порядку первой правки).
    pub files: Vec<String>,
}

/// Сводка журнала сессии для пакета восстановления (§9.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    /// Правленые файлы, до [`SUMMARY_FILES_MAX`], по порядку первой правки.
    pub edited: Vec<String>,
    /// Ошибок инструментов (`Ev::Error`).
    pub errors: u32,
    /// Текст последней ошибки.
    pub last_error: Option<String>,
    /// Пары «ошибка → исправление».
    pub fixes: Vec<Fix>,
    /// Типы субагентов без повторов.
    pub subagents: Vec<String>,
    /// Записей в память (`Ev::MemoryWrite`).
    pub memory_writes: u32,
}

/// Сводка событий журнала (весь журнал сессии — до сжатия).
pub fn summarize(events: &[(Timestamp, Ev)]) -> Summary {
    let mut s = Summary {
        fixes: fixes(events),
        ..Summary::default()
    };
    for (_, e) in events {
        match e {
            Ev::Edit { path } => {
                if s.edited.len() < SUMMARY_FILES_MAX && !s.edited.contains(path) {
                    s.edited.push(path.clone());
                }
            }
            Ev::Error { text, err_fp, .. } => {
                s.errors += 1;
                s.last_error = Some(if text.is_empty() {
                    err_fp.clone()
                } else {
                    text.clone()
                });
            }
            Ev::Subagent { agent_type, .. } => {
                if !agent_type.is_empty() && !s.subagents.contains(agent_type) {
                    s.subagents.push(agent_type.clone());
                }
            }
            Ev::MemoryWrite { .. } => s.memory_writes += 1,
            _ => {}
        }
    }
    s
}

/// Пары «ошибка → исправление» (§9.3) с моментом успеха, закрывшего пару.
pub fn fixes_at(events: &[(Timestamp, Ev)]) -> Vec<(Timestamp, Fix)> {
    // открытые ошибки по `exe`: (ошибка, правленые после неё файлы)
    let mut open: Vec<(String, String, Vec<String>)> = Vec::new();
    let mut texts: Vec<(&str, &str)> = Vec::new();
    let mut out = Vec::new();
    for (ts, e) in events {
        match e {
            Ev::Error { err_fp, text, .. } => texts.push((err_fp.as_str(), text.as_str())),
            Ev::Cmd {
                exe,
                ok: false,
                err_fp,
            } => {
                let fp = err_fp.clone().unwrap_or_default();
                let error = texts
                    .iter()
                    .rev()
                    .find(|(f, t)| *f == fp && !t.is_empty())
                    .map_or(fp.clone(), |(_, t)| t.to_string());
                // повтор той же команды с ошибкой — последняя ошибка, правки копятся дальше
                match open.iter_mut().find(|(x, _, _)| x == exe) {
                    Some(o) => o.1 = error,
                    None => open.push((exe.clone(), error, Vec::new())),
                }
            }
            Ev::Edit { path } => {
                for (_, _, files) in &mut open {
                    if files.len() < FIX_FILES_MAX && !files.contains(path) {
                        files.push(path.clone());
                    }
                }
            }
            Ev::Cmd { exe, ok: true, .. } => {
                if let Some(i) = open.iter().position(|(x, _, _)| x == exe) {
                    let (_, error, files) = open.remove(i);
                    // успех без правок — не исправление (ошибка была временной)
                    if !files.is_empty() && !error.is_empty() {
                        out.push((*ts, Fix { error, files }));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Пары «ошибка → исправление» (§9.3): `Cmd {ok: false, err_fp}` → правки (`Edit`) → `Cmd`
/// того же `exe` с `ok: true`. Успех без правок пару не даёт.
pub fn fixes(events: &[(Timestamp, Ev)]) -> Vec<Fix> {
    fixes_at(events).into_iter().map(|(_, f)| f).collect()
}

/// Ошибка пары для строк сводки и напоминания: до [`FIX_ERROR_CHARS`] символов.
pub fn fix_error(f: &Fix) -> String {
    clip(&f.error, FIX_ERROR_CHARS)
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// Текст сводки (§9.2), не длиннее [`SUMMARY_MAX_CHARS`] символов: «До сжатия в этой сессии:
/// правились …; ошибки: N (последняя: …); исправлены: …; субагенты: …; записей в память: N».
/// Пустые части опускаются; в журнале нет ничего из перечисленного — пусто.
pub fn render_summary(s: &Summary) -> String {
    if s.edited.is_empty() && s.errors == 0 && s.subagents.is_empty() && s.memory_writes == 0 {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    if !s.edited.is_empty() {
        parts.push(format!("правились {}", s.edited.join(", ")));
    }
    if s.errors > 0 {
        let mut p = format!("ошибки: {}", s.errors);
        if let Some(e) = &s.last_error {
            p.push_str(&format!(" (последняя: {})", clip(e, FIX_ERROR_CHARS)));
        }
        parts.push(p);
    }
    if !s.fixes.is_empty() {
        let skip = s.fixes.len().saturating_sub(SUMMARY_FIXES_MAX);
        let pairs: Vec<String> = s.fixes[skip..]
            .iter()
            .map(|f| format!("{} → {}", fix_error(f), f.files.join(", ")))
            .collect();
        parts.push(format!("исправлены: {}", pairs.join("; ")));
    }
    if !s.subagents.is_empty() {
        parts.push(format!("субагенты: {}", s.subagents.join(", ")));
    }
    parts.push(format!("записей в память: {}", s.memory_writes));
    let text = format!(
        "ZeroAIMemory: До сжатия в этой сессии: {}",
        parts.join("; ")
    );
    clip(&text, SUMMARY_MAX_CHARS)
}

/// Отпечаток нормализованной ошибки: первые 16 hex blake3.
pub fn err_fingerprint(normalized: &str) -> String {
    blake3::hash(normalized.as_bytes()).to_hex()[..16].to_string()
}

/// Нормализованная ошибка для отпечатка и текста события: первые 4 КиБ без ANSI-escape, пути и
/// URL (токены с `/` или `\`), числа, `0x…` и hex от 7 знаков с цифрой — заменяются, числовые
/// хвосты позиции (`a.rs:12:5`) снимаются, пробелы схлопываются **[решение исполнителя]**:
/// одна и та же ошибка с другим номером строки или временным путём даёт тот же отпечаток.
pub fn normalize_error(text: &str) -> String {
    let text = super::head_bytes(text, 4096);
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI: ESC [ … буква
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        clean.push(if c.is_control() { ' ' } else { c });
    }
    let mut out: Vec<String> = Vec::new();
    for tok in clean.split_whitespace() {
        let word = if tok.contains('/') || tok.contains('\\') {
            "<path>".to_string()
        } else if !tok.chars().any(char::is_alphabetic) {
            "<n>".to_string()
        } else if is_hexish(tok) {
            "<hex>".to_string()
        } else {
            strip_position(tok).to_string()
        };
        out.push(word);
    }
    out.join(" ")
}

/// Снимает числовые хвосты позиции `:12:5` (`a.rs:12:5` → `a.rs`; `error:` не меняется).
fn strip_position(mut t: &str) -> &str {
    loop {
        let digits = t.len() - t.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            return t;
        }
        match t[..t.len() - digits].strip_suffix(':') {
            Some(r) if !r.is_empty() => t = r,
            _ => return t,
        }
    }
}

/// `0x…` или hex от 7 знаков, содержащий цифру (хэш, адрес).
fn is_hexish(t: &str) -> bool {
    let t = t.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return !h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit());
    }
    t.len() >= 7
        && t.bytes().all(|b| b.is_ascii_hexdigit())
        && t.bytes().any(|b| b.is_ascii_digit())
}

/// Текст ошибки для события: нормализованный, до [`ERROR_TEXT_MAX`] символов.
pub fn error_text(normalized: &str) -> String {
    normalized.chars().take(ERROR_TEXT_MAX).collect()
}

/// Слова команды с учётом кавычек `"…"` и `'…'` (и `\\` вне одинарных) — грубый разбор
/// shell, достаточный для первого слова.
fn shell_words(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            // в двойных кавычках `\` экранирует только `"`, `\`, `$` и `` ` `` (POSIX):
            // `"C:\Tools\zam.exe"` остаётся путём Windows
            (Some('"'), '\\') => match chars.clone().next() {
                Some(n @ ('"' | '\\' | '$' | '`')) => {
                    chars.next();
                    cur.push(n);
                }
                _ => cur.push('\\'),
            },
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            (None, c) => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// Присваивание окружения перед командой: `ИМЯ=…` (имя — `[A-Za-z_][A-Za-z0-9_]*`).
fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut cs = name.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Первое слово команды без аргументов и пути (`FOO="a b" /usr/bin/git push` → `git`), до 64
/// символов; пусто — `None`. Слова — с учётом кавычек (fix round 1, M-3).
pub fn exe_of(command: &str) -> Option<String> {
    let words = shell_words(command);
    let word = words.iter().find(|w| !is_assignment(w))?;
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let base: String = base.chars().take(64).collect();
    (!base.is_empty()).then_some(base)
}

/// Замена секрета в тексте ошибки события.
pub const SECRET: &str = "‹secret›";

/// Ключ похож на секрет: содержит `token`, `secret`, `password`, `passwd`, `api_key`/`apikey`
/// или равен `authorization` (без учёта регистра, `-` как `_`).
fn secret_key(k: &str) -> bool {
    let k = k
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .to_lowercase()
        .replace('-', "_");
    !k.is_empty()
        && (["token", "secret", "password", "passwd", "api_key", "apikey"]
            .iter()
            .any(|w| k.contains(w))
            || k == "authorization")
}

/// Длинный (≥ 32) высокоэнтропийный токен: символы base64/hex/`-_.`, буквы и цифры вперемешку,
/// энтропия Шеннона ≥ 3,5 бита на символ.
fn high_entropy(t: &str) -> bool {
    let t = t.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | ')' | '(' | '[' | ']'));
    if t.chars().count() < 32
        || !t
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_' | '.'))
        || !t.chars().any(|c| c.is_ascii_digit())
        || !t.chars().any(|c| c.is_ascii_alphabetic())
    {
        return false;
    }
    let mut freq = std::collections::HashMap::new();
    for c in t.chars() {
        *freq.entry(c).or_insert(0usize) += 1;
    }
    let n = t.chars().count() as f64;
    let h: f64 = freq
        .values()
        .map(|&k| {
            let p = k as f64 / n;
            -p * p.log2()
        })
        .sum();
    h >= 3.5
}

/// Маскирует секреты в тексте ошибки перед записью в журнал (fix round 1, M-4; в `zam-cli` нет
/// сканера секретов `zam-markdown` — простая функция): `ключ=значение` и `ключ: значение` для
/// ключей [`secret_key`], `Bearer <…>`/`Basic <…>` и длинные высокоэнтропийные токены →
/// [`SECRET`].
pub fn mask_secrets(text: &str) -> String {
    let words: Vec<&str> = text.split(' ').collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut mask_next = false;
    for w in words {
        if mask_next && !w.is_empty() {
            // `Authorization: Bearer <…>` — схема остаётся, маскируется значение
            if matches!(w.to_ascii_lowercase().as_str(), "bearer" | "basic") {
                out.push(w.to_string());
            } else {
                out.push(SECRET.to_string());
                mask_next = false;
            }
            continue;
        }
        if let Some((k, v)) = w.split_once(['=', ':'])
            && secret_key(k)
        {
            if v.is_empty() {
                // `password: x`, `token= x`
                out.push(w.to_string());
                mask_next = true;
            } else {
                let sep = &w[k.len()..k.len() + 1];
                out.push(format!("{k}{sep}{SECRET}"));
            }
            continue;
        }
        let lw = w.to_ascii_lowercase();
        if lw == "bearer" || lw == "basic" {
            out.push(w.to_string());
            mask_next = true;
            continue;
        }
        if secret_key(w) && w.ends_with(':') {
            out.push(w.to_string());
            mask_next = true;
            continue;
        }
        if high_entropy(w) {
            out.push(SECRET.to_string());
            continue;
        }
        out.push(w.to_string());
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp/zam-cli-journal")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn all_events() -> Vec<Ev> {
        vec![
            Ev::Prompt { len: 42 },
            Ev::Read {
                path: "src/a.rs".into(),
            },
            Ev::Edit {
                path: "docs/б в.md".into(),
            },
            Ev::Cmd {
                exe: "cargo".into(),
                ok: true,
                err_fp: None,
            },
            Ev::Cmd {
                exe: "git".into(),
                ok: false,
                err_fp: Some("0123456789abcdef".into()),
            },
            Ev::Error {
                tool: "Bash".into(),
                err_fp: "0123456789abcdef".into(),
                text: "bad interpreter".into(),
            },
            Ev::Subagent {
                agent_type: "Explore".into(),
                agent_id: "a1".into(),
            },
            Ev::Compact {
                trigger: "auto".into(),
            },
            Ev::MemoryWrite {
                kind: "insight".into(),
            },
        ]
    }

    #[test]
    fn journal_roundtrip_all_events() {
        let r = root("roundtrip");
        let evs = all_events();
        for (i, e) in evs.iter().enumerate() {
            append_at(&r, "s1", Timestamp(1000 + i as u64), e).unwrap();
        }
        let back = read(&r, "s1");
        assert_eq!(back.len(), evs.len());
        for (i, (ts, e)) in back.iter().enumerate() {
            assert_eq!(*ts, Timestamp(1000 + i as u64));
            assert_eq!(e, &evs[i]);
        }
        let text = std::fs::read_to_string(path(&r, "s1")).unwrap();
        let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(
            first,
            serde_json::json!({"ts": 1000, "ev": "prompt", "len": 42})
        );
        // другая сессия — другой файл
        assert!(read(&r, "s2").is_empty());
        // длинное поле укорачивается, строка ≤ 4 КиБ
        append(
            &r,
            "s3",
            &Ev::Read {
                path: "я".repeat(5000),
            },
        )
        .unwrap();
        let t = std::fs::read(path(&r, "s3")).unwrap();
        assert!(t.len() <= LINE_MAX);
        assert_eq!(read(&r, "s3").len(), 1);
    }

    /// Review Focus 5: 8 потоков × 500 строк дописывают параллельно; все строки разбираются;
    /// файл > 1 МБ усекается до ≤ 512 КиБ по границе строки, последние строки целы.
    #[test]
    fn journal_concurrent_appends_parse_and_trim() {
        let r = root("concurrent");
        let filler = "x".repeat(300);
        std::thread::scope(|sc| {
            for t in 0..8 {
                let (r, filler) = (&r, &filler);
                sc.spawn(move || {
                    for i in 0..500 {
                        let ev = Ev::Read {
                            path: format!("t{t}/{i}/{filler}"),
                        };
                        append(r, "par", &ev).unwrap();
                    }
                });
            }
        });
        // всего ~1,3 МБ — усечение было; каждая оставшаяся строка — целое событие
        let p = path(&r, "par");
        let bytes = std::fs::read(&p).unwrap();
        assert!(bytes.len() as u64 <= FILE_MAX, "{}", bytes.len());
        assert!(bytes.ends_with(b"\n"));
        let lines: Vec<&[u8]> = bytes
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .collect();
        let evs = read(&r, "par");
        assert_eq!(evs.len(), lines.len(), "битых строк нет");
        assert!(evs.len() < 8 * 500, "голова отброшена");
        // последняя строка каждого потока, дописанная после последнего усечения, цела; финальная
        // запись после всех потоков — последняя в файле
        append(&r, "par", &Ev::Prompt { len: 7 }).unwrap();
        let evs = read(&r, "par");
        assert_eq!(evs.last().unwrap().1, Ev::Prompt { len: 7 });
        // однопоточно до превышения: сразу после усечения файл ≤ 512 КиБ
        let mut prev = std::fs::metadata(&p).unwrap().len();
        loop {
            append(
                &r,
                "par",
                &Ev::Read {
                    path: filler.clone(),
                },
            )
            .unwrap();
            let len = std::fs::metadata(&p).unwrap().len();
            if len < prev {
                assert!(len <= TRIM_TO, "после усечения {len}");
                break;
            }
            prev = len;
        }
        assert_eq!(
            read(&r, "par").last().unwrap().1,
            Ev::Read {
                path: filler.clone()
            }
        );
        // замок усечения снят
        let dir = p.parent().unwrap();
        assert!(
            std::fs::read_dir(dir)
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().ends_with(".lock"))
        );
    }

    #[test]
    fn journal_skips_broken_line() {
        let r = root("broken");
        append_at(&r, "s", Timestamp(1), &Ev::Prompt { len: 1 }).unwrap();
        let p = path(&r, "s");
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"{\"ts\":2,\"ev\":\"read\",\"pa\n").unwrap();
        f.write_all(b"not json\n{\"ts\":3,\"ev\":\"unknown\"}\n\xff\xfe\n")
            .unwrap();
        drop(f);
        append_at(&r, "s", Timestamp(4), &Ev::Prompt { len: 4 }).unwrap();
        let evs = read(&r, "s");
        assert_eq!(
            evs,
            vec![
                (Timestamp(1), Ev::Prompt { len: 1 }),
                (Timestamp(4), Ev::Prompt { len: 4 })
            ]
        );
    }

    #[test]
    fn journal_has_no_prompt_text_or_command_args() {
        assert_eq!(
            exe_of("FOO=1 /usr/bin/git push --force secret-token").as_deref(),
            Some("git")
        );
        assert_eq!(
            exe_of("\"C:\\Tools\\zam.exe\" log").as_deref(),
            Some("zam.exe")
        );
        assert_eq!(exe_of("   "), None);
        let r = root("privacy");
        let prompt = "мой секретный промпт с паролем hunter2";
        append(
            &r,
            "s",
            &Ev::Prompt {
                len: prompt.chars().count(),
            },
        )
        .unwrap();
        append(
            &r,
            "s",
            &Ev::Cmd {
                exe: exe_of("curl -H 'Authorization: Bearer sk-123' https://x").unwrap(),
                ok: true,
                err_fp: None,
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(path(&r, "s")).unwrap();
        for leak in ["секретный", "hunter2", "Bearer", "sk-123", "https://x"] {
            assert!(!text.contains(leak), "{leak}: {text}");
        }
        assert!(text.contains("\"exe\":\"curl\""), "{text}");
    }

    #[test]
    fn exe_of_quoted_assignments() {
        assert_eq!(exe_of("API_KEY=\"sk live\" make").as_deref(), Some("make"));
        assert_eq!(
            exe_of("GIT_AUTHOR_NAME=\"John Doe\" git commit -m 'x y'").as_deref(),
            Some("git")
        );
        assert_eq!(
            exe_of("A='1 2' B=3 ./run.sh --x").as_deref(),
            Some("run.sh")
        );
        assert_eq!(
            exe_of("\"/opt/my tools/bin/cargo\" test").as_deref(),
            Some("cargo")
        );
        // `=` не в имени переменной — команда
        assert_eq!(exe_of("--x=1 foo").as_deref(), Some("--x=1"));
        assert_eq!(exe_of("A=1"), None);
    }

    #[test]
    fn secrets_masked_in_error_text() {
        let m = mask_secrets(
            "fail token=abc123 api_key: sk-live-1 password=hunter2 Authorization: Bearer eyJhbGciOiJIUzI1 x",
        );
        for leak in ["abc123", "sk-live-1", "hunter2", "eyJhbGciOiJIUzI1"] {
            assert!(!m.contains(leak), "{leak}: {m}");
        }
        assert!(m.contains("token=‹secret›"), "{m}");
        assert!(m.starts_with("fail ") && m.ends_with(" x"), "{m}");
        let k = mask_secrets("key AKIAIOSFODNN7EXAMPLEwJalrXUtnFEMIK7MDENGbPxRfiCY rejected");
        assert_eq!(k, "key ‹secret› rejected");
        // обычные слова и пути не трогаются
        let plain = "error: <path> line <n> at <hex> cannot find module_resolution_strategy";
        assert_eq!(mask_secrets(plain), plain);
        assert_eq!(
            mask_secrets("MY_SECRET_TOKEN=x1"),
            "MY_SECRET_TOKEN=‹secret›"
        );
    }

    fn evs(list: Vec<Ev>) -> Vec<(Timestamp, Ev)> {
        list.into_iter()
            .enumerate()
            .map(|(i, e)| (Timestamp(i as u64 + 1), e))
            .collect()
    }

    fn fail(exe: &str, fp: &str, text: &str) -> Vec<Ev> {
        vec![
            Ev::Error {
                tool: "Bash".into(),
                err_fp: fp.into(),
                text: text.into(),
            },
            Ev::Cmd {
                exe: exe.into(),
                ok: false,
                err_fp: Some(fp.into()),
            },
        ]
    }

    fn ok(exe: &str) -> Ev {
        Ev::Cmd {
            exe: exe.into(),
            ok: true,
            err_fp: None,
        }
    }

    fn edit(p: &str) -> Ev {
        Ev::Edit { path: p.into() }
    }

    #[test]
    fn summarize_events() {
        let mut l = vec![Ev::Prompt { len: 3 }];
        for i in 0..12 {
            l.push(edit(&format!("f{i}.rs")));
        }
        l.push(edit("f0.rs"));
        l.extend(fail("cargo", "aa", "error E0425 first"));
        l.push(edit("a.rs"));
        l.push(ok("cargo"));
        l.extend(fail("npm", "bb", "npm ERR last"));
        for t in ["Explore", "Plan", "Explore"] {
            l.push(Ev::Subagent {
                agent_type: t.into(),
                agent_id: "x".into(),
            });
        }
        for k in ["insight", "changelog"] {
            l.push(Ev::MemoryWrite { kind: k.into() });
        }
        let s = summarize(&evs(l));
        assert_eq!(s.edited.len(), SUMMARY_FILES_MAX);
        assert_eq!(s.edited[0], "f0.rs");
        assert_eq!(s.errors, 2);
        assert_eq!(s.last_error.as_deref(), Some("npm ERR last"));
        assert_eq!(s.subagents, ["Explore", "Plan"]);
        assert_eq!(s.memory_writes, 2);
        // a.rs — 14-я правка: в список «правились» не входит, но в пару — да
        assert_eq!(
            s.fixes,
            [Fix {
                error: "error E0425 first".into(),
                files: vec!["a.rs".into()]
            }]
        );
        let t = render_summary(&s);
        assert!(
            t.starts_with("ZeroAIMemory: До сжатия в этой сессии: правились f0.rs, f1.rs"),
            "{t}"
        );
        assert!(
            t.contains("; ошибки: 2 (последняя: npm ERR last); исправлены: error E0425 first → a.rs; субагенты: Explore, Plan; записей в память: 2"),
            "{t}"
        );
        assert!(t.chars().count() <= SUMMARY_MAX_CHARS);
        // пустой журнал — без сводки; только записи — одна часть
        assert_eq!(render_summary(&summarize(&[])), "");
        let w = summarize(&evs(vec![Ev::MemoryWrite {
            kind: "task".into(),
        }]));
        assert_eq!(
            render_summary(&w),
            "ZeroAIMemory: До сжатия в этой сессии: записей в память: 1"
        );
        // длинная сводка обрезается
        let long = Summary {
            edited: vec!["я".repeat(400); 10],
            ..Summary::default()
        };
        let t = render_summary(&long);
        assert!(t.chars().count() <= SUMMARY_MAX_CHARS);
        assert!(t.chars().count() > SUMMARY_MAX_CHARS - 3 && t.ends_with('…'));
    }

    #[test]
    fn fixes_pairs_detected() {
        let mut l = fail("cargo", "aa", "error[E0425]: cannot find value");
        l.extend([edit("a.rs"), edit("b.rs"), edit("a.rs"), ok("cargo")]);
        let f = fixes(&evs(l.clone()));
        assert_eq!(
            f,
            [Fix {
                error: "error[E0425]: cannot find value".into(),
                files: vec!["a.rs".into(), "b.rs".into()]
            }]
        );
        let at = fixes_at(&evs(l));
        assert_eq!(at[0].0, Timestamp(6), "момент — успех, закрывший пару");
        // успех без правок — не пара; повтор ошибки — последняя ошибка
        let mut l = fail("cargo", "aa", "first");
        l.push(ok("cargo"));
        l.extend(fail("cargo", "bb", "second"));
        l.extend(fail("cargo", "cc", "third"));
        l.extend([edit("c.rs"), ok("cargo")]);
        let f = fixes(&evs(l));
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].error, "third");
        assert_eq!(f[0].files, ["c.rs"]);
    }

    #[test]
    fn fix_not_detected_other_exe() {
        let mut l = fail("cargo", "aa", "error E0425");
        l.extend([edit("a.rs"), ok("npm"), ok("git")]);
        assert!(fixes(&evs(l.clone())).is_empty());
        // ошибка инструмента без команды (Edit) пару не открывает
        let l = vec![
            Ev::Error {
                tool: "Edit".into(),
                err_fp: "dd".into(),
                text: "old_string not found".into(),
            },
            edit("a.rs"),
            ok("cargo"),
        ];
        assert!(fixes(&evs(l)).is_empty());
    }

    #[test]
    fn error_normalization_and_fingerprint() {
        let a = normalize_error(
            "\u{1b}[31merror\u{1b}[0m: /tmp/x1/a.rs:12:5 line 42 at 0x7ffd1234 deadbeef1",
        );
        let b = normalize_error("error: C:\\t\\b.rs:3:1 line 7 at 0x10 0123abc9");
        assert_eq!(a, b, "{a} | {b}");
        assert_eq!(a, "error: <path> line <n> at <hex> <hex>");
        let fp = err_fingerprint(&a);
        assert_eq!(fp.len(), 16);
        assert!(fp.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(normalize_error("E0425 in main.rs:3"), "E0425 in main.rs");
        assert_eq!(error_text(&"я".repeat(300)).chars().count(), ERROR_TEXT_MAX);
    }
}
