//! Автосинхронизация карты кода из хуков (спецификация M5.6 §5, §10; решения контроллера §15
//! п. 5 и составителя плана 7): `session-start` и `checkpoint` (после правок) запускают
//! `zam code sync --background` отдельным процессом и не ждут его.
//!
//! - **Условия** ([`maybe_spawn_sync`]): решение есть, `code.enabled && code.auto`, тип подсказок
//!   `code_map` не выключен (клиент и кэш сервера); прошло не меньше `code.auto_interval_min`
//!   с последней успешной синхронизации (`cache/code-sync.json`) и с последнего запуска из хука
//!   (`cache/code-auto.json` — иначе неудачная фоновая синхронизация, например без сервера,
//!   запускалась бы каждым хуком) или `HEAD` другой; замок `cache/code-sync.lock` не занят
//!   (`File::try_lock` — открыть, попробовать, сразу отпустить: блокировку ОС держит идущая
//!   `zam code sync`, содержимое файла не читается — на Windows занятый файл не читается).
//! - **Без процесса `git`:** `HEAD` читается из файлов `.git` ([`git_head_fast`]: `.git/HEAD`,
//!   файл ветки или `packed-refs`; `.git`-файл worktree — `gitdir:` и `commondir`).
//! - **Запуск** — `current_exe() code sync --background`, stdio — null. Windows —
//!   `CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP` **[решение исполнителя]**: своя консоль без
//!   окна, которую наследуют дочерние `git.exe` (с `DETACHED_PROCESS` консоли нет, флаг
//!   `CREATE_NO_WINDOW` по документации `CreateProcess` тогда игнорируется, и каждый `git.exe`
//!   открыл бы своё окно); Ctrl+C консоли агента до процесса не доходит. Стандартные
//!   дескрипторы хука перед запуском становятся ненаследуемыми (иначе фоновый процесс держал бы
//!   канал вывода хука, и агент ждал бы его конца). Unix — своя группа процессов
//!   (`process_group(0)`). Бюджет — ≤ 10 мс к хуку (§10); время запуска — в строке `hooks.log`.
//! - **Fail-open:** любая ошибка — строка в `hooks.log`, хук продолжает работу.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::config;
use super::state::{SyncMark, cache_dir};
use crate::cmd::hook::{Env, Flag, Settings, cache};

/// Предел `packed-refs`, который хук читает построчно, байт.
const PACKED_REFS_MAX: u64 = 1024 * 1024;

/// Предел читаемых файлов `.git` и кэша, байт (`packed-refs` больших репозиториев — мегабайты).
const READ_MAX: u64 = 16 * 1024 * 1024;

/// Последний запуск фоновой синхронизации из хука.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Attempt {
    /// Секунды Unix.
    at: u64,
    #[serde(default)]
    head: Option<String>,
}

