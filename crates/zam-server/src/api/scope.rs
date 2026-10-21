//! ÐÐ´Ñ€ÐµÑÐ°Ñ†Ð¸Ñ Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð² Ð¸ Ð¿Ð¾Ð»Ð¸Ñ‚Ð¸ÐºÐ° Ð´Ð¾ÑÑ‚ÑƒÐ¿Ð° (Â§7.1, Â§11.4, Â§11.6).
//!
//! `SpaceRef` Ð² Ð¿ÑƒÑ‚Ð¸ `{space}` Ð¸Ð»Ð¸ Ð¿Ð¾Ð»Ðµ `space`: `rules` | `human` | `knowledge` |
//! GUID Ñ€ÐµÑˆÐµÐ½Ð¸Ñ (UUID Ñ Ð´ÐµÑ„Ð¸ÑÐ°Ð¼Ð¸, Ñ€ÐµÐ³Ð¸ÑÑ‚Ñ€ Ð½Ðµ Ð²Ð°Ð¶ÐµÐ½) | `Id` Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð° (26 ÑÐ¸Ð¼Ð²Ð¾Ð»Ð¾Ð²).
//! ÐÐµÐ¸Ð·Ð²ÐµÑÑ‚Ð½Ð¾Ðµ â†’ 404.
//!
//! **ÐžÐ±Ð»Ð°ÑÑ‚Ð¸.** Ð—Ð°Ð¿Ñ€Ð¾Ñ Ñ€Ð°Ð±Ð¾Ñ‚Ð°ÐµÑ‚ Ð² Ð¾Ð´Ð½Ð¾Ð¹ Ð±Ð°Ð·Ðµ: Ð»Ð¸Ñ‡Ð½Ð¾Ð¹ Ð±Ð°Ð·Ðµ Ð¿Ð¾Ð»ÑŒÐ·Ð¾Ð²Ð°Ñ‚ÐµÐ»Ñ Ð·Ð°Ð¿Ñ€Ð¾ÑÐ° Ð¸Ð»Ð¸ Ð±Ð°Ð·Ðµ
//! Ð²Ð»Ð°Ð´ÐµÐ»ÑŒÑ†Ð° Ð¾Ð±Ñ‰ÐµÐ³Ð¾ Ñ€ÐµÑˆÐµÐ½Ð¸Ñ â€” Ð¸ Ñ‚Ð¾Ð³Ð´Ð° Ñ‚Ð¾Ð»ÑŒÐºÐ¾ Ð² Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ðµ ÑÑ‚Ð¾Ð³Ð¾ Ñ€ÐµÑˆÐµÐ½Ð¸Ñ ([`MemScope`]):
//! - GUID Ñ€ÐµÑˆÐµÐ½Ð¸Ñ Ð¸Ñ‰ÐµÑ‚ÑÑ Ð² Ñ€ÐµÐµÑÑ‚Ñ€Ðµ `system.db` (ÐºÐ°Ð¶Ð´Ñ‹Ð¹ Ð·Ð°Ð¿Ñ€Ð¾Ñ, Ð±ÐµÐ· ÐºÑÑˆÐ°): Ñ€Ð¾Ð»ÑŒ â†’ Ð±Ð°Ð·Ð°
//!   Ð²Ð»Ð°Ð´ÐµÐ»ÑŒÑ†Ð° â†’ Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð¾ `space_id`. ÐÐµ Ð·Ð°Ñ€ÐµÐ³Ð¸ÑÑ‚Ñ€Ð¸Ñ€Ð¾Ð²Ð°Ð½ â†’ 404;
//! - `Id` Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð° â€” Ð² Ð»Ð¸Ñ‡Ð½Ð¾Ð¹ Ð±Ð°Ð·Ðµ, Ð¸Ð½Ð°Ñ‡Ðµ Ñ‚Ð¾Ð»ÑŒÐºÐ¾ `space_id` Ñ€ÐµÑˆÐµÐ½Ð¸Ñ Ð¸Ð· Ñ€ÐµÐµÑÑ‚Ñ€Ð°;
//! - ÑƒÐ·ÐµÐ» Ð¿Ð¾ id ([`node_scope`]) â€” Ð² Ð»Ð¸Ñ‡Ð½Ð¾Ð¹ Ð±Ð°Ð·Ðµ, Ð·Ð°Ñ‚ÐµÐ¼ Ð² Ð±Ð°Ð·Ð°Ñ… Ð²Ð»Ð°Ð´ÐµÐ»ÑŒÑ†ÐµÐ² Ñ€ÐµÑˆÐµÐ½Ð¸Ð¹, Ð³Ð´Ðµ Ñƒ
//!   Ð¿Ð¾Ð»ÑŒÐ·Ð¾Ð²Ð°Ñ‚ÐµÐ»Ñ ÐµÑÑ‚ÑŒ Ð·Ð°Ð¿Ñ€Ð¾Ñ Ð´Ð¾ÑÑ‚ÑƒÐ¿Ð°; ÑƒÐ·ÐµÐ» Ð¿Ñ€Ð¸Ð½Ð¸Ð¼Ð°ÐµÑ‚ÑÑ, Ñ‚Ð¾Ð»ÑŒÐºÐ¾ ÐµÑÐ»Ð¸ Ð¾Ð½ Ð² Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ðµ
//!   Ñ€ÐµÑˆÐµÐ½Ð¸Ñ (Ð¸Ð½Ð°Ñ‡Ðµ 404: Rules, Human, Knowledge Ð²Ð»Ð°Ð´ÐµÐ»ÑŒÑ†Ð° ÑƒÑ‡Ð°ÑÑ‚Ð½Ð¸ÐºÑƒ Ð½Ðµ Ð²Ð¸Ð´Ð½Ñ‹).
//!
//! **ÐŸÐ¾Ð»Ð¸Ñ‚Ð¸ÐºÐ°:**
//! - `Human` Ð¿Ð¾ Ñ‚Ð¾ÐºÐµÐ½Ñƒ â†’ 404 (ÑÑƒÑ‰ÐµÑÑ‚Ð²Ð¾Ð²Ð°Ð½Ð¸Ðµ Ð½Ðµ Ñ€Ð°ÑÐºÑ€Ñ‹Ð²Ð°ÐµÑ‚ÑÑ) â€” Ð´Ð»Ñ Ñ‡Ñ‚ÐµÐ½Ð¸Ñ Ð¸ Ð·Ð°Ð¿Ð¸ÑÐ¸;
//! - Ð·Ð°Ð¿Ð¸ÑÑŒ Ð¿Ð¾ Ñ‚Ð¾ÐºÐµÐ½Ñƒ `ro` â†’ 403;
//! - Ð·Ð°Ð¿Ð¸ÑÑŒ Ð² `Rules` Ð¿Ð¾ Ñ‚Ð¾ÐºÐµÐ½Ñƒ Ð¿Ñ€Ð¸ `rules.agent_write = false` â†’ 403;
//! - ÑƒÑ‡Ð°ÑÑ‚Ð½Ð¸Ðº: Ð´Ð¾ÑÑ‚ÑƒÐ¿ Ñ‚Ð¾Ð»ÑŒÐºÐ¾ Ð¿Ñ€Ð¸ Ð¾Ð´Ð¾Ð±Ñ€ÐµÐ½Ð½Ð¾Ð¼ Ð·Ð°Ð¿Ñ€Ð¾ÑÐµ (`pending`/`rejected`/`revoked` â†’ 403
//!   `/problems/solution-access`), Ð·Ð°Ð¿Ð¸ÑÑŒ â€” Ð¿Ñ€Ð¸ Ñ€ÐµÐ¶Ð¸Ð¼Ðµ ÑƒÑ‡Ð°ÑÑ‚Ð½Ð¸ÐºÐ° `rw` Ð¸ Ñ‚Ð¾ÐºÐµÐ½Ðµ `rw`;
//! - Ð°Ð´Ð¼Ð¸Ð½Ð¸ÑÑ‚Ñ€Ð°Ñ‚Ð¾Ñ€ Ð¼Ð¾Ð¶ÐµÑ‚ Ð²ÑÑ‘.
//!
//! ÐÐ°ÑÑ‚Ñ€Ð¾Ð¹ÐºÐ° `rules.agent_write` Ð¸ Ñ€Ð¾Ð»ÑŒ Ð² Ñ€ÐµÑˆÐµÐ½Ð¸Ð¸ Ñ‡Ð¸Ñ‚Ð°ÑŽÑ‚ÑÑ Ð¸Ð· `system.db` Ð½Ð° ÐºÐ°Ð¶Ð´Ð¾Ð¼ Ð·Ð°Ð¿Ñ€Ð¾ÑÐµ
//! (Ñ€ÐµÑˆÐµÐ½Ð¸Ñ Ð¾ Ð´Ð¾ÑÑ‚ÑƒÐ¿Ðµ Ð½Ðµ ÐºÑÑˆÐ¸Ñ€ÑƒÑŽÑ‚ÑÑ): Ð¾Ñ‚Ð·Ñ‹Ð² Ð´ÐµÐ¹ÑÑ‚Ð²ÑƒÐµÑ‚ ÑÐ¾ ÑÐ»ÐµÐ´ÑƒÑŽÑ‰ÐµÐ³Ð¾ Ð·Ð°Ð¿Ñ€Ð¾ÑÐ°.
use axum::http::HeaderMap;
use zam_core::{Id, SolutionGuid};
use zam_store::{
    Access, Db, Node, SolutionInfo, SolutionRole, Space, SpaceKind, StoreError, User, get_node,
    get_solution, get_space, list_spaces, space_of_kind,
};

