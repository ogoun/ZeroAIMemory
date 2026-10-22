//! Адресация пространств и политика доступа (§7.1, §11.4, §11.6).
//!
//! `SpaceRef` в пути `{space}` или поле `space`: `rules` | `human` | `knowledge` |
//! GUID решения (UUID с дефисами, регистр не важен) | `Id` пространства (26 символов).
//! Неизвестное → 404.
//!
//! **Области.** Запрос работает в одной базе: личной базе пользователя запроса или базе
//! владельца общего решения — и тогда только в пространстве этого решения ([`MemScope`]):
//! - GUID решения ищется в реестре `system.db` (каждый запрос, без кэша): роль → база
//!   владельца → пространство `space_id`. Не зарегистрирован → 404;
//! - `Id` пространства — в личной базе, иначе только `space_id` решения из реестра;
//! - узел по id ([`node_scope`]) — в личной базе, затем в базах владельцев решений, где у
//!   пользователя есть запрос доступа; узел принимается, только если он в пространстве
//!   решения (иначе 404: Rules, Human, Knowledge владельца участнику не видны);
//! - запись журнала по id ([`entry_scope`]) — то же правило, что у узла;
//! - ребро графа по id ([`edge_scope`]) — по пространству источника; видимость цели и
//!   область графа ([`graph_spaces`]) — участнику только пространство решения.
//!
//! **Политика:**
//! - `Human` по токену → 404 (существование не раскрывается) — для чтения и записи;
//! - запись по токену `ro` → 403;
//! - запись в `Rules` по токену при `rules.agent_write = false` → 403;
//! - участник: доступ только при одобренном запросе (`pending`/`rejected`/`revoked` → 403
//!   `/problems/solution-access`), запись — при режиме участника `rw` и токене `rw`;
//! - администратор может всё.
//!
//! Настройка `rules.agent_write` и роль в решении читаются из `system.db` на каждом запросе
//! (решения о доступе не кэшируются): отзыв действует со следующего запроса.
use axum::http::HeaderMap;
use zam_core::{Id, SolutionGuid};
use zam_store::{
    Access, Db, EdgeView, Entry, Node, SolutionInfo, SolutionRole, Space, SpaceKind, StoreError,
    User, get_edge, get_entry, get_node, get_solution, get_space, list_spaces, space_of_kind,
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

/// Отношение запроса к области памяти.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Личное пространство пользователя (Rules, Human, Knowledge).
    Personal,
    /// Своё решение.
    Owner,
    /// Одобренный участник чужого решения с режимом.
    Member(Access),
    /// Администратор (cookie WebUI) — без ограничений.
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

    /// Режим доступа к области: участнику — его режим, администратору — `admin`,
    /// остальным — `rw`.
    pub fn access_str(self) -> &'static str {
        match self {
            Role::Member(a) => a.as_str(),
            Role::Admin => "admin",
            Role::Personal | Role::Owner => "rw",
        }
    }
}

/// Разобранная ссылка на пространство.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceRef {
    Fixed(SpaceKind),
    Solution(SolutionGuid),
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
            _ => match s.parse::<SolutionGuid>() {
                Ok(g) => SpaceRef::Solution(g),
                // `Id::from_str` принимает и формы UUID — здесь только 26 символов
                Err(_) if s.len() == 26 => SpaceRef::Id(s.parse().map_err(|_| ApiError::NotFound)?),
                Err(_) => return Err(ApiError::NotFound),
            },
        })
    }
}

/// Пространство по ссылке в **личной** базе пользователя (без реестра решений).
pub async fn find_space(db: &Db, r: &SpaceRef) -> Result<Space, ApiError> {
    match r {
        SpaceRef::Fixed(kind) => Ok(space_of_kind(db, *kind).await?),
        SpaceRef::Solution(g) => get_solution(db, g.as_str())
            .await?
            .ok_or(ApiError::NotFound),
        SpaceRef::Id(id) => get_space(db, *id).await?.ok_or(ApiError::NotFound),
    }
}

/// Чистая политика доступа к пространству (см. модуль); роль участника — в
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