fn attempt_path(root: &Path) -> PathBuf {
    cache_dir(root).join("code-auto.json")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Небольшой текстовый файл; нет, больше [`READ_MAX`] или не UTF-8 — `None`.
fn read_small(path: &Path) -> Option<String> {
    let m = std::fs::metadata(path).ok()?;
    if !m.is_file() || m.len() > READ_MAX {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// sha коммита: 40 или 64 hex — в нижнем регистре.
fn sha_of(s: &str) -> Option<String> {
    let s = s.trim();
    (matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| s.to_ascii_lowercase())
}

/// `HEAD` репозитория над `root` без процесса `git`: `.git/HEAD` — sha (отсоединённый) или
/// `ref: refs/…` → файл ссылки (в каталоге worktree, затем в общем) или строка `packed-refs`.
/// Не git, битые файлы или ссылки ещё нет (пустой репозиторий) — `None`.
pub fn git_head_fast(root: &Path) -> Option<String> {
    let dot = root
        .ancestors()
        .map(|d| d.join(".git"))
        .find(|p| p.exists())?;
    let gitdir = if dot.is_file() {
        // worktree и подмодуль: `gitdir: <путь>` (относительно каталога файла)
        let line = read_small(&dot)?;
        let p = PathBuf::from(line.trim().strip_prefix("gitdir:")?.trim());
        if p.is_absolute() {
            p
        } else {
            dot.parent()?.join(p)
        }
    } else {
        dot
    };
    let head = read_small(&gitdir.join("HEAD"))?;
    let Some(r) = head.trim().strip_prefix("ref:") else {
        return sha_of(&head);
    };
    let r = r.trim();
    if r.is_empty() || r.contains("..") || !r.starts_with("refs/") {
        return None;
    }
    let common = read_small(&gitdir.join("commondir"))
        .map(|c| {
            let p = PathBuf::from(c.trim());
            if p.is_absolute() { p } else { gitdir.join(p) }
        })
        .unwrap_or_else(|| gitdir.clone());
    for base in [&gitdir, &common] {
        if let Some(sha) = read_small(&base.join(r)).as_deref().and_then(sha_of) {
            return Some(sha);
        }
    }
    packed_ref(&common.join("packed-refs"), r)
}

/// sha ссылки `r` в `packed-refs`: построчно с ранним выходом; файл больше
/// [`PACKED_REFS_MAX`] — `None` (запуск — только по времени; ревью задачи 6, M6).
fn packed_ref(path: &Path, r: &str) -> Option<String> {
    use std::io::BufRead;
    let f = std::fs::File::open(path).ok()?;
    if f.metadata().ok()?.len() > PACKED_REFS_MAX {
        return None;
    }
    std::io::BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.starts_with(['#', '^']))
        .find_map(|l| {
            let (sha, name) = l.split_once(' ')?;
            (name.trim() == r).then(|| sha_of(sha)).flatten()
        })
}

/// Секунды Unix из RFC 3339 (как пишет `cache::now_rfc3339`); иное — `None`
/// (`zam_core::time::unix_secs`: один календарь на сервер и клиент).
fn rfc3339_secs(s: &str) -> Option<u64> {
    zam_core::time::unix_secs(s.trim()).and_then(|v| u64::try_from(v).ok())
}

/// Пора ли запускать: опорная точка — самое позднее из успешной синхронизации этого решения и
/// запуска из хука; её нет — пора; иначе — прошло `interval_min` или `HEAD` другой. `HEAD`
/// опоры неизвестен (синхронизация без git: нет git, «dubious ownership») — сравнение с `HEAD`
/// последнего запуска из хука, нет и его — только по времени (ревью задачи 6, M5).
fn due(
    mark: Option<&SyncMark>,
    attempt: Option<&Attempt>,
    solution: &str,
    now: u64,
    head: Option<&str>,
    interval_min: u32,
) -> bool {
    let ok = mark
        .filter(|m| m.solution == solution)
        .and_then(|m| Some((rfc3339_secs(&m.last_ok_at)?, m.head.as_deref())));
    let tried = attempt.map(|a| (a.at, a.head.as_deref()));
    let Some((at, last_head)) = [ok, tried].into_iter().flatten().max_by_key(|(t, _)| *t) else {
        return true;
    };
    let last_head = last_head.or_else(|| attempt.and_then(|a| a.head.as_deref()));
    now.saturating_sub(at) >= u64::from(interval_min) * 60
        || head.is_some_and(|h| last_head.is_some_and(|l| l != h))
}

/// Идёт синхронизация проекта: замок `cache/code-sync.lock` занят другим процессом (как у
/// `sync::SyncLock` — блокировка ОС). Нет файла или не открывается — не идёт (фоновый процесс
/// сам выйдет с «уже идёт», если ошибся хук).
fn lock_busy(root: &Path) -> bool {
    let path = cache_dir(root).join("code-sync.lock");
    let Ok(f) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
    else {
        return false;
    };
    match f.try_lock() {
        Ok(()) => {
            let _ = f.unlock();
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(_) => false,
    }
}

/// Запуск фоновой синхронизации карты из хука `session-start` или `checkpoint`, если пора (см.
/// модуль); `true` — процесс запущен. Хук его не ждёт; ошибки — в `hooks.log`.
pub fn maybe_spawn_sync(env: &Env, s: &Settings) -> bool {
    let t0 = Instant::now();
    let Some(solution) = env.solution.as_ref().map(|x| x.id.as_str()) else {
        return false;
    };
    if !s.flag_on(Flag::CodeMap) {
        return false;
    }
    let cfg = match config::load(&env.home, &env.root) {
        Ok(c) => c,
        Err(e) => {
            env.log(&format!("карта кода: [code] не прочитан: {}", e.msg));
            return false;
        }
    };
    if !cfg.enabled || !cfg.auto {
        return false;
    }
    let now = now_secs();
    let head = git_head_fast(&env.root);
    let mark = SyncMark::load(&env.root);
    let attempt: Option<Attempt> =
        read_small(&attempt_path(&env.root)).and_then(|t| serde_json::from_str(&t).ok());
    if !due(
        mark.as_ref(),
        attempt.as_ref(),
        solution,
        now,
        head.as_deref(),
        cfg.auto_interval_min,
    ) || lock_busy(&env.root)
    {
        return false;
    }
    let checks = t0.elapsed();
    if let Err(e) = spawn(&env.cwd) {
        env.log(&format!(
            "карта кода: фоновая синхронизация не запущена: {e}"
        ));
        return false;
    }
    let spawned = t0.elapsed();
    let a = Attempt { at: now, head };
    if let Err(e) = serde_json::to_vec(&a)
        .map_err(std::io::Error::other)
        .and_then(|b| cache::write_atomic(&attempt_path(&env.root), &b))
    {
        env.log(&format!("карта кода: code-auto.json: {e}"));
    }
    // время — по частям: проверки, создание процесса и отметка запуска `code-auto.json`.
    // Редкие десятки мс здесь — отметка: создание временного файла на занятом диске (замер
    // 17b-http M6: создание процесса — p95 3 мс, «горб» 45–120 мс — у записи файла и без запуска
    // процесса), а не `CreateProcess`
    let total = t0.elapsed();
    let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
    env.log(&format!(
        "карта кода: фоновая синхронизация карты запущена за {:.2} мс (проверки {:.2} мс, процесс \
         {:.2} мс, отметка {:.2} мс)",
        ms(total),
        ms(checks),
        ms(spawned.saturating_sub(checks)),
        ms(total.saturating_sub(spawned))
    ));
    true
}

/// `current_exe() code sync --background` в каталоге `cwd`, без окна консоли и без ожидания.
fn spawn(cwd: &Path) -> std::io::Result<()> {
    let mut c = Command::new(std::env::current_exe()?);
    c.args(["code", "sync", "--background"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
        std_handles_not_inherited();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    // процесс не ждётся: `Child` без `wait` (unix — после выхода хука его подберёт init)
    c.spawn().map(drop)
}

/// Стандартные дескрипторы хука — ненаследуемые: `CreateProcess` с `bInheritHandles` передал
/// бы фоновому процессу канал вывода хука, и агент ждал бы конца синхронизации.
#[cfg(windows)]
fn std_handles_not_inherited() {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetHandleInformation(h: *mut c_void, mask: u32, flags: u32) -> i32;
    }
    let handles = [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ];
    for h in handles {
        if h.is_null() {
            continue;
        }
        // SAFETY: дескрипторы стандартных потоков процесса живут до его конца; функция только
        // меняет флаг наследования, результат не важен (fail-open).
        unsafe {
            SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp/zam-cli-code-auto")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rfc3339_round_trip() {
        assert_eq!(rfc3339_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339_secs("2000-02-29T00:00:00Z"), Some(951_782_400));
        assert_eq!(rfc3339_secs("2026-10-27T12:01:01Z"), Some(1_793_102_461));
        assert_eq!(
            rfc3339_secs("2026-10-27T12:01:01.123Z"),
            Some(1_793_102_461)
        );
        let now = cache::now_rfc3339();
        let back = rfc3339_secs(&now).unwrap();
        assert!(now_secs().abs_diff(back) <= 2, "{now}");
        for bad in ["", "2026-10-27", "2026-13-01T00:00:00Z", "x-y-zTa:b:cZ"] {
            assert_eq!(rfc3339_secs(bad), None, "{bad}");
        }
    }

    /// Прежний разбор клиента (до M6, задача 6) — эталон совпадения.
    fn legacy_rfc3339_secs(s: &str) -> Option<u64> {
        let s = s.trim().strip_suffix('Z')?;
        let (date, time) = s.split_once('T')?;
        let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
        let (y, m, dd) = (d.next()??, d.next()??, d.next()??);
        let time = time.split('.').next()?;
        let mut t = time.split(':').map(|x| x.parse::<i64>().ok());
        let (hh, mi, ss) = (t.next()??, t.next()??, t.next()??);
        if !(1..=12).contains(&m) || !(1..=31).contains(&dd) || hh > 23 || mi > 59 || ss > 60 {
            return None;
        }
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + dd - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        u64::try_from(days * 86_400 + hh * 3600 + mi * 60 + ss).ok()
    }

    /// `zam_core::time::unix_secs` разбирает метки клиента так же, как прежний календарь.
    #[test]
    fn rfc3339_secs_matches_legacy() {
        for secs in (0..4_102_444_800_u64).step_by(3_599_993) {
            for s in [
                zam_core::time::rfc3339_from_secs(secs as i64),
                zam_core::time::rfc3339_from_secs(secs as i64).replace('Z', ".123456Z"),
            ] {
                assert_eq!(rfc3339_secs(&s), legacy_rfc3339_secs(&s), "{s}");
            }
        }
    }

    #[test]
    fn due_by_interval_head_and_latest_point() {
        let mark = |at: &str, head: Option<&str>| SyncMark {
            server: "s".into(),
            solution: "g".into(),
            last_ok_at: at.into(),
            head: head.map(str::to_string),
        };
        let t = 1_793_102_461; // 2026-10-27T12:01:01Z
        let m = mark("2026-10-27T12:01:01Z", Some("a"));
        // ни синхронизации, ни запуска — пора
        assert!(due(None, None, "g", t, Some("a"), 30));
        // свежая синхронизация, тот же HEAD — нет; другой HEAD или прошло 30 мин — да
        assert!(!due(Some(&m), None, "g", t + 60, Some("a"), 30));
        assert!(due(Some(&m), None, "g", t + 60, Some("b"), 30));
        assert!(due(Some(&m), None, "g", t + 30 * 60, Some("a"), 30));
        // HEAD неизвестен (не git) — только по времени
        assert!(!due(Some(&m), None, "g", t + 60, None, 30));
        // ревью задачи 6, M5: синхронизация без HEAD (git не ответил), хук HEAD знает — не на
        // каждом хуке: сравнение с HEAD последнего запуска, нет его — по времени
        let nohead = mark("2026-10-27T12:01:01Z", None);
        assert!(!due(Some(&nohead), None, "g", t + 60, Some("a"), 30));
        let tried_a = Attempt {
            at: t - 10,
            head: Some("a".into()),
        };
        assert!(!due(
            Some(&nohead),
            Some(&tried_a),
            "g",
            t + 60,
            Some("a"),
            30
        ));
        assert!(due(
            Some(&nohead),
            Some(&tried_a),
            "g",
            t + 60,
            Some("b"),
            30
        ));
        // отметка другого решения не считается
        assert!(due(Some(&m), None, "other", t + 60, Some("a"), 30));
        // запуск из хука новее старой синхронизации: опора — он (и его HEAD)
        let old = mark("2026-10-27T10:00:00Z", Some("a"));
        let a = Attempt {
            at: t,
            head: Some("b".into()),
        };
        assert!(!due(Some(&old), Some(&a), "g", t + 60, Some("b"), 30));
        assert!(due(Some(&old), Some(&a), "g", t + 60, Some("a"), 30));
    }

    #[test]
    fn head_from_files_without_git() {
        let r = tmp("head");
        let sha1 = "a".repeat(40);
        let sha2 = "B".repeat(40);
        let g = r.join("repo/.git");
        std::fs::create_dir_all(g.join("refs/heads")).unwrap();
        std::fs::write(g.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let repo = r.join("repo");
        // ветки ещё нет — нет HEAD
        assert_eq!(git_head_fast(&repo), None);
        std::fs::write(g.join("refs/heads/main"), format!("{sha1}\n")).unwrap();
        assert_eq!(git_head_fast(&repo.join("sub")), Some(sha1.clone()));
        std::fs::remove_file(g.join("refs/heads/main")).unwrap();
        std::fs::write(
            g.join("packed-refs"),
            format!("# pack-refs\n{sha2} refs/heads/main\n^{sha1}\n"),
        )
        .unwrap();
        assert_eq!(git_head_fast(&repo), Some(sha2.to_ascii_lowercase()));
        // M6: `packed-refs` больше 1 МиБ не читается
        let mut big = String::from("# pack-refs\n");
        while big.len() <= PACKED_REFS_MAX as usize {
            big.push_str(&format!("{sha1} refs/tags/t{}\n", big.len()));
        }
        big.push_str(&format!("{sha2} refs/heads/main\n"));
        std::fs::write(g.join("packed-refs"), &big).unwrap();
        assert_eq!(git_head_fast(&repo), None);
        std::fs::write(g.join("packed-refs"), format!("{sha2} refs/heads/main\n")).unwrap();
        // отсоединённый
        std::fs::write(g.join("HEAD"), format!("{sha1}\n")).unwrap();
        assert_eq!(git_head_fast(&repo), Some(sha1.clone()));
        // worktree: `.git`-файл → gitdir, ветки — в общем каталоге
        let wt = r.join("wt");
        let wg = g.join("worktrees/wt");
        std::fs::create_dir_all(&wg).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", wg.display())).unwrap();
        std::fs::write(wg.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(wg.join("commondir"), "../..\n").unwrap();
        assert_eq!(git_head_fast(&wt), Some(sha2.to_ascii_lowercase()));
        // выход за пределы `refs/` — не читается
        std::fs::write(wg.join("HEAD"), "ref: ../../etc/passwd\n").unwrap();
        assert_eq!(git_head_fast(&wt), None);
    }

    #[test]
    fn lock_busy_only_while_held() {
        let r = tmp("lock");
        assert!(!lock_busy(&r));
        std::fs::create_dir_all(cache_dir(&r)).unwrap();
        let path = cache_dir(&r).join("code-sync.lock");
        std::fs::write(&path, b"").unwrap();
        // файл без блокировки (остался после синхронизации) — не занят
        assert!(!lock_busy(&r));
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        held.lock().unwrap();
        assert!(lock_busy(&r));
        held.unlock().unwrap();
        assert!(!lock_busy(&r));
    }
}