use crate::app::AppState;
use crate::auth::{Principal, target_user};
use crate::error::ApiError;

/// ÐšÐ»ÑŽÑ‡ Ð½Ð°ÑÑ‚Ñ€Ð¾Ð¹ÐºÐ¸ Ð·Ð°Ð¿Ð¸ÑÐ¸ Ð°Ð³ÐµÐ½Ñ‚Ð° Ð² Rules.
pub const RULES_AGENT_WRITE: &str = "rules.agent_write";

/// Ð§Ñ‚ÐµÐ½Ð¸Ðµ Ð¸Ð»Ð¸ Ð·Ð°Ð¿Ð¸ÑÑŒ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Read,
    Write,
}

/// ÐžÑ‚Ð½Ð¾ÑˆÐµÐ½Ð¸Ðµ Ð·Ð°Ð¿Ñ€Ð¾ÑÐ° Ðº Ð¾Ð±Ð»Ð°ÑÑ‚Ð¸ Ð¿Ð°Ð¼ÑÑ‚Ð¸.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Ð›Ð¸Ñ‡Ð½Ð¾Ðµ Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð¾ Ð¿Ð¾Ð»ÑŒÐ·Ð¾Ð²Ð°Ñ‚ÐµÐ»Ñ (Rules, Human, Knowledge).
    Personal,
    /// Ð¡Ð²Ð¾Ñ‘ Ñ€ÐµÑˆÐµÐ½Ð¸Ðµ.
    Owner,
    /// ÐžÐ´Ð¾Ð±Ñ€ÐµÐ½Ð½Ñ‹Ð¹ ÑƒÑ‡Ð°ÑÑ‚Ð½Ð¸Ðº Ñ‡ÑƒÐ¶Ð¾Ð³Ð¾ Ñ€ÐµÑˆÐµÐ½Ð¸Ñ Ñ Ñ€ÐµÐ¶Ð¸Ð¼Ð¾Ð¼.
    Member(Access),
    /// ÐÐ´Ð¼Ð¸Ð½Ð¸ÑÑ‚Ñ€Ð°Ñ‚Ð¾Ñ€ (cookie WebUI) â€” Ð±ÐµÐ· Ð¾Ð³Ñ€Ð°Ð½Ð¸Ñ‡ÐµÐ½Ð¸Ð¹.
    Admin,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Personal => "personal",
            Role::Owner => "owner",
            Role::Member(_) => "member",
            Role::Admin => "admin",
        }
    }

    /// Ð ÐµÐ¶Ð¸Ð¼ Ð´Ð¾ÑÑ‚ÑƒÐ¿Ð° Ðº Ð¾Ð±Ð»Ð°ÑÑ‚Ð¸: ÑƒÑ‡Ð°ÑÑ‚Ð½Ð¸ÐºÑƒ â€” ÐµÐ³Ð¾ Ñ€ÐµÐ¶Ð¸Ð¼, Ð°Ð´Ð¼Ð¸Ð½Ð¸ÑÑ‚Ñ€Ð°Ñ‚Ð¾Ñ€Ñƒ â€” `admin`,
    /// Ð¾ÑÑ‚Ð°Ð»ÑŒÐ½Ñ‹Ð¼ â€” `rw`.
    pub fn access_str(self) -> &'static str {
        match self {
            Role::Member(a) => a.as_str(),
            Role::Admin => "admin",
            Role::Personal | Role::Owner => "rw",
        }
    }
}

