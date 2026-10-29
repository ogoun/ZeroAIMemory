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
/// Предел чтения журнала, байт (файл не должен быть больше; лишнее с головы не читается).
#[allow(dead_code)]
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

/// Усечение головы до ≤ [`TRIM_TO`] по границе строки: временный файл и rename под замком.
fn trim(p: &Path) -> std::io::Result<()> {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("events");
    let dir = p.parent().unwrap_or(Path::new("."));
    let lock = dir.join(format!(".{name}.lock"));
    let stale = std::fs::metadata(&lock)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > LOCK_TTL);
    if stale {
        let _ = std::fs::remove_file(&lock);
    }
    if std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .is_err()
    {
        // усекает другой процесс
        return Ok(());
    }
    let r = (|| {
        let bytes = std::fs::read(p)?;
        if (bytes.len() as u64) <= FILE_MAX {
            return Ok(());
        }
        let start = bytes.len() - TRIM_TO as usize;
        // первая целая строка после точки отреза
        let cut = bytes[start..]
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |i| start + i + 1);
        let keep = &bytes[cut..];
        let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
        {
            let mut f = super::cache::private_options().open(&tmp)?;
            f.write_all(keep)?;
        }
        std::fs::rename(&tmp, p).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    })();
    let _ = std::fs::remove_file(&lock);
    r
}

/// События журнала по порядку записи; битые строки пропускаются. Читатели — восстановление
/// и «ошибка → исправление» (задача 9 этапа).
#[allow(dead_code)]
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

/// Первое слово команды без аргументов и пути (`FOO=1 /usr/bin/git push` → `git`), до 64
/// символов; пусто — `None`.
pub fn exe_of(command: &str) -> Option<String> {
    let word = command
        .split_whitespace()
        .find(|w| !(w.contains('=') && !w.starts_with(['"', '\''])))?;
    let word = word.trim_matches(['"', '\'']);
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let base: String = base.chars().take(64).collect();
    (!base.is_empty()).then_some(base)
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
