//! Очередь внутренних работ (M7.3 §6.3 пп. 3, 4, 8): REST очереди, ручной запуск, итог и возврат
//! работы, пакет `/recall {event: "work"}`.
//!
//! - `GET /works?state=&kind=&limit=&user=` → `{works: […], modes: {<вид>: {mode, reason}},
//!   paused?}` — очередь и последние работы с итогами, новые первыми (`limit` 1..=200, по
//!   умолчанию 50). Человеку — исходник, пауза исполнителя; токену — работы его пользователя с
//!   маской `‹secret›`, без паузы (снять её агент не может, §6.2 п. 5). Сначала — сроки работ
//!   агента (истёкшие аренды, 7 суток без итога). `?user=` — чужой пользователь только
//!   администратору (как `GET /works/calls`); без него — пользователь запроса (`X-Zam-User`
//!   администратора).
//! - `GET /works/usage?user=` → `{day: {tokens, limit}, month: {tokens, limit}, ceiling,
//!   last_night, night, modes, check: {server, user, reason}, paused?}` — расход токенов модели
//!   суток и месяца UTC по журналу отправок, пределы ([`crate::works::exec::token_limits`] — те
//!   же, по которым сервер отказывает), ночная проверка сервера и пользователя (как у
//!   `GET /check/status`).
//! - `GET /admin/works` (M7.4 §7.6 п. 2, признак `admin_works`; только администратор) →
//!   `{users: [{user, login, modes, paused?, queued, running, failed_7d, last_night, tokens:
//!   {day, month}, check: {status, verdict}}]}` — сводка по всем пользователям тем же кодом, что
//!   `GET /works/usage` и `GET /check/status` ([`admin_summary`]). С M7.6 (§9.9, признак
//!   `admin_works_settings`) в строке — личный предел `limits` (`works.limits` или `null`) и
//!   действующий `limit: {daily, monthly}`, в ответе — потолки `ceiling: {daily, monthly}` и
//!   действующие настройки сервера `settings` (как `GET /admin/works/settings`,
//!   [`super::works_admin`]).
//! - `POST /works/run {kind, space?}` — ручной запуск вида (§6.3 п. 2), **только человек**
//!   (токену — 403); `[works] enabled = false` — 409 `/problems/works-disabled` (решение 76);
//!   действующий режим вида `off` — 409 `code: works_off` с причиной; предел ручных запусков
//!   ([`crate::works::queue::PACKAGES_PER_RUN`] пакетов вида за сутки UTC) исчерпан — 409
//!   `code: manual_limit`. Режим `agent` — 200 `{mode, run: null, queued: [j:…]}` (работы ждут
//!   агента); `model` — 202 `{mode, run: "manual:<id>", queued: []}`: выполнение сразу, вне окна,
//!   учтённым запуском ночного исполнителя (`NightRunner::spawn_tracked`; сервер
//!   останавливается — 503).
//! - `POST /works/{id}/result {items?, skip?, session_id?}` (`id` — `j:<id>` или id; нужно
//!   `items` или `skip`, прочие поля — 400) — итог работы (§6.3 п. 4): только от сессии,
//!   захватившей её (`running`; `X-Zam-Session` или `session_id`), иначе 409
//!   `/problems/work-not-claimed`; токен `ro` — 403; без сессии — 400
//!   `/problems/session-required`. Ответ `{work, state, accepted: [n], rejected: [{n, target,
//!   reason}], skipped: [n], skip: [{n, target, why}], skip_reason}`. Итоги одной работы
//!   принимаются по одному (замок работы; ждущий — не дольше [`FINISH_WAIT`], затем 409
//!   `code: result_in_progress`); под замком работа перечитывается. Повтор с тем же
//!   `Idempotency-Key` и тем же телом — прежний ответ без повторной проверки и записи (и пока
//!   первый ещё идёт — после него); тот же ключ с другим телом — 422
//!   `/problems/idempotency-mismatch`; второй итог с другим ключом — 409. Записи принятого и
//!   закрытие работы — одна транзакция (`zam_store::works::finish_claimed`): снятая, истёкшая
//!   или закрытая работа ничего не записывает.
//! - `POST /works/{id}/release` — вернуть работу агента `running` в очередь без итога: токен —
//!   только захватившая сессия, человек — любую работу агента своего пользователя; работу модели
//!   выполняет сервер — 409 `code: model_work`.
//! - Пакет `POST /recall {event: "work", kind?, budget_tokens?}` ([`package`]): захват
//!   старейшей работы агента `queued` пространств сессии (решение сессии, если пользователь — его
//!   владелец; Knowledge; Rules) и Markdown пакета; работ нет — «Внутренних работ нет.».
//!   Участнику общего решения работ владельца нет (его база и настройки — владельца).
//!
//! Работы — служебные данные (ссылка `j:`): `memory_read` их не читает, в экспорт и архив
//! пространства не входят; маршруты — вне `memory_routes` (итог не меняет знания).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use axum::Extension;
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use zam_core::Id;
use zam_store::SpaceKind;
use zam_store::works::{self as store, Work, WorkKind, WorkMode, WorkState};

