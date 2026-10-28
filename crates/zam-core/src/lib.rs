//! Ядро ZeroAIMemory: общие типы и логика.

pub mod id;
pub mod solution;
pub mod target;
pub mod token;

pub use id::{Id, ParseIdError};
pub use solution::SolutionGuid;
pub use target::{TargetKind, TargetRef};
pub use token::{NewToken, generate_token, hash_token, looks_like_token, mask_token};

/// Текущая версия блока инструкции агента ZeroAIMemory в CLAUDE.md/AGENTS.md (§14.2, §15.5):
/// маркер `<!-- zeroaimemory:begin vN -->`. Нижняя граница версии сервера: действующая версия —
/// `max(AGENT_BLOCK_VERSION, версия шаблонов в settings)` (`zam_server::agent_setup`), с ней
/// сравнивают `/recall(session_start)` и `zam status`, её пишет в маркер `GET /agent-setup`.
pub const AGENT_BLOCK_VERSION: u32 = 2;

/// Переменные окружения, которые ставит агент (Claude Code, Codex) своим подпроцессам и по
/// которым `zam` понимает, что его запустил агент, и берёт сессию хуков (§10.3, задача 10 M4).
/// Тесты и smoke, запускающие `zam` изнутри сессии агента, убирают их из окружения дочернего
/// процесса — иначе окружение самого разработчика утекает в проверку (задача 16 M4, M-5).
pub const AGENT_ENV_VARS: [&str; 6] = [
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "CODEX_THREAD_ID",
];
