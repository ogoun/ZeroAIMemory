//! Ночная проверка памяти по REST (M7.3 §6.4 п. 5, §6.5 пп. 1, 5, 7) — база пользователя запроса
//! (токен — его владелец, сессия WebUI — свой пользователь, администратор — `X-Zam-User`).
//! Человеку — исходник, токену — вопросы проб с маской секретов (`‹secret›`).
//!
//! - `GET /check/status` → `{last: {run, trigger, night, status, started_at, finished_at,
//!   verdict, flags, metrics, compared_with} | null, running: {run} | null, trend: [до 30: run,
//!   date, trigger, status, verdict, hit_search, hit_prompt (test), negatives_shown, p95],
//!   failing: [до 20: probe, question, target, rank, advice, advice_text, neighbor?], advice:
//!   [{code, text}], probes: {active, by_kind, by_split}, check: {server, user}}`; вердикт —
//!   `{kind, flags, after_update, after_update_version?, few, episode?, base_n, base_of: 5,
//!   changed?, compared_with?}`.
//! - `GET /check/runs?limit=` (1..=365, 30) → `{items}` — прогоны, новые первыми.
//! - `GET /check/runs/{id}` → прогон и `results` по пробам (вопрос, вид, часть, ранги, время).
//! - `POST /check/run` (человек или токен `rw`; `ro` — 403) → 202 `{run, status: "running"}`:
//!   ручной прогон учтённым запуском раннера; у пользователя уже идёт прогон — 409
//!   `/problems/check-running` (`run`); `[works] enabled = false` — 409 `/problems/works-disabled`
//!   (решение 76; `check = off` и `works.check = off` ручной прогон не выключают); сервер
//!   останавливается — 503.
//! - `POST /check/accept {run}` — «Принять как норму», только сессия WebUI (токен — 403: агент не
//!   прячет регрессию); принять можно только последний законченный ночной прогон открытого
//!   эпизода (иначе 409 `/problems/check-accept-not-latest`); аудит `check.accept {run, verdict}`.
//! - `GET /check/probes?kind=&status=&split=&limit=` (1..=1000, 200) → `{items}` — пробы, новые
//!   первыми.
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use zam_core::Id;
use zam_store::check::{AcceptOutcome, CheckRun, RunStatus};
use zam_store::probes::{Probe, ProbeFilter, ProbeKind, Split};

use super::{json_body, parse_id, query};
use crate::app::AppState;
use crate::auth::{Client, Principal};
use crate::check::{StartError, run as check_run};
use crate::config::CheckMode;
use crate::error::ApiError;

/// Прогонов в `trend` статуса.
pub const TREND_MAX: usize = 30;
/// `GET /check/runs`: по умолчанию и предел.
pub const RUNS_LIMIT_DEFAULT: usize = 30;
pub const RUNS_LIMIT_MAX: usize = 365;
/// `GET /check/probes`: по умолчанию и предел.
pub const PROBES_LIMIT_DEFAULT: usize = 200;
pub const PROBES_LIMIT_MAX: usize = 1000;

/// Тип problem: общий выключатель `[works] enabled = false`.
pub const PROBLEM_WORKS_DISABLED: &str = "/problems/works-disabled";
/// Тип problem: у пользователя уже идёт прогон.
pub const PROBLEM_CHECK_RUNNING: &str = "/problems/check-running";
/// Тип problem: принять как норму можно только последний ночной прогон открытого эпизода.
pub const PROBLEM_ACCEPT_NOT_LATEST: &str = "/problems/check-accept-not-latest";

fn typed(
    status: StatusCode,
    kind: &'static str,
    detail: &str,
    extra: &[(&str, Value)],
) -> ApiError {
    let mut m = Map::new();
    for (k, v) in extra {
        m.insert((*k).to_string(), v.clone());
    }
    ApiError::Typed {
        status,
        kind,
        detail: detail.to_string(),
        extra: m,
    }
}

/// Пользователь запроса (несуществующий — 404).
async fn user_of(st: &AppState, p: &Principal, h: &HeaderMap) -> Result<Id, ApiError> {
    Ok(crate::auth::target_user(st, p, h).await?.id)
}