use super::agent_view;
use super::scope::require_rw;
use super::search::targets::{ScopeReq, resolve_targets};
use super::sessions::{McpOrigin, RequestSession, explicit_only, request_session};
use super::{json_body, query};
use crate::app::AppState;
use crate::auth::{AdminOnly, Principal};
use crate::config::WorksMode;
use crate::error::ApiError;
use crate::works::kinds::{Author, ItemCtx};
use crate::works::package::{PACKAGE_BUDGET, PACKAGE_BUDGET_MAX, WORKS_PACKAGE_EMPTY};
use crate::works::queue::{self, LEASE, Plan};
use crate::works::{NightCtx, settings};

/// `detail` ответа 409 `/problems/works-disabled`.
pub const WORKS_DISABLED_DETAIL: &str =
    "внутренние работы выключены на сервере ([works] enabled = false)";
/// `detail` ответа 400 пакета без сессии (§6.3 п. 3).
pub const SESSION_REQUIRED_DETAIL: &str = "пакет работы — только в сессии: итог принимает \
     захватившая сессия (X-Zam-Session; MCP — session_id, CLI — ZAM_SESSION)";
/// `detail` ответа 403 токену `ro` (§6.3 п. 3).
pub const RW_REQUIRED_DETAIL: &str = "итог работы требует токен rw: по токену ro работа не \
     захватывается";
/// Работ в ответе `GET /works` по умолчанию и наибольшее.
pub const LIST_LIMIT_DEFAULT: usize = 50;
pub const LIST_LIMIT_MAX: usize = 200;
/// Событие учёта выданного пакета (`session_tokens`).
pub const TOKEN_EVENT: &str = "work";

/// Работа по ссылке `j:<id>` или id.
fn work_id(raw: &str) -> Result<Id, ApiError> {
    let s = raw.trim();
    s.strip_prefix("j:")
        .unwrap_or(s)
        .parse()
        .map_err(|_| ApiError::BadRequest("работа: j:<id>".into()))
}

/// Вид по имени; неизвестный — 400.
fn parse_kind(s: &str) -> Result<WorkKind, ApiError> {
    WorkKind::parse(s.trim()).ok_or_else(|| {
        let names: Vec<&str> = WorkKind::ALL.iter().map(|k| k.as_str()).collect();
        ApiError::BadRequest(format!("kind: {}", names.join(" | ")))
    })
}

/// Работа как JSON: `id` — `j:<id>`; `items` — ссылки, ревизии, виды и заголовки кандидатов
/// (без текста пакета); агенту (`mask`) — заголовки и итог с маской.
pub fn work_json(w: &Work, mask: bool) -> Value {
    let items: Vec<Value> = w
        .items
        .iter()
        .map(|i| {
            let title = if mask {
                agent_view::mask(&i.title).into_owned()
            } else {
                i.title.clone()
            };
            json!({"n": i.n, "target": i.target, "rev": i.rev, "kind": i.kind, "title": title})
        })
        .collect();
    let mut result = w.result.clone().unwrap_or(Value::Null);
    if let Value::Object(o) = &mut result {
        // служебные поля приёма: ответ исполнителю — копия итога, отпечаток тела — сверка повтора
        o.remove("response");
        o.remove("request_hash");
    }
    if mask {
        agent_view::mask_value(&mut result);
    }
    let ts = |t: Option<jiff::Timestamp>| t.map(zam_store::time::fmt_ts);
    json!({
        "id": format!("j:{}", w.id),
        "kind": w.kind.as_str(),
        "space": w.space_id,
        "mode": w.mode.as_str(),
        "origin": w.origin.as_str(),
        "night": w.night.map(|d| d.to_string()),
        "state": w.state.as_str(),
        "items": items,
        "claimed_session": w.claimed_session,
        "claimed_until": ts(w.claimed_until),
        "attempts": w.attempts,
        "created_at": zam_store::time::fmt_ts(w.created_at),
        "started_at": ts(w.started_at),
        "finished_at": ts(w.finished_at),
        "result": result,
        "error_class": w.error_class,
    })
}

