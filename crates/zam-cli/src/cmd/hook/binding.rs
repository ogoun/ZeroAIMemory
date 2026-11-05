//! Привязка `zam mcp` к сессии агента (M6 §7.1).
//!
//! Агент не сообщает MCP-серверу свою сессию, а процесс агента у хука и у `zam mcp` общий
//! (stdio-сервер MCP — подпроцесс агента, как и хуки). Поэтому хук `session-start` пишет
//! `.zeroaimemory/cache/agent-<pid>.json {session_id, agent_pid, started_at}` на каждого своего
//! предка до первого процесса агента включительно ([`procs::AGENT_PROC_NAMES`], не глубже
//! [`BINDING_ANCESTORS_MAX`]): у Codex хук и MCP могут идти через разные промежуточные
//! оболочки (`sh -c`), а общий у них — сам агент. `zam mcp` идёт по своей цепочке так же и
//! берёт первый подходящий файл. Несколько агентов в одном проекте различаются pid.
//!
//! **Переиспользование pid** (ревью задачи 3, I1). Файлы на эфемерных оболочках остаются
//! после их выхода, а Windows быстро отдаёт pid новым процессам. Поэтому в файле —
//! `agent_pid` (первый агент цепочки писателя, без агента — родитель), и читатель принимает
//! файл, только если этот pid есть в **его** цепочке; файлы старше [`SESSION_TTL`] не
//! читаются.
//!
//! **[решение исполнителя задачи 3]** Агента в цепочке нет (запуск не агентом, тесты) — файл
//! пишется только на непосредственного родителя (базовое правило §7.1): иначе привязка легла
//! бы на общие процессы вроде терминала и досталась бы чужому агенту. Файл перезаписывает
//! каждый `session-start` (новая сессия, `/clear`, `resume`), удаляет `gc_cache` через 7 дней.
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::cache::{self, SESSION_TTL};
use super::procs::{self, Proc};

/// Глубина поиска процесса агента в цепочке предков.
pub const BINDING_ANCESTORS_MAX: usize = 4;

/// Префикс имени файла привязки в кэше (`agent-<pid>.json`).
pub const BINDING_PREFIX: &str = "agent-";

/// Файл привязки.
#[derive(Debug, Serialize, Deserialize)]
struct Binding {
    session_id: String,
    /// Процесс агента, к которому относится привязка (см. модуль).
    agent_pid: u32,
    /// RFC 3339 UTC.
    started_at: String,
}

/// Найденная привязка: сессия, файл и его время изменения (по нему `zam mcp` замечает
/// перезапись после `/clear`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub session: String,
    pub path: PathBuf,
    pub mtime: SystemTime,
}

/// Файл привязки процесса `pid`.
pub fn binding_path(root: &Path, pid: u32) -> PathBuf {
    root.join(".zeroaimemory")
        .join("cache")
        .join(format!("{BINDING_PREFIX}{pid}.json"))
}

/// Предки до первого процесса агента включительно; агента нет — только родитель.
pub fn chain() -> Vec<Proc> {
    let all = procs::ancestors(BINDING_ANCESTORS_MAX);
    match all.iter().position(procs::is_agent) {
        Some(i) => all.into_iter().take(i + 1).collect(),
        None => all.into_iter().take(1).collect(),
    }
}

/// Пишет привязку сессии `session` на предков текущего процесса (см. модуль). Цепочка не
/// прочиталась — ничего не пишется.
pub fn write_binding(root: &Path, session: &str) -> io::Result<()> {
    let chain = chain();
    let Some(agent) = chain.last() else {
        return Ok(());
    };
    let b = Binding {
        session_id: session.to_string(),
        agent_pid: agent.pid,
        started_at: cache::now_rfc3339(),
    };
    for p in &chain {
        cache::write_json_file(&binding_path(root, p.pid), &b)?;
    }
    Ok(())
}

/// Первая подходящая привязка по цепочке `chain` (см. модуль): файл не старше
/// [`SESSION_TTL`], `agent_pid` из этой же цепочки, корректный id (непустой, до 128 печатных
/// символов).
pub fn find_binding(root: &Path, chain: &[Proc]) -> Option<Found> {
    chain.iter().find_map(|p| {
        let path = binding_path(root, p.pid);
        let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;
        // время в будущем (часы сдвинуты) — как свежий файл
        let age = SystemTime::now()
            .duration_since(mtime)
            .unwrap_or(Duration::ZERO);
        if age > SESSION_TTL {
            return None;
        }
        let b: Binding = cache::read_json_file(&path).ok()??;
        if !chain.iter().any(|c| c.pid == b.agent_pid) {
            return None;
        }
        let id = b.session_id.trim();
        (!id.is_empty() && id.chars().count() <= 128 && !id.chars().any(char::is_control)).then(
            || Found {
                session: id.to_string(),
                path,
                mtime,
            },
        )
    })
}

