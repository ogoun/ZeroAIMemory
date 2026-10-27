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
/// маркер `<!-- zeroaimemory:begin vN -->`. `/recall(session_start)` напоминает об устаревшем
/// блоке, если хук передал `agent_block_version` меньше этой; `zam init` и `GET /agent-setup`
/// пишут блок с ней.
pub const AGENT_BLOCK_VERSION: u32 = 2;
