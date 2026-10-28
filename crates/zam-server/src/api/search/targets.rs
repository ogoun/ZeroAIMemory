//! Область поиска (§9.1, §11.6): цели — базы с пространствами, по которым идёт один
//! запрос `POST /search`.
//!
//! - **Личная база** пользователя запроса — одна цель: решения пользователя, Knowledge и
//!   Rules по флагам (`include_knowledge`, `include_rules`, по умолчанию да), Human — только
//!   администратору с `include_human: true` (токен его не видит никогда).
//! - **Чужое решение** из `scope.solution` или заголовка `X-Zam-Solution` — отдельная цель:
//!   база владельца и ровно одно пространство решения. Роль читается из `system.db` на каждом
//!   запросе (кэша нет): одобренный участник — цель; ожидание, отклонение, отзыв — решение
//!   исключается, в `warnings` — `solution access pending|rejected|revoked`. Нет запроса
//!   доступа или решение не зарегистрировано — молча ничего (как неизвестное решение в M2).
//!   Администратору (cookie) чужое решение доступно всегда.
//! - `all_solutions` — все доступные: свои решения и одобренные чужие (без предупреждений
//!   о прочих).
//! - `scope.spaces` — явный список по тем же правилам: `rules|human|knowledge` — личная
//!   база; GUID — своё решение, иначе чужое; `Id` — пространство личной базы, иначе только
//!   пространство решения из реестра (Knowledge, Rules, Human владельца по `Id` не находятся).
//!   Неизвестная форма или пространство пропускается (ответ 200).
//! - Недоступная база владельца (не открылась, пространство решения не читается) — решение
//!   пропускается с WARN в журнал и `warnings: ["solution unavailable"]`
//!   **[решение исполнителя]**; поиск по остальным базам продолжается.
use axum::http::HeaderMap;
use serde::Deserialize;
use zam_core::{Id, SolutionGuid};
use zam_store::{
    Db, SolutionInfo, SolutionRole, Space, SpaceKind, get_solution, get_space, list_spaces,
};

use super::super::scope::{
    Role, SpaceRef, find_space, skip_foreign, solution_space, token_can_read,
};
use super::SOLUTION_HEADER;
use crate::app::AppState;
use crate::auth::{Principal, target_user};
use crate::error::ApiError;

/// Предупреждение: база владельца решения недоступна, решение пропущено.
pub const WARN_SOLUTION_UNAVAILABLE: &str = "solution unavailable";

/// `scope` запроса поиска.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct ScopeReq {
    pub solution: Option<String>,
    pub include_knowledge: Option<bool>,
    pub include_rules: Option<bool>,
    pub all_solutions: bool,
    pub path_prefix: Option<String>,
    pub spaces: Option<Vec<String>>,
    pub include_human: bool,
    /// Строгий фильтр области (§9.1): только узлы и записи с этой `area`.
    pub area: Option<String>,
}

/// Цель поиска: база и её пространства в области запроса (непустой список).
pub struct SearchTarget {
    /// Владелец базы (тенант точек Qdrant).
    pub owner: Id,
    pub db: Db,
    pub spaces: Vec<Space>,
    /// Отношение пользователя запроса к цели: `Personal` (или `Admin`) — личная база,
    /// `Member` (или `Admin`) — чужое решение.
    pub role: Role,
    /// Чужое решение из реестра; `None` — личная база.
    pub solution: Option<SolutionInfo>,
}

impl SearchTarget {
    /// Все пространства цели.
    pub fn space_ids(&self) -> Vec<Id> {
        self.spaces.iter().map(|s| s.id).collect()
    }

    /// Пространства цели для векторного канала: Human в Qdrant нет никогда.
    pub fn vector_space_ids(&self) -> Vec<Id> {
        self.spaces
            .iter()
            .filter(|s| s.kind != SpaceKind::Human)
            .map(|s| s.id)
            .collect()
    }
}

fn is_admin(p: &Principal) -> bool {
    p.is_admin()
}

/// Решение запроса: `scope.solution` (некорректный GUID — 400) или заголовок
/// `X-Zam-Solution` (некорректное значение игнорируется).
pub fn request_solution(
    solution: Option<&str>,
    h: &HeaderMap,
) -> Result<Option<SolutionGuid>, ApiError> {
    Ok(match solution {
        Some(g) => Some(
            g.trim()
                .parse::<SolutionGuid>()
                .map_err(|_| ApiError::BadRequest("scope.solution: GUID решения".into()))?,
        ),
        None => h
            .get(SOLUTION_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<SolutionGuid>().ok()),
    })
}

/// Чужое решение до проверки роли.
enum Foreign {
    Guid(SolutionGuid),
    Info(SolutionInfo),
}

