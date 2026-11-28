//! Ночная проверка памяти по REST (M7.3 §6.4 п. 5, §6.5 пп. 1, 5, 7) — база пользователя запроса
//! (токен — его владелец, сессия WebUI — свой пользователь, администратор — `X-Zam-User`).
//! Человеку — исходник, токену — вопросы проб с маской секретов (`‹secret›`).
//!
//! - `GET /check/status` → `{last: {run, trigger, night, status, started_at, finished_at,
//!   verdict, flags, metrics, compared_with} | null, running: {run} | null, trend: [до 30: run,
//!   date, trigger, status, verdict, hit_search, hit_prompt (test), negatives_shown, p95],
//!   failing: [до 20: probe, question, target, rank, advice, advice_text, neighbor?], advice:
//!   [{code, text}], probes: {active, by_kind, by_split}, check: {server, user, reason}}`
//!   ([`check_json`]: `reason` — `disabled` | `check_off` | `null`); вердикт —
//!   `{kind, flags, after_update, after_update_version?, few, episode?, base_n, base_of: 5,
//!   changed?, compared_with?}`.
//! - `GET /check/runs?limit=&trigger=&status=` (1..=365, 30) → `{items}` — прогоны, новые первыми;
//!   `trigger` — `nightly | manual`, `status` — `running | done | skipped | timeout | failed`
//!   (последний законченный ночной прогон — `?trigger=nightly&status=done&limit=1`: по нему WebUI
//!   решает, открыта ли регрессия, как строка дайджеста; задача 8, ревью r1).
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
//! - `POST /check/probes {question, targets: ["<буква>:<id>"], space?}` (M7.4 §7.6 п. 3, признак
//!   `check_probes_manage`; человек или токен `rw`, `ro` — 403) → 201 `{id: "x:<id>"}` — ручная
//!   проба вида `manual`: автор по принципалу (токен — `agent`, сессия WebUI — `human`), часть —
//!   общим правилом по первой цели (`zam_search::eval::probe_split`), ревизии целей — текущие.
//!   Цель — живая заметка `n:` или запись журнала своей буквы
//!   ([`zam_store::probes::live_target`]); не найдена, не действует или не из пространства
//!   пробы — 422; пространство — `space` (`knowledge`, `rules`, GUID решения, id) или цели,
//!   Human — 422 (агент его не видит — проба ничего не меряет). Повтор вопроса — 409
//!   `/problems/probe-duplicate`, активных проб с целью — `PROBES_MAX` — 409
//!   `/problems/probes-limit`; ссылка не `<буква>:<id>`, без целей или больше
//!   [`MANUAL_TARGETS_MAX`], пустой или длиннее [`MANUAL_QUESTION_MAX`] вопрос — 400.
//! - `DELETE /check/probes/{id} {reason}` — снять пробу: **только человек** (сессия WebUI, как
//!   `accept`; агентский токен — 403: агент не прячет регрессию, решение 109); `reason` 3..=500
//!   символов (иначе 422 `/problems/reason-required`) — причина снятия в пробе и аудит
//!   `check.probe.retire {probe, reason}`; снятая, чужая и несуществующая — 404; посторонний
//!   вопрос набора сервера — 409 `code: negative_probe` (набор вернул бы его следующим прогоном).
use std::ops::RangeInclusive;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use zam_core::Id;
use zam_store::SpaceKind;
use zam_store::check::{AcceptOutcome, CheckRun, RunStatus, Trigger};
use zam_store::probes::{
    InsertOutcome, NewProbe, PROBES_MAX, Probe, ProbeAuthor, ProbeFilter, ProbeKind, Split,
};

use super::{json_body, parse_id, query};
use crate::app::AppState;
use crate::auth::{Client, Principal};
use crate::check::{StartError, run as check_run};
use crate::config::CheckMode;
use crate::error::ApiError;
use crate::works::settings::UserWorks;

