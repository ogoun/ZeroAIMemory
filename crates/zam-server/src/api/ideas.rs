//! Журнал «Идеи» (спецификация M5.7 §4–§9): предложение, решение и «принять как свою».
//!
//! - `POST /entries/{id}/propose {text?, space?}` → 200 `{entry, question}`: идея `new` или
//!   `deferred` → `proposed` и вопрос-предложение с вариантами «начать / отложить /
//!   отказаться» ([`zam_store::propose_idea`]). Предложение — действие агента: сессии WebUI 422.
//!   `space` — пространство будущей задачи; без него — решение запроса (`X-Zam-Solution`), если
//!   оно в базе идеи. Идея `accepted` со снятой или удалённой задачей (§16) — вопрос «вернуть /
//!   отказаться» (`data.proposal: reopen`), статус прежний; `text` обязателен (нет — 422);
//! - `POST /entries/{id}/decide {decision, horizon?, reason?, by?, channel?, space?}` → 200
//!   `{entry, task?, question?}` ([`zam_store::decide_idea`]); `by` и `channel` — по правилам
//!   ответа на вопрос ([`super::questions`]). Права проверяются до хранилища: токен без `by` —
//!   400 `by-required`, `by: agent` — 403 `idea-human-decision`, кроме `decline` своей идеи
//!   (`origin: agent`);
//! - `POST /entries/{id}/adopt` → 200 `Entry`: только человек (сессия WebUI), токену 403;
//! - права правки и корзины идей человека ([`deny_token_on_human_idea`]), `origin` при создании
//!   ([`create_data`]) и связанные записи в `GET /entries/{id}` ([`attach_idea`]).
//!
//! Аудит в `system.db` без текста: `entry.idea.propose`, `entry.idea.decide {decision}`,
//! `entry.idea.adopt`.
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use jiff::Span;
use jiff::civil::Date;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use zam_core::Id;
use zam_store::{
    AnswerBy, Channel, Db, Entry, EntryKind, IdeaData, IdeaDecision, IdeaError, IdeaOrigin,
    NewDecision, StoreError, adopt_idea, decide_idea, get_entry, propose_idea,
};

use super::entries::{audit_details, unprocessable};
use super::json_body;
use super::nodes::audit_node;
use super::scope::{EntryScope, Mode, SpaceRef, deny_report_mutation, entry_scope, live_entry};
use crate::app::AppState;
use crate::attrib::attribution;
use crate::auth::{Client, Principal};
use crate::error::ApiError;

impl From<IdeaError> for ApiError {
    fn from(e: IdeaError) -> Self {
        match e {
            IdeaError::NotIdea => unprocessable("запись не является идеей (kind: idea)"),
            IdeaError::AgentOrigin => ApiError::IdeaAgentOrigin,
            IdeaError::AlreadyDecided { task_id } => ApiError::AlreadyDecided { task_id },
            IdeaError::AlreadyAdopted => ApiError::AlreadyAdopted,
            IdeaError::Transition { from, to } => {
                let mut extra = Map::new();
                extra.insert("code".into(), json!("transition-not-allowed"));
                extra.insert("from".into(), json!(from.as_str()));
                extra.insert("to".into(), json!(to));
                ApiError::Conflict {
                    detail: format!("переход идеи {} → {to} не разрешён", from.as_str()),
                    extra,
                }
            }
            e @ IdeaError::ReasonRequired => ApiError::ReasonRequired(e.to_string()),
            IdeaError::BadHorizon(m) | IdeaError::TaskSpace(m) => unprocessable(m),
            e @ (IdeaError::IdeaDeleted | IdeaError::StaleProposal | IdeaError::OriginChanged) => {
                ApiError::conflict(e.to_string())
            }
            e @ IdeaError::ReopenTextRequired => unprocessable(e.to_string()),
            // ошибки проверки тела операции идеи (`text`, `reason` длиннее предела) — 422
            IdeaError::Store(StoreError::BadInput(m)) => unprocessable(m),
            IdeaError::Store(e) => e.into(),
        }
    }
}

/// Сессия WebUI (человек), а не токен агента.
fn is_session(p: &Principal) -> bool {
    matches!(p, Principal::User { .. } | Principal::Admin { .. })
}

/// Происхождение идеи из её `data` (повреждённое — `agent`, как в хранилище).
fn origin_of(e: &Entry) -> IdeaOrigin {
    IdeaData::from_stored(&e.data).origin
}

