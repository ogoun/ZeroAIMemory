//! Журнал задач (спецификация M4.5 §3): захват и снятие захвата, длительность в ответах.
//!
//! - `POST /entries/{id}/claim {ttl_min?: 1..=1440 (120), agent?}` → 200 `Entry`: задача
//!   свободна, срок прежнего захвата истёк или захват той же сессии — `data.claimed_by`
//!   ставится или продлевается; иначе 409 `/problems/task-claimed` с `claimed_by`. Сессия —
//!   `X-Zam-Session` (MCP без заголовка подставляет неявную, R13); без неё — 400. Захват —
//!   одна транзакция с проверкой текущего `claimed_by`, ревизию записи не создаёт (`If-Match`
//!   не нужен) и пишется в аудит записи (`entry.claim` в `system.db`);
//! - `POST /entries/{id}/release` → 200 `Entry`: снимает захват своей сессии; чужой действующий
//!   захват снимает только человек (WebUI), агенту — 403. Аудит `entry.release`;
//! - смена статуса и полей задачи — `PATCH /entries/{id}` ([`super::entries::patch`]);
//! - у `done`-задач в ответах (запись, лента) есть `duration_min` — `done_at − started_at`;
//! - у задачи из идеи (запись, лента) — `idea {id, title, status, space_id}` и `ideas` (M6.5 §9);
//! - подзадачи `part_of` (M7 §5.4, задача 5 M7.2; признак `task_subtasks`): у задачи (запись,
//!   лента, эхо) — `parent`, `subtasks`, `last_activity_at`, `stale` ([`attach_task`] по дереву
//!   [`zam_store::task_tree`] — одно построение на запрос), у общей в `GET /entries/{id}` — строки
//!   `subtasks.items` ([`attach_subtask_items`]).
//!
//! Запись задач идёт общим путём `POST /spaces/{space}/entries` (`kind: task`) в пространствах
//! решения и Knowledge; `data.status` при создании — 422 [`STATUS_ON_CREATE`] (статус новой
//! задачи — `open`, его меняет правка; M6.6 §8, решение координатора 19).
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;
use serde_json::{Value, json};
use zam_core::Id;
use zam_store::time::fmt_ts;
use zam_store::{
    CLAIM_TTL_DEFAULT_MIN, CLAIM_TTL_MAX_MIN, Claim, Entry, EntryKind, EntryStatus, IdeaSpaces,
    TaskData, TaskError, TaskIdea, TaskTree, claim_task, get_entry, release_task,
};

use super::entries::{audit_details, unprocessable};
use super::json_body;
use super::nodes::audit_node;
use super::scope::{Mode, Role, deny_report_mutation, entry_scope, live_entry};
use crate::app::AppState;
use crate::attrib::attribution;
use crate::auth::{Client, Principal};
use crate::error::ApiError;

/// Текст 422 на `data.status` новой задачи: как задать статус — правкой (MCP и CLI).
pub const STATUS_ON_CREATE: &str =
    "data.status: статус задаётся правкой — memory_update_entry action=status (zam task status)";

impl From<TaskError> for ApiError {
    fn from(e: TaskError) -> Self {
        match e {
            TaskError::Claimed(c) => ApiError::TaskClaimed(c),
            TaskError::NotClaimedBySession => ApiError::Denied(e.to_string()),
            TaskError::NotTask => unprocessable("запись не является задачей"),
            TaskError::Store(e) => e.into(),
        }
    }
}

/// Идеи каких пространств видны у задачи пространства `space` (поле `idea`, фильтр «из идей»):
/// участнику общего решения — только идеи самого решения (личная Knowledge владельца, из идеи
/// которой задача «начать» могла появиться в решении, ему не видна), владельцу базы и
/// администратору — любые (M6.5 §9).
pub fn idea_spaces(role: Role, space: Id) -> IdeaSpaces {
    match role {
        Role::Member(_) => IdeaSpaces::Only(vec![space]),
        Role::Personal | Role::Owner | Role::Admin => IdeaSpaces::All,
    }
}

/// Порог «давно без обновлений», часов (M7 §5.4 п. 2, решение координатора 22): константа
/// сервера, ключа конфигурации нет.
pub const TASK_STALE_HOURS: i64 = 12;
/// Предел строк `subtasks.items` карточки `GET /entries/{id}`; сверх — `subtasks.truncated`.
pub const SUBTASKS_MAX: usize = 200;