/// Действующее право записи (§11.6): токен `rw` и роль `Personal | Owner | Member(Rw)`;
/// администратор — всегда.
pub fn effective_write(p: &Principal, role: Role) -> Result<(), ApiError> {
    let Principal::Token(t) = p else {
        return Ok(());
    };
    if t.access == Access::Ro {
        return Err(ApiError::Forbidden);
    }
    match role {
        Role::Member(Access::Ro) => Err(ApiError::Denied(
            "запись запрещена: доступ участника к решению — ro".into(),
        )),
        _ => Ok(()),
    }
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

/// Политика пространства и роли для режима `mode`.
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

/// Доступная пользователю запроса область одной базы: его личная база (личные
/// пространства и свои решения) или база владельца общего решения (ровно одно пространство
/// решения).
pub struct MemScope {
    /// Кто действует (пользователь токена или `X-Zam-User` администратора).
    pub user: User,
    /// Чья база.
    pub owner: Id,
    pub db: Db,
    pub space: Space,
    pub role: Role,
}

/// То же плюс узел (в том числе удалённый — решает обработчик).
pub struct NodeScope {
    pub user: User,
    pub owner: Id,
    pub db: Db,
    pub space: Space,
    pub node: Node,
    pub role: Role,
}

/// То же плюс запись журнала (в том числе удалённая — решает обработчик).
pub struct EntryScope {
    pub user: User,
    pub owner: Id,
    pub db: Db,
    pub space: Space,
    pub entry: Entry,
    pub role: Role,
}

/// То же плюс ребро графа (живое; `space` — пространство источника).
pub struct EdgeScope {
    pub user: User,
    pub owner: Id,
    pub db: Db,
    pub space: Space,
    pub edge: EdgeView,
    pub role: Role,
}

/// Читаемая область одной базы: личная (все пространства, доступные принципалу) или
/// пространство чужого решения. `role` — отношение пользователя к области (`Personal` для
/// личной базы, `Member` для чужого решения), а не права принципала.
pub struct Readable {
    pub owner: Id,
    pub db: Db,
    pub spaces: Vec<Space>,
    pub role: Role,
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

fn is_admin(p: &Principal) -> bool {
    matches!(p, Principal::Admin { .. })
}

/// Роль в пространстве личной базы.
fn personal_role(p: &Principal, space: &Space) -> Role {
    match (is_admin(p), space.kind) {
        (true, _) => Role::Admin,
        (false, SpaceKind::Solution) => Role::Owner,
        (false, _) => Role::Personal,
    }
}

/// Роль из реестра → [`Role`] или отказ: ожидание, отклонение, отзыв — 403
/// `solution-access`; нет запроса — 404.
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

/// Роль принципала в решении: администратор — `Admin`, токен — по реестру.
pub(crate) async fn solution_role(
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

/// База владельца и пространство решения из реестра. Пространство обязано быть
/// пространством `solution` с этим GUID — иначе 404.
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
            "реестр решений указывает на пространство другого вида или GUID"
        );
        return Err(ApiError::NotFound);
    }
    Ok((db, space))
}

/// Область решения из реестра для пользователя запроса.
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

/// Область личной базы.
fn personal_scope(p: &Principal, user: User, db: Db, space: Space) -> MemScope {
    MemScope {
        owner: user.id,
        role: personal_role(p, &space),
        user,
        db,
        space,
    }
}

/// Пространство по сырой ссылке с проверкой доступа.
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
            // чужая база — только пространство решения из реестра
            let info = st
                .system
                .solution_by_space(id)
                .await?
                .ok_or(ApiError::NotFound)?;
            solution_scope(st, p, user, &info, mode).await
        }
    }
}

/// Чужие решения, в базах владельцев которых ищется узел или вложение: токену — решения с
/// его запросом доступа в любом состоянии (отказ — ошибкой роли), администратору — все
/// решения других владельцев.
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

/// Чужая база не открылась или не читается: решение пропускается с WARN (без текста
/// заметок), ответ строится по остальным базам.
pub fn skip_foreign(info: &SolutionInfo, e: &dyn std::fmt::Display) {
    tracing::warn!(
        owner = %info.owner_id,
        guid = %info.guid,
        error = %e,
        "база владельца решения недоступна: решение пропущено"
    );
}

/// База владельца чужого решения; ошибка открытия — WARN и `None`.
pub async fn owner_db(st: &AppState, info: &SolutionInfo) -> Option<Db> {
    match st.users.get(info.owner_id).await {
        Ok(db) => Some(db),
        Err(e) => {
            skip_foreign(info, &e);
            None
        }
    }
}

/// Объект базы (узел, запись) в доступной принципалу области.
struct Located<T> {
    user: User,
    owner: Id,
    db: Db,
    space: Space,
    item: T,
    role: Role,
}

