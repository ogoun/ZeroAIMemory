//! Состояние векторного индекса и пересборка (§8.1, §17, §18 ТЗ).
//!
//! - `GET /index/status` — любой аутентифицированный. Токен видит только своего
//!   пользователя (очередь, неудавшиеся задания, точки); администратор — пользователя из
//!   `X-Zam-User` или, без заголовка, сумму по всем. Модель, устройство и Qdrant (состояние,
//!   версия, RSS процесса — метрика `zam_qdrant_rss_bytes`, перезапуски) и RSS самого
//!   `zam-server` (`server.rss_bytes`, цели §17) общие для сервера.
//! - `POST /admin/reindex {user?, space?, full?}` — только администратор, аудит
//!   `index.reindex`, ответ 202 с `job_count` (заметок области). Без `full` — точки
//!   области удаляются и пишутся заново из кэша `embeddings` без инференса (промахи кэша —
//!   задания); с `full` — перечанковка и задания для всех заметок области. Выполняет
//!   воркер индексации, итог — в `reindex.last` статуса и аудите `index.reindex_done`.

use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, StatusCode};
use jiff::Timestamp;
use serde::Deserialize;
use serde_json::{Value, json};
use zam_core::Id;
use zam_index::supervisor::QdrantState;
use zam_index::{ReindexOutcome, ReindexRequest, VectorFilter};
use zam_ml::Device;
use zam_store::time::fmt_ts;
use zam_store::{FailedJob, JobStats, failed_jobs, indexable_count, job_stats};

use crate::app::AppState;
use crate::auth::{AdminOnly, Client, Principal, USER_HEADER, target_user};
use crate::error::ApiError;

use super::json_body;
use super::scope::{SpaceRef, find_space};

/// Неудавшихся заданий в ответе статуса.
const FAILED_ITEMS: usize = 20;
/// Предел ожидания подсчёта точек в Qdrant для статуса.
const POINTS_TIMEOUT: Duration = Duration::from_secs(2);

/// Пользователи, которых видит запрос статуса: токен — свой; администратор —
/// из `X-Zam-User`, иначе все (`all = true`).
async fn status_users(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
) -> Result<(Vec<Id>, bool), ApiError> {
    match p {
        Principal::Admin { .. } if headers.get(USER_HEADER).is_none() => {
            let all = st.system.list_users().await?;
            Ok((all.into_iter().map(|u| u.id).collect(), true))
        }
        _ => Ok((vec![target_user(st, p, headers).await?.id], false)),
    }
}

fn qdrant_state(s: &Option<QdrantState>) -> (&'static str, Option<String>) {
    match s {
        None => ("off", None),
        Some(QdrantState::Starting) => ("starting", None),
        Some(QdrantState::Ready { version, .. }) => ("ready", Some(version.clone())),
        Some(QdrantState::Restarting { .. }) => ("restarting", None),
        Some(QdrantState::Broken { .. }) => ("broken", None),
        Some(QdrantState::External) => ("external", None),
    }
}

fn failed_json(user: Id, f: &FailedJob) -> Value {
    json!({
        "id": f.id,
        "user_id": user,
        "node_id": f.node_id,
        "space_id": f.space_id,
        "path": f.path,
        "op": f.op.as_str(),
        "error": f.error,
        "attempts": f.attempts,
        "updated_at": fmt_ts(f.updated_at),
    })
}

fn reindex_json(o: &ReindexOutcome) -> Value {
    let req = &o.request;
    let mut v = json!({
        "user": req.user,
        "space": req.space,
        "full": req.full,
        "finished_at": fmt_ts(o.finished_at),
    });
    match &o.result {
        Ok(r) => {
            v["result"] = json!({
                "users": r.users,
                "nodes": r.nodes,
                "points": r.points,
                "folders": r.folders,
                "misses": r.misses,
                "folder_misses": r.folder_misses,
                "jobs": r.jobs,
                "gc_deleted": r.gc_deleted,
                "recreated": r.recreated,
                "failed_users": r.failed_users.iter()
                    .map(|(u, e)| json!({"user_id": u, "error": e}))
                    .collect::<Vec<_>>(),
                "elapsed_ms": r.elapsed_ms,
            });
        }
        Err(e) => v["error"] = json!(e),
    }
    v
}

