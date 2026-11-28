//! `GET /spaces` — пространства пользователя запроса (§6.3, §11.6; токену — без Human):
//! личные (свои решения — `role: owner`, `shared`: есть одобренные участники) и
//! пространства чужих решений из баз владельцев (`role: member`, `access`, `owner`);
//! `access_requests_pending` — ожидающие запросы к решениям пользователя.
//! `GET /spaces/{space}/tree` — дерево узлов пространства.
//! `GET /spaces/{space}/counts` — число записей разделов (журналы, открытые, заметки, шаги планов;
//! M7 §5.3, M7.3 §6.14 п. 10).
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use zam_store::{
    AccessState, NodePath, SpaceKind, get_node_by_path, list_children, list_spaces, space_counts,
};

use super::query;
use super::scope::{Mode, Role, readable_areas, space_scope, token_can_read};
use crate::app::AppState;
use crate::auth::{Principal, target_user};
use crate::error::ApiError;

/// Допустимая глубина выдачи дерева.
const DEPTH_RANGE: std::ops::RangeInclusive<u32> = 1..=5;

pub async fn list(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let user = target_user(&st, &p, &headers).await?;
    // Human токену не виден ни в каком виде (его узлы по токену — 404)
    let mut spaces = Vec::new();
    for area in readable_areas(&st, &p, &user).await? {
        let owner = match area.role {
            Role::Member(_) => Some(json!({
                "id": area.owner,
                "name": st.system.get_user(area.owner).await?.map(|u| u.name),
            })),
            _ => None,
        };
        for s in area.spaces {
            let mut v = serde_json::to_value(&s)
                .map_err(|e| ApiError::Internal(format!("сериализация пространства: {e}")))?;
            let Value::Object(o) = &mut v else {
                continue;
            };
            match (area.role, s.kind) {
                (Role::Member(a), _) => {
                    o.insert("role".into(), json!("member"));
                    o.insert("access".into(), json!(a.as_str()));
                    o.insert("owner".into(), owner.clone().unwrap_or(Value::Null));
                }
                (_, SpaceKind::Solution) => {
                    o.insert("role".into(), json!("owner"));
                    o.insert("shared".into(), json!(shared(&st, &s).await?));
                }
                _ => {
                    o.insert("role".into(), json!("personal"));
                }
            }
            spaces.push(v);
        }
    }
    let pending = st.system.pending_requests_for_owner(user.id).await?;
    let access = match &p {
        Principal::Token(t) => t.access.as_str(),
        // сессия пользователя — как его `rw`-токен (§11.4)
        Principal::User { .. } => "rw",
        Principal::Admin { .. } => "admin",
    };
    Ok(Json(json!({
        "user": { "id": user.id, "name": user.name },
        "access": access,
        "spaces": spaces,
        "access_requests_pending": pending,
    })))
}

/// У решения есть одобренные участники (незарегистрированное — нет).
async fn shared(st: &AppState, s: &zam_store::Space) -> Result<bool, ApiError> {
    let Some(guid) = s
        .solution_guid
        .as_deref()
        .and_then(|g| g.parse::<zam_core::SolutionGuid>().ok())
    else {
        return Ok(false);
    };
    match st.system.list_access(&guid).await {
        Ok(l) => Ok(l.iter().any(|e| e.state == AccessState::Approved)),
        Err(zam_store::StoreError::NotFound) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
pub struct TreeQuery {
    #[serde(default)]
    path: String,
    depth: Option<u32>,
    /// `L0` — сводки уже входят в узлы; параметр принимается для совместимости.
    #[allow(dead_code)]
    tier: Option<String>,
}

/// `{space, items}`: узлы под `path` (пусто — корень) до глубины `depth` (1..=5,
/// по умолчанию 1) в прямом порядке обхода; у каждого узла есть `l0`.
pub async fn tree(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(space): Path<String>,
    q: Result<Query<TreeQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let depth = q.depth.unwrap_or(1);
    if !DEPTH_RANGE.contains(&depth) {
        return Err(ApiError::BadRequest("depth: от 1 до 5".into()));
    }
    let sc = space_scope(&st, &p, &headers, &space, Mode::Read).await?;
    let path = q.path.trim_matches('/');
    let parent = if path.is_empty() {
        None
    } else {
        let np = NodePath::parse(path)?;
        Some(get_node_by_path(&sc.db, sc.space.id, &np).await?.id)
    };
    let items = list_children(&sc.db, sc.space.id, parent, depth).await?;
    // агенту по токену — `frontmatter` и L0 узлов с маской секретов (задача 5 M7.1; MCP
    // `memory_tree`)
    let mut items = json!(items);
    super::agent_view::view_json(&p, &mut items, super::agent_view::NODE_FIELDS);
    Ok(Json(json!({ "space": sc.space, "items": items })))
}

/// `GET /spaces/{space}/counts` → `{journals: {<журнал>: n}, open: {questions, tasks}, notes,
/// task_steps}` —
/// число записей разделов для дерева и полосы журналов WebUI (спецификация M7 §5.3 п. 1, признак
/// `space_counts`). Журналы вида пространства, число — живые записи без корзины, все статусы:
/// длина ленты `GET …/entries?kind=` того же зрителя ([`space_counts`]); `code` — файлы карты
/// кода (карта выключена — 0: её маршруты отвечают 404); `vault` — записи Vault зрителя тем же
/// чтением и базой, что `GET …/vault` ([`super::vault::count_of`]: Vault участника — в его базе).
/// Права — чтение пространства, как у ленты: участнику — только пространство решения; токену
/// журнала `reports` нет (отчёт агенту не виден нигде, M5.5 §7). `task_steps` (признак
/// `space_counts_steps`, M7.3 §6.14 п. 10) — шаги планов (подзадачи `part_of`) среди
/// `journals.tasks` по правилу `top_level=1`: подсказка числа «Задач» в WebUI.
pub async fn counts(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(space): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let sc = space_scope(&st, &p, &headers, &space, Mode::Read).await?;
    let c = space_counts(&sc.db, &sc.space, p.is_human()).await?;
    let mut journals = Map::new();
    for (name, n) in c.journals {
        let n = if name == "code" && !st.cfg.code.enabled {
            0
        } else {
            n
        };
        journals.insert(name.into(), json!(n));
    }
    if let Some(n) = super::vault::count_of(&st, &sc.user, &sc.space).await? {
        journals.insert("vault".into(), json!(n));
    }
    Ok(Json(json!({
        "journals": journals,
        "open": { "questions": c.open_questions, "tasks": c.open_tasks },
        "notes": c.notes,
        "task_steps": c.task_steps,
    })))
}

/// Пространства, которые принципал может читать (для сводных списков).
pub(crate) async fn readable_space_ids(
    db: &zam_store::Db,
    p: &Principal,
) -> Result<Vec<zam_core::Id>, ApiError> {
    Ok(list_spaces(db)
        .await?
        .into_iter()
        .filter(|s| token_can_read(p, s))
        .map(|s| s.id)
        .collect())
}
