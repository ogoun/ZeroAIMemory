//! Цепочка предков процесса (M6 §7.1): по ней хук `session-start` и `zam mcp` находят общий
//! процесс агента ([`super::binding`]).
//!
//! - **Windows** — снимок процессов ToolHelp32 (`CreateToolhelp32Snapshot`,
//!   `Process32FirstW`/`Process32NextW`: `th32ParentProcessID`, `szExeFile`).
//! - **Linux** — `/proc/<pid>/stat`: поле 2 (`comm`, в скобках, может содержать пробелы и
//!   скобки — берётся до последней `)`) и поле 4 (`ppid`).
//! - Прочие ОС и любая ошибка — пустой список (привязки нет, `zam mcp` идёт неявным путём).
//! - Имя — без расширения `.exe`, в нижнем регистре ([`norm_name`]).
//! - Юнит-тесты (и фича `test-hooks`) — подмена списка ([`set_fake_ancestors`], на поток).

/// Процесс цепочки: pid и имя (без расширения, нижний регистр).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub name: String,
}

/// Имена процессов агента (без расширения, нижний регистр): Codex, Claude Code (родной
/// бинарник и Node.js).
pub const AGENT_PROC_NAMES: [&str; 3] = ["codex", "claude", "node"];

/// Процесс агента по имени ([`AGENT_PROC_NAMES`]): точное имя или имя с суффиксом через `-`
/// (`codex-x86_64-pc-windows-msvc`, `claude-2.1`) — сборки агентов называют бинарник по-разному.
pub fn is_agent(p: &Proc) -> bool {
    AGENT_PROC_NAMES.iter().any(|n| {
        p.name == *n
            || p.name
                .strip_prefix(n)
                .is_some_and(|rest| rest.starts_with('-'))
    })
}

/// Имя исполняемого файла без расширения `.exe`, в нижнем регистре.
pub fn norm_name(exe: &str) -> String {
    let n = exe.trim().to_lowercase();
    n.strip_suffix(".exe").map(str::to_string).unwrap_or(n)
}

#[cfg(any(test, feature = "test-hooks"))]
thread_local! {
    static FAKE: std::cell::RefCell<Option<Vec<Proc>>> = const { std::cell::RefCell::new(None) };
}

/// Подменяет цепочку предков текущего потока (юнит-тесты и фича `test-hooks`).
#[cfg(any(test, feature = "test-hooks"))]
#[cfg_attr(not(test), allow(dead_code))]
pub fn set_fake_ancestors(v: Vec<Proc>) {
    FAKE.with(|f| *f.borrow_mut() = Some(v));
}

/// Предки текущего процесса, от родителя вверх, не больше `max`; сбой — пустой список.
pub fn ancestors(max: usize) -> Vec<Proc> {
    #[cfg(any(test, feature = "test-hooks"))]
    if let Some(v) = FAKE.with(|f| f.borrow().clone()) {
        return v.into_iter().take(max).collect();
    }
    if max == 0 {
        return Vec::new();
    }
    os::ancestors(std::process::id(), max)
}

/// Подъём по таблице «pid → (ppid, имя)» от `pid`: без циклов (Windows переиспользует pid, и
/// у процесса с умершим родителем ppid может указать на чужой процесс) и без pid 0.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
fn walk(start: u32, max: usize, parent_of: impl Fn(u32) -> Option<(u32, String)>) -> Vec<Proc> {
    let mut out: Vec<Proc> = Vec::new();
    let mut seen = vec![start];
    let Some((mut pid, _)) = parent_of(start) else {
        return out;
    };
    while out.len() < max && pid != 0 && !seen.contains(&pid) {
        let Some((ppid, name)) = parent_of(pid) else {
            break;
        };
        seen.push(pid);
        out.push(Proc {
            pid,
            name: norm_name(&name),
        });
        pid = ppid;
    }
    out
}