/// `GET /index/status`.
pub async fn status(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (users, all) = status_users(&st, &p, &headers).await?;
    let rt = &st.index;
    // очередь и неудавшиеся задания видимых пользователей
    let mut queue = JobStats::default();
    let mut failed: Vec<(Id, FailedJob)> = Vec::new();
    // при обходе всех баз неоткрывающаяся (повреждённая) не ломает ответ: она в
    // `unavailable_dbs`; база, указанная явно, — ошибка как обычно
    let mut unavailable: Vec<Value> = Vec::new();
    for u in &users {
        let one = async {
            let db = st.users.get(*u).await?;
            let s = job_stats(&db).await?;
            let f = failed_jobs(&db, FAILED_ITEMS).await?;
            Ok::<_, zam_store::StoreError>((s, f))
        };
        let (s, f) = match one.await {
            Ok(v) => v,
            Err(e) if all => {
                unavailable.push(json!({ "user_id": u, "error": e.to_string() }));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        queue.queued += s.queued;
        queue.running += s.running;
        queue.failed += s.failed;
        queue.oldest = match (queue.oldest, s.oldest) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        failed.extend(f.into_iter().map(|f| (*u, f)));
    }
    failed.sort_by_key(|(_, f)| std::cmp::Reverse(f.updated_at));
    failed.truncate(FAILED_ITEMS);

    let available = rt.vector_available();
    let reason = available.err();
    let wst = rt.worker.status();
    let (qstate, version) = qdrant_state(&rt.qdrant_state());
    let sup = rt.supervisor();
    // точки видимых пользователей; Qdrant недоступен или долго отвечает — null
    let points = match &rt.vector {
        Some(v) if wst.vector_ready => {
            let count = async {
                let mut n = 0u64;
                for u in &users {
                    let f = VectorFilter {
                        user_id: *u,
                        space_ids: Vec::new(),
                        path_prefix: None,
                        levels: Vec::new(),
                    };
                    n += v.count(&f).await.ok()?;
                }
                Some(n)
            };
            tokio::time::timeout(POINTS_TIMEOUT, count)
                .await
                .ok()
                .flatten()
        }
        _ => None,
    };
    let device = match &rt.device {
        Some(d) => json!({
            "kind": match d.device { Device::Cpu => "cpu", Device::Cuda { .. } => "cuda" },
            "reason": d.reason,
        }),
        None => json!({ "kind": null, "reason": rt.ml_error.clone().unwrap_or_default() }),
    };
    let own = |u: Option<Id>| all || u.is_some_and(|u| users.contains(&u));
    let last = wst
        .last_reindex
        .as_ref()
        .filter(|o| own(o.request.user))
        .map(reindex_json);
    let skipped: Vec<Value> = wst
        .rebuild_skipped
        .iter()
        .filter(|d| own(Some(d.user)))
        .map(|d| json!({"user_id": d.user, "error": d.error, "attempts": d.attempts}))
        .collect();
    Ok(Json(json!({
        "vector": {
            "available": reason.is_none(),
            "reason": reason.map(|r| r.as_str()),
            "text": reason.map(|r| r.text()),
            "hint": reason.map(|r| r.hint()),
        },
        "device": device,
        // RSS самого zam-server (рабочий набор на Windows), без Qdrant — цели §17
        "server": {
            "rss_bytes": zam_index::supervisor::rss_bytes(std::process::id()),
        },
        "model": {
            "id": rt.model_id(),
            "dim": rt.embedder.as_ref().map_or(0, |e| e.dim()),
            "collection_model": wst.collection_model,
            "error": rt.ml_error,
        },
        "qdrant": {
            "state": qstate,
            "version": version,
            "rss_bytes": sup.as_ref().and_then(|s| s.rss_bytes()),
            "points": points,
            "restarts": sup.as_ref().map_or(0, |s| s.restarts()),
            "error": rt.qdrant_error,
        },
        "queue": {
            "queued": queue.queued,
            "running": queue.running,
            "failed": queue.failed,
            "oldest": queue.oldest.map(fmt_ts),
        },
        "failed_items": failed.iter().map(|(u, f)| failed_json(*u, f)).collect::<Vec<_>>(),
        // где воркер тратит время с запуска сервера (общие для сервера, как и Qdrant)
        "worker": {
            "jobs_done": rt.worker.jobs_done(),
            "embed_ms": rt.worker.busy().0.as_millis() as u64,
            "qdrant_ms": rt.worker.busy().1.as_millis() as u64,
        },
        "reindex": {
            "running": wst.reindex_running,
            "queued": wst.reindex_queued,
            "last": last,
        },
        "rebuild_skipped": skipped,
        "unavailable_dbs": unavailable,
    })))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ReindexBody {
    user: Option<String>,
    space: Option<String>,
    #[serde(default)]
    full: bool,
}

/// Область reindex (общая для API и CLI).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReindexScope {
    pub user: Option<Id>,
    pub space: Option<Id>,
    /// Индексируемых заметок в открывшихся базах области.
    pub job_count: i64,
    /// Базы, которые не открылись при обходе всех: `(пользователь, ошибка)`. Reindex их
    /// пропустит и отметит исключёнными (reindex — когда база снова откроется).
    pub unavailable: Vec<(Id, String)>,
}

/// Область reindex: пользователь (проверен), пространство (проверено), число заметок.
/// База, указанная явно, должна открыться; при обходе всех неоткрывающиеся пропускаются.
pub async fn reindex_scope(
    st_system: &zam_store::SystemStore,
    st_users: &zam_store::UserDbs,
    user: Option<&str>,
    space: Option<&str>,
) -> Result<ReindexScope, ApiError> {
    let user = user
        .map(|u| {
            u.trim()
                .parse::<Id>()
                .map_err(|_| ApiError::BadRequest("user: некорректный идентификатор".into()))
        })
        .transpose()?;
    if space.is_some() && user.is_none() {
        return Err(ApiError::BadRequest(
            "space задаётся только вместе с user".into(),
        ));
    }
    let users: Vec<Id> = match user {
        Some(u) => {
            st_system.get_user(u).await?.ok_or(ApiError::NotFound)?;
            vec![u]
        }
        None => st_system
            .list_users()
            .await?
            .into_iter()
            .map(|u| u.id)
            .collect(),
    };
    let mut space_id = None;
    let mut job_count = 0i64;
    let mut unavailable = Vec::new();
    for u in &users {
        let db = match st_users.get(*u).await {
            Ok(db) => db,
            Err(e) if user.is_none() => {
                unavailable.push((*u, e.to_string()));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        if let Some(raw) = space {
            space_id = Some(find_space(&db, &SpaceRef::parse(raw)?).await?.id);
        }
        job_count += indexable_count(&db, space_id).await?;
    }
    Ok(ReindexScope {
        user,
        space: space_id,
        job_count,
        unavailable,
    })
}

/// `POST /admin/reindex`.
pub async fn reindex(
    _: AdminOnly,
    State(st): State<AppState>,
    client: Client,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let req: ReindexBody = match body {
        Ok(b) if b.iter().all(u8::is_ascii_whitespace) => ReindexBody::default(),
        other => json_body(other)?,
    };
    let rt = &st.index;
    if !rt.worker.status().running {
        let reason = rt.vector_available().err();
        return Err(ApiError::conflict(format!(
            "векторный индекс не запущен ({}): {}; на остановленном сервере — `zam-server reindex`",
            reason.map_or("broken", |r| r.as_str()),
            reason.map_or("см. журнал", |r| r.hint()),
        )));
    }
    let scope = reindex_scope(
        &st.system,
        &st.users,
        req.user.as_deref(),
        req.space.as_deref(),
    )
    .await?;
    let (user, space, job_count) = (scope.user, scope.space, scope.job_count);
    let unavailable: Vec<Value> = scope
        .unavailable
        .iter()
        .map(|(u, e)| json!({ "user_id": u, "error": e }))
        .collect();
    st.system
        .audit(
            "admin",
            user,
            "index.reindex",
            space.map(|s| s.to_string()).as_deref(),
            Some(&client.ip.to_string()),
            json!({
                "full": req.full, "space": space, "job_count": job_count,
                "unavailable_dbs": unavailable.len(),
            }),
        )
        .await?;
    rt.worker.request_reindex(ReindexRequest {
        user,
        space,
        full: req.full,
    });
    tracing::info!(
        user = ?user.map(|u| u.to_string()),
        full = req.full,
        job_count,
        "reindex поставлен воркеру"
    );
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "job_count": job_count,
            "user": user,
            "space": space,
            "full": req.full,
            "unavailable_dbs": unavailable,
            "accepted_at": fmt_ts(Timestamp::now()),
        })),
    ))
}