/// Действующие режимы видов `{<вид>: {mode, reason}}`.
fn modes_json(u: &settings::UserWorks, st: &AppState) -> Value {
    let m: Map<String, Value> = u
        .effective(&st.works())
        .into_iter()
        .map(|(k, mode, why)| {
            (
                k.as_str().to_string(),
                json!({"mode": mode.as_str(), "reason": why}),
            )
        })
        .collect();
    Value::Object(m)
}

/// Пауза исполнителя `{class, since, from, to}` (показ — только человеку).
fn pause_json(u: &settings::UserWorks) -> Option<Value> {
    u.pause.as_ref().map(|x| {
        json!({"class": x.class, "since": zam_store::time::fmt_ts(x.since), "from": x.from, "to": x.to})
    })
}

/// Пауза исполнителя для человека (`{class, since}`); агенту — нет.
fn paused_json(p: &Principal, u: &settings::UserWorks) -> Option<Value> {
    if !p.is_human() {
        return None;
    }
    pause_json(u)
}

#[derive(Deserialize)]
pub struct ListQuery {
    state: Option<String>,
    kind: Option<String>,
    limit: Option<usize>,
    user: Option<String>,
}

#[derive(Deserialize)]
pub struct UserQuery {
    user: Option<String>,
}

/// Пользователь очереди: `?user=` — чужой только администратору (токену и сессии пользователя —
/// свой id или 403); без него — пользователь запроса ([`crate::auth::target_user`]: владелец
/// токена, пользователь сессии, у администратора — `X-Zam-User` или он сам). Несуществующий —
/// 404. Та же функция у подключённой модели (`api::works_model`, там токену — 403 до неё;
/// финальное ревью M7.3, Minor 6).
pub(crate) async fn works_user(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    user: Option<&str>,
) -> Result<Id, ApiError> {
    let Some(raw) = user.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(crate::auth::target_user(st, p, headers).await?.id);
    };
    let id = super::parse_id(raw)?;
    match p {
        Principal::Admin { .. } => {
            st.system.get_user(id).await?.ok_or(ApiError::NotFound)?;
            Ok(id)
        }
        _ if p.bound_user() == Some(id) => Ok(id),
        _ => Err(ApiError::Forbidden),
    }
}

/// `GET /works?state=&kind=&limit=` → `{works, modes, paused?}`.
pub async fn list(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let state = match q.state.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => Some(WorkState::parse(s).ok_or_else(|| {
            ApiError::BadRequest(
                "state: queued | running | done | failed | expired | cancelled".into(),
            )
        })?),
    };
    let kind = match q.kind.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => Some(parse_kind(s)?),
    };
    let limit = q.limit.unwrap_or(LIST_LIMIT_DEFAULT);
    if !(1..=LIST_LIMIT_MAX).contains(&limit) {
        return Err(ApiError::BadRequest(format!(
            "limit: от 1 до {LIST_LIMIT_MAX}"
        )));
    }
    let user = works_user(&st, &p, &headers, q.user.as_deref()).await?;
    let db = st.users.get(user).await?;
    store::expire(&db, zam_store::time::now()).await?;
    let works = store::list(&db, state, kind, limit).await?;
    let u = settings::load(&st.system, user).await?;
    let mask = agent_view::masks(&p);
    let mut out = json!({
        "works": works.iter().map(|w| work_json(w, mask)).collect::<Vec<_>>(),
        "modes": modes_json(&u, &st),
    });
    if let Some(pz) = paused_json(&p, &u) {
        out["paused"] = pz;
    }
    Ok(Json(out))
}

