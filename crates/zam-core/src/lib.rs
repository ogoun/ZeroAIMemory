//! Ядро ZeroAIMemory: общие типы и логика.

pub mod id;
pub mod solution;
pub mod target;
pub mod token;

pub use id::{Id, ParseIdError};
pub use solution::SolutionGuid;
pub use target::{TargetKind, TargetRef};
pub use token::{NewToken, generate_token, hash_token, looks_like_token, mask_token};