/// Ð Ð°Ð·Ð¾Ð±Ñ€Ð°Ð½Ð½Ð°Ñ ÑÑÑ‹Ð»ÐºÐ° Ð½Ð° Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð¾.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceRef {
    Fixed(SpaceKind),
    Solution(SolutionGuid),
    Id(Id),
}

impl SpaceRef {
    /// ÐÐµÐ¸Ð·Ð²ÐµÑÑ‚Ð½Ð°Ñ Ñ„Ð¾Ñ€Ð¼Ð° â†’ 404 (ÐºÐ°Ðº Ð¸ Ð½ÐµÑÑƒÑ‰ÐµÑÑ‚Ð²ÑƒÑŽÑ‰ÐµÐµ Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð¾).
    pub fn parse(raw: &str) -> Result<SpaceRef, ApiError> {
        let s = raw.trim();
        Ok(match s {
            "rules" => SpaceRef::Fixed(SpaceKind::Rules),
            "human" => SpaceRef::Fixed(SpaceKind::Human),
            "knowledge" => SpaceRef::Fixed(SpaceKind::Knowledge),
            _ => match s.parse::<SolutionGuid>() {
                Ok(g) => SpaceRef::Solution(g),
                // `Id::from_str` Ð¿Ñ€Ð¸Ð½Ð¸Ð¼Ð°ÐµÑ‚ Ð¸ Ñ„Ð¾Ñ€Ð¼Ñ‹ UUID â€” Ð·Ð´ÐµÑÑŒ Ñ‚Ð¾Ð»ÑŒÐºÐ¾ 26 ÑÐ¸Ð¼Ð²Ð¾Ð»Ð¾Ð²
                Err(_) if s.len() == 26 => SpaceRef::Id(s.parse().map_err(|_| ApiError::NotFound)?),
                Err(_) => return Err(ApiError::NotFound),
            },
        })
    }
}

/// ÐŸÑ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð¾ Ð¿Ð¾ ÑÑÑ‹Ð»ÐºÐµ Ð² **Ð»Ð¸Ñ‡Ð½Ð¾Ð¹** Ð±Ð°Ð·Ðµ Ð¿Ð¾Ð»ÑŒÐ·Ð¾Ð²Ð°Ñ‚ÐµÐ»Ñ (Ð±ÐµÐ· Ñ€ÐµÐµÑÑ‚Ñ€Ð° Ñ€ÐµÑˆÐµÐ½Ð¸Ð¹).
pub async fn find_space(db: &Db, r: &SpaceRef) -> Result<Space, ApiError> {
    match r {
        SpaceRef::Fixed(kind) => Ok(space_of_kind(db, *kind).await?),
        SpaceRef::Solution(g) => get_solution(db, g.as_str())
            .await?
            .ok_or(ApiError::NotFound),
        SpaceRef::Id(id) => get_space(db, *id).await?.ok_or(ApiError::NotFound),
    }
}

/// Ð§Ð¸ÑÑ‚Ð°Ñ Ð¿Ð¾Ð»Ð¸Ñ‚Ð¸ÐºÐ° Ð´Ð¾ÑÑ‚ÑƒÐ¿Ð° Ðº Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ñƒ (ÑÐ¼. Ð¼Ð¾Ð´ÑƒÐ»ÑŒ); Ñ€Ð¾Ð»ÑŒ ÑƒÑ‡Ð°ÑÑ‚Ð½Ð¸ÐºÐ° â€” Ð²
/// [`effective_write`].
pub fn check_access(
    p: &Principal,
    space: &Space,
    mode: Mode,
    rules_agent_write: bool,
) -> Result<(), ApiError> {
    let Principal::Token(t) = p else {
        return Ok(());
    };
    if space.kind == SpaceKind::Human {
        return Err(ApiError::NotFound);
    }
    if mode == Mode::Write {
        if t.access == Access::Ro {
            return Err(ApiError::Forbidden);
        }
        if space.kind == SpaceKind::Rules && !rules_agent_write {
            return Err(ApiError::Forbidden);
        }
    }
    Ok(())
}