/// Прогонов в `trend` статуса.
pub const TREND_MAX: usize = 30;
/// `GET /check/runs`: по умолчанию и предел.
pub const RUNS_LIMIT_DEFAULT: usize = 30;
pub const RUNS_LIMIT_MAX: usize = 365;
/// `GET /check/probes`: по умолчанию и предел.
pub const PROBES_LIMIT_DEFAULT: usize = 200;
pub const PROBES_LIMIT_MAX: usize = 1000;
/// Ручная проба: вопрос, символов (как запрос `memory_search` — до 512).
pub const MANUAL_QUESTION_MAX: usize = 512;
/// Ручная проба: целей (как у пробы из кэша, `zam_store::probes::CACHE_PROBE_TARGETS`).
pub const MANUAL_TARGETS_MAX: usize = zam_store::probes::CACHE_PROBE_TARGETS;
/// Причина снятия пробы, символов.
pub const RETIRE_REASON_CHARS: RangeInclusive<usize> = 3..=500;

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
    let last_json = last.map(|r| {
        let mut o = run_json(&p, r, false);
        o["compared_with"] = r
            .verdict
            .as_ref()
            .map_or(Value::Null, |v| v["compared_with"].clone());
        o
    });
    // советы с текстом (у `after_update` — с версией эпизода, `run_advice`)
    let advice: Vec<Value> = last
        .map(|r| check_run::run_advice(r.verdict.as_ref(), r.metrics.as_ref()))
        .unwrap_or_default();
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
        "check": check_json(&st.works(), &works),
    })))
}

/// Ночная проверка сервера и пользователя: `{server: nightly | off, user: on | off, reason}`;
/// `reason` — почему сервер её не проводит: `disabled` (`[works] enabled = false`), `check_off`
/// (`[works] check = "off"`), иначе `null`. Одна форма у `GET /check/status` и
/// `GET /works/usage` (выключатель «Ночная проверка моей памяти»; финальное ревью M7.3,
/// Minor 12).
pub(crate) fn check_json(cfg: &crate::config::WorksSection, works: &UserWorks) -> Value {
    let reason = if !cfg.enabled {
        Some("disabled")
    } else if cfg.check != CheckMode::Nightly {
        Some("check_off")
    } else {
        None
    };
    json!({
        "server": if reason.is_none() { "nightly" } else { "off" },
        "user": if works.check { "on" } else { "off" },
        "reason": reason,
    })
}

/// Последний законченный прогон пользователя `{status, verdict}` — тот же `last`, что у
/// `GET /check/status` (прерванные помечены): строка сводки администратора `GET /admin/works`
/// (M7.4 §7.6 п. 2). Прогонов нет — оба `null`.
pub(crate) async fn last_brief(
    st: &AppState,
    user: Id,
    db: &zam_store::Db,
) -> Result<Value, ApiError> {
    crate::check::ensure_marked(st, user, db).await?;
    // идущий прогон у пользователя — не больше одного: последний законченный — среди трёх
    let runs = zam_store::check::runs(db, 3).await?;
    let last = runs.iter().find(|r| r.status != RunStatus::Running);
    Ok(json!({
        "status": last.map(|r| r.status.as_str()),
        "verdict": last.and_then(CheckRun::verdict_kind),
    }))
}