/// Находит объект по id: сначала в личной базе пользователя, затем в базах владельцев
/// решений, к которым у него есть запрос доступа. Чужой объект принимается, только если он
/// в пространстве решения (иначе 404); сбой чужой базы — пропуск решения с WARN.
/// `get` читает объект из базы, `space_of` отдаёт его пространство.
async fn locate<T>(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    id: Id,
    mode: Mode,
    get: impl AsyncFn(&Db, Id) -> Result<T, StoreError>,
    space_of: impl Fn(&T) -> Id,
) -> Result<Located<T>, ApiError> {
    let user = target_user(st, p, headers).await?;
    let db = st.users.get(user.id).await?;
    match get(&db, id).await {
        Ok(item) => {
            let space = get_space(&db, space_of(&item))
                .await?
                .ok_or(ApiError::NotFound)?;
            authorize(st, p, &space, mode).await?;
            return Ok(Located {
                owner: user.id,
                role: personal_role(p, &space),
                user,
                db,
                space,
                item,
            });
        }
        Err(StoreError::NotFound) => {}
        Err(e) => return Err(e.into()),
    }
    for (info, role) in foreign_solutions(st, p, &user).await? {
        let Some(odb) = owner_db(st, &info).await else {
            continue;
        };
        let item = match get(&odb, id).await {
            Ok(n) => n,
            Err(StoreError::NotFound) => continue,
            Err(e) => {
                skip_foreign(&info, &e);
                continue;
            }
        };
        // объект владельца вне пространства решения участнику не существует
        if space_of(&item) != info.space_id {
            continue;
        }
        let role = role?;
        let (db, space) = match solution_space(st, &info).await {
            Ok(v) => v,
            Err(ApiError::NotFound) => continue,
            Err(e) => {
                skip_foreign(&info, &e);
                continue;
            }
        };
        authorize_role(st, p, &space, role, mode).await?;
        return Ok(Located {
            user,
            owner: info.owner_id,
            db,
            space,
            item,
            role,
        });
    }
    Err(ApiError::NotFound)
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
    let l = locate(
        st,
        p,
        headers,
        id,
        mode,
        async |db: &Db, id| get_node(db, id).await,
        |n: &Node| n.space_id,
    )
    .await?;
    Ok(NodeScope {
        user: l.user,
        owner: l.owner,
        db: l.db,
        space: l.space,
        node: l.item,
        role: l.role,
    })
}

/// Запись журнала по id с проверкой доступа к её пространству: то же правило, что у
/// [`node_scope`] (в чужой базе — только записи пространства решения; Knowledge, Rules и
/// Human владельца участнику не существуют).
pub async fn entry_scope(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    raw_id: &str,
    mode: Mode,
) -> Result<EntryScope, ApiError> {
    let id = super::parse_id(raw_id)?;
    let l = locate(
        st,
        p,
        headers,
        id,
        mode,
        async |db: &Db, id| get_entry(db, id).await,
        |e: &Entry| e.space_id,
    )
    .await?;
    Ok(EntryScope {
        user: l.user,
        owner: l.owner,
        db: l.db,
        space: l.space,
        entry: l.item,
        role: l.role,
    })
}

/// Ребро графа по id с проверкой доступа к пространству источника (правило [`node_scope`]).
/// Видимость цели ребра проверяет обработчик ([`graph_spaces`]).
pub async fn edge_scope(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    raw_id: &str,
    mode: Mode,
) -> Result<EdgeScope, ApiError> {
    let id = super::parse_id(raw_id)?;
    let l = locate(
        st,
        p,
        headers,
        id,
        mode,
        async |db: &Db, id| get_edge(db, id).await?.ok_or(StoreError::NotFound),
        |e: &EdgeView| e.src_space,
    )
    .await?;
    Ok(EdgeScope {
        user: l.user,
        owner: l.owner,
        db: l.db,
        space: l.space,
        edge: l.item,
        role: l.role,
    })
}

/// Область графа запроса в базе области (`role` из [`MemScope`]/[`NodeScope`]): участнику
/// чужого решения — только пространство решения `space` (Knowledge, Rules, Human, другие
/// решения владельца ему не видны, §11.6); иначе — пространства базы, видимые принципалу
/// (по токену — без Human).
pub async fn graph_spaces(
    p: &Principal,
    db: &Db,
    space: &Space,
    role: Role,
) -> Result<Vec<Id>, ApiError> {
    if let Role::Member(_) = role {
        return Ok(vec![space.id]);
    }
    Ok(list_spaces(db)
        .await?
        .into_iter()
        .filter(|s| token_can_read(p, s))
        .map(|s| s.id)
        .collect())
}

/// Читаемые области пользователя `user`: личная база (пространства, видимые принципалу)
/// и по базе на каждое одобренное чужое решение (только его пространство).
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
            // пространство решения пропало у владельца — решение пропускается
            Err(ApiError::NotFound) => {}
            Err(e) => skip_foreign(&info, &e),
        }
    }
    Ok(out)
}

/// Живая запись (удалённая → 404).
pub fn live_entry(s: &EntryScope) -> Result<(), ApiError> {
    match s.entry.deleted_at {
        Some(_) => Err(ApiError::NotFound),
        None => Ok(()),
    }
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
            human: false,
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