/// Ð”ÐµÐ¹ÑÑ‚Ð²ÑƒÑŽÑ‰ÐµÐµ Ð¿Ñ€Ð°Ð²Ð¾ Ð·Ð°Ð¿Ð¸ÑÐ¸ (Â§11.6): Ñ‚Ð¾ÐºÐµÐ½ `rw` Ð¸ Ñ€Ð¾Ð»ÑŒ `Personal | Owner | Member(Rw)`;
/// Ð°Ð´Ð¼Ð¸Ð½Ð¸ÑÑ‚Ñ€Ð°Ñ‚Ð¾Ñ€ â€” Ð²ÑÐµÐ³Ð´Ð°.
pub fn effective_write(p: &Principal, role: Role) -> Result<(), ApiError> {
    let Principal::Token(t) = p else {
        return Ok(());
    };
    if t.access == Access::Ro {
        return Err(ApiError::Forbidden);
    }
    match role {
        Role::Member(Access::Ro) => Err(ApiError::Denied(
            "Ð·Ð°Ð¿Ð¸ÑÑŒ Ð·Ð°Ð¿Ñ€ÐµÑ‰ÐµÐ½Ð°: Ð´Ð¾ÑÑ‚ÑƒÐ¿ ÑƒÑ‡Ð°ÑÑ‚Ð½Ð¸ÐºÐ° Ðº Ñ€ÐµÑˆÐµÐ½Ð¸ÑŽ â€” ro".into(),
        )),
        _ => Ok(()),
    }
}

/// Ð¢Ð¾ÐºÐµÐ½ Ð¼Ð¾Ð¶ÐµÑ‚ Ñ‡Ð¸Ñ‚Ð°Ñ‚ÑŒ Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð¾ (Ð´Ð»Ñ Ñ„Ð¸Ð»ÑŒÑ‚Ñ€Ð°Ñ†Ð¸Ð¸ ÑÐ¿Ð¸ÑÐºÐ¾Ð²).
pub fn token_can_read(p: &Principal, space: &Space) -> bool {
    check_access(p, space, Mode::Read, false).is_ok()
}

/// Ð—Ð°Ð¿Ð¸ÑÑŒ `ro`-Ñ‚Ð¾ÐºÐµÐ½Ð¾Ð¼ Ð·Ð°Ð¿Ñ€ÐµÑ‰ÐµÐ½Ð° Ð½ÐµÐ·Ð°Ð²Ð¸ÑÐ¸Ð¼Ð¾ Ð¾Ñ‚ Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð° (Ñ€ÐµÑˆÐµÐ½Ð¸Ñ Ð¸ Ñ‚. Ð¿.).
pub fn require_rw(p: &Principal) -> Result<(), ApiError> {
    match p {
        Principal::Token(t) if t.access == Access::Ro => Err(ApiError::Forbidden),
        _ => Ok(()),
    }
}

/// ÐŸÑ€Ð¾Ð²ÐµÑ€ÐºÐ° Ð´Ð¾ÑÑ‚ÑƒÐ¿Ð°; Ð½Ð°ÑÑ‚Ñ€Ð¾Ð¹ÐºÐ° Ñ‡Ð¸Ñ‚Ð°ÐµÑ‚ÑÑ, Ñ‚Ð¾Ð»ÑŒÐºÐ¾ ÐµÑÐ»Ð¸ Ð¾Ð½Ð° Ñ‡Ñ‚Ð¾-Ñ‚Ð¾ Ñ€ÐµÑˆÐ°ÐµÑ‚.
pub async fn authorize(
    st: &AppState,
    p: &Principal,
    space: &Space,
    mode: Mode,
) -> Result<(), ApiError> {
    let needs_setting =
        matches!(p, Principal::Token(_)) && mode == Mode::Write && space.kind == SpaceKind::Rules;
    let allowed = needs_setting
        && st
            .system
            .get_setting::<bool>(RULES_AGENT_WRITE)
            .await?
            .unwrap_or(false);
    check_access(p, space, mode, allowed)
}

/// ÐŸÐ¾Ð»Ð¸Ñ‚Ð¸ÐºÐ° Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð° Ð¸ Ñ€Ð¾Ð»Ð¸ Ð´Ð»Ñ Ñ€ÐµÐ¶Ð¸Ð¼Ð° `mode`.
async fn authorize_role(
    st: &AppState,
    p: &Principal,
    space: &Space,
    role: Role,
    mode: Mode,
) -> Result<(), ApiError> {
    authorize(st, p, space, mode).await?;
    if mode == Mode::Write {
        effective_write(p, role)?;
    }
    Ok(())
}

/// Ð”Ð¾ÑÑ‚ÑƒÐ¿Ð½Ð°Ñ Ð¿Ð¾Ð»ÑŒÐ·Ð¾Ð²Ð°Ñ‚ÐµÐ»ÑŽ Ð·Ð°Ð¿Ñ€Ð¾ÑÐ° Ð¾Ð±Ð»Ð°ÑÑ‚ÑŒ Ð¾Ð´Ð½Ð¾Ð¹ Ð±Ð°Ð·Ñ‹: ÐµÐ³Ð¾ Ð»Ð¸Ñ‡Ð½Ð°Ñ Ð±Ð°Ð·Ð° (Ð»Ð¸Ñ‡Ð½Ñ‹Ðµ
/// Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð° Ð¸ ÑÐ²Ð¾Ð¸ Ñ€ÐµÑˆÐµÐ½Ð¸Ñ) Ð¸Ð»Ð¸ Ð±Ð°Ð·Ð° Ð²Ð»Ð°Ð´ÐµÐ»ÑŒÑ†Ð° Ð¾Ð±Ñ‰ÐµÐ³Ð¾ Ñ€ÐµÑˆÐµÐ½Ð¸Ñ (Ñ€Ð¾Ð²Ð½Ð¾ Ð¾Ð´Ð½Ð¾ Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð¾
/// Ñ€ÐµÑˆÐµÐ½Ð¸Ñ).
pub struct MemScope {
    /// ÐšÑ‚Ð¾ Ð´ÐµÐ¹ÑÑ‚Ð²ÑƒÐµÑ‚ (Ð¿Ð¾Ð»ÑŒÐ·Ð¾Ð²Ð°Ñ‚ÐµÐ»ÑŒ Ñ‚Ð¾ÐºÐµÐ½Ð° Ð¸Ð»Ð¸ `X-Zam-User` Ð°Ð´Ð¼Ð¸Ð½Ð¸ÑÑ‚Ñ€Ð°Ñ‚Ð¾Ñ€Ð°).
    pub user: User,
    /// Ð§ÑŒÑ Ð±Ð°Ð·Ð°.
    pub owner: Id,
    pub db: Db,
    pub space: Space,
    pub role: Role,
}