/// База пользователя запроса с помеченными прерванными прогонами
/// ([`crate::check::ensure_marked`]).
async fn db_of(st: &AppState, user: Id) -> Result<zam_store::Db, ApiError> {
    let db = st.users.get(user).await?;
    crate::check::ensure_marked(st, user, &db).await?;
    Ok(db)
}

/// Текст для зрителя `p` (агенту — с маской).
fn view(p: &Principal, s: &str) -> String {
    crate::api::agent_view::view(p, s).into_owned()
}

/// Провалы прогона для зрителя: вопрос с маской у токена, текст совета.
fn failing_view(p: &Principal, metrics: Option<&Value>) -> Vec<Value> {
    metrics
        .and_then(|m| m["failing"].as_array())
        .into_iter()
        .flatten()
        .map(|f| {
            let mut f = f.clone();
            if let Some(q) = f["question"].as_str() {
                f["question"] = json!(view(p, q));
            }
            if let Some(code) = f["advice"].as_str() {
                f["advice_text"] = json!(check_run::advice_text(code, None));
            }
            f
        })
        .collect()
}

/// JSON прогона; `full` — с провалами в метриках (у списка их нет).
fn run_json(p: &Principal, r: &CheckRun, full: bool) -> Value {
    let mut metrics = r.metrics.clone();
    if let Some(m) = metrics.as_mut().and_then(Value::as_object_mut) {
        if full {
            m.insert("failing".into(), json!(failing_view(p, r.metrics.as_ref())));
        } else {
            m.remove("failing");
        }
    }
    let ts = zam_store::time::fmt_ts;
    json!({
        "run": r.id.to_string(),
        "trigger": r.trigger.as_str(),
        "night": r.night.map(|d| d.to_string()),
        "status": r.status.as_str(),
        "started_at": ts(r.started_at),
        "finished_at": r.finished_at.map(ts),
        "fingerprint": r.fingerprint,
        "fp_hash": r.fp_hash,
        "context": r.context,
        "metrics": metrics,
        "verdict": r.verdict,
        "flags": r.flags,
        "episode": r.episode.map(|e| e.to_string()),
        "accepted_at": r.accepted_at.map(ts),
        "accepted_by": r.accepted_by,
        "error": r.error,
    })
}

/// Строка тренда: дата (ночь или день начала, UTC), вердикт, `hit_*` части `test`, посторонние и
/// `p95`.
fn trend_json(r: &CheckRun) -> Value {
    let m = r.metrics.as_ref();
    let date = r.night.map(|d| d.to_string()).unwrap_or_else(|| {
        r.started_at
            .to_zoned(jiff::tz::TimeZone::UTC)
            .date()
            .to_string()
    });
    json!({
        "run": r.id.to_string(),
        "date": date,
        "trigger": r.trigger.as_str(),
        "status": r.status.as_str(),
        "verdict": r.verdict_kind(),
        "hit_search": m.and_then(|m| m["test"]["hit_search"].as_f64()),
        "hit_prompt": m.and_then(|m| m["test"]["hit_prompt"].as_f64()),
        "negatives_shown": m.and_then(|m| m["negatives_shown"].as_f64()),
        "p95": m.and_then(|m| m["search_p95_ms"].as_f64()),
    })
}

fn counts_json(v: &[(String, u32)]) -> Value {
    Value::Object(v.iter().map(|(k, n)| (k.clone(), json!(n))).collect())
}