/// `data` новой идеи (`POST /spaces/{space}/entries` `kind: idea`, §4): у сессии WebUI
/// `origin` всегда `human` (нет поля — подставляется, `agent` — 422); у токена поле обязательно
/// (нет — 422 [`zam_store::IDEA_ORIGIN_REQUIRED`]). `check_date: false` — повтор по
/// `Idempotency-Key`: структура `data` проверяется, дата `horizon` — нет (сверка без часов).
pub(crate) fn create_data(
    p: &Principal,
    data: &Value,
    check_date: bool,
) -> Result<Value, ApiError> {
    let Value::Object(o) = data else {
        return Err(unprocessable("data должно быть JSON-объектом"));
    };
    let asked = o
        .get("origin")
        .filter(|v| !v.is_null())
        .map(|v| v.as_str().map(str::trim).and_then(IdeaOrigin::parse));
    let origin = match (is_session(p), asked) {
        (true, None | Some(Some(IdeaOrigin::Human))) => IdeaOrigin::Human,
        (true, Some(_)) => {
            return Err(unprocessable(
                "data.origin: идея из WebUI — всегда human (идея человека)",
            ));
        }
        (false, Some(Some(o))) => o,
        (false, _) => return Err(unprocessable(zam_store::IDEA_ORIGIN_REQUIRED)),
    };
    let mut o = o.clone();
    o.insert("origin".into(), json!(origin.as_str()));
    let data = Value::Object(o);
    if check_date {
        zam_store::check_idea(&data, origin)
    } else {
        zam_store::check_idea_shape(&data, origin)
    }
    .map_err(unprocessable)?;
    Ok(data)
}

/// Токен по идее человека (`origin: human`): правка содержимого и корзина — 403
/// `idea-human-owned` (§7.1); `content: false` — чистое дополнение `append_body` (разрешено).
pub(crate) fn deny_token_on_human_idea(
    p: &Principal,
    e: &Entry,
    content: bool,
) -> Result<(), ApiError> {
    if e.kind == EntryKind::Idea && !is_session(p) && content && origin_of(e) == IdeaOrigin::Human {
        return Err(ApiError::IdeaHumanOwned);
    }
    Ok(())
}

/// Связанные с идеей записи для `GET /entries/{id}`: задача `data.task_id` и
/// вопрос-предложение `data.question_id`.
#[derive(Debug, Default)]
pub struct IdeaLinks {
    pub task: Option<Linked>,
    pub question: Option<Linked>,
}

/// Связанная запись: id, статус и заголовок; удалённая — `deleted` (статус и заголовок не
/// раскрываются, WebUI показывает «задача удалена»).
#[derive(Debug)]
pub struct Linked {
    pub id: Id,
    pub entry: Option<Entry>,
}

async fn linked(db: &Db, id: Option<Id>) -> Result<Option<Linked>, ApiError> {
    let Some(id) = id else {
        return Ok(None);
    };
    let entry = match get_entry(db, id).await {
        Ok(e) if e.deleted_at.is_none() => Some(e),
        Ok(_) | Err(StoreError::NotFound) => None,
        Err(e) => return Err(e.into()),
    };
    Ok(Some(Linked { id, entry }))
}

/// Связанные записи идеи `e` (у прочих видов — пусто).
pub async fn idea_links(db: &Db, e: &Entry) -> Result<IdeaLinks, ApiError> {
    if e.kind != EntryKind::Idea {
        return Ok(IdeaLinks::default());
    }
    let d = IdeaData::from_stored(&e.data);
    Ok(IdeaLinks {
        task: linked(db, d.task_id).await?,
        question: linked(db, d.question_id).await?,
    })
}

/// У идеи — `task {id, status, title}` и `question {id, status}`, если есть; удалённая
/// связанная запись — `{id, deleted: true}`.
pub fn attach_idea(v: &mut Value, e: &Entry, links: &IdeaLinks) {
    if e.kind != EntryKind::Idea {
        return;
    }
    let Value::Object(o) = v else {
        return;
    };
    if let Some(t) = &links.task {
        o.insert(
            "task".into(),
            match &t.entry {
                Some(x) => json!({"id": t.id, "status": x.status, "title": x.title}),
                None => json!({"id": t.id, "deleted": true}),
            },
        );
    }
    if let Some(q) = &links.question {
        o.insert(
            "question".into(),
            match &q.entry {
                Some(x) => json!({"id": q.id, "status": x.status}),
                None => json!({"id": q.id, "deleted": true}),
            },
        );
    }
}