/// Расход токенов модели суток и месяца UTC по журналу отправок — одно определение у
/// `GET /works/usage` и сводки администратора `GET /admin/works`.
async fn tokens_used(db: &zam_store::Db) -> Result<(u64, u64), ApiError> {
    let now = zam_store::time::now();
    let day = zam_store::work_calls::tokens_since(db, zam_store::time::day_start_utc(now)).await?;
    let month =
        zam_store::work_calls::tokens_since(db, zam_store::time::month_start_utc(now)).await?;
    Ok((day, month))
}

/// Упавшие работы сводки администратора — за столько суток (`failed_7d`).
pub const ADMIN_FAILED_DAYS: i64 = 7;

/// Строка сводки администратора одного пользователя (M7.4 §7.6 п. 2): режимы, пауза, очередь,
/// упавшие за [`ADMIN_FAILED_DAYS`] суток, последняя ночь, токены, последний прогон проверки —
/// теми же функциями, что `GET /works/usage`, `GET /works` и `GET /check/status`.
async fn admin_row(st: &AppState, u: &zam_store::User) -> Result<Value, ApiError> {
    let db = st.users.get(u.id).await?;
    let now = zam_store::time::now();
    store::expire(&db, now).await?;
    let since = now - jiff::SignedDuration::from_hours(24 * ADMIN_FAILED_DAYS);
    let c = store::state_counts(&db, since).await?;
    let (day, month) = tokens_used(&db).await?;
    let w = settings::load(&st.system, u.id).await?;
    let last = store::last_night(&db).await?;
    // действующие пределы — те же, по которым сервер отказывает (M7.6 §9.9, решение 172)
    let (daily, monthly) = crate::works::exec::token_limits(&w, &st.works());
    let mut row = json!({
        "user": u.id,
        "login": u.name,
        "modes": modes_json(&w, st),
        "queued": c.queued,
        "running": c.running,
        "failed_7d": c.failed_since,
        "last_night": last.map(|d| d.to_string()),
        "tokens": {"day": day, "month": month},
        "limits": w.value(settings::WORKS_LIMITS, &st.works()),
        "limit": {"daily": daily, "monthly": monthly},
        "check": super::check::last_brief(st, u.id, &db).await?,
    });
    if let Some(pz) = pause_json(&w) {
        row["paused"] = pz;
    }
    Ok(row)
}

/// `GET /admin/works` → `{users: [...]}` — только администратор (M7.4 §7.6 п. 2, признак
/// `admin_works`): строка на каждого пользователя ([`admin_row`]); база пользователя не
/// открылась — строка `{user, login, error}` (WARN в журнал), сводка остальных не теряется.
/// Подробности — `GET /works?user=`, `GET /works/calls?user=`.
pub async fn admin_summary(
    _: AdminOnly,
    State(st): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let mut users = Vec::new();
    for u in st.system.list_users().await? {
        users.push(match admin_row(&st, &u).await {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!(user = %u.id, error = %e, "сводка работ: база пользователя не прочитана");
                json!({"user": u.id, "login": u.name, "error": e.to_string()})
            }
        });
    }
    let cfg = st.works();
    Ok(Json(json!({
        "users": users,
        // M7.6 §9.9: потолки и действующие настройки сервера (`GET /admin/works/settings`)
        "ceiling": {"daily": cfg.max_daily_tokens, "monthly": cfg.max_monthly_tokens},
        "settings": st.works_cfg.to_json(),
    })))
}

