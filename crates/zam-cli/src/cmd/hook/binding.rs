//! Привязка `zam mcp` к сессии агента (M6 §7.1).
//!
//! Агент не сообщает MCP-серверу свою сессию, а процесс агента у хука и у `zam mcp` общий.
//! Поэтому хук `session-start` пишет `.zeroaimemory/cache/agent-<pid>.json {session_id,
//! started_at}` на каждого своего предка до первого процесса агента включительно
//! ([`procs::AGENT_PROC_NAMES`], не глубже [`BINDING_ANCESTORS_MAX`]): у Codex хук и MCP могут
//! идти через разные промежуточные оболочки (`sh -c`), а общий у них — сам агент. `zam mcp`
//! идёт по своей цепочке так же и берёт первый найденный файл. Несколько агентов в одном
//! проекте различаются pid.
//!
//! **[решение исполнителя задачи 3]** Агента в цепочке нет (запуск не агентом, тесты) — файл
//! пишется только на непосредственного родителя (базовое правило §7.1): иначе привязка легла
//! бы на общие процессы вроде терминала и досталась бы чужому агенту. Файл перезаписывает
//! каждый `session-start` (новая сессия, `/clear`, `resume`), удаляет `gc_cache` через 7 дней.
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::cache;
use super::procs::{self, Proc};

/// Глубина поиска процесса агента в цепочке предков.
pub const BINDING_ANCESTORS_MAX: usize = 4;

/// Файл привязки.
#[derive(Debug, Serialize, Deserialize)]
struct Binding {
    session_id: String,
    /// RFC 3339 UTC.
    started_at: String,
}

/// Префикс имени файла привязки в кэше (`agent-<pid>.json`).
pub const BINDING_PREFIX: &str = "agent-";

fn path(root: &Path, pid: u32) -> PathBuf {
    root.join(".zeroaimemory")
        .join("cache")
        .join(format!("{BINDING_PREFIX}{pid}.json"))
}

/// Предки до первого процесса агента включительно; агента нет — только родитель.
fn chain() -> Vec<Proc> {
    let all = procs::ancestors(BINDING_ANCESTORS_MAX);
    match all.iter().position(procs::is_agent) {
        Some(i) => all.into_iter().take(i + 1).collect(),
        None => all.into_iter().take(1).collect(),
    }
}

/// Пишет привязку сессии `session` на предков текущего процесса (см. модуль). Цепочка не
/// прочиталась — ничего не пишется.
pub fn write_binding(root: &Path, session: &str) -> io::Result<()> {
    let b = Binding {
        session_id: session.to_string(),
        started_at: cache::now_rfc3339(),
    };
    for p in chain() {
        cache::write_json_file(&path(root, p.pid), &b)?;
    }
    Ok(())
}

/// Сессия из первой найденной по своей цепочке привязки (корректный id: непустой, до 128
/// печатных символов); нет — `None`.
pub fn read_binding(root: &Path) -> Option<String> {
    chain().into_iter().find_map(|p| {
        let b: Binding = cache::read_json_file(&path(root, p.pid)).ok()??;
        let id = b.session_id.trim();
        (!id.is_empty() && id.chars().count() <= 128 && !id.chars().any(char::is_control))
            .then(|| id.to_string())
    })
}

#[cfg(all(test, feature = "test-hooks"))]
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

    /// Codex: хук — `zam ← sh ← codex`, MCP — `zam ← bash ← codex`; общий предок — `codex`.
    /// Файлы — на `sh` и `codex`, выше агента — нет; MCP находит сессию через `codex`.
    #[test]
    fn binding_found_via_fake_ancestors() {
        let root = tmp("fake-ancestors");
        set_fake_ancestors(vec![p(11, "sh"), p(22, "codex"), p(33, "explorer")]);
        write_binding(&root, "sess-1").unwrap();
        assert!(path(&root, 11).is_file());
        assert!(path(&root, 22).is_file());
        assert!(!path(&root, 33).is_file(), "выше агента не пишется");
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
        // агента в цепочке нет — только родитель, и чтение не идёт выше агента
        set_fake_ancestors(vec![p(88, "pwsh"), p(33, "explorer")]);
        write_binding(&root, "sess-3").unwrap();
        assert!(path(&root, 88).is_file());
        assert!(!path(&root, 33).is_file());
        assert_eq!(read_binding(&root).as_deref(), Some("sess-3"));
        set_fake_ancestors(vec![p(66, "bash"), p(77, "claude")]);
        assert_eq!(read_binding(&root), None);
        // цепочка не прочиталась — ни записи, ни чтения
        set_fake_ancestors(Vec::new());
        write_binding(&root, "sess-4").unwrap();
        assert_eq!(read_binding(&root), None);
        // битый и некорректный файл — как нет
        std::fs::write(path(&root, 99), "{").unwrap();
        set_fake_ancestors(vec![p(99, "node")]);
        assert_eq!(read_binding(&root), None);
        std::fs::write(
            path(&root, 99),
            r#"{"session_id": " ", "started_at": "2026-11-05T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(read_binding(&root), None);
    }
}