/// Ð¢Ð¾ Ð¶Ðµ Ð¿Ð»ÑŽÑ ÑƒÐ·ÐµÐ» (Ð² Ñ‚Ð¾Ð¼ Ñ‡Ð¸ÑÐ»Ðµ ÑƒÐ´Ð°Ð»Ñ‘Ð½Ð½Ñ‹Ð¹ â€” Ñ€ÐµÑˆÐ°ÐµÑ‚ Ð¾Ð±Ñ€Ð°Ð±Ð¾Ñ‚Ñ‡Ð¸Ðº).
pub struct NodeScope {
    pub user: User,
    pub owner: Id,
    pub db: Db,
    pub space: Space,
    pub node: Node,
    pub role: Role,
}

/// Ð§Ð¸Ñ‚Ð°ÐµÐ¼Ð°Ñ Ð¾Ð±Ð»Ð°ÑÑ‚ÑŒ Ð¾Ð´Ð½Ð¾Ð¹ Ð±Ð°Ð·Ñ‹: Ð»Ð¸Ñ‡Ð½Ð°Ñ (Ð²ÑÐµ Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð°, Ð´Ð¾ÑÑ‚ÑƒÐ¿Ð½Ñ‹Ðµ Ð¿Ñ€Ð¸Ð½Ñ†Ð¸Ð¿Ð°Ð»Ñƒ) Ð¸Ð»Ð¸
/// Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð¾ Ñ‡ÑƒÐ¶Ð¾Ð³Ð¾ Ñ€ÐµÑˆÐµÐ½Ð¸Ñ. `role` â€” Ð¾Ñ‚Ð½Ð¾ÑˆÐµÐ½Ð¸Ðµ Ð¿Ð¾Ð»ÑŒÐ·Ð¾Ð²Ð°Ñ‚ÐµÐ»Ñ Ðº Ð¾Ð±Ð»Ð°ÑÑ‚Ð¸ (`Personal` Ð´Ð»Ñ
/// Ð»Ð¸Ñ‡Ð½Ð¾Ð¹ Ð±Ð°Ð·Ñ‹, `Member` Ð´Ð»Ñ Ñ‡ÑƒÐ¶Ð¾Ð³Ð¾ Ñ€ÐµÑˆÐµÐ½Ð¸Ñ), Ð° Ð½Ðµ Ð¿Ñ€Ð°Ð²Ð° Ð¿Ñ€Ð¸Ð½Ñ†Ð¸Ð¿Ð°Ð»Ð°.
pub struct Readable {
    pub owner: Id,
    pub db: Db,
    pub spaces: Vec<Space>,
    pub role: Role,
}

/// Ð‘Ð°Ð·Ð° Ð¿Ð¾Ð»ÑŒÐ·Ð¾Ð²Ð°Ñ‚ÐµÐ»Ñ Ð·Ð°Ð¿Ñ€Ð¾ÑÐ°.
pub async fn user_db(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
) -> Result<(User, Db), ApiError> {
    let user = target_user(st, p, headers).await?;
    let db = st.users.get(user.id).await?;
    Ok((user, db))
}

fn is_admin(p: &Principal) -> bool {
    matches!(p, Principal::Admin { .. })
}

/// Ð Ð¾Ð»ÑŒ Ð² Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ðµ Ð»Ð¸Ñ‡Ð½Ð¾Ð¹ Ð±Ð°Ð·Ñ‹.
fn personal_role(p: &Principal, space: &Space) -> Role {
    match (is_admin(p), space.kind) {
        (true, _) => Role::Admin,
        (false, SpaceKind::Solution) => Role::Owner,
        (false, _) => Role::Personal,
    }
}

/// Ð Ð¾Ð»ÑŒ Ð¸Ð· Ñ€ÐµÐµÑÑ‚Ñ€Ð° â†’ [`Role`] Ð¸Ð»Ð¸ Ð¾Ñ‚ÐºÐ°Ð·: Ð¾Ð¶Ð¸Ð´Ð°Ð½Ð¸Ðµ, Ð¾Ñ‚ÐºÐ»Ð¾Ð½ÐµÐ½Ð¸Ðµ, Ð¾Ñ‚Ð·Ñ‹Ð² â€” 403
/// `solution-access`; Ð½ÐµÑ‚ Ð·Ð°Ð¿Ñ€Ð¾ÑÐ° â€” 404.
fn access_role(guid: &SolutionGuid, r: Option<SolutionRole>) -> Result<Role, ApiError> {
    let denied = |state| ApiError::SolutionAccess {
        guid: guid.clone(),
        state,
    };
    match r {
        Some(SolutionRole::Owner) => Ok(Role::Owner),
        Some(SolutionRole::Member(a)) => Ok(Role::Member(a)),
        Some(SolutionRole::Pending) => Err(denied("pending")),
        Some(SolutionRole::Rejected) => Err(denied("rejected")),
        Some(SolutionRole::Revoked) => Err(denied("revoked")),
        None => Err(ApiError::NotFound),
    }
}