/// `GET /works/usage?user=` → `{day, month, ceiling, last_night, night, modes, check, paused?}`.
pub async fn usage(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<UserQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let user = works_user(&st, &p, &headers, q.user.as_deref()).await?;
    let db = st.users.get(user).await?;
    let (day, month) = tokens_used(&db).await?;
    let u = settings::load(&st.system, user).await?;
    let cfg = &st.works();
    // те же пределы, по которым сервер отказывает (финальное ревью M7.3, Minor 6)
    let (daily, monthly) = crate::works::exec::token_limits(&u, cfg);
    let last = store::last_night(&db).await?;
    let mut out = json!({
        "day": {"tokens": day, "limit": daily},
        "month": {"tokens": month, "limit": monthly},
        "ceiling": {"daily": cfg.max_daily_tokens, "monthly": cfg.max_monthly_tokens},
        "last_night": last.map(|d| d.to_string()),
        "night": {"at": cfg.at, "tz": cfg.tz, "window_min": cfg.window_min},
        "modes": modes_json(&u, &st),
        // ночная проверка сервера и пользователя — для выключателя «Ночная проверка моей памяти»
        // (финальное ревью M7.3, Minor 12; та же форма, что у `GET /check/status`)
        "check": super::check::check_json(cfg, &u),
    });
    if let Some(pz) = paused_json(&p, &u) {
        out["paused"] = pz;
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunBody {
    kind: String,
    /// `knowledge`, `rules`, GUID решения или id пространства.
    space: Option<String>,
}

/// Пространство ручного запуска: `knowledge`, `rules`, GUID решения или id пространства — из
/// пространств вида ([`crate::works::kinds::space_kinds`]); иначе 400.
async fn run_space(db: &zam_store::Db, kind: WorkKind, raw: &str) -> Result<Id, ApiError> {
    let s = raw.trim();
    let allowed = crate::works::kinds::space_kinds(kind);
    let spaces = zam_store::list_spaces(db).await?;
    spaces
        .iter()
        .filter(|x| allowed.contains(&x.kind))
        .find(|x| {
            x.kind.as_str() == s
                || x.id.to_string() == s
                || x.solution_guid
                    .as_deref()
                    .is_some_and(|g| g.eq_ignore_ascii_case(s))
        })
        .map(|x| x.id)
        .ok_or_else(|| {
            ApiError::BadRequest(format!(
                "space: пространство вида {} вашей базы (knowledge, rules, GUID решения)",
                kind.as_str()
            ))
        })
}

/// `POST /works/run {kind, space?}` → 200 (агент) или 202 (модель).
pub async fn run(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if !p.is_human() {
        // агент не запускает работы (и обращения к модели) сам — решение координатора 35
        return Err(ApiError::Forbidden);
    }
    if !st.works().enabled {
        return Err(ApiError::WorksDisabled(WORKS_DISABLED_DETAIL.into()));
    }
    let b: RunBody = json_body(body)?;
    let kind = parse_kind(&b.kind)?;
    let user = crate::auth::target_user(&st, &p, &headers).await?.id;
    let db = st.users.get(user).await?;
    let only = match b.space.as_deref() {
        None => None,
        Some(s) => Some(run_space(&db, kind, s).await?),
    };
    let u = settings::load(&st.system, user).await?;
    let (mode, why) = settings::effective_mode(kind, &u, &st.works());
    if mode == WorksMode::Off {
        return Err(ApiError::conflict_code(
            "works_off",
            format!(
                "режим вида {} — off ({}): выберите agent или model в настройках",
                kind.as_str(),
                why.unwrap_or("выбран")
            ),
            &[("reason", json!(why))],
        ));
    }
    let run = Id::new();
    let plan = Plan::manual(user, kind, only, run);
    if queue::packages_left(&db, &plan).await? == 0 {
        return Err(ApiError::conflict_code(
            "manual_limit",
            format!(
                "ручных запусков вида {} за сутки UTC — не больше {} пакетов",
                kind.as_str(),
                queue::PACKAGES_PER_RUN
            ),
            &[],
        ));
    }
    if mode == WorksMode::Agent {
        let ctx = NightCtx::new(st.clone());
        let queued = queue::manual_agent(&ctx, &plan).await?;
        return Ok((
            StatusCode::OK,
            Json(json!({
                "mode": "agent",
                "run": Value::Null,
                "queued": queued.iter().map(|w| format!("j:{}", w.id)).collect::<Vec<_>>(),
            })),
        ));
    }
    // модель — сразу, учтённым запуском раннера: контекст строит раннер, сюда — только данные
    st.night
        .spawn_tracked(move |ctx| queue::manual_model(ctx, plan))
        .map_err(|_| ApiError::ShuttingDown { retry_after_s: 5 })?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"mode": "model", "run": format!("manual:{run}"), "queued": []})),
    ))
}

/// Сколько итог ждёт замка работы, пока принимается другой итог той же работы.
pub const FINISH_WAIT: Duration = Duration::from_secs(30);

/// Замки работ: итоги и возврат одной работы — по одному (повтор с тем же ключом ждёт первый и
/// получает его ответ). Запись — пока замок кто-то держит или ждёт.
static FINISHING: LazyLock<Mutex<HashMap<Id, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Default::default);