/// `GET /check/status`.
pub async fn status(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let user = user_of(&st, &p, &headers).await?;
    let db = db_of(&st, user).await?;
    let runs = zam_store::check::runs(&db, TREND_MAX + 1).await?;
    let running = crate::check::running(user).or_else(|| {
        runs.iter()
            .find(|r| r.status == RunStatus::Running)
            .map(|r| r.id)
    });
    let last = runs.iter().find(|r| r.status != RunStatus::Running);
    let counts = zam_store::probes::probe_counts(&db).await?;
    let works = crate::works::settings::load(&st.system, user).await?;
    let server_check = st.cfg.works.enabled && st.cfg.works.check == CheckMode::Nightly;
    let last_json = last.map(|r| {
        let mut o = run_json(&p, r, false);
        o["compared_with"] = r
            .verdict
            .as_ref()
            .map_or(Value::Null, |v| v["compared_with"].clone());
        o
    });
    let version = last
        .and_then(|r| r.verdict.as_ref())
        .and_then(|v| v["after_update_version"].as_str());
    let advice: Vec<Value> = last
        .map(|r| check_run::run_advice(r.verdict.as_ref(), r.metrics.as_ref()))
        .unwrap_or_default()
        .into_iter()
        .map(|mut a| {
            if a["code"] == "after_update" {
                a["text"] = json!(check_run::advice_text("after_update", version));
            }
            a
        })
        .collect();
    Ok(Json(json!({
        "last": last_json,
        "running": running.map(|r| json!({"run": r.to_string()})),
        "trend": runs
            .iter()
            .filter(|r| r.status != RunStatus::Running)
            .take(TREND_MAX)
            .map(trend_json)
            .collect::<Vec<_>>(),
        "failing": failing_view(&p, last.and_then(|r| r.metrics.as_ref())),
        "advice": advice,
        "probes": {
            "active": counts.active,
            "by_kind": counts_json(&counts.by_kind),
            "by_split": counts_json(&counts.by_split),
        },
        "check": {
            "server": if server_check { "nightly" } else { "off" },
            "user": if works.check { "on" } else { "off" },
        },
    })))
}

#[derive(Deserialize)]
pub struct RunsQuery {
    limit: Option<usize>,
}

/// `GET /check/runs?limit=`.
pub async fn runs(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<RunsQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let limit = q.limit.unwrap_or(RUNS_LIMIT_DEFAULT);
    if !(1..=RUNS_LIMIT_MAX).contains(&limit) {
        return Err(ApiError::BadRequest(format!(
            "limit: от 1 до {RUNS_LIMIT_MAX}"
        )));
    }
    let user = user_of(&st, &p, &headers).await?;
    let db = db_of(&st, user).await?;
    let runs = zam_store::check::runs(&db, limit).await?;
    Ok(Json(json!({
        "items": runs.iter().map(|r| run_json(&p, r, false)).collect::<Vec<_>>(),
    })))
}

