//! Очередь внутренних работ (M7.3 §6.3 пп. 3, 4, 8): REST очереди, ручной запуск, итог и возврат
//! работы, пакет `/recall {event: "work"}`.
//!
//! - `GET /works?state=&kind=&limit=&user=` → `{works: […], modes: {<вид>: {mode, reason}},
//!   paused?}` — очередь и последние работы с итогами, новые первыми (`limit` 1..=200, по
//!   умолчанию 50). Человеку — исходник, пауза исполнителя; токену — работы его пользователя с
//!   маской `‹secret›`, без паузы (снять её агент не может, §6.2 п. 5). Сначала — сроки работ
//!   агента (истёкшие аренды, 7 суток без итога).
//! - `GET /works/usage` → `{day: {tokens, limit}, month: {tokens, limit}, ceiling, last_night,
//!   modes, paused?}` — расход токенов модели суток и месяца UTC по журналу отправок, пределы.
//! - `POST /works/run {kind, space?}` — ручной запуск вида (§6.3 п. 2), **только человек**
//!   (токену — 403); `[works] enabled = false` — 409 `/problems/works-disabled` (решение 76);
//!   действующий режим вида `off` — 409 `code: works_off` с причиной; предел ручных запусков
//!   ([`crate::works::queue::PACKAGES_PER_RUN`] пакетов вида за сутки UTC) исчерпан — 409
//!   `code: manual_limit`. Режим `agent` — 200 `{mode, run: null, queued: [j:…]}` (работы ждут
//!   агента); `model` — 202 `{mode, run: "manual:<id>", queued: []}`: выполнение сразу, вне окна,
//!   учтённым запуском ночного исполнителя (`NightRunner::spawn_tracked`; сервер
//!   останавливается — 503).
//! - `POST /works/{id}/result {items, skip?}` (`id` — `j:<id>` или id) — итог работы (§6.3 п. 4):
//!   только от сессии, захватившей её (`running`; `X-Zam-Session`), иначе 409
//!   `/problems/work-not-claimed`; токен `ro` — 403; без сессии — 400
//!   `/problems/session-required`. Ответ `{work, state, accepted: [n], rejected: [{n, target,
//!   reason}], skipped: [n], skip: [{n, target, why}], skip_reason}`; повтор с тем же
//!   `Idempotency-Key` — прежний ответ, второй итог — 409.
//! - `POST /works/{id}/release` — вернуть работу `running` в очередь без итога: токен — только
//!   захватившая сессия, человек — любую работу своего пользователя.
//! - Пакет `POST /recall {event: "work", kind?, budget_tokens?}` ([`package`]): захват
//!   старейшей работы агента `queued` пространств сессии (решение сессии, если пользователь — его
//!   владелец; Knowledge; Rules) и Markdown пакета; работ нет — «Внутренних работ нет.».
//!   Участнику общего решения работ владельца нет (его база и настройки — владельца).
//!
//! Работы — служебные данные (ссылка `j:`): `memory_read` их не читает, в экспорт и архив
//! пространства не входят; маршруты — вне `memory_routes` (итог не меняет знания).

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

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
use crate::agent_setup::Lang;
use crate::app::AppState;
use crate::auth::Principal;
use crate::config::WorksMode;
use crate::error::ApiError;
use crate::works::kinds::{Author, ItemCtx};
use crate::works::package::{self, PACKAGE_BUDGET, PACKAGE_BUDGET_MAX, WORKS_PACKAGE_EMPTY};
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
        // ответ исполнителю — копия итога: в ленте не повторяется
        o.remove("response");
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
        .effective(&st.cfg.works)
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

/// Пауза исполнителя для человека (`{class, since}`); агенту — нет.
fn paused_json(p: &Principal, u: &settings::UserWorks) -> Option<Value> {
    if !p.is_human() {
        return None;
    }
    u.pause.as_ref().map(|x| {
        json!({"class": x.class, "since": zam_store::time::fmt_ts(x.since), "from": x.from, "to": x.to})
    })
}

