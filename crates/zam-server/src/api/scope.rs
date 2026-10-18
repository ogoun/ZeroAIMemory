//! Адресация пространств и политика доступа (§7.1, §11.4).
//!
//! `SpaceRef` в пути `{space}` или поле `space`: `rules` | `human` | `knowledge` |
//! GUID решения (UUID с дефисами, регистр не важен) | `Id` пространства (26 символов).
//! Неизвестное → 404.
//!
//! Политика:
//! - `Human` по токену → 404 (существование не раскрывается) — для чтения и записи;
//! - запись по токену `ro` → 403;
//! - запись в `Rules` по токену при `rules.agent_write = false` → 403;
//! - администратор может всё.
//!
//! Настройка `rules.agent_write` читается из `system.db` на каждом запросе записи
//! (решения о доступе не кэшируются).
use axum::http::HeaderMap;
use zam_core::Id;
use zam_store::{
    Access, Db, Node, Space, SpaceKind, User, get_node, get_solution, get_space, space_of_kind,
};

use crate::app::AppState;
use crate::auth::{Principal, target_user};
use crate::error::ApiError;

/// Ключ настройки записи агента в Rules.
pub const RULES_AGENT_WRITE: &str = "rules.agent_write";

/// Чтение или запись.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Read,
    Write,
}

/// Разобранная ссылка на пространство.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceRef {
    Fixed(SpaceKind),
    /// GUID решения в каноническом виде: нижний регистр с дефисами.
    Solution(String),
    Id(Id),
}

impl SpaceRef {
    /// Неизвестная форма → 404 (как и несуществующее пространство).
    pub fn parse(raw: &str) -> Result<SpaceRef, ApiError> {
        let s = raw.trim();
        Ok(match s {
            "rules" => SpaceRef::Fixed(SpaceKind::Rules),
            "human" => SpaceRef::Fixed(SpaceKind::Human),
            "knowledge" => SpaceRef::Fixed(SpaceKind::Knowledge),
            _ => match canonical_guid(s) {
                Some(g) => SpaceRef::Solution(g),
                // `Id::from_str` принимает и формы UUID — здесь только 26 символов
                None if s.len() == 26 => SpaceRef::Id(s.parse().map_err(|_| ApiError::NotFound)?),
                None => return Err(ApiError::NotFound),
            },
        })
    }
}

/// UUID любой версии строго в форме с дефисами (36 символов) → нижний регистр.
pub fn canonical_guid(s: &str) -> Option<String> {
    if s.len() != 36 {
        return None;
    }
    uuid::Uuid::try_parse(s)
        .ok()
        .map(|u| u.hyphenated().to_string())
}

/// Пространство по ссылке в базе пользователя.
pub async fn find_space(db: &Db, r: &SpaceRef) -> Result<Space, ApiError> {
    match r {
        SpaceRef::Fixed(kind) => Ok(space_of_kind(db, *kind).await?),
        SpaceRef::Solution(g) => get_solution(db, g).await?.ok_or(ApiError::NotFound),
        SpaceRef::Id(id) => get_space(db, *id).await?.ok_or(ApiError::NotFound),
    }
}

/// Чистая политика доступа (см. модуль).
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

/// Токен может читать пространство (для фильтрации списков).
pub fn token_can_read(p: &Principal, space: &Space) -> bool {
    check_access(p, space, Mode::Read, false).is_ok()
}

/// Запись `ro`-токеном запрещена независимо от пространства (решения и т. п.).
pub fn require_rw(p: &Principal) -> Result<(), ApiError> {
    match p {
        Principal::Token(t) if t.access == Access::Ro => Err(ApiError::Forbidden),
        _ => Ok(()),
    }
}

/// Проверка доступа; настройка читается, только если она что-то решает.
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

/// Пользователь запроса, его база и проверенное пространство.
pub struct Scope {
    pub user: User,
    pub db: Db,
    pub space: Space,
}

/// То же плюс узел (в том числе удалённый — решает обработчик).
pub struct NodeScope {
    pub user: User,
    pub db: Db,
    pub space: Space,
    pub node: Node,
}

/// База пользователя запроса.
pub async fn user_db(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
) -> Result<(User, Db), ApiError> {
    let user = target_user(st, p, headers).await?;
    let db = st.users.get(user.id).await?;
    Ok((user, db))
}

/// Пространство по сырой ссылке с проверкой доступа.
pub async fn space_scope(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    raw: &str,
    mode: Mode,
) -> Result<Scope, ApiError> {
    let r = SpaceRef::parse(raw)?;
    let (user, db) = user_db(st, p, headers).await?;
    let space = find_space(&db, &r).await?;
    authorize(st, p, &space, mode).await?;
    Ok(Scope { user, db, space })
}

/// Узел по id с проверкой доступа к его пространству.
pub async fn node_scope(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    raw_id: &str,
    mode: Mode,
) -> Result<NodeScope, ApiError> {
    let id = super::parse_id(raw_id)?;
    let (user, db) = user_db(st, p, headers).await?;
    let node = get_node(&db, id).await?;
    let space = get_space(&db, node.space_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize(st, p, &space, mode).await?;
    Ok(NodeScope {
        user,
        db,
        space,
        node,
    })
}

/// Живой узел (удалённый → 404).
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
    fn space_ref_forms() {
        assert_eq!(
            SpaceRef::parse("rules").unwrap(),
            SpaceRef::Fixed(SpaceKind::Rules)
        );
        let g = "0B3C2A8E-6F1D-4C5E-9A7B-1D2E3F405162";
        assert_eq!(
            SpaceRef::parse(g).unwrap(),
            SpaceRef::Solution(g.to_lowercase())
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