/// Сессия из привязки по своей цепочке предков ([`find_binding`]).
#[cfg_attr(not(test), allow(dead_code))]
pub fn read_binding(root: &Path) -> Option<String> {
    find_binding(root, &chain()).map(|f| f.session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use procs::set_fake_ancestors;

    fn tmp(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp/zam-cli-binding")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn p(pid: u32, name: &str) -> Proc {
        Proc {
            pid,
            name: name.into(),
        }
    }

    fn put(root: &Path, pid: u32, session: &str, agent_pid: u32) {
        let path = binding_path(root, pid);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                r#"{{"session_id": "{session}", "agent_pid": {agent_pid}, "started_at": "2026-11-05T00:00:00Z"}}"#
            ),
        )
        .unwrap();
    }

    /// Codex: хук — `zam ← sh ← codex`, MCP — `zam ← bash ← codex`; общий предок — `codex`.
    /// Файлы — на `sh` и `codex`, выше агента — нет; MCP находит сессию через `codex`.
    #[test]
    fn binding_found_via_fake_ancestors() {
        let root = tmp("fake-ancestors");
        set_fake_ancestors(vec![p(11, "sh"), p(22, "codex"), p(33, "explorer")]);
        write_binding(&root, "sess-1").unwrap();
        assert!(binding_path(&root, 11).is_file());
        assert!(binding_path(&root, 22).is_file());
        assert!(!binding_path(&root, 33).is_file(), "выше агента не пишется");
        let f: serde_json::Value =
            serde_json::from_slice(&std::fs::read(binding_path(&root, 11)).unwrap()).unwrap();
        assert_eq!(f["agent_pid"], 22, "{f}");
        set_fake_ancestors(vec![p(44, "bash"), p(22, "codex"), p(33, "explorer")]);
        assert_eq!(read_binding(&root).as_deref(), Some("sess-1"));
        // `/clear` — новая сессия у того же агента
        set_fake_ancestors(vec![p(55, "cmd"), p(22, "codex")]);
        write_binding(&root, "sess-2").unwrap();
        set_fake_ancestors(vec![p(44, "bash"), p(22, "codex")]);
        assert_eq!(read_binding(&root).as_deref(), Some("sess-2"));
        // ближний предок сильнее: свой файл у промежуточной оболочки
        set_fake_ancestors(vec![p(55, "cmd"), p(22, "codex")]);
        assert_eq!(read_binding(&root).as_deref(), Some("sess-2"));
        // другой агент того же проекта — свой pid, чужую привязку не видит
        set_fake_ancestors(vec![p(66, "bash"), p(77, "claude"), p(33, "explorer")]);
        assert_eq!(read_binding(&root), None);
        // агента в цепочке нет — только родитель
        set_fake_ancestors(vec![p(88, "pwsh"), p(33, "explorer")]);
        write_binding(&root, "sess-3").unwrap();
        assert!(binding_path(&root, 88).is_file());
        assert!(!binding_path(&root, 33).is_file());
        assert_eq!(read_binding(&root).as_deref(), Some("sess-3"));
        set_fake_ancestors(vec![p(66, "bash"), p(77, "claude")]);
        assert_eq!(read_binding(&root), None);
        // цепочка не прочиталась — ни записи, ни чтения
        set_fake_ancestors(Vec::new());
        write_binding(&root, "sess-4").unwrap();
        assert_eq!(read_binding(&root), None);
        // битый и некорректный файл — как нет
        std::fs::write(binding_path(&root, 99), "{").unwrap();
        set_fake_ancestors(vec![p(99, "node")]);
        assert_eq!(read_binding(&root), None);
        put(&root, 99, " ", 99);
        assert_eq!(read_binding(&root), None);
        // файл прежнего формата (без agent_pid) не принимается
        std::fs::write(
            binding_path(&root, 99),
            r#"{"session_id": "old", "started_at": "2026-11-05T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(read_binding(&root), None);
    }

    /// I1: pid умершей оболочки достался процессу в моей цепочке, а файл на нём — от чужого
    /// агента (его `agent_pid` вне моей цепочки): не принимается; свой агент дальше по цепочке —
    /// находится.
    #[test]
    fn foreign_file_on_reused_pid_ignored() {
        let root = tmp("reused-pid");
        put(&root, 101, "foreign", 500);
        set_fake_ancestors(vec![p(101, "bash"), p(202, "claude")]);
        assert_eq!(read_binding(&root), None);
        put(&root, 202, "mine", 202);
        assert_eq!(read_binding(&root).as_deref(), Some("mine"));
    }

    /// Файл старше `SESSION_TTL` не читается.
    #[test]
    fn stale_binding_ignored() {
        let root = tmp("stale");
        put(&root, 303, "old-sess", 303);
        set_fake_ancestors(vec![p(303, "codex")]);
        assert_eq!(read_binding(&root).as_deref(), Some("old-sess"));
        let old = SystemTime::now() - SESSION_TTL - Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(binding_path(&root, 303))
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(read_binding(&root), None);
    }
}
