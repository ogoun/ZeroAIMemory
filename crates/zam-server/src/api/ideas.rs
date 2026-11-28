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
//! - `POST /entries/{id}/adopt {by?, channel?}` → 200 `Entry`: «принять как свою» — решение
//!   человека. Сессия WebUI — как раньше (тело не читается); с M7.6 (§9.5 п. 4, признак
//!   `idea_adopt_token`) и токен: `by: human` — агент передаёт слова человека (как `decide` и
//!   ответ на вопрос), без `by` — 400 `by-required`, `by: agent` — 403 `idea-human-decision`
//!   **[решение координатора 162]**; сервер до 0.0.19 токену отвечает 403;
//! - `POST /entries/{id}/review {outcome, summary, missing?, basis?, similar?, task?, space?,
//!   message?}` → 200 `{entry, task?, replay}` (M6.5 §5, [`zam_store::review_idea`]): разбор —
//!   действие агента (сессии WebUI 422), только идеи решения (Knowledge 422), только в режиме
//!   `ideas.mode = review` владельца базы (иначе 409 `idea-mode`); повтор — по заголовку
//!   `Idempotency-Key`, как у записи;
//! - режим идей в `propose`: идея `new` решения в режиме `review` — 409 `idea-mode` (её
//!   разбирает агент), прочие случаи M5.7 (`deferred`, `accepted` со снятой задачей, Knowledge) —
//!   как раньше (M6.5 §7);
//! - права правки и корзины идей человека ([`deny_token_on_human_idea`]), `origin` при создании
//!   ([`create_data`]), связанные записи в `GET /entries/{id}` ([`attach_idea`]) и имена
//!   пользователей разборов ([`attach_reviewers`]).
//!
//! Аудит в `system.db` без текста: `entry.idea.propose`, `entry.idea.decide {decision}`,
//! `entry.idea.adopt {by, channel}`, `entry.idea.review {outcome, task?}`.
use std::collections::HashMap;

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
    AnswerBy, Channel, Db, Entry, EntryKind, EntryStatus, IdeaData, IdeaDecision, IdeaError,
    IdeaOrigin, IdeaPriority, NewDecision, NewReview, ReviewOutcome, ReviewTask, SpaceKind,
    StoreError, adopt_idea, decide_idea, get_entry, propose_idea, review_idea,
};

use super::entries::{audit_details, idempotency_key, unprocessable};
use super::json_body;
use super::nodes::audit_node;
use super::scope::{EntryScope, Mode, SpaceRef, deny_report_mutation, entry_scope, live_entry};
use super::user_settings::ideas_mode;
use crate::app::AppState;
use crate::attrib::attribution;
use crate::auth::{Client, Principal};
use crate::config::IdeasMode;
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
            // поля разбора и привязка к не открытой задаче (M6.5 §5) — 422
            IdeaError::ReviewInput(m) => unprocessable(m),
            e @ IdeaError::ReviewTaskNotOpen => unprocessable(e.to_string()),
            IdeaError::TaskRevived { question } => {
                let mut extra = Map::new();
                extra.insert("code".into(), json!("task-revived"));
                extra.insert("question".into(), json!(question));
                ApiError::Conflict {
                    detail: e_text_task_revived(question),
                    extra,
                }
            }
            // ошибки проверки тела операции идеи (`text`, `reason` длиннее предела) — 422
            IdeaError::Store(StoreError::BadInput(m)) => unprocessable(m),
            IdeaError::Store(e) => e.into(),
        }
    }
}