/// `GET /check/runs/{id}`.
pub async fn run(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_id(&id)?;
    let user = user_of(&st, &p, &headers).await?;
    let db = db_of(&st, user).await?;
    let r = zam_store::check::get_run(&db, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let probes: std::collections::HashMap<Id, Probe> = zam_store::probes::list_probes(
        &db,
        &ProbeFilter {
            limit: usize::MAX >> 1,
            ..ProbeFilter::default()
        },
    )
    .await?
    .into_iter()
    .map(|p| (p.id, p))
    .collect();
    let results: Vec<Value> = zam_store::check::results(&db, id)
        .await?
        .into_iter()
        .map(|x| {
            let pr = probes.get(&x.probe_id);
            json!({
                "probe": format!("x:{}", x.probe_id),
                "space": x.space.map(|s| s.to_string()),
                "question": pr.map(|pr| view(&p, &pr.question)),
                "kind": pr.map(|pr| pr.kind.as_str()),
                "split": pr.map(|pr| pr.split.as_str()),
                "targets": pr.map(|pr| json!(pr.targets)),
                "search_rank": x.search_rank,
                "search_cut": x.search_cut,
                "deep_rank": x.deep_rank,
                "prompt_rank": x.prompt_rank,
                "search_ms": x.search_ms,
                "prompt_ms": x.prompt_ms,
            })
        })
        .collect();
    let mut o = run_json(&p, &r, true);
    o["results"] = json!(results);
    Ok(Json(o))
}

/// `POST /check/run` → 202 `{run, status}`.
pub async fn start(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    crate::api::scope::require_rw(&p)?;
    let user = user_of(&st, &p, &headers).await?;
    match crate::check::start_manual(&st, user) {
        Ok(id) => Ok((
            StatusCode::ACCEPTED,
            Json(json!({"run": id.to_string(), "status": "running"})),
        )
            .into_response()),
        Err(StartError::Disabled) => Err(typed(
            StatusCode::CONFLICT,
            PROBLEM_WORKS_DISABLED,
            "внутренние работы и проверка памяти выключены на сервере ([works] enabled = false)",
            &[],
        )),
        Err(StartError::Running(r)) => Err(typed(
            StatusCode::CONFLICT,
            PROBLEM_CHECK_RUNNING,
            "прогон проверки памяти уже идёт — дождитесь его конца",
            &[("run", json!(r.to_string()))],
        )),
        Err(StartError::Stopping) => Err(ApiError::ShuttingDown { retry_after_s: 5 }),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptReq {
    run: String,
}

/// `POST /check/accept {run}` — только сессия WebUI.
pub async fn accept(
    State(st): State<AppState>,
    client: Client,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    if !p.is_human() {
        return Err(ApiError::Denied(
            "норму проверки принимает только человек (WebUI «Проверка памяти»)".into(),
        ));
    }
    let req: AcceptReq = json_body(body)?;
    let run = parse_id(&req.run)?;
    let user = user_of(&st, &p, &headers).await?;
    let db = db_of(&st, user).await?;
    let by = p.actor();
    let verdict = match zam_store::check::accept_run(&db, run, &by).await? {
        AcceptOutcome::Accepted(v) => v,
        AcceptOutcome::NotLatest => {
            return Err(typed(
                StatusCode::CONFLICT,
                PROBLEM_ACCEPT_NOT_LATEST,
                "принять как норму можно только последний ночной прогон идущего эпизода регрессии",
                &[],
            ));
        }
    };
    st.system
        .audit(
            &by,
            Some(user),
            "check.accept",
            Some(&run.to_string()),
            Some(&client.ip.to_string()),
            json!({"run": run.to_string(), "verdict": verdict["kind"]}),
        )
        .await?;
    let r = zam_store::check::get_run(&db, run)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(run_json(&p, &r, false)))
}

#[derive(Deserialize)]
pub struct ProbesQuery {
    kind: Option<String>,
    status: Option<String>,
    split: Option<String>,
    limit: Option<usize>,
}

fn probe_json(p: &Principal, pr: &Probe) -> Value {
    let ts = zam_store::time::fmt_ts;
    json!({
        "id": pr.reference(),
        "space": pr.space.map(|s| s.to_string()),
        "question": view(p, &pr.question),
        "kind": pr.kind.as_str(),
        "targets": pr.targets,
        "split": pr.split.as_str(),
        "overlap": pr.overlap,
        "author_kind": pr.author_kind.as_str(),
        "author": pr.author,
        "work": pr.work_id.map(|w| format!("j:{w}")),
        "status": if pr.active { "active" } else { "retired" },
        "retired_reason": pr.retired_reason,
        "created_at": ts(pr.created_at),
        "retired_at": pr.retired_at.map(ts),
    })
}

/// `GET /check/probes?kind=&status=&split=&limit=`.
pub async fn probes(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<ProbesQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let kind = q
        .kind
        .as_deref()
        .map(|k| {
            ProbeKind::parse(k).ok_or_else(|| {
                ApiError::BadRequest("kind: probe | cache | negative | manual".into())
            })
        })
        .transpose()?;
    let active = match q.status.as_deref() {
        None => None,
        Some("active") => Some(true),
        Some("retired") => Some(false),
        Some(_) => return Err(ApiError::BadRequest("status: active | retired".into())),
    };
    let split = q
        .split
        .as_deref()
        .map(|s| Split::parse(s).ok_or_else(|| ApiError::BadRequest("split: dev | test".into())))
        .transpose()?;
    let limit = q.limit.unwrap_or(PROBES_LIMIT_DEFAULT);
    if !(1..=PROBES_LIMIT_MAX).contains(&limit) {
        return Err(ApiError::BadRequest(format!(
            "limit: от 1 до {PROBES_LIMIT_MAX}"
        )));
    }
    let user = user_of(&st, &p, &headers).await?;
    let db = db_of(&st, user).await?;
    let items = zam_store::probes::list_probes(
        &db,
        &ProbeFilter {
            kind,
            active,
            split,
            limit,
        },
    )
    .await?;
    Ok(Json(json!({
        "items": items.iter().map(|pr| probe_json(&p, pr)).collect::<Vec<_>>(),
    })))
}