/// Цели запроса и предупреждения (см. модуль).
pub async fn resolve_targets(
    st: &AppState,
    p: &Principal,
    h: &HeaderMap,
    sc: &ScopeReq,
) -> Result<(Vec<SearchTarget>, Vec<String>), ApiError> {
    let user = target_user(st, p, h).await?;
    let db = st.users.get(user.id).await?;
    // Human — только человеку (сессия WebUI) и только своей базы
    let human_ok = p.is_human() && sc.include_human;
    let mut personal: Vec<Space> = Vec::new();
    let mut foreign: Vec<Foreign> = Vec::new();
    if let Some(list) = &sc.spaces {
        for raw in list {
            // неизвестная форма или пространство — просто не добавляется
            let Ok(r) = SpaceRef::parse(raw) else {
                continue;
            };
            match r {
                SpaceRef::Fixed(_) => match find_space(&db, &r).await {
                    Ok(s) => personal.push(s),
                    Err(ApiError::NotFound) => {}
                    Err(e) => return Err(e),
                },
                SpaceRef::Solution(g) => match get_solution(&db, g.as_str()).await? {
                    Some(s) => personal.push(s),
                    None => foreign.push(Foreign::Guid(g)),
                },
                SpaceRef::Id(id) => match get_space(&db, id).await? {
                    Some(s) => personal.push(s),
                    // чужая база — только пространство решения из реестра
                    None => {
                        if let Some(info) = st.system.solution_by_space(id).await? {
                            foreign.push(Foreign::Info(info));
                        }
                    }
                },
            }
        }
    } else {
        if let Some(g) = request_solution(sc.solution.as_deref(), h)? {
            match get_solution(&db, g.as_str()).await? {
                Some(s) => personal.push(s),
                None => foreign.push(Foreign::Guid(g)),
            }
        }
        for s in list_spaces(&db).await? {
            let take = match s.kind {
                SpaceKind::Knowledge => sc.include_knowledge.unwrap_or(true),
                SpaceKind::Rules => sc.include_rules.unwrap_or(true),
                SpaceKind::Solution => sc.all_solutions,
                SpaceKind::Human => human_ok,
            };
            if take {
                personal.push(s);
            }
        }
        if sc.all_solutions {
            for (info, role) in st.system.accessible_solutions(user.id).await? {
                if matches!(role, SolutionRole::Member(_)) {
                    foreign.push(Foreign::Info(info));
                }
            }
        }
    }
    personal.retain(|s| token_can_read(p, s) && (s.kind != SpaceKind::Human || human_ok));
    personal.sort_by_key(|s| s.id);
    personal.dedup_by_key(|s| s.id);

    let mut targets: Vec<SearchTarget> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    if !personal.is_empty() {
        targets.push(SearchTarget {
            owner: user.id,
            db,
            spaces: personal,
            role: if is_admin(p) {
                Role::Admin
            } else {
                Role::Personal
            },
            solution: None,
        });
    }
    for f in foreign {
        let info = match f {
            Foreign::Info(i) => i,
            Foreign::Guid(g) => match st.system.get_solution(&g).await? {
                Some(i) => i,
                None => continue,
            },
        };
        // своё решение не нашлось в личной базе (пространство пропало) или уже добавлено
        if info.owner_id == user.id
            || targets
                .iter()
                .any(|t| t.solution.as_ref().is_some_and(|s| s.guid == info.guid))
        {
            continue;
        }
        let role = if is_admin(p) {
            Role::Admin
        } else {
            match st.system.solution_role(&info.guid, user.id).await? {
                Some(SolutionRole::Member(a)) => Role::Member(a),
                Some(SolutionRole::Pending) => {
                    warnings.push("solution access pending".into());
                    continue;
                }
                Some(SolutionRole::Rejected) => {
                    warnings.push("solution access rejected".into());
                    continue;
                }
                Some(SolutionRole::Revoked) => {
                    warnings.push("solution access revoked".into());
                    continue;
                }
                Some(SolutionRole::Owner) | None => continue,
            }
        };
        match solution_space(st, &info).await {
            Ok((odb, space)) => targets.push(SearchTarget {
                owner: info.owner_id,
                db: odb,
                spaces: vec![space],
                role,
                solution: Some(info),
            }),
            // пространство решения пропало у владельца — решение пропускается
            Err(ApiError::NotFound) => {}
            Err(e) => {
                skip_foreign(&info, &e);
                warnings.push(WARN_SOLUTION_UNAVAILABLE.into());
            }
        }
    }
    dedup_keep_order(&mut warnings);
    Ok((targets, warnings))
}

/// Убирает повторы, сохраняя порядок первых вхождений.
pub fn dedup_keep_order(v: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|w| seen.insert(w.clone()));
}