/// Живая идея по id для записи (иначе 404 / 422).
async fn idea_scope(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    id: &str,
) -> Result<EntryScope, ApiError> {
    let sc = entry_scope(st, p, headers, id, Mode::Write).await?;
    live_entry(&sc)?;
    deny_report_mutation(p, &sc)?;
    if sc.entry.kind != EntryKind::Idea {
        return Err(unprocessable("операция есть только у идей (kind: idea)"));
    }
    Ok(sc)
}

/// Решение запроса (`X-Zam-Solution`) в базе идеи: id его пространства или `None` (другая база,
/// неизвестный GUID). Решение чужой базы для задачи недоступно (§5.2, п. 1).
async fn request_solution(db: &Db, headers: &HeaderMap) -> Result<Option<Id>, ApiError> {
    let Some(g) = super::search::targets::request_solution(None, headers)? else {
        return Ok(None);
    };
    Ok(zam_store::get_solution(db, g.as_str()).await?.map(|s| s.id))
}

/// Явное пространство будущей задачи (`space` тела) в базе идеи; нет такого — 422.
async fn task_space(db: &Db, raw: Option<&str>) -> Result<Option<Id>, ApiError> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let bad = || {
        unprocessable(
            "space: задача общей идеи — в Knowledge или в решении этой базы, идеи решения — в её \
             пространстве",
        )
    };
    let r = SpaceRef::parse(raw).map_err(|_| bad())?;
    match super::scope::find_space(db, &r).await {
        Ok(s) => Ok(Some(s.id)),
        Err(ApiError::NotFound) => Err(bad()),
        Err(e) => Err(e),
    }
}

/// Аудит операции идеи (без текста).
async fn audit(
    st: &AppState,
    p: &Principal,
    client: &Client,
    sc: &EntryScope,
    action: &str,
    extra: Value,
) -> Result<(), ApiError> {
    let mut details = audit_details(sc.space.id, EntryKind::Idea);
    if let (Value::Object(d), Value::Object(x)) = (&mut details, extra) {
        d.extend(x);
    }
    audit_node(
        st,
        p,
        client,
        sc.user.id,
        sc.owner,
        action,
        sc.entry.id,
        details,
    )
    .await
}