#[cfg(windows)]
mod os {
    use std::collections::HashMap;

    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    pub fn ancestors(me: u32, max: usize) -> Vec<super::Proc> {
        let table = snapshot();
        super::walk(me, max, |pid| table.get(&pid).cloned())
    }

    /// Все процессы: pid → (ppid, имя файла). Сбой — пустая таблица.
    fn snapshot() -> HashMap<u32, (u32, String)> {
        let mut out = HashMap::new();
        // SAFETY: снимок закрывается ниже; `PROCESSENTRY32W` — POD с заданным `dwSize`.
        unsafe {
            let Ok(h) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                return out;
            };
            let mut e = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut ok = Process32FirstW(h, &mut e).is_ok();
            while ok {
                let len = e
                    .szExeFile
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(e.szExeFile.len());
                let name = String::from_utf16_lossy(&e.szExeFile[..len]);
                out.insert(e.th32ProcessID, (e.th32ParentProcessID, name));
                ok = Process32NextW(h, &mut e).is_ok();
            }
            let _ = CloseHandle(h);
        }
        out
    }
}

#[cfg(target_os = "linux")]
mod os {
    pub fn ancestors(me: u32, max: usize) -> Vec<super::Proc> {
        super::walk(me, max, |pid| {
            let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            super::parse_stat(&s)
        })
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod os {
    pub fn ancestors(_me: u32, _max: usize) -> Vec<super::Proc> {
        Vec::new()
    }
}

/// `/proc/<pid>/stat` → (ppid, comm).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_stat(s: &str) -> Option<(u32, String)> {
    let open = s.find('(')?;
    let close = s.rfind(')')?;
    let comm = s.get(open + 1..close)?.to_string();
    let mut rest = s.get(close + 1..)?.split_whitespace();
    let _state = rest.next()?;
    let ppid = rest.next()?.parse().ok()?;
    Some((ppid, comm))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_normalized() {
        assert_eq!(norm_name("Claude.EXE"), "claude");
        assert_eq!(norm_name("node.exe"), "node");
        assert_eq!(norm_name("codex"), "codex");
        assert!(is_agent(&Proc {
            pid: 1,
            name: norm_name("Codex.exe")
        }));
        assert!(!is_agent(&Proc {
            pid: 1,
            name: "bash".into()
        }));
        let agent = |n: &str| {
            is_agent(&Proc {
                pid: 1,
                name: norm_name(n),
            })
        };
        assert!(agent("codex-x86_64-pc-windows-msvc.exe") && agent("claude-2.1"));
        assert!(agent("Claude.exe") && agent("node.exe"));
        assert!(!agent("codexer") && !agent("claudette") && !agent("nodemon"));
    }

    #[test]
    fn stat_with_spaces_and_parens() {
        assert_eq!(
            parse_stat("4242 (my (odd) proc) S 17 4242 4242 0 -1"),
            Some((17, "my (odd) proc".into()))
        );
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn walk_stops_on_cycle_zero_and_max() {
        let t = |pid: u32| match pid {
            10 => Some((20, "zam.exe".to_string())),
            20 => Some((30, "bash.exe".into())),
            30 => Some((20, "claude.exe".into())),
            _ => None,
        };
        let names = |v: Vec<Proc>| v.into_iter().map(|p| p.name).collect::<Vec<_>>();
        // цикл 20 → 30 → 20 — обрыв
        assert_eq!(names(walk(10, 8, t)), ["bash", "claude"]);
        assert_eq!(names(walk(10, 1, t)), ["bash"]);
        let z = |pid: u32| (pid == 5).then(|| (0, "x".to_string()));
        assert!(walk(5, 4, z).is_empty());
    }

    /// Настоящая цепочка: у процесса тестов есть хотя бы родитель (cargo или оболочка).
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn real_ancestors_have_parent() {
        let v = os::ancestors(std::process::id(), 4);
        assert!(!v.is_empty());
        assert!(v.len() <= 4);
        assert!(v.iter().all(|p| p.pid != std::process::id()));
    }
}