/// Текст 409 «задача возобновлена» у `propose` (тот же, что у ошибки хранилища).
fn e_text_task_revived(question: Id) -> String {
    IdeaError::TaskRevived { question }.to_string()
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
        o.insert("task".into(), task_value(t.id, t.entry.as_ref()));
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

/// Имена пользователей разборов идеи `e` — подпись «чей агент разбирал» в карточке WebUI (M6.5
/// §7 «Общие решения»; финальное ревью ветки, M4): поле ответа `GET /entries/{id}` `reviewers
/// {<id пользователя>: <имя>}` по `by_user` из `data.review` и `data.reviews`. Только в выдаче —
/// в `data` не хранится; пользователя нет — его id в поле нет; у идеи без разборов поля нет.
pub async fn attach_reviewers(st: &AppState, v: &mut Value, e: &Entry) -> Result<(), ApiError> {
    if e.kind != EntryKind::Idea {
        return Ok(());
    }
    let d = IdeaData::from_stored(&e.data);
    let mut ids: Vec<Id> = Vec::new();
    for r in d.review.iter().chain(&d.reviews) {
        if !ids.contains(&r.by_user) {
            ids.push(r.by_user);
        }
    }
    if ids.is_empty() {
        return Ok(());
    }
    let mut names = Map::new();
    for id in ids {
        if let Some(u) = st.system.get_user(id).await? {
            names.insert(id.to_string(), json!(u.name));
        }
    }
    if let Value::Object(o) = v {
        o.insert("reviewers".into(), Value::Object(names));
    }
    Ok(())
}

/// `task` идеи: `{id, status, title}`, удалённая или несуществующая — `{id, deleted: true}`.
fn task_value(id: Id, task: Option<&Entry>) -> Value {
    match task {
        Some(x) => json!({"id": id, "status": x.status, "title": x.title}),
        None => json!({"id": id, "deleted": true}),
    }
}

/// Задачи идей ленты (`data.task_id`) одним запросом: id → живая задача (M6.5 §9: строка решения
/// «В план → k:<id> «…»» ленты WebUI).
pub async fn idea_tasks_many(db: &Db, items: &[Entry]) -> Result<HashMap<Id, Entry>, ApiError> {
    let ids: Vec<Id> = items
        .iter()
        .filter(|e| e.kind == EntryKind::Idea)
        .filter_map(|e| IdeaData::from_stored(&e.data).task_id)
        .collect();
    Ok(zam_store::live_entries_by_ids(db, &ids).await?)
}

/// У идеи ленты — `task` в виде [`attach_idea`] по задачам [`idea_tasks_many`].
pub fn attach_idea_task(v: &mut Value, e: &Entry, tasks: &HashMap<Id, Entry>) {
    if e.kind != EntryKind::Idea {
        return;
    }
    let Some(id) = IdeaData::from_stored(&e.data).task_id else {
        return;
    };
    if let Value::Object(o) = v {
        o.insert("task".into(), task_value(id, tasks.get(&id)));
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

/// Запись для ответа: с находками секретов (`secrets_detected`, без значений). Маска текста
/// агенту — у ответа целиком ([`shown`]).
fn entry_value(e: &Entry) -> Value {
    let mut v = json!(e);
    super::secrets::attach_entry(&mut v, e);
    v
}

/// Ответ операции идеи зрителю `p`: агенту по токену — записи `entry`, `question`, `task` с маской
/// секретов (задача 5 M7.1, [`super::agent_view::ENTRY_FIELDS`]; длинные — в пуле, круг правок 2).
async fn shown(p: &Principal, out: Value) -> Result<Json<Value>, ApiError> {
    super::agent_view::view_entries_in(p, out, &["entry", "question", "task"])
        .await
        .map(Json)
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
    // M6.5 §7: новую идею человека в решении разбирает агент — двух противоречивых указаний у
    // него нет; `deferred`, `accepted` со снятой задачей и Knowledge — по M5.7, «от агента» —
    // прежняя ошибка хранилища
    if sc.space.kind == SpaceKind::Solution
        && sc.entry.status.unwrap_or(EntryStatus::New) == EntryStatus::New
        && origin_of(&sc.entry) == IdeaOrigin::Human
        && ideas_mode(&st, sc.owner).await? == IdeasMode::Review
    {
        return Err(ApiError::IdeaMode(
            "идею разбирает агент (ideas.mode = review): action=review, а не propose".into(),
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
        match propose_idea(&sc.db, sc.entry.id, req.text, space, solution, &attrib).await {
            Ok(v) => v,
            // вопрос «вернуть» устарел и записан (задача снова живая) — аудит записи, затем 409
            // (финальное ревью M5.7, Minor 2)
            Err(IdeaError::TaskRevived { question }) => {
                st.index.notify();
                audit(
                    &st,
                    &p,
                    &client,
                    &sc,
                    "entry.idea.propose",
                    json!({"question": question, "obsolete": "task-revived"}),
                )
                .await?;
                return Err(IdeaError::TaskRevived { question }.into());
            }
            Err(e) => return Err(e.into()),
        };
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
    shown(
        &p,
        json!({
            "entry": entry_value(&idea),
            "question": entry_value(&question),
        }),
    )
    .await
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
    shown(&p, out).await
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AdoptReq {
    by: Option<AnswerBy>,
    channel: Option<Channel>,
    message: Option<String>,
}

/// `POST /entries/{id}/adopt` — «принять как свою» (см. модуль): сессия WebUI — тело не читается;
/// токен — `by: human` по правилам [`by_channel`], `by: agent` — 403 `idea-human-decision`.
pub async fn adopt(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let sc = idea_scope(&st, &p, &headers, &id).await?;
    let (user, req) = match &p {
        Principal::User { user_id, .. } => (*user_id, AdoptReq::default()),
        Principal::Admin { .. } => (st.admin_id, AdoptReq::default()),
        Principal::Token(t) => {
            let req: AdoptReq = match &body {
                Ok(b) if b.iter().all(u8::is_ascii_whitespace) => AdoptReq::default(),
                _ => json_body(body)?,
            };
            (t.user_id, req)
        }
    };
    let (by, channel) = by_channel(&p, req.by, req.channel)?;
    // «своей» идею делает только человек: агент от своего имени — 403 (как у `decide`)
    if by == AnswerBy::Agent {
        return Err(ApiError::IdeaHumanDecision);
    }
    let attrib = attribution(&p, &headers, req.message)?;
    let e = adopt_idea(&sc.db, sc.entry.id, user, &attrib).await?;
    st.index.notify();
    let extra = json!({"by": by.as_str(), "channel": channel.as_str()});
    audit(&st, &p, &client, &sc, "entry.idea.adopt", extra).await?;
    Ok(Json(
        super::insights::entry_json(&p, &sc.db, &e, sc.role).await?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewReq {
    /// `plan | return | decline | done`; нет или иное — 422.
    outcome: Option<String>,
    /// Разбор; нет — 422 хранилища `summary: …` (поле названо, как у прочих полей разбора).
    summary: Option<String>,
    missing: Option<String>,
    basis: Option<Vec<String>>,
    similar: Option<Vec<String>>,
    task: Option<ReviewTaskReq>,
    /// Пространство идеи (GUID решения), если клиент его называет; другое — 422.
    space: Option<String>,
    message: Option<String>,
}

/// `task` разбора `plan`: новая задача `{title?, body?, priority?}` или уже открытая задача
/// плана `{id: "k:<id>"}`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewTaskReq {
    id: Option<String>,
    title: Option<String>,
    body: Option<String>,
    priority: Option<String>,
}

impl ReviewTaskReq {
    /// Задача плана хранилища; `id` вместе с полями новой задачи, неверный `id` или
    /// `priority` — 422.
    fn parse(self) -> Result<ReviewTask, ApiError> {
        if let Some(raw) = self.id {
            if self.title.is_some() || self.body.is_some() || self.priority.is_some() {
                return Err(unprocessable(
                    "task: либо {id} уже открытой задачи плана, либо {title, body, priority} \
                     новой",
                ));
            }
            let raw = raw.trim();
            let id = raw
                .strip_prefix("k:")
                .unwrap_or(raw)
                .trim()
                .parse::<Id>()
                .map_err(|_| unprocessable("task.id: ссылка на задачу k:<id>"))?;
            return Ok(ReviewTask::Existing(id));
        }
        let priority = self
            .priority
            .as_deref()
            .map(|s| {
                IdeaPriority::parse(s.trim())
                    .ok_or_else(|| unprocessable("task.priority: low, normal или high"))
            })
            .transpose()?;
        Ok(ReviewTask::New {
            title: self.title,
            body: self.body,
            priority,
        })
    }
}

/// Текст разбора `field` — не больше `limits.note_max_bytes` байт: тот же 413
/// `/problems/too-large`, что у тела записи ([`super::entries::check_size`]), `detail` — с
/// именем поля.
fn review_size(st: &AppState, field: &str, text: Option<&str>) -> Result<(), ApiError> {
    let max = st.cfg.limits.note_max_bytes;
    match text {
        Some(t) if t.len() > max => Err(ApiError::PayloadTooLarge(format!(
            "{field}: {} байт, предел {max}",
            t.len()
        ))),
        _ => Ok(()),
    }
}

/// `space` тела разбора: только пространство самой идеи (задача плана заводится в нём).
async fn review_space(sc: &EntryScope, raw: Option<&str>) -> Result<(), ApiError> {
    let bad = || unprocessable("space: разбор — в пространстве идеи, задача плана — там же");
    match task_space(&sc.db, raw).await {
        Ok(None) => Ok(()),
        Ok(Some(s)) if s == sc.space.id => Ok(()),
        Ok(Some(_)) | Err(ApiError::Unprocessable(_)) => Err(bad()),
        Err(e) => Err(e),
    }
}

/// `POST /entries/{id}/review` (M6.5 §5): разбор новой идеи человека агентом.
pub async fn review(
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
            "разбор — действие агента: человек решает идею сам (decide) или дописывает её",
        ));
    }
    if sc.space.kind != SpaceKind::Solution {
        return Err(unprocessable(
            "разбор — у идей решения: у общих идей Knowledge нет плана и ТЗ одного проекта, они \
             остаются по M5.7 (propose, decide)",
        ));
    }
    // режим владельца базы: в общем решении — его, а не участника (§7 «Общие решения»)
    if ideas_mode(&st, sc.owner).await? == IdeasMode::Propose {
        return Err(ApiError::IdeaMode(
            "разбор выключен (ideas.mode = propose): предложи идею человеку — action=propose"
                .into(),
        ));
    }
    let req: ReviewReq = json_body(body)?;
    // тексты разбора — под пределом обычной записи, как тело записи задачи (финальное ревью
    // M6.5, M5); заголовок задачи плана ограничивает хранилище (500 символов, как у задачи)
    review_size(&st, "summary", req.summary.as_deref())?;
    review_size(&st, "missing", req.missing.as_deref())?;
    review_size(
        &st,
        "task.body",
        req.task.as_ref().and_then(|t| t.body.as_deref()),
    )?;
    let outcome = req
        .outcome
        .as_deref()
        .map(str::trim)
        .and_then(ReviewOutcome::parse)
        .ok_or_else(|| unprocessable("outcome: plan, return, decline или done"))?;
    // новая задача плана по токену: базы нет — метка `‹secret›` значения не хранит (решение
    // координатора по задаче 6 M7.1)
    let task_marks = req.task.as_ref().map_or(0, |t| {
        [&t.title, &t.body]
            .into_iter()
            .flatten()
            .map(|s| super::secret_restore::marks_in(s))
            .sum::<usize>()
    });
    let task = req
        .task
        .map(ReviewTaskReq::parse)
        .transpose()?
        .unwrap_or(ReviewTask::None);
    review_space(&sc, req.space.as_deref()).await?;
    // ключ повтора — на один вызов разбора, из заголовка, как у записи (ruling 5)
    let key = idempotency_key(&headers)?;
    let attrib = attribution(&p, &headers, req.message)?;
    let r = review_idea(
        &sc.db,
        sc.entry.id,
        NewReview {
            outcome,
            summary: req.summary.unwrap_or_default(),
            missing: req.missing,
            basis: req.basis.unwrap_or_default(),
            similar: req.similar.unwrap_or_default(),
            task,
            // агент разбирает от имени владельца токена (участника общего решения — тоже)
            by_user: sc.user.id,
            session: None,
            key,
            min_chars: st.cfg.ideas.review_min_chars as usize,
        },
        &attrib,
    )
    .await?;
    // повтор по ключу — без записи и без аудита
    if !r.replay {
        st.index.notify();
        let mut extra = json!({ "outcome": outcome.as_str() });
        if let Some(t) = &r.task {
            extra["task"] = json!(t.id);
        }
        audit(&st, &p, &client, &sc, "entry.idea.review", extra).await?;
    }
    let mut out = json!({ "entry": entry_value(&r.idea), "replay": r.replay });
    if let Some(t) = &r.task {
        out["task"] = entry_value(t);
    }
    let Json(mut out) = shown(&p, out).await?;
    if !r.replay && r.task.is_some() && super::agent_view::masks(&p) {
        super::secret_restore::attach_kept(&mut out, task_marks);
    }
    Ok(Json(out))
}

/// Статус идеи меняют только операции этого модуля (§5): `PATCH status` — 422.
pub(crate) fn status_patch_error() -> ApiError {
    unprocessable("статус идеи меняется операциями propose, decide, adopt, review")
}