/// Ð Ð¾Ð»ÑŒ Ð¿Ñ€Ð¸Ð½Ñ†Ð¸Ð¿Ð°Ð»Ð° Ð² Ñ€ÐµÑˆÐµÐ½Ð¸Ð¸: Ð°Ð´Ð¼Ð¸Ð½Ð¸ÑÑ‚Ñ€Ð°Ñ‚Ð¾Ñ€ â€” `Admin`, Ñ‚Ð¾ÐºÐµÐ½ â€” Ð¿Ð¾ Ñ€ÐµÐµÑÑ‚Ñ€Ñƒ.
async fn solution_role(
    st: &AppState,
    p: &Principal,
    user: &User,
    info: &SolutionInfo,
) -> Result<Role, ApiError> {
    if is_admin(p) {
        return Ok(Role::Admin);
    }
    let r = st.system.solution_role(&info.guid, user.id).await?;
    access_role(&info.guid, r)
}

/// Ð‘Ð°Ð·Ð° Ð²Ð»Ð°Ð´ÐµÐ»ÑŒÑ†Ð° Ð¸ Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð¾ Ñ€ÐµÑˆÐµÐ½Ð¸Ñ Ð¸Ð· Ñ€ÐµÐµÑÑ‚Ñ€Ð°. ÐŸÑ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð¾ Ð¾Ð±ÑÐ·Ð°Ð½Ð¾ Ð±Ñ‹Ñ‚ÑŒ
/// Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð¾Ð¼ `solution` Ñ ÑÑ‚Ð¸Ð¼ GUID â€” Ð¸Ð½Ð°Ñ‡Ðµ 404.
pub async fn solution_space(st: &AppState, info: &SolutionInfo) -> Result<(Db, Space), ApiError> {
    let db = st.users.get(info.owner_id).await?;
    let space = get_space(&db, info.space_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if space.kind != SpaceKind::Solution
        || space.solution_guid.as_deref() != Some(info.guid.as_str())
    {
        tracing::warn!(
            guid = %info.guid,
            owner = %info.owner_id,
            space = %info.space_id,
            "Ñ€ÐµÐµÑÑ‚Ñ€ Ñ€ÐµÑˆÐµÐ½Ð¸Ð¹ ÑƒÐºÐ°Ð·Ñ‹Ð²Ð°ÐµÑ‚ Ð½Ð° Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð¾ Ð´Ñ€ÑƒÐ³Ð¾Ð³Ð¾ Ð²Ð¸Ð´Ð° Ð¸Ð»Ð¸ GUID"
        );
        return Err(ApiError::NotFound);
    }
    Ok((db, space))
}

/// ÐžÐ±Ð»Ð°ÑÑ‚ÑŒ Ñ€ÐµÑˆÐµÐ½Ð¸Ñ Ð¸Ð· Ñ€ÐµÐµÑÑ‚Ñ€Ð° Ð´Ð»Ñ Ð¿Ð¾Ð»ÑŒÐ·Ð¾Ð²Ð°Ñ‚ÐµÐ»Ñ Ð·Ð°Ð¿Ñ€Ð¾ÑÐ°.
async fn solution_scope(
    st: &AppState,
    p: &Principal,
    user: User,
    info: &SolutionInfo,
    mode: Mode,
) -> Result<MemScope, ApiError> {
    let role = solution_role(st, p, &user, info).await?;
    let (db, space) = solution_space(st, info).await?;
    authorize_role(st, p, &space, role, mode).await?;
    Ok(MemScope {
        user,
        owner: info.owner_id,
        db,
        space,
        role,
    })
}

/// ÐžÐ±Ð»Ð°ÑÑ‚ÑŒ Ð»Ð¸Ñ‡Ð½Ð¾Ð¹ Ð±Ð°Ð·Ñ‹.
fn personal_scope(p: &Principal, user: User, db: Db, space: Space) -> MemScope {
    MemScope {
        owner: user.id,
        role: personal_role(p, &space),
        user,
        db,
        space,
    }
}

/// ÐŸÑ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð¾ Ð¿Ð¾ ÑÑ‹Ñ€Ð¾Ð¹ ÑÑÑ‹Ð»ÐºÐµ Ñ Ð¿Ñ€Ð¾Ð²ÐµÑ€ÐºÐ¾Ð¹ Ð´Ð¾ÑÑ‚ÑƒÐ¿Ð°.
pub async fn space_scope(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    raw: &str,
    mode: Mode,
) -> Result<MemScope, ApiError> {
    let r = SpaceRef::parse(raw)?;
    let user = target_user(st, p, headers).await?;
    match r {
        SpaceRef::Fixed(kind) => {
            let db = st.users.get(user.id).await?;
            let space = space_of_kind(&db, kind).await?;
            authorize(st, p, &space, mode).await?;
            Ok(personal_scope(p, user, db, space))
        }
        SpaceRef::Solution(g) => {
            let info = st
                .system
                .get_solution(&g)
                .await?
                .ok_or(ApiError::NotFound)?;
            solution_scope(st, p, user, &info, mode).await
        }
        SpaceRef::Id(id) => {
            let db = st.users.get(user.id).await?;
            if let Some(space) = get_space(&db, id).await? {
                authorize(st, p, &space, mode).await?;
                return Ok(personal_scope(p, user, db, space));
            }
            // Ñ‡ÑƒÐ¶Ð°Ñ Ð±Ð°Ð·Ð° â€” Ñ‚Ð¾Ð»ÑŒÐºÐ¾ Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð¾ Ñ€ÐµÑˆÐµÐ½Ð¸Ñ Ð¸Ð· Ñ€ÐµÐµÑÑ‚Ñ€Ð°
            let info = st
                .system
                .solution_by_space(id)
                .await?
                .ok_or(ApiError::NotFound)?;
            solution_scope(st, p, user, &info, mode).await
        }
    }
}

/// Ð§ÑƒÐ¶Ð¸Ðµ Ñ€ÐµÑˆÐµÐ½Ð¸Ñ, Ð² Ð±Ð°Ð·Ð°Ñ… Ð²Ð»Ð°Ð´ÐµÐ»ÑŒÑ†ÐµÐ² ÐºÐ¾Ñ‚Ð¾Ñ€Ñ‹Ñ… Ð¸Ñ‰ÐµÑ‚ÑÑ ÑƒÐ·ÐµÐ» Ð¸Ð»Ð¸ Ð²Ð»Ð¾Ð¶ÐµÐ½Ð¸Ðµ: Ñ‚Ð¾ÐºÐµÐ½Ñƒ â€” Ñ€ÐµÑˆÐµÐ½Ð¸Ñ Ñ
/// ÐµÐ³Ð¾ Ð·Ð°Ð¿Ñ€Ð¾ÑÐ¾Ð¼ Ð´Ð¾ÑÑ‚ÑƒÐ¿Ð° Ð² Ð»ÑŽÐ±Ð¾Ð¼ ÑÐ¾ÑÑ‚Ð¾ÑÐ½Ð¸Ð¸ (Ð¾Ñ‚ÐºÐ°Ð· â€” Ð¾ÑˆÐ¸Ð±ÐºÐ¾Ð¹ Ñ€Ð¾Ð»Ð¸), Ð°Ð´Ð¼Ð¸Ð½Ð¸ÑÑ‚Ñ€Ð°Ñ‚Ð¾Ñ€Ñƒ â€” Ð²ÑÐµ
/// Ñ€ÐµÑˆÐµÐ½Ð¸Ñ Ð´Ñ€ÑƒÐ³Ð¸Ñ… Ð²Ð»Ð°Ð´ÐµÐ»ÑŒÑ†ÐµÐ².
pub async fn foreign_solutions(
    st: &AppState,
    p: &Principal,
    user: &User,
) -> Result<Vec<(SolutionInfo, Result<Role, ApiError>)>, ApiError> {
    if is_admin(p) {
        return Ok(st
            .system
            .list_solutions()
            .await?
            .into_iter()
            .filter(|i| i.owner_id != user.id)
            .map(|i| (i, Ok(Role::Admin)))
            .collect());
    }
    Ok(st
        .system
        .member_solutions(user.id)
        .await?
        .into_iter()
        .map(|(i, r)| {
            let role = access_role(&i.guid, Some(r));
            (i, role)
        })
        .collect())
}

/// Ð£Ð·ÐµÐ» Ð¿Ð¾ id Ñ Ð¿Ñ€Ð¾Ð²ÐµÑ€ÐºÐ¾Ð¹ Ð´Ð¾ÑÑ‚ÑƒÐ¿Ð° Ðº ÐµÐ³Ð¾ Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ñƒ.
pub async fn node_scope(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    raw_id: &str,
    mode: Mode,
) -> Result<NodeScope, ApiError> {
    let id = super::parse_id(raw_id)?;
    let user = target_user(st, p, headers).await?;
    let db = st.users.get(user.id).await?;
    match get_node(&db, id).await {
        Ok(node) => {
            let space = get_space(&db, node.space_id)
                .await?
                .ok_or(ApiError::NotFound)?;
            authorize(st, p, &space, mode).await?;
            return Ok(NodeScope {
                owner: user.id,
                role: personal_role(p, &space),
                user,
                db,
                space,
                node,
            });
        }
        Err(StoreError::NotFound) => {}
        Err(e) => return Err(e.into()),
    }
    for (info, role) in foreign_solutions(st, p, &user).await? {
        let odb = st.users.get(info.owner_id).await?;
        let node = match get_node(&odb, id).await {
            Ok(n) => n,
            Err(StoreError::NotFound) => continue,
            Err(e) => return Err(e.into()),
        };
        // ÑƒÐ·ÐµÐ» Ð²Ð»Ð°Ð´ÐµÐ»ÑŒÑ†Ð° Ð²Ð½Ðµ Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð° Ñ€ÐµÑˆÐµÐ½Ð¸Ñ ÑƒÑ‡Ð°ÑÑ‚Ð½Ð¸ÐºÑƒ Ð½Ðµ ÑÑƒÑ‰ÐµÑÑ‚Ð²ÑƒÐµÑ‚
        if node.space_id != info.space_id {
            continue;
        }
        let role = role?;
        let (db, space) = solution_space(st, &info).await?;
        authorize_role(st, p, &space, role, mode).await?;
        return Ok(NodeScope {
            user,
            owner: info.owner_id,
            db,
            space,
            node,
            role,
        });
    }
    Err(ApiError::NotFound)
}

/// Ð§Ð¸Ñ‚Ð°ÐµÐ¼Ñ‹Ðµ Ð¾Ð±Ð»Ð°ÑÑ‚Ð¸ Ð¿Ð¾Ð»ÑŒÐ·Ð¾Ð²Ð°Ñ‚ÐµÐ»Ñ `user`: Ð»Ð¸Ñ‡Ð½Ð°Ñ Ð±Ð°Ð·Ð° (Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð°, Ð²Ð¸Ð´Ð¸Ð¼Ñ‹Ðµ Ð¿Ñ€Ð¸Ð½Ñ†Ð¸Ð¿Ð°Ð»Ñƒ)
/// Ð¸ Ð¿Ð¾ Ð±Ð°Ð·Ðµ Ð½Ð° ÐºÐ°Ð¶Ð´Ð¾Ðµ Ð¾Ð´Ð¾Ð±Ñ€ÐµÐ½Ð½Ð¾Ðµ Ñ‡ÑƒÐ¶Ð¾Ðµ Ñ€ÐµÑˆÐµÐ½Ð¸Ðµ (Ñ‚Ð¾Ð»ÑŒÐºÐ¾ ÐµÐ³Ð¾ Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð¾).
pub async fn readable_areas(
    st: &AppState,
    p: &Principal,
    user: &User,
) -> Result<Vec<Readable>, ApiError> {
    let db = st.users.get(user.id).await?;
    let spaces = list_spaces(&db)
        .await?
        .into_iter()
        .filter(|s| token_can_read(p, s))
        .collect();
    let mut out = vec![Readable {
        owner: user.id,
        db,
        spaces,
        role: Role::Personal,
    }];
    for (info, role) in st.system.accessible_solutions(user.id).await? {
        let SolutionRole::Member(a) = role else {
            continue;
        };
        match solution_space(st, &info).await {
            Ok((db, space)) => out.push(Readable {
                owner: info.owner_id,
                db,
                spaces: vec![space],
                role: Role::Member(a),
            }),
            // Ð¿Ñ€Ð¾ÑÑ‚Ñ€Ð°Ð½ÑÑ‚Ð²Ð¾ Ñ€ÐµÑˆÐµÐ½Ð¸Ñ Ð¿Ñ€Ð¾Ð¿Ð°Ð»Ð¾ Ñƒ Ð²Ð»Ð°Ð´ÐµÐ»ÑŒÑ†Ð° â€” Ñ€ÐµÑˆÐµÐ½Ð¸Ðµ Ð¿Ñ€Ð¾Ð¿ÑƒÑÐºÐ°ÐµÑ‚ÑÑ
            Err(ApiError::NotFound) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// Ð–Ð¸Ð²Ð¾Ð¹ ÑƒÐ·ÐµÐ» (ÑƒÐ´Ð°Ð»Ñ‘Ð½Ð½Ñ‹Ð¹ â†’ 404).
pub fn live(s: &NodeScope) -> Result<(), ApiError> {
    match s.node.deleted_at {
        Some(_) => Err(ApiError::NotFound),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zam_store::AuthToken;

    fn space(kind: SpaceKind) -> Space {
        Space {
            id: Id::new(),
            kind,
            solution_guid: None,
            name: "x".into(),
        }
    }

    fn token(access: Access) -> Principal {
        Principal::Token(AuthToken {
            token_id: Id::new(),
            user_id: Id::new(),
            access,
        })
    }

    #[test]
    fn policy_matrix() {
        let admin = Principal::Admin {
            session_id: "s".into(),
        };
        let (rw, ro) = (token(Access::Rw), token(Access::Ro));
        let h = space(SpaceKind::Human);
        let r = space(SpaceKind::Rules);
        let k = space(SpaceKind::Knowledge);
        let nf = |res: Result<(), ApiError>| matches!(res, Err(ApiError::NotFound));
        let fb = |res: Result<(), ApiError>| matches!(res, Err(ApiError::Forbidden));
        for m in [Mode::Read, Mode::Write] {
            assert!(nf(check_access(&rw, &h, m, true)));
            assert!(nf(check_access(&ro, &h, m, true)));
            assert!(check_access(&admin, &h, m, false).is_ok());
            assert!(check_access(&admin, &r, m, false).is_ok());
        }
        assert!(check_access(&ro, &k, Mode::Read, false).is_ok());
        assert!(fb(check_access(&ro, &k, Mode::Write, true)));
        assert!(check_access(&rw, &k, Mode::Write, false).is_ok());
        assert!(check_access(&rw, &r, Mode::Read, false).is_ok());
        assert!(fb(check_access(&rw, &r, Mode::Write, false)));
        assert!(check_access(&rw, &r, Mode::Write, true).is_ok());
        assert!(fb(check_access(&ro, &r, Mode::Write, true)));
    }

    #[test]
    fn effective_write_matrix() {
        let admin = Principal::Admin {
            session_id: "s".into(),
        };
        let (rw, ro) = (token(Access::Rw), token(Access::Ro));
        let roles = [
            Role::Personal,
            Role::Owner,
            Role::Member(Access::Rw),
            Role::Member(Access::Ro),
            Role::Admin,
        ];
        for r in roles {
            assert!(effective_write(&admin, r).is_ok(), "{r:?}");
            assert!(
                matches!(effective_write(&ro, r), Err(ApiError::Forbidden)),
                "{r:?}"
            );
        }
        for r in [Role::Personal, Role::Owner, Role::Member(Access::Rw)] {
            assert!(effective_write(&rw, r).is_ok(), "{r:?}");
        }
        assert!(matches!(
            effective_write(&rw, Role::Member(Access::Ro)),
            Err(ApiError::Denied(_))
        ));
    }

    #[test]
    fn access_role_states() {
        let g = SolutionGuid::new_v7();
        let state = |r| match access_role(&g, Some(r)) {
            Err(ApiError::SolutionAccess { state, .. }) => state,
            _ => "ok",
        };
        assert_eq!(state(SolutionRole::Pending), "pending");
        assert_eq!(state(SolutionRole::Rejected), "rejected");
        assert_eq!(state(SolutionRole::Revoked), "revoked");
        assert!(matches!(access_role(&g, None), Err(ApiError::NotFound)));
        assert_eq!(
            access_role(&g, Some(SolutionRole::Member(Access::Ro))).unwrap(),
            Role::Member(Access::Ro)
        );
    }

    #[test]
    fn space_ref_forms() {
        assert_eq!(
            SpaceRef::parse("rules").unwrap(),
            SpaceRef::Fixed(SpaceKind::Rules)
        );
        let g = "0B3C2A8E-6F1D-4C5E-9A7B-1D2E3F405162";
        assert_eq!(
            SpaceRef::parse(g).unwrap(),
            SpaceRef::Solution(g.to_lowercase().parse().unwrap())
        );
        let id = Id::new();
        assert_eq!(SpaceRef::parse(&id.to_string()).unwrap(), SpaceRef::Id(id));
        for bad in ["Rules", "solution", "0b3c2a8e6f1d4c5e9a7b1d2e3f405162", ""] {
            assert!(
                matches!(SpaceRef::parse(bad), Err(ApiError::NotFound)),
                "{bad}"
            );
        }
    }
}