/// Поля задачи в ответах: у `done` — `duration_min` (`done_at − started_at` в минутах, `null` —
/// нет `started_at`); у задачи из идеи (`ideas` — её живые идеи по системным рёбрам
/// `derived_from`, [`zam_store::ideas_of_tasks`]) — `idea {id, title, status, space_id}` первой идеи и
/// `ideas` — все (M6.5 §9: пометка «из идеи» в «Задачах» WebUI). Подзадачи по дереву `tree`
/// пространства задачи (M7 §5.4 п. 2): `parent {id, title, status, subtasks {total, done,
/// dropped}} | null` — родитель и сводка его подзадач; `subtasks` — сводка прямых подзадач (у
/// общей, иначе поля нет); `last_activity_at` — наибольшее `updated_at` задачи и её прямых
/// подзадач; `stale: true` — [`is_stale`] на момент `now` (иначе поля нет). Заголовок в `parent`
/// агенту по токену маскирует [`super::agent_view::ENTRY_FIELDS`].
pub fn attach_task(v: &mut Value, e: &Entry, ideas: &[TaskIdea], tree: &TaskTree, now: Timestamp) {
    if e.kind != EntryKind::Task {
        return;
    }
    let Value::Object(o) = v else {
        return;
    };
    let task = TaskData::from_stored(&e.data);
    if e.status == Some(EntryStatus::Done) {
        o.insert("duration_min".into(), json!(task.duration_min()));
    }
    if let Some(first) = ideas.first() {
        o.insert("idea".into(), json!(first));
        o.insert("ideas".into(), json!(ideas));
    }
    let parent = tree
        .parent
        .get(&e.id)
        .and_then(|p| tree.nodes.get(p).map(|n| (*p, n)));
    let parent = match parent {
        Some((pid, n)) => {
            let s = tree.summary(pid).unwrap_or_default();
            json!({"id": pid, "title": n.title, "status": n.status,
                   "subtasks": {"total": s.total, "done": s.done, "dropped": s.dropped}})
        }
        None => Value::Null,
    };
    o.insert("parent".into(), parent);
    if let Some(s) = tree.summary(e.id) {
        o.insert("subtasks".into(), json!(s));
    }
    let last = tree.last_activity(e.id, e.updated_at);
    o.insert("last_activity_at".into(), json!(fmt_ts(last)));
    if is_stale(e.status, task.claimed_by.as_ref(), last, now) {
        o.insert("stale".into(), json!(true));
    }
}

/// «Давно без обновлений» (M7 §5.4 п. 2): статус `in_progress`, живого захвата нет (срок
/// `claimed_by.until` прошёл или захвата нет), последняя активность `last` старше
/// [`TASK_STALE_HOURS`].
pub fn is_stale(
    status: Option<EntryStatus>,
    claim: Option<&Claim>,
    last: Timestamp,
    now: Timestamp,
) -> bool {
    status == Some(EntryStatus::InProgress)
        && !claim.is_some_and(|c| c.live(now))
        && last < now - SignedDuration::from_hours(TASK_STALE_HOURS)
}