#[derive(Deserialize)]
pub struct RunsQuery {
    limit: Option<usize>,
    trigger: Option<String>,
    status: Option<String>,
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
    let trigger = match q
        .trigger
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => None,
        Some(s) => Some(
            Trigger::parse(s)
                .ok_or_else(|| ApiError::BadRequest("trigger: nightly | manual".into()))?,
        ),
    };
    let status = match q.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => Some(RunStatus::parse(s).ok_or_else(|| {
            ApiError::BadRequest("status: running | done | skipped | timeout | failed".into())
        })?),
    };
    let user = user_of(&st, &p, &headers).await?;
    let db = db_of(&st, user).await?;
    let runs = zam_store::check::runs_of(&db, trigger, status, limit).await?;
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
    let rows = zam_store::check::results(&db, id).await?;
    // пробы этого прогона по id (снятые — тоже), а не весь набор базы
    let mut ids: Vec<Id> = rows.iter().map(|x| x.probe_id).collect();
    ids.sort_unstable();
    ids.dedup();
    let probes: std::collections::HashMap<Id, Probe> = zam_store::probes::probes_by_ids(&db, ids)
        .await?
        .into_iter()
        .map(|p| (p.id, p))
        .collect();
    let results: Vec<Value> = rows
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
        // общий выключатель — тот же ответ, что у ручных запусков работ (решение координатора 76)
        Err(StartError::Disabled) => Err(ApiError::WorksDisabled(
            crate::api::works::WORKS_DISABLED_DETAIL.into(),
        )),
        Err(StartError::Running(run)) => Err(ApiError::CheckRunning { run }),
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
        AcceptOutcome::NotLatest => return Err(ApiError::CheckAcceptNotLatest),
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeReq {
    question: String,
    targets: Vec<String>,
    /// `knowledge`, `rules`, GUID решения или id пространства; без поля — пространство целей.
    space: Option<String>,
}

/// Ссылка формы `<буква>:<id>` (буква — латинская строчная, id — разбирается).
fn ref_form(s: &str) -> bool {
    s.split_once(':').is_some_and(|(l, id)| {
        !l.is_empty() && l.bytes().all(|b| b.is_ascii_lowercase()) && id.parse::<Id>().is_ok()
    })
}

/// Пространства, которые меряет проба: Knowledge, Rules, решения личной базы (Human агент не
/// видит — проба ничего бы не мерила).
const PROBE_SPACES: [SpaceKind; 3] = [SpaceKind::Knowledge, SpaceKind::Rules, SpaceKind::Solution];

/// Пространство пробы по `space` (`knowledge`, `rules`, GUID решения или id) — из пространств
/// [`PROBE_SPACES`] базы; иначе 400.
async fn probe_space(db: &zam_store::Db, raw: &str) -> Result<Id, ApiError> {
    let s = raw.trim();
    zam_store::list_spaces(db)
        .await?
        .iter()
        .filter(|x| PROBE_SPACES.contains(&x.kind))
        .find(|x| {
            x.kind.as_str() == s
                || x.id.to_string() == s
                || x.solution_guid
                    .as_deref()
                    .is_some_and(|g| g.eq_ignore_ascii_case(s))
        })
        .map(|x| x.id)
        .ok_or_else(|| {
            ApiError::BadRequest(
                "space: knowledge, rules, GUID решения или id пространства вашей базы".into(),
            )
        })
}