/// Запись с маскированием секретов для ответа.
fn entry_value(e: &Entry) -> Value {
    let mut v = json!(e);
    super::secrets::attach_entry(&mut v, e);
    v
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ProposeReq {
    text: Option<String>,
    space: Option<String>,
    message: Option<String>,
}

/// `POST /entries/{id}/propose`.
pub async fn propose(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let sc = idea_scope(&st, &p, &headers, &id).await?;
    if is_session(&p) {
        return Err(unprocessable(
            "предложение — действие агента: человек решает идею сам (decide)",
        ));
    }
    let req: ProposeReq = if matches!(&body, Ok(b) if b.is_empty()) {
        ProposeReq::default()
    } else {
        json_body(body)?
    };
    let space = task_space(&sc.db, req.space.as_deref()).await?;
    let solution = request_solution(&sc.db, &headers).await?;
    let attrib = attribution(&p, &headers, req.message)?;
    let (idea, question) =
        propose_idea(&sc.db, sc.entry.id, req.text, space, solution, &attrib).await?;
    // повтор при открытом вопросе — без новой ревизии и без аудита
    if idea.rev != sc.entry.rev {
        st.index.notify();
        audit(
            &st,
            &p,
            &client,
            &sc,
            "entry.idea.propose",
            json!({"question": question.id}),
        )
        .await?;
    }
    Ok(Json(json!({
        "entry": entry_value(&idea),
        "question": entry_value(&question),
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecideReq {
    decision: String,
    horizon: Option<String>,
    reason: Option<String>,
    by: Option<AnswerBy>,
    channel: Option<Channel>,
    space: Option<String>,
    message: Option<String>,
}

/// `horizon` тела: дата `YYYY-MM-DD` (иначе 422).
pub(crate) fn parse_horizon(raw: Option<&str>) -> Result<Option<Date>, ApiError> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<Date>()
                .map_err(|_| unprocessable("horizon: дата YYYY-MM-DD"))
        })
        .transpose()
}

/// `horizon` отсрочки по умолчанию: сегодня (UTC) + `[ideas] defer_days`.
pub(crate) fn default_horizon(st: &AppState) -> Result<Date, ApiError> {
    zam_store::time::utc_today()
        .checked_add(Span::new().days(i64::from(st.cfg.ideas.defer_days)))
        .map_err(|e| ApiError::Internal(format!("horizon по умолчанию: {e}")))
}

/// `channel` и `by` решения или ответа человека по правилам `answer` (§7.9): у сессии WebUI по
/// умолчанию `webui`/`human`, `by: agent` — 422; у токена `channel` — `chat|cli` (`webui` — 422), `by` обязателен
/// (нет — 400 `by-required`).
pub(crate) fn by_channel(
    p: &Principal,
    by: Option<AnswerBy>,
    channel: Option<Channel>,
) -> Result<(AnswerBy, Channel), ApiError> {
    let session = is_session(p);
    let channel = channel.unwrap_or(if session {
        Channel::Webui
    } else {
        Channel::Chat
    });
    if !session && channel == Channel::Webui {
        return Err(unprocessable(
            "channel: webui — ответ из WebUI; агент отвечает с channel chat или cli",
        ));
    }
    let by = match by {
        // в WebUI отвечает и решает человек: оформиться агентом сессия не может
        Some(AnswerBy::Agent) if session => {
            return Err(unprocessable(
                "by: agent — у агента; в WebUI отвечает и решает человек (by: human)",
            ));
        }
        Some(b) => b,
        None if session => AnswerBy::Human,
        None => return Err(ApiError::ByRequired),
    };
    Ok((by, channel))
}

/// Кто решает (§5): `by: agent` — только `decline` идеи «от агента»; иначе 403
/// `idea-human-decision`. Единственное место этого правила.
fn check_decider(by: AnswerBy, decision: IdeaDecision, e: &Entry) -> Result<(), ApiError> {
    if by == AnswerBy::Agent
        && !(decision == IdeaDecision::Decline && origin_of(e) == IdeaOrigin::Agent)
    {
        return Err(ApiError::IdeaHumanDecision);
    }
    Ok(())
}

/// `POST /entries/{id}/decide`.
pub async fn decide(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let sc = idea_scope(&st, &p, &headers, &id).await?;
    let req: DecideReq = json_body(body)?;
    let decision = IdeaDecision::parse(req.decision.trim())
        .ok_or_else(|| unprocessable("decision: accept, defer, decline или reopen"))?;
    let (by, channel) = by_channel(&p, req.by, req.channel)?;
    check_decider(by, decision, &sc.entry)?;
    let horizon = parse_horizon(req.horizon.as_deref())?;
    let space = task_space(&sc.db, req.space.as_deref()).await?;
    if space.is_some() && decision != IdeaDecision::Accept {
        return Err(unprocessable("space: только у decision accept"));
    }
    let request_solution = request_solution(&sc.db, &headers).await?;
    let attrib = attribution(&p, &headers, req.message)?;
    let d = decide_idea(
        &sc.db,
        sc.entry.id,
        NewDecision {
            decision,
            horizon,
            reason: req.reason,
            by,
            channel,
            task_space: space,
            request_solution,
            defer_days: st.cfg.ideas.defer_days,
            // права «кто решает» проверены по этому происхождению; сверка — в транзакции
            expected_origin: Some(origin_of(&sc.entry)),
        },
        &attrib,
    )
    .await?;
    st.index.notify();
    let mut extra = json!({
        "decision": decision.as_str(),
        "by": by.as_str(),
        "channel": channel.as_str(),
    });
    if let Some(t) = &d.task {
        extra["task"] = json!(t.id);
    }
    if let Some(q) = &d.question {
        extra["question"] = json!(q.id);
    }
    audit(&st, &p, &client, &sc, "entry.idea.decide", extra).await?;
    let mut out = json!({ "entry": entry_value(&d.idea) });
    if let Some(t) = &d.task {
        out["task"] = entry_value(t);
    }
    if let Some(q) = &d.question {
        out["question"] = entry_value(q);
    }
    Ok(Json(out))
}

/// `POST /entries/{id}/adopt` — «принять как свою»: только человек в WebUI, токену 403.
pub async fn adopt(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let sc = idea_scope(&st, &p, &headers, &id).await?;
    let user = match &p {
        Principal::User { user_id, .. } => *user_id,
        Principal::Admin { .. } => st.admin_id,
        Principal::Token(_) => {
            return Err(ApiError::Denied(
                "«принять как свою» — действие человека в WebUI".into(),
            ));
        }
    };
    let attrib = attribution(&p, &headers, None)?;
    let e = adopt_idea(&sc.db, sc.entry.id, user, &attrib).await?;
    st.index.notify();
    audit(&st, &p, &client, &sc, "entry.idea.adopt", json!({})).await?;
    Ok(Json(super::insights::entry_json(&sc.db, &e).await?))
}

/// Статус идеи меняют только операции этого модуля (§5): `PATCH status` — 422.
pub(crate) fn status_patch_error() -> ApiError {
    unprocessable("статус идеи меняется операциями propose, decide, adopt")
}