/// Захваченный замок работы; при уничтожении запись снимается, если её больше никто не ждёт.
struct Finishing {
    id: Id,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl Finishing {
    /// Замок работы `id`: ждёт не дольше [`FINISH_WAIT`], иначе 409 `code: result_in_progress`.
    async fn take(id: Id) -> Result<Finishing, ApiError> {
        let lock = FINISHING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id)
            .or_default()
            .clone();
        // и при неудаче — та же уборка записи, что после итога
        let mut f = Finishing { id, guard: None };
        match tokio::time::timeout(FINISH_WAIT, lock.lock_owned()).await {
            Ok(g) => {
                f.guard = Some(g);
                Ok(f)
            }
            Err(_) => Err(ApiError::conflict_code(
                "result_in_progress",
                format!("итог работы j:{id} ещё принимается — повторите позже"),
                &[],
            )),
        }
    }
}

impl Drop for Finishing {
    fn drop(&mut self) {
        drop(self.guard.take());
        let mut map = FINISHING.lock().unwrap_or_else(|e| e.into_inner());
        if map.get(&self.id).is_some_and(|l| Arc::strong_count(l) == 1) {
            map.remove(&self.id);
        }
    }
}

/// Тело итога: `items` и/или `skip` (хотя бы одно), `session_id` — явная сессия (как
/// `X-Zam-Session`); прочие поля — 400.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultBody {
    #[serde(default)]
    items: Value,
    #[serde(default)]
    skip: Value,
    session_id: Option<String>,
}

/// Отпечаток тела итога (`items`, `skip`) для сверки повтора по `Idempotency-Key`.
fn request_hash(items: &Value, skip: &Value) -> String {
    let body = json!({"items": items, "skip": skip}).to_string();
    blake3::hash(body.as_bytes()).to_hex().to_string()
}

/// Повтор итога с тем же ключом: прежний ответ, если тело то же; другое тело — 422.
fn replay(result: &Value, hash: &str) -> Result<Json<Value>, ApiError> {
    if result.get("request_hash").and_then(Value::as_str) != Some(hash) {
        return Err(ApiError::IdempotencyMismatch(
            "Idempotency-Key уже использован другим итогом этой работы: новому итогу — новый ключ"
                .into(),
        ));
    }
    Ok(Json(result.get("response").cloned().unwrap_or(Value::Null)))
}

/// Сессия итога и возврата: явная (`X-Zam-Session` или `session_id`), иначе 400.
async fn work_session(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    origin: Option<&McpOrigin>,
    body_session: Option<&str>,
) -> Result<RequestSession, ApiError> {
    explicit_only(request_session(st, p, headers, origin, body_session).await?)
        .ok_or_else(|| ApiError::SessionRequired(SESSION_REQUIRED_DETAIL.into()))
}

fn not_claimed(w: &Work) -> ApiError {
    ApiError::WorkNotClaimed(format!(
        "работа j:{} — {}: итог и возврат принимаются только от сессии, захватившей её \
         (running); снятая, истёкшая и закрытая работа итога не принимает",
        w.id,
        w.state.as_str()
    ))
}

/// Работа с повтором по ключу: тот же ключ — прежний ответ (или 422), иначе `None`.
fn replayed(w: &Work, key: Option<&str>, hash: &str) -> Option<Result<Json<Value>, ApiError>> {
    let (k, rk) = (key?, w.result_key.as_deref()?);
    (k == rk).then(|| replay(w.result.as_ref().unwrap_or(&Value::Null), hash))
}