/// `POST /check/probes {question, targets, space?}` → 201 `{id}` (см. модуль).
pub async fn create_probe(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    crate::api::scope::require_rw(&p)?;
    let req: ProbeReq = json_body(body)?;
    let question = req.question.trim();
    if question.is_empty() || question.chars().count() > MANUAL_QUESTION_MAX {
        return Err(ApiError::BadRequest(format!(
            "question: вопрос, от 1 до {MANUAL_QUESTION_MAX} символов"
        )));
    }
    let mut refs: Vec<&str> = Vec::new();
    for t in req.targets.iter().map(|t| t.trim()) {
        if !ref_form(t) {
            return Err(ApiError::BadRequest(format!(
                "targets: «{t}» — не ссылка <буква>:<id> (n:<id> заметки, i:<id> insight …)"
            )));
        }
        if !refs.contains(&t) {
            refs.push(t);
        }
    }
    if refs.is_empty() || refs.len() > MANUAL_TARGETS_MAX {
        return Err(ApiError::BadRequest(format!(
            "targets: от 1 до {MANUAL_TARGETS_MAX} ссылок на записи, которые должен находить вопрос"
        )));
    }
    let user = user_of(&st, &p, &headers).await?;
    let db = st.users.get(user).await?;
    let mut space = match req.space.as_deref() {
        Some(s) => Some(probe_space(&db, s).await?),
        None => None,
    };
    let mut targets = Vec::with_capacity(refs.len());
    for r in &refs {
        let Some((t, of)) = zam_store::probes::live_target(&db, r).await? else {
            return Err(ApiError::Unprocessable(format!(
                "цель {r} не найдена или не действует (удалена, замещена, отозвана; буква — \
                 не того вида)"
            )));
        };
        match space {
            None => space = Some(of),
            Some(s) if s != of => {
                return Err(ApiError::Unprocessable(format!(
                    "цель {r} — из другого пространства, чем проба: цели пробы — записи одного \
                     пространства"
                )));
            }
            Some(_) => {}
        }
        targets.push(t);
    }
    let space = space.ok_or_else(|| ApiError::Internal("проба без пространства".into()))?;
    let kind = zam_store::list_spaces(&db)
        .await?
        .into_iter()
        .find(|x| x.id == space)
        .map(|x| x.kind);
    if !kind.is_some_and(|k| PROBE_SPACES.contains(&k)) {
        return Err(ApiError::Unprocessable(
            "цели в Human: агент Human не видит — проба ничего не мерила бы".into(),
        ));
    }
    let probe = NewProbe {
        space: Some(space),
        question: question.to_string(),
        kind: ProbeKind::Manual,
        split: zam_search::eval::probe_split(&targets[0].target),
        targets,
        overlap: None,
        author_kind: if p.is_human() {
            ProbeAuthor::Human
        } else {
            ProbeAuthor::Agent
        },
        author: Some(p.actor()),
        work_id: None,
    };
    match zam_store::probes::insert_probe(&db, probe).await? {
        InsertOutcome::Inserted(id) => {
            Ok((StatusCode::CREATED, Json(json!({"id": format!("x:{id}")}))))
        }
        InsertOutcome::Duplicate => Err(ApiError::ProbeDuplicate(
            "такой вопрос уже есть у активной пробы пространства (без учёта регистра и пробелов)"
                .into(),
        )),
        InsertOutcome::Full => Err(ApiError::ProbesLimit(format!(
            "активных проб с целью — {PROBES_MAX}: новые не принимаются; лишние снимает человек \
             на странице «Проверка памяти»"
        ))),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetireReq {
    reason: Option<String>,
}

/// `DELETE /check/probes/{id} {reason}` → 204 — только человек (см. модуль).
pub async fn retire_probe(
    State(st): State<AppState>,
    client: Client,
    p: Principal,
    headers: HeaderMap,
    Path(raw): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<StatusCode, ApiError> {
    if !p.is_human() {
        return Err(ApiError::Denied(
            "пробу снимает только человек (WebUI «Проверка памяти»): агент не прячет регрессию — \
             опишите ошибочную пробу человеку"
                .into(),
        ));
    }
    let raw = raw.trim();
    let id = parse_id(raw.strip_prefix("x:").unwrap_or(raw))?;
    let req: RetireReq = json_body(body)?;
    let reason = req.reason.as_deref().map_or("", str::trim);
    if !RETIRE_REASON_CHARS.contains(&reason.chars().count()) {
        return Err(ApiError::ReasonRequired(format!(
            "reason: причина снятия, от {} до {} символов",
            RETIRE_REASON_CHARS.start(),
            RETIRE_REASON_CHARS.end()
        )));
    }
    let user = user_of(&st, &p, &headers).await?;
    let db = st.users.get(user).await?;
    let pr = zam_store::probes::probes_by_ids(&db, vec![id])
        .await?
        .into_iter()
        .find(|x| x.active)
        .ok_or(ApiError::NotFound)?;
    if pr.kind == ProbeKind::Negative {
        return Err(ApiError::conflict_code(
            "negative_probe",
            "посторонний вопрос — из набора сервера: снять его нельзя (набор вернёт его \
             следующим прогоном)",
            &[],
        ));
    }
    if !zam_store::probes::retire_probe(&db, id, reason).await? {
        return Err(ApiError::NotFound);
    }
    let probe = pr.reference();
    st.system
        .audit(
            &p.actor(),
            Some(user),
            "check.probe.retire",
            Some(&probe),
            Some(&client.ip.to_string()),
            json!({"probe": probe, "reason": reason}),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