#[derive(Deserialize)]
pub struct ListQuery {
    state: Option<String>,
    kind: Option<String>,
    limit: Option<usize>,
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
    let user = crate::auth::target_user(&st, &p, &headers).await?.id;
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

/// Начало месяца UTC момента `t`.
fn month_start(t: jiff::Timestamp) -> jiff::Timestamp {
    let d = t.to_zoned(jiff::tz::TimeZone::UTC).date().first_of_month();
    d.to_zoned(jiff::tz::TimeZone::UTC)
        .map_or(t, |z| z.timestamp())
}

/// `GET /works/usage` → `{day, month, ceiling, last_night, modes, paused?}`.
pub async fn usage(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let user = crate::auth::target_user(&st, &p, &headers).await?.id;
    let db = st.users.get(user).await?;
    let now = zam_store::time::now();
    let day = zam_store::work_calls::tokens_since(&db, zam_store::time::day_start_utc(now)).await?;
    let month = zam_store::work_calls::tokens_since(&db, month_start(now)).await?;
    let u = settings::load(&st.system, user).await?;
    let cfg = &st.cfg.works;
    let (mut daily, mut monthly) = (cfg.max_daily_tokens, cfg.max_monthly_tokens);
    if let Some(l) = u.limits {
        daily = daily.min(l.daily_tokens);
        monthly = monthly.min(l.monthly_tokens);
    }
    let last = store::last_night(&db).await?;
    let mut out = json!({
        "day": {"tokens": day, "limit": daily},
        "month": {"tokens": month, "limit": monthly},
        "ceiling": {"daily": cfg.max_daily_tokens, "monthly": cfg.max_monthly_tokens},
        "last_night": last.map(|d| d.to_string()),
        "night": {"at": cfg.at, "tz": cfg.tz, "window_min": cfg.window_min},
        "modes": modes_json(&u, &st),
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

fn conflict(code: &str, detail: String) -> ApiError {
    let mut extra = Map::new();
    extra.insert("code".into(), json!(code));
    ApiError::Conflict { detail, extra }
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
    if !st.cfg.works.enabled {
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
    let (mode, why) = settings::effective_mode(kind, &u, &st.cfg.works);
    if mode == WorksMode::Off {
        let mut extra = Map::new();
        extra.insert("code".into(), json!("works_off"));
        extra.insert("reason".into(), json!(why));
        return Err(ApiError::Conflict {
            detail: format!(
                "режим вида {} — off ({}): выберите agent или model в настройках",
                kind.as_str(),
                why.unwrap_or("выбран")
            ),
            extra,
        });
    }
    let run = Id::new();
    let plan = Plan::manual(user, kind, only, run);
    if queue::packages_left(&db, &plan).await? == 0 {
        return Err(conflict(
            "manual_limit",
            format!(
                "ручных запусков вида {} за сутки UTC — не больше {} пакетов",
                kind.as_str(),
                queue::PACKAGES_PER_RUN
            ),
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

/// Работы, итог которых сейчас принимается (одна проверка итога на работу за раз).
static FINISHING: LazyLock<Mutex<HashSet<Id>>> = LazyLock::new(Default::default);

/// Работа в [`FINISHING`] до уничтожения.
struct Finishing(Id);

impl Finishing {
    fn take(id: Id) -> Option<Finishing> {
        FINISHING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id)
            .then_some(Finishing(id))
    }
}

impl Drop for Finishing {
    fn drop(&mut self) {
        FINISHING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

#[derive(Deserialize)]
pub struct ResultBody {
    #[serde(default)]
    items: Value,
    #[serde(default)]
    skip: Value,
    /// Явная сессия (как `X-Zam-Session`; MCP передаёт заголовок).
    session_id: Option<String>,
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

/// `POST /works/{id}/result {items, skip?}` → итог работы.
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
    let s = work_session(
        &st,
        &p,
        &headers,
        origin.as_deref(),
        b.session_id.as_deref(),
    )
    .await?;
    let key = super::entries::idempotency_key(&headers)?;
    let user = crate::auth::target_user(&st, &p, &headers).await?.id;
    let db = st.users.get(user).await?;
    let w = store::get(&db, id).await?.ok_or(ApiError::NotFound)?;
    // повтор с тем же ключом — прежний ответ (решение координатора 70)
    if let (Some(k), Some(rk)) = (key.as_deref(), w.result_key.as_deref())
        && k == rk
        && let Some(resp) = w.result.as_ref().and_then(|r| r.get("response"))
    {
        return Ok(Json(resp.clone()));
    }
    store::expire(&db, zam_store::time::now()).await?;
    let w = store::get(&db, id).await?.ok_or(ApiError::NotFound)?;
    if w.state != WorkState::Running
        || w.mode != WorkMode::Agent
        || w.claimed_session.as_deref() != Some(s.touch.id.as_str())
    {
        return Err(not_claimed(&w));
    }
    let Some(_guard) = Finishing::take(id) else {
        return Err(ApiError::WorkNotClaimed(format!(
            "итог работы j:{id} уже принимается — дождитесь ответа"
        )));
    };
    let author = Author::Agent(s.touch.id.clone());
    let ictx = ItemCtx {
        st: &st,
        user,
        db: &db,
        work: &w,
        author: &author,
    };
    let data = json!({"items": b.items, "skip": b.skip});
    let a = queue::accept(&ictx, &data, &[])
        .await
        .map_err(ApiError::BadRequest)?;
    let closed = store::finish_claimed(
        &db,
        id,
        Some(&s.touch.id),
        a.state,
        a.result,
        key.as_deref(),
    )
    .await?;
    if !closed {
        let w = store::get(&db, id).await?.ok_or(ApiError::NotFound)?;
        return Err(not_claimed(&w));
    }
    Ok(Json(a.response))
}

#[derive(Deserialize, Default)]
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
    let w = store::get(&db, id).await?.ok_or(ApiError::NotFound)?;
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
    let mut claimed: Option<Work> = None;
    // общий выключатель — «ничего»: работы не выдаются и не снимаются (включат — продолжатся)
    while st.cfg.works.enabled
        && let Some(w) = store::claim_next(&db, &spaces, kind, &s.touch.id, LEASE).await?
    {
        // действующий режим — перед выдачей (снятие при смене режима могло не пройти)
        if settings::effective_mode(w.kind, &works, &st.cfg.works).0 != WorksMode::Agent {
            store::finish(
                &db,
                w.id,
                WorkState::Cancelled,
                Value::Null,
                Some(store::MODE_CHANGED),
            )
            .await?;
            continue;
        }
        claimed = Some(w);
        break;
    }
    let text = match &claimed {
        None => WORKS_PACKAGE_EMPTY.to_string(),
        Some(w) => {
            let cands: Vec<(usize, crate::works::kinds::Candidate)> = w
                .items
                .iter()
                .map(|i| (i.n, crate::works::kinds::Candidate::from_item(i)))
                .collect();
            let (md, payload, rest) = package::render_numbered(w, &cands, w.kind, Lang::Ru, budget);
            if !rest.is_empty() {
                // хвост сверх бюджета — в следующую работу
                let keep: HashSet<usize> = payload.items.iter().map(|i| i.n).collect();
                let items = w
                    .items
                    .iter()
                    .filter(|i| keep.contains(&i.n))
                    .cloned()
                    .collect();
                store::set_items(&db, w.id, items).await?;
            }
            md
        }
    };
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
        "work": claimed.as_ref().map(|w| format!("j:{}", w.id)),
    }))
}