/// `POST /works/{id}/result {items?, skip?, session_id?}` → итог работы.
pub async fn result(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    origin: Option<Extension<McpOrigin>>,
    Path(raw): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    require_rw(&p).map_err(|_| ApiError::Denied(RW_REQUIRED_DETAIL.into()))?;
    let id = work_id(&raw)?;
    let b: ResultBody = json_body(body)?;
    if b.items.is_null() && b.skip.is_null() {
        return Err(ApiError::BadRequest(
            "итог — items: [{n, …}] или skip: \"<причина>\" (отказ от всей работы)".into(),
        ));
    }
    let s = work_session(
        &st,
        &p,
        &headers,
        origin.as_deref(),
        b.session_id.as_deref(),
    )
    .await?;
    let key = super::entries::idempotency_key(&headers)?;
    let hash = request_hash(&b.items, &b.skip);
    let user = crate::auth::target_user(&st, &p, &headers).await?.id;
    let db = st.users.get(user).await?;
    store::get(&db, id).await?.ok_or(ApiError::NotFound)?;
    // итоги одной работы — по одному: под замком работа перечитывается
    let _lock = Finishing::take(id).await?;
    store::expire(&db, zam_store::time::now()).await?;
    let w = store::get(&db, id).await?.ok_or(ApiError::NotFound)?;
    // повтор с тем же ключом — прежний ответ без проверки и записи (решение координатора 70)
    if let Some(r) = replayed(&w, key.as_deref(), &hash) {
        return r;
    }
    if w.state != WorkState::Running
        || w.mode != WorkMode::Agent
        || w.claimed_session.as_deref() != Some(s.touch.id.as_str())
    {
        return Err(not_claimed(&w));
    }
    let author = Author::Agent(s.touch.id.clone());
    let ictx = ItemCtx {
        st: &st,
        user,
        db: &db,
        work: &w,
        author: &author,
    };
    let data = json!({"items": b.items, "skip": b.skip});
    let mut a = queue::accept(&ictx, &data, &[])
        .await
        .map_err(ApiError::BadRequest)?;
    a.result["request_hash"] = json!(hash);
    // записи принятого и закрытие — одна транзакция: работа, снятая между проверкой и закрытием,
    // ничего не записывает
    let closed = store::finish_claimed(
        &db,
        id,
        Some(&s.touch.id),
        a.state,
        a.result,
        key.as_deref(),
        a.writes,
    )
    .await?;
    match closed {
        store::Closed::Closed { warnings } => {
            let mut r = a.response;
            if !warnings.is_empty() {
                r["warnings"] = json!(warnings);
            }
            Ok(Json(r))
        }
        store::Closed::Replay(prev) => replay(&prev, &hash),
        store::Closed::NotClaimed(Some(w)) => Err(not_claimed(&w)),
        store::Closed::NotClaimed(None) => Err(ApiError::NotFound),
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ReleaseBody {
    session_id: Option<String>,
}

/// `POST /works/{id}/release` → `{work, state: "queued"}`.
pub async fn release(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    origin: Option<Extension<McpOrigin>>,
    Path(raw): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    require_rw(&p).map_err(|_| ApiError::Denied(RW_REQUIRED_DETAIL.into()))?;
    let id = work_id(&raw)?;
    let b: ReleaseBody = match &body {
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => ReleaseBody::default(),
        _ => json_body(body)?,
    };
    let user = crate::auth::target_user(&st, &p, &headers).await?.id;
    let db = st.users.get(user).await?;
    store::get(&db, id).await?.ok_or(ApiError::NotFound)?;
    // не посреди приёма итога той же работы
    let _lock = Finishing::take(id).await?;
    let w = store::get(&db, id).await?.ok_or(ApiError::NotFound)?;
    if w.mode == WorkMode::Model {
        return Err(ApiError::conflict_code(
            "model_work",
            format!(
                "работу j:{id} выполняет подключённая модель на сервере — вернуть её в очередь \
                 нельзя"
            ),
            &[],
        ));
    }
    if w.state != WorkState::Running {
        return Err(not_claimed(&w));
    }
    if !p.is_human() {
        let s = work_session(
            &st,
            &p,
            &headers,
            origin.as_deref(),
            b.session_id.as_deref(),
        )
        .await?;
        if w.claimed_session.as_deref() != Some(s.touch.id.as_str()) {
            return Err(not_claimed(&w));
        }
    }
    if !store::release(&db, id).await? {
        return Err(not_claimed(&w));
    }
    Ok(Json(json!({"work": format!("j:{id}"), "state": "queued"})))
}

/// Запрос пакета работы (поля тела `/recall`).
pub(crate) struct PackageReq<'a> {
    /// Явная сессия запроса.
    pub session: Option<RequestSession>,
    pub kind: Option<&'a str>,
    pub budget: Option<usize>,
    /// Решение тела (сильнее заголовка).
    pub solution: Option<String>,
    /// Вызов из MCP: учёт ведёт инструмент.
    pub mcp: bool,
}

/// Пакет работы `POST /recall {event: "work", kind?, budget_tokens?}` (§6.3 п. 3; MCP
/// `memory_recall event=work`, `zam work next`): захват работы агента и Markdown пакета.
/// `headers` — заголовки с решением запроса (у `/recall` решение тела сильнее заголовка).
pub(crate) async fn package(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: PackageReq<'_>,
) -> Result<Value, ApiError> {
    let PackageReq {
        session,
        kind,
        budget,
        solution,
        mcp,
    } = req;
    let t0 = std::time::Instant::now();
    require_rw(p).map_err(|_| ApiError::Denied(RW_REQUIRED_DETAIL.into()))?;
    let kind = match kind.map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => Some(parse_kind(s)?),
    };
    let budget = budget.unwrap_or(PACKAGE_BUDGET);
    if !(1..=PACKAGE_BUDGET_MAX).contains(&budget) {
        return Err(ApiError::BadRequest(format!(
            "budget_tokens: у пакета работы от 1 до {PACKAGE_BUDGET_MAX}"
        )));
    }
    let Some(s) = session else {
        return Err(ApiError::SessionRequired(SESSION_REQUIRED_DETAIL.into()));
    };
    let user = crate::auth::target_user(st, p, headers).await?.id;
    // пространства сессии — только личная база: чужое решение (участник) работ не даёт
    let scope = ScopeReq {
        solution,
        include_knowledge: Some(true),
        include_rules: Some(true),
        ..ScopeReq::default()
    };
    let (targets, warnings) = resolve_targets(st, p, headers, &scope).await?;
    let spaces: Vec<Id> = targets
        .iter()
        .filter(|t| t.owner == user && t.solution.is_none())
        .flat_map(|t| {
            t.spaces
                .iter()
                .filter(|x| x.kind != SpaceKind::Human)
                .map(|x| x.id)
        })
        .collect();
    let db = st.users.get(user).await?;
    store::expire(&db, zam_store::time::now()).await?;
    let works = settings::load(&st.system, user).await?;
    let mut claimed: Option<(Id, String)> = None;
    // общий выключатель — «ничего»: работы не выдаются и не снимаются (включат — продолжатся)
    while st.works().enabled
        && let Some(w) = store::claim_next(&db, &spaces, kind, &s.touch.id, LEASE).await?
    {
        let id = w.id;
        // действующий режим — перед выдачей (снятие при смене режима могло не пройти)
        if settings::effective_mode(w.kind, &works, &st.works()).0 != WorksMode::Agent {
            store::finish(
                &db,
                id,
                WorkState::Cancelled,
                Value::Null,
                Some(store::MODE_CHANGED),
            )
            .await?;
            continue;
        }
        match queue::prepare(&db, w, budget).await? {
            Some(pk) => {
                claimed = Some((id, pk.text));
                break;
            }
            // все кандидаты удалены или исправлены — работа снята, следующая
            None => {
                store::finish(
                    &db,
                    id,
                    WorkState::Cancelled,
                    Value::Null,
                    Some(store::STALE),
                )
                .await?;
            }
        }
    }
    let text = claimed
        .as_ref()
        .map_or_else(|| WORKS_PACKAGE_EMPTY.to_string(), |(_, md)| md.clone());
    let tokens = zam_store::count_tokens(&text);
    let collect_ms = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
    // учёт — событие `work` (вызов из MCP учитывает сам инструмент, событием `mcp`)
    let mut record_ms = None;
    if !mcp {
        let rt = std::time::Instant::now();
        if let Err(e) = zam_store::add_tokens(&s.db, &s.touch, TOKEN_EVENT, tokens as u64).await {
            tracing::warn!(error = %e, "внутренние работы: учёт пакета не записан");
        }
        record_ms = Some(u64::try_from(rt.elapsed().as_millis()).unwrap_or(u64::MAX));
    }
    Ok(json!({
        "text": text,
        "items": [],
        "tokens": tokens,
        "rules_tokens": 0,
        "timings": {
            "collect_ms": collect_ms,
            "record_ms": record_ms,
            "total_ms": u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
        },
        "warnings": warnings,
        "work": claimed.as_ref().map(|(id, _)| format!("j:{id}")),
    }))
}
