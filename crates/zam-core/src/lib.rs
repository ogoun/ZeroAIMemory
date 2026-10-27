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