/// Строки подзадач общей задачи — только карточка `GET /entries/{id}` (M7 §5.4 п. 2; в ленте и
/// эхо записи их нет): `subtasks.items [{id, title, status, updated_at, claimed: {agent, until} |
/// null}]` в порядке создания ребра (порядок шагов плана), до [`SUBTASKS_MAX`], и
/// `subtasks.truncated` — строк больше предела. `claimed` — живой на `now` захват. Без
/// `subtasks` ([`attach_task`] не нашёл подзадач) — ничего.
pub fn attach_subtask_items(v: &mut Value, id: Id, tree: &TaskTree, now: Timestamp) {
    let Some(s) = v.get_mut("subtasks").and_then(Value::as_object_mut) else {
        return;
    };
    let kids = tree.children_of(id);
    let items: Vec<Value> = kids
        .iter()
        .take(SUBTASKS_MAX)
        .filter_map(|k| {
            let n = tree.nodes.get(k)?;
            let claimed = match n.claim_until {
                Some(until) if n.claimed(now) => {
                    json!({"agent": n.claim_agent, "until": fmt_ts(until)})
                }
                _ => Value::Null,
            };
            Some(json!({"id": k, "title": n.title, "status": n.status,
                        "updated_at": fmt_ts(n.updated_at), "claimed": claimed}))
        })
        .collect();
    s.insert("items".into(), Value::Array(items));
    s.insert("truncated".into(), json!(kids.len() > SUBTASKS_MAX));
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ClaimReq {
    ttl_min: Option<u32>,
    agent: Option<String>,
}

/// Живая задача по id для записи (иначе 404 / 422).
async fn task_scope(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    id: &str,
) -> Result<super::scope::EntryScope, ApiError> {
    let sc = entry_scope(st, p, headers, id, Mode::Write).await?;
    live_entry(&sc)?;
    // отчёт — 409 (токену 404), а не «не задача»: неизменность раньше вида операции
    deny_report_mutation(p, &sc)?;
    if sc.entry.kind != EntryKind::Task {
        return Err(unprocessable("захват есть только у задач (kind: task)"));
    }
    Ok(sc)
}

/// `POST /entries/{id}/claim`.
pub async fn claim(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let sc = task_scope(&st, &p, &headers, &id).await?;
    let req: ClaimReq = if matches!(&body, Ok(b) if b.is_empty()) {
        ClaimReq::default()
    } else {
        json_body(body)?
    };
    let ttl = req.ttl_min.unwrap_or(CLAIM_TTL_DEFAULT_MIN);
    if !(1..=CLAIM_TTL_MAX_MIN).contains(&ttl) {
        return Err(unprocessable(format!(
            "ttl_min: от 1 до {CLAIM_TTL_MAX_MIN}"
        )));
    }
    let attrib = attribution(&p, &headers, None)?;
    let Some(session) = attrib.session_id.as_deref() else {
        return Err(ApiError::BadRequest(
            "захват требует сессии: заголовок X-Zam-Session (хук session-start создаёт её)".into(),
        ));
    };
    claim_task(
        &sc.db,
        sc.entry.id,
        session,
        req.agent.as_deref(),
        ttl,
        zam_store::time::now(),
    )
    .await?;
    let mut details = audit_details(sc.space.id, EntryKind::Task);
    details["session"] = json!(session);
    details["ttl_min"] = json!(ttl);
    audit_node(
        &st,
        &p,
        &client,
        sc.user.id,
        sc.owner,
        "entry.claim",
        sc.entry.id,
        details,
    )
    .await?;
    let e = get_entry(&sc.db, sc.entry.id).await?;
    Ok(Json(
        super::insights::entry_json(&p, &sc.db, &e, sc.role).await?,
    ))
}

/// `POST /entries/{id}/release`.
pub async fn release(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let sc = task_scope(&st, &p, &headers, &id).await?;
    let attrib = attribution(&p, &headers, None)?;
    let human = attrib.author_kind == zam_store::AuthorKind::Human;
    let session = attrib.session_id.as_deref().unwrap_or("");
    let freed = release_task(&sc.db, sc.entry.id, session, human).await?;
    if let Some(c) = freed {
        let mut details = audit_details(sc.space.id, EntryKind::Task);
        details["session"] = json!(c.session);
        if c.session != session {
            details["by"] = json!(if human { "human" } else { "session" });
        }
        audit_node(
            &st,
            &p,
            &client,
            sc.user.id,
            sc.owner,
            "entry.release",
            sc.entry.id,
            details,
        )
        .await?;
    }
    let e = get_entry(&sc.db, sc.entry.id).await?;
    Ok(Json(
        super::insights::entry_json(&p, &sc.db, &e, sc.role).await?,
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use zam_store::TaskNode;

    use super::*;

    /// Строки подзадач — до [`SUBTASKS_MAX`] в порядке шагов, сверх — `truncated: true`;
    /// `claimed` — только живой захват (агент и срок); без сводки — ничего.
    #[test]
    fn subtask_items_capped_and_claimed() {
        let now: Timestamp = "2026-11-22T12:00:00Z".parse().unwrap();
        let p = Id::new();
        let kids: Vec<Id> = (0..SUBTASKS_MAX + 1).map(|_| Id::new()).collect();
        let node = |claim_until: Option<Timestamp>| TaskNode {
            title: "шаг".into(),
            status: EntryStatus::Open,
            updated_at: now,
            claim_until,
            claim_agent: Some("claude".into()),
        };
        let mut nodes: HashMap<Id, TaskNode> = kids.iter().map(|k| (*k, node(None))).collect();
        nodes.insert(p, node(None));
        nodes.insert(kids[0], node(Some(now + SignedDuration::from_mins(5))));
        nodes.insert(kids[1], node(Some(now - SignedDuration::from_mins(5))));
        let edges: Vec<(Id, Id)> = kids.iter().map(|k| (*k, p)).collect();
        let tree = TaskTree::build(nodes, &edges);
        let mut v = json!({"subtasks": tree.summary(p).unwrap_or_default()});
        attach_subtask_items(&mut v, p, &tree, now);
        let items = v["subtasks"]["items"].as_array().unwrap();
        assert_eq!(items.len(), SUBTASKS_MAX);
        assert_eq!(v["subtasks"]["truncated"], true);
        assert_eq!(v["subtasks"]["total"], SUBTASKS_MAX + 1);
        assert_eq!(items[0]["id"], kids[0].to_string());
        assert_eq!(items[0]["claimed"]["agent"], "claude");
        assert!(items[1]["claimed"].is_null(), "истёкший захват");
        let mut lone = json!({"id": "x"});
        attach_subtask_items(&mut lone, kids[2], &tree, now);
        assert_eq!(lone, json!({"id": "x"}));
    }

    /// «Давно без обновлений»: только `in_progress`, без живого захвата и старше 12 ч.
    #[test]
    fn stale_rule() {
        let now: Timestamp = "2026-11-22T12:00:00Z".parse().unwrap();
        let old = now - SignedDuration::from_hours(TASK_STALE_HOURS) - SignedDuration::from_secs(1);
        let fresh = now - SignedDuration::from_hours(TASK_STALE_HOURS - 1);
        let claim = |until| Claim {
            session: "s".into(),
            agent: None,
            until,
        };
        let ip = Some(EntryStatus::InProgress);
        assert!(is_stale(ip, None, old, now));
        assert!(!is_stale(ip, None, fresh, now));
        assert!(!is_stale(Some(EntryStatus::Blocked), None, old, now));
        let live = claim(now + SignedDuration::from_mins(1));
        assert!(!is_stale(ip, Some(&live), old, now));
        let expired = claim(now - SignedDuration::from_mins(1));
        assert!(is_stale(ip, Some(&expired), old, now));
    }
}
