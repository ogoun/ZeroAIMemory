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
//! - у `done`-задач в ответах (запись, лента) есть `duration_min` — `done_at − started_at`.
//!
//! Запись задач идёт общим путём `POST /spaces/{space}/entries` (`kind: task`) в пространствах
//! решения и Knowledge.
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use jiff::Timestamp;
use serde::Deserialize;
use serde_json::{Value, json};
use zam_store::{
    CLAIM_TTL_DEFAULT_MIN, CLAIM_TTL_MAX_MIN, Entry, EntryKind, EntryStatus, TaskData, TaskError,
    claim_task, get_entry, release_task,
};

use super::entries::{audit_details, unprocessable};
use super::json_body;
use super::nodes::audit_node;
use super::scope::{Mode, entry_scope, live_entry};
use crate::app::AppState;
use crate::attrib::attribution;
use crate::auth::{Client, Principal};
use crate::error::ApiError;

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

/// `duration_min` у `done`-задачи: `done_at − started_at` в минутах, `null` — нет `started_at`.
pub fn attach_task(v: &mut Value, e: &Entry) {
    if e.kind == EntryKind::Task
        && e.status == Some(EntryStatus::Done)
        && let Value::Object(o) = v
    {
        o.insert(
            "duration_min".into(),
            json!(TaskData::from_stored(&e.data).duration_min()),
        );
    }
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
        Timestamp::now(),
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
    Ok(Json(super::insights::entry_json(&sc.db, &e).await?))
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
    Ok(Json(super::insights::entry_json(&sc.db, &e).await?))
}
