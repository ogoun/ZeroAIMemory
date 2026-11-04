//! Журнал «Идеи» (спецификация M5.7 §4–§6): записи `idea` — замысел человека в `title`/`body`,
//! решение по нему в `status` и `data`.
//!
//! - [`IdeaData`] — `data` идеи: `priority`, `horizon`, `links` ведёт клиент; `origin` задаётся
//!   при создании (меняет его только [`adopt_idea`]); `task_id`, `prev_task_ids`,
//!   `question_id`, `proposed_at`, `decided`, `adopted_at`, `adopted_by`, `done_at` — сервер;
//! - [`check_idea_transition`] — таблица переходов §5 (единственное место правил, в том числе
//!   выхода из `accepted` при снятой или удалённой задаче — ждёт ответа владельца);
//! - операции — по одной транзакции писателя: [`propose_idea`] (вопрос с вариантами
//!   [`IDEA_OPTIONS`]), [`decide_idea`] (переход и задача при `accept`), [`adopt_idea`];
//!   общий шаг перехода [`apply_decision_tx`] вызывает и ответ на вопрос-предложение
//!   ([`crate::answer_question`]). Логическая ошибка после записи откатывает всё ([`atomic`]);
//! - хуки транзакций журналов: смена статуса задачи ([`on_task_status_tx`]), устаревание и
//!   удаление вопроса-предложения ([`on_proposal_closed_tx`]), удаление идеи
//!   ([`on_entry_deleted_tx`]);
//! - отбор для проактивности: [`proactive_ideas`] и предикат [`idea_candidate`] (§6).

use std::collections::HashSet;

use jiff::civil::Date;
use jiff::{Span, Timestamp};
use rusqlite::{Connection, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use zam_core::{Id, TargetKind, TargetRef};

use crate::entries::{
    ENTRY_COLS, Entry, EntryKind, EntryStatus, Maker, NewEntry, REASON_MAX_CHARS, TITLE_MAX_CHARS,
    ensure_not_report, entry_row, insert_entry_as, load_entry, revise_with,
};
use crate::graph::EdgeType;
use crate::nodes::Attribution;
use crate::questions::{
    AnswerBy, Channel, NormalizedAnswer, QuestionData, QuestionError, answer_tx, in_list,
};
use crate::tasks::{TASK_LINK_MAX_CHARS, TASK_LINKS_MAX, TaskData, links_field};
use crate::time::{fmt_ts, parse_ts, ser_ts, utc_today};
use crate::user_db::SpaceKind;
use crate::{Db, StoreError};

/// Ссылок в `data.links` идеи и длина ссылки — как у задачи.
pub const IDEA_LINKS_MAX: usize = TASK_LINKS_MAX;
pub const IDEA_LINK_MAX_CHARS: usize = TASK_LINK_MAX_CHARS;
/// Предел `text` предложения (почему сейчас), символов.
pub const PROPOSE_TEXT_MAX_CHARS: usize = 2000;
/// `horizon` отсрочки по умолчанию, дней от сегодня (`ideas.defer_days` сервера).
pub const IDEA_DEFER_DAYS_DEFAULT: u32 = 30;
/// Варианты вопроса-предложения `(ключ, текст)`; фиксирует сервер (§5.1).
pub const IDEA_OPTIONS: [(&str, &str); 3] = [
    ("accept", "начать"),
    ("defer", "отложить"),
    ("decline", "отказаться"),
];

/// Приоритет идеи.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdeaPriority {
    Low,
    Normal,
    High,
}

impl IdeaPriority {
    pub fn as_str(self) -> &'static str {
        match self {
            IdeaPriority::Low => "low",
            IdeaPriority::Normal => "normal",
            IdeaPriority::High => "high",
        }
    }

    pub fn parse(s: &str) -> Option<IdeaPriority> {
        match s {
            "low" => Some(IdeaPriority::Low),
            "normal" => Some(IdeaPriority::Normal),
            "high" => Some(IdeaPriority::High),
            _ => None,
        }
    }

    /// Порядок отбора: `high` 2, `normal` 1, `low` 0.
    pub fn rank(self) -> u8 {
        match self {
            IdeaPriority::Low => 0,
            IdeaPriority::Normal => 1,
            IdeaPriority::High => 2,
        }
    }
}

/// Происхождение идеи: человек (в том числе записанная агентом по его словам) или агент сам.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdeaOrigin {
    Human,
    Agent,
}

impl IdeaOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            IdeaOrigin::Human => "human",
            IdeaOrigin::Agent => "agent",
        }
    }

    pub fn parse(s: &str) -> Option<IdeaOrigin> {
        match s {
            "human" => Some(IdeaOrigin::Human),
            "agent" => Some(IdeaOrigin::Agent),
            _ => None,
        }
    }
}

/// Решение по идее (`decide`, §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdeaDecision {
    Accept,
    Defer,
    Decline,
    Reopen,
}

impl IdeaDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            IdeaDecision::Accept => "accept",
            IdeaDecision::Defer => "defer",
            IdeaDecision::Decline => "decline",
            IdeaDecision::Reopen => "reopen",
        }
    }

    pub fn parse(s: &str) -> Option<IdeaDecision> {
        match s {
            "accept" => Some(IdeaDecision::Accept),
            "defer" => Some(IdeaDecision::Defer),
            "decline" => Some(IdeaDecision::Decline),
            "reopen" => Some(IdeaDecision::Reopen),
            _ => None,
        }
    }

    /// Ключ варианта вопроса-предложения ([`IDEA_OPTIONS`]); у `reopen` варианта нет.
    pub fn option_key(self) -> Option<&'static str> {
        match self {
            IdeaDecision::Accept | IdeaDecision::Defer | IdeaDecision::Decline => {
                Some(self.as_str())
            }
            IdeaDecision::Reopen => None,
        }
    }

    /// Решение по ключу варианта вопроса-предложения; прочее — `None`.
    pub fn from_option(key: &str) -> Option<IdeaDecision> {
        IdeaDecision::parse(key).filter(|d| d.option_key().is_some())
    }
}

/// Последнее решение по идее (`data.decided`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decided {
    pub decision: IdeaDecision,
    pub by: AnswerBy,
    pub channel: Channel,
    #[serde(serialize_with = "ser_ts")]
    pub at: Timestamp,
}

/// Разобранная `data` идеи.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeaData {
    pub priority: IdeaPriority,
    /// «Не раньше» (UTC): до этой даты `deferred` не предлагается.
    pub horizon: Option<Date>,
    pub links: Vec<String>,
    pub origin: IdeaOrigin,
    /// Задача, созданная «начать».
    pub task_id: Option<Id>,
    /// Прежние задачи идеи (`reopen` из `accepted` при снятой или удалённой задаче).
    pub prev_task_ids: Vec<Id>,
    /// Последний вопрос-предложение.
    pub question_id: Option<Id>,
    pub proposed_at: Option<Timestamp>,
    pub decided: Option<Decided>,
    pub adopted_at: Option<Timestamp>,
    pub adopted_by: Option<Id>,
    pub done_at: Option<Timestamp>,
}

/// Умолчания разбора: `normal` и `agent` — повреждённое происхождение никогда не делает идею
/// предлагаемой (анти-цель §3: агент не начинает без согласия человека)
/// **[решение исполнителя]**.
impl Default for IdeaData {
    fn default() -> IdeaData {
        IdeaData {
            priority: IdeaPriority::Normal,
            horizon: None,
            links: Vec::new(),
            origin: IdeaOrigin::Agent,
            task_id: None,
            prev_task_ids: Vec::new(),
            question_id: None,
            proposed_at: None,
            decided: None,
            adopted_at: None,
            adopted_by: None,
            done_at: None,
        }
    }
}

/// Поля, которые ведёт клиент.
const CLIENT_KEYS: [&str; 3] = ["priority", "horizon", "links"];
/// Поля, которые ведёт сервер.
const SERVER_KEYS: [&str; 8] = [
    "task_id",
    "prev_task_ids",
    "question_id",
    "proposed_at",
    "decided",
    "adopted_at",
    "adopted_by",
    "done_at",
];
/// Подсказка обязательного `data.origin` (по образцу обязательного `by` ответа, §4).
pub const IDEA_ORIGIN_REQUIRED: &str = "data.origin: укажите human — идея человека (записана по его \
                               словам) или agent — своя";

fn priority_field(v: &Value) -> Result<IdeaPriority, String> {
    match v {
        Value::Null => Ok(IdeaPriority::Normal),
        Value::String(s) => IdeaPriority::parse(s.trim())
            .ok_or_else(|| "data.priority: low, normal или high".into()),
        _ => Err("data.priority: low, normal или high".into()),
    }
}

fn horizon_field(v: &Value) -> Result<Option<Date>, String> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => s
            .trim()
            .parse::<Date>()
            .map(Some)
            .map_err(|_| "data.horizon: дата YYYY-MM-DD".into()),
        _ => Err("data.horizon: дата YYYY-MM-DD".into()),
    }
}

/// `horizon` — строго позже сегодняшней даты (UTC): одно правило у `defer` и правки `data`.
pub(crate) fn check_horizon(h: Date, today: Date) -> Result<(), String> {
    if h <= today {
        return Err(format!(
            "horizon: не раньше завтрашней даты (UTC), а не {h}"
        ));
    }
    Ok(())
}

/// `links` из базы: битые элементы (не строка, пустая, длиннее предела) пропускаются по
/// одному, остальные сохраняются.
fn stored_links(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in v.as_array().into_iter().flatten().filter_map(Value::as_str) {
        let s = s.trim();
        if !s.is_empty() && s.chars().count() <= IDEA_LINK_MAX_CHARS && !out.iter().any(|o| o == s)
        {
            out.push(s.to_string());
        }
    }
    out
}

fn id_of(v: &Value) -> Option<Id> {
    v.as_str()?.parse().ok()
}

impl IdeaData {
    /// `data` из базы; повреждённые поля — умолчания (запись всегда читается).
    pub fn from_stored(data: &Value) -> IdeaData {
        let get = |k: &str| data.get(k).filter(|v| !v.is_null());
        let ts = |k: &str| get(k).and_then(|v| v.as_str()?.parse::<Timestamp>().ok());
        IdeaData {
            priority: get("priority")
                .and_then(|v| priority_field(v).ok())
                .unwrap_or(IdeaPriority::Normal),
            horizon: get("horizon").and_then(|v| horizon_field(v).ok().flatten()),
            links: get("links").map(stored_links).unwrap_or_default(),
            origin: get("origin")
                .and_then(Value::as_str)
                .and_then(IdeaOrigin::parse)
                .unwrap_or(IdeaOrigin::Agent),
            task_id: get("task_id").and_then(id_of),
            prev_task_ids: get("prev_task_ids")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(id_of).collect())
                .unwrap_or_default(),
            question_id: get("question_id").and_then(id_of),
            proposed_at: ts("proposed_at"),
            decided: get("decided").and_then(|v| serde_json::from_value(v.clone()).ok()),
            adopted_at: ts("adopted_at"),
            adopted_by: get("adopted_by").and_then(id_of),
            done_at: ts("done_at"),
        }
    }

    /// Применяет клиентский патч `data`: `priority`, `horizon`, `links`; `null` снимает поле
    /// (`priority` — к `normal`). `origin` (только при создании) и поля сервера — ошибка
    /// `data.<поле>: …` (422). Ошибка не оставляет частичного результата. `horizon` — строго
    /// позже сегодняшней даты UTC ([`check_horizon`]).
    pub fn apply_patch(&mut self, patch: &Value) -> Result<(), String> {
        self.patch_at(patch, Some(utc_today()))
    }

    /// [`IdeaData::apply_patch`] с датой проверки `horizon`: `None` — без проверки даты
    /// (нормализация уже записанного запроса, не зависит от часов).
    fn patch_at(&mut self, patch: &Value, today: Option<Date>) -> Result<(), String> {
        let o = patch
            .as_object()
            .ok_or_else(|| "data должно быть JSON-объектом".to_string())?;
        for k in o.keys() {
            if k == "origin" {
                return Err(
                    "data.origin: задаётся только при создании; идею агента человек \
                            принимает как свою (adopt)"
                        .into(),
                );
            }
            if SERVER_KEYS.contains(&k.as_str()) {
                return Err(format!("data.{k}: поле ведёт сервер"));
            }
            if !CLIENT_KEYS.contains(&k.as_str()) {
                return Err(format!("data.{k}: неизвестное поле"));
            }
        }
        let mut next = self.clone();
        for (k, v) in o {
            match k.as_str() {
                "priority" => next.priority = priority_field(v)?,
                "horizon" => {
                    next.horizon = horizon_field(v)?;
                    if let (Some(h), Some(t)) = (next.horizon, today) {
                        check_horizon(h, t).map_err(|e| format!("data.{e}"))?;
                    }
                }
                "links" => next.links = links_field(v)?,
                _ => unreachable!("ключи проверены выше"),
            }
        }
        *self = next;
        Ok(())
    }

    /// Нормализованная `data` для записи: `priority` и `origin` всегда (по ним фильтруют
    /// лента и отбор), прочие — если заданы.
    pub fn to_value(&self) -> Value {
        let mut o = Map::new();
        o.insert("priority".into(), json!(self.priority.as_str()));
        o.insert("origin".into(), json!(self.origin.as_str()));
        if let Some(h) = self.horizon {
            o.insert("horizon".into(), json!(h.to_string()));
        }
        if !self.links.is_empty() {
            o.insert("links".into(), json!(self.links));
        }
        if let Some(t) = self.task_id {
            o.insert("task_id".into(), json!(t.to_string()));
        }
        if !self.prev_task_ids.is_empty() {
            let ids: Vec<String> = self.prev_task_ids.iter().map(Id::to_string).collect();
            o.insert("prev_task_ids".into(), json!(ids));
        }
        if let Some(q) = self.question_id {
            o.insert("question_id".into(), json!(q.to_string()));
        }
        if let Some(t) = self.proposed_at {
            o.insert("proposed_at".into(), json!(fmt_ts(t)));
        }
        if let Some(d) = &self.decided {
            o.insert("decided".into(), json!(d));
        }
        if let Some(t) = self.adopted_at {
            o.insert("adopted_at".into(), json!(fmt_ts(t)));
        }
        if let Some(u) = self.adopted_by {
            o.insert("adopted_by".into(), json!(u.to_string()));
        }
        if let Some(t) = self.done_at {
            o.insert("done_at".into(), json!(fmt_ts(t)));
        }
        Value::Object(o)
    }
}

/// Новая идея: `origin` — уже решённое сервером происхождение (WebUI → `human`; токен — из
/// `data`). `data.origin`, если есть, должен совпадать с ним; поля сервера — ошибка.
pub fn check_idea(data: &Value, origin: IdeaOrigin) -> Result<IdeaData, String> {
    check_idea_at(data, origin, Some(utc_today()))
}

/// [`check_idea`] без проверки даты `horizon` (только структура): повтор по `Idempotency-Key`
/// после полуночи UTC — та же запись, а ошибки структуры — те же, что у первого запроса.
pub fn check_idea_shape(data: &Value, origin: IdeaOrigin) -> Result<IdeaData, String> {
    check_idea_at(data, origin, None)
}

/// [`check_idea`] с датой проверки `horizon` (`None` — без проверки даты).
fn check_idea_at(
    data: &Value,
    origin: IdeaOrigin,
    today: Option<Date>,
) -> Result<IdeaData, String> {
    let mut o = data
        .as_object()
        .cloned()
        .ok_or_else(|| "data должно быть JSON-объектом".to_string())?;
    match o.remove("origin") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) if IdeaOrigin::parse(s.trim()) == Some(origin) => {}
        Some(_) => {
            return Err(format!(
                "data.origin: у этой идеи происхождение {}",
                origin.as_str()
            ));
        }
    }
    let mut d = IdeaData {
        origin,
        ..IdeaData::default()
    };
    d.patch_at(&Value::Object(o), today)?;
    Ok(d)
}

/// `data` новой идеи из запроса записи: `data.origin` обязателен (`human | agent`).
pub(crate) fn idea_from_request(data: &Value) -> Result<IdeaData, String> {
    request_at(data, Some(utc_today()))
}

/// Нормализованная `data` запроса идеи без проверки даты `horizon`: сверка повтора по
/// `Idempotency-Key` не зависит от часов (повтор после полуночи UTC — та же запись).
pub(crate) fn idea_request_value(data: &Value) -> Result<Value, String> {
    request_at(data, None).map(|d| d.to_value())
}

fn request_at(data: &Value, today: Option<Date>) -> Result<IdeaData, String> {
    let origin = data
        .get("origin")
        .and_then(Value::as_str)
        .and_then(|s| IdeaOrigin::parse(s.trim()))
        .ok_or_else(|| IDEA_ORIGIN_REQUIRED.to_string())?;
    check_idea_at(data, origin, today)
}

/// Состояние задачи идеи для выхода из `accepted` (§5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Live,
    Dropped,
    Deleted,
    Done,
}

/// Можно ли выйти из `accepted` вручную (`reopen`, `decline`): задача снята (`dropped`),
/// удалена или её нет. Единственное место правила «задача снята или удалена» — ждёт ответа
/// владельца (журнал этапа, вопрос 9) **[решение составителя]**.
fn task_released(task: Option<TaskState>) -> bool {
    matches!(task, None | Some(TaskState::Dropped | TaskState::Deleted))
}

/// Допустимость перехода §5 (без прав «кто»). `Ok` — новый статус; `Err` — машинный код:
/// `transition-not-allowed`, `already-decided` (только `accept` из `accepted`|`done`),
/// `idea-agent-origin` (`accept` идеи «от агента»), `reason-required` (`reopen` без причины).
/// Повторный `defer` по `deferred` разрешён. `task` — состояние задачи `data.task_id`.
pub fn check_idea_transition(
    cur: EntryStatus,
    d: &IdeaData,
    task: Option<TaskState>,
    decision: IdeaDecision,
    reason: Option<&str>,
) -> Result<EntryStatus, &'static str> {
    use EntryStatus::*;
    const TNA: &str = "transition-not-allowed";
    let open = matches!(cur, New | Proposed | Deferred);
    let to = match decision {
        IdeaDecision::Accept => match cur {
            Accepted | Done => return Err("already-decided"),
            _ if !open => return Err(TNA),
            _ if d.origin == IdeaOrigin::Agent => return Err("idea-agent-origin"),
            _ => Accepted,
        },
        IdeaDecision::Defer if open => Deferred,
        IdeaDecision::Decline if open => Declined,
        IdeaDecision::Decline if cur == Accepted && task_released(task) => Declined,
        IdeaDecision::Reopen
            if matches!(cur, Declined | Deferred) || (cur == Accepted && task_released(task)) =>
        {
            New
        }
        _ => return Err(TNA),
    };
    if decision == IdeaDecision::Reopen && !reason.is_some_and(|r| !r.trim().is_empty()) {
        return Err("reason-required");
    }
    Ok(to)
}

/// Решение по идее, как его присылает клиент.
#[derive(Debug, Clone)]
pub struct NewDecision {
    pub decision: IdeaDecision,
    /// Только у `defer`: не раньше завтрашней даты (UTC); `None` — сегодня + `defer_days`.
    pub horizon: Option<Date>,
    /// Причина (в ревизию идеи): у `reopen` обязательна, у `decline` — по желанию.
    pub reason: Option<String>,
    pub by: AnswerBy,
    pub channel: Channel,
    /// Пространство задачи `accept`, явно (`space` запроса).
    pub task_space: Option<Id>,
    /// Решение запроса (`X-Zam-Solution`), если оно в этой базе.
    pub request_solution: Option<Id>,
    /// `horizon` отсрочки по умолчанию, дней.
    pub defer_days: u32,
    /// Происхождение идеи, по которому вызывающий проверил права «кто решает»; другое в
    /// транзакции — [`IdeaError::OriginChanged`] (идею приняли как свою между чтением и
    /// записью). `None` — без сверки.
    pub expected_origin: Option<IdeaOrigin>,
}

/// Итог `decide`: идея, задача `accept` и вопрос-предложение, отвеченный этим решением.
#[derive(Debug, Clone)]
pub struct Decision {
    pub idea: Entry,
    pub task: Option<Entry>,
    pub question: Option<Entry>,
}

/// Ошибки операций над идеей.
#[derive(Debug, thiserror::Error)]
pub enum IdeaError {
    /// Запись не идея: 422.
    #[error("запись не является идеей")]
    NotIdea,
    /// Идея «от агента»: ни `propose`, ни `accept` до «принять как свою» — 409
    /// `idea-agent-origin`.
    #[error("идея от агента: сначала человек принимает её как свою")]
    AgentOrigin,
    /// «Начать» уже было: 409 `already-decided` с задачей.
    #[error("по идее уже решено «начать»")]
    AlreadyDecided { task_id: Option<Id> },
    /// Идея уже человека: 409 `already-adopted`.
    #[error("идея уже принята как своя")]
    AlreadyAdopted,
    /// Переход не разрешён: 409 `transition-not-allowed`.
    #[error("переход идеи {} → {to} не разрешён", .from.as_str())]
    Transition { from: EntryStatus, to: &'static str },
    /// `reopen` без причины: 422 `reason-required`.
    #[error("reason: у reopen причина обязательна")]
    ReasonRequired,
    /// `horizon` вне правил: 422.
    #[error("{0}")]
    BadHorizon(String),
    /// Пространство задачи вне правил §5.2: 422.
    #[error("{0}")]
    TaskSpace(String),
    /// Идея удалена (ответ на её вопрос-предложение): 409.
    #[error("идея удалена")]
    IdeaDeleted,
    /// Происхождение идеи изменилось после проверки прав вызывающим: 409.
    #[error("происхождение идеи изменилось (принята как своя) — перечитайте её и повторите")]
    OriginChanged,
    /// Вопрос — не действующее предложение идеи (у неё другой `question_id`): 409.
    #[error("предложение устарело: у идеи другой вопрос-предложение или его нет")]
    StaleProposal,
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Ошибка по машинному коду [`check_idea_transition`].
fn transition_error(
    code: &'static str,
    from: EntryStatus,
    d: &IdeaData,
    decision: IdeaDecision,
) -> IdeaError {
    match code {
        "already-decided" => IdeaError::AlreadyDecided { task_id: d.task_id },
        "idea-agent-origin" => IdeaError::AgentOrigin,
        "reason-required" => IdeaError::ReasonRequired,
        _ => IdeaError::Transition {
            from,
            to: decision.as_str(),
        },
    }
}

/// Шаг транзакции «всё или ничего» (точка сохранения): и логическая ошибка `Ok(Err(_))`, и
/// ошибка хранилища `Err(_)` откатывают записи шага.
pub(crate) fn atomic<T, E>(
    tx: &Connection,
    f: impl FnOnce(&Connection) -> Result<Result<T, E>, StoreError>,
) -> Result<Result<T, E>, StoreError> {
    tx.execute_batch("SAVEPOINT zam_atomic")?;
    match f(tx) {
        Ok(Ok(v)) => {
            tx.execute_batch("RELEASE zam_atomic")?;
            Ok(Ok(v))
        }
        Ok(Err(e)) => {
            tx.execute_batch("ROLLBACK TO zam_atomic; RELEASE zam_atomic")?;
            Ok(Err(e))
        }
        Err(e) => {
            // шаг откатывается и здесь: вызывающий может продолжить транзакцию после ошибки;
            // сбой отката не заслоняет исходную ошибку
            let _ = tx.execute_batch("ROLLBACK TO zam_atomic; RELEASE zam_atomic");
            Err(e)
        }
    }
}

fn entry_ref(id: Id) -> TargetRef {
    TargetRef {
        kind: TargetKind::Entry,
        id,
    }
}

/// Живая идея: нет или удалена — `NotFound`, отчёт — `ReportImmutable`, другой вид — `NotIdea`.
fn live_idea(c: &Connection, id: Id) -> Result<Result<Entry, IdeaError>, StoreError> {
    let e = load_entry(c, id)?;
    if e.deleted_at.is_some() {
        return Err(StoreError::NotFound);
    }
    ensure_not_report(&e)?;
    if e.kind != EntryKind::Idea {
        return Ok(Err(IdeaError::NotIdea));
    }
    Ok(Ok(e))
}

/// Живая запись по id, если есть.
fn live_opt(c: &Connection, id: Id) -> Result<Option<Entry>, StoreError> {
    match load_entry(c, id) {
        Ok(e) if e.deleted_at.is_none() => Ok(Some(e)),
        Ok(_) | Err(StoreError::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Открытый живой вопрос-предложение идеи (`data.question_id`), если есть.
fn open_question(c: &Connection, d: &IdeaData) -> Result<Option<Entry>, StoreError> {
    let Some(qid) = d.question_id else {
        return Ok(None);
    };
    Ok(live_opt(c, qid)?
        .filter(|q| q.kind == EntryKind::Question && q.status == Some(EntryStatus::Open)))
}

/// Состояние задачи `data.task_id`; задачи нет в данных — `None`.
fn task_state(c: &Connection, d: &IdeaData) -> Result<Option<TaskState>, StoreError> {
    let Some(tid) = d.task_id else {
        return Ok(None);
    };
    Ok(Some(match live_opt(c, tid)? {
        None => TaskState::Deleted,
        Some(t) => match t.status {
            Some(EntryStatus::Dropped) => TaskState::Dropped,
            Some(EntryStatus::Done) => TaskState::Done,
            _ => TaskState::Live,
        },
    }))
}

/// Вид пространства этой базы; нет — `None`.
fn space_kind(c: &Connection, id: Id) -> Result<Option<SpaceKind>, StoreError> {
    use rusqlite::OptionalExtension;
    Ok(c.prepare_cached("SELECT kind FROM spaces WHERE id = ?1")?
        .query_row([id], |r| r.get(0))
        .optional()?)
}

/// Пространство задачи идеи по §5.2, п. 1: явное (`space` запроса), затем `data.task_space`
/// вопроса-предложения, затем решение запроса той же базы, затем пространство идеи. Допустимо
/// пространство идеи и — у идеи Knowledge — решение этой базы. Явное или из вопроса вне правил
/// — [`IdeaError::TaskSpace`]; неподходящее решение запроса пропускается: у идеи решения
/// задача всегда в её пространстве **[решение исполнителя]**.
fn resolve_task_space(
    c: &Connection,
    idea: &Entry,
    explicit: Option<Id>,
    from_question: Option<Id>,
    request_solution: Option<Id>,
) -> Result<Result<Id, IdeaError>, StoreError> {
    let idea_kind = space_kind(c, idea.space_id)?;
    let allowed = |s: Id| -> Result<bool, StoreError> {
        if s == idea.space_id {
            return Ok(true);
        }
        Ok(idea_kind == Some(SpaceKind::Knowledge)
            && space_kind(c, s)? == Some(SpaceKind::Solution))
    };
    if let Some(s) = explicit.or(from_question) {
        return Ok(if allowed(s)? {
            Ok(s)
        } else {
            Err(IdeaError::TaskSpace(
                "space: задача общей идеи — в Knowledge или в решении этой базы, идеи решения — \
                 в её пространстве"
                    .into(),
            ))
        });
    }
    if let Some(s) = request_solution
        && allowed(s)?
    {
        return Ok(Ok(s));
    }
    Ok(Ok(idea.space_id))
}

/// Усечение до `max` символов с многоточием; переводы строк — пробелами.
fn clip(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        return s;
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// Заголовок вопроса-предложения «Начать идею «…»?» не длиннее [`TITLE_MAX_CHARS`].
fn proposal_title(title: &str) -> String {
    const HEAD: &str = "Начать идею «";
    const TAIL: &str = "»?";
    let room = TITLE_MAX_CHARS - HEAD.chars().count() - TAIL.chars().count();
    format!("{HEAD}{}{TAIL}", clip(title, room))
}

/// Ссылка на идею в теле вопроса и задачи.
fn idea_line(id: Id) -> String {
    format!("Идея: w:{id}")
}

/// Текст через пустую строку (пустые части пропускаются).
fn joined(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Предлагает начать идею одной транзакцией писателя (§5.1): новый вопрос с вариантами
/// [`IDEA_OPTIONS`] (`data.idea`, `data.task_space`), системное ребро `relates_to` вопрос →
/// идея, идея → `proposed` с `question_id` и `proposed_at`. Возвращает `(идея, вопрос)`; при
/// уже открытом вопросе — тот же вопрос без новой ревизии. `task_space`: явный → решение
/// запроса той же базы → пространство идеи. «От агента» — [`IdeaError::AgentOrigin`]; статус
/// не `new`/`deferred` — [`IdeaError::Transition`]; `text` длиннее
/// [`PROPOSE_TEXT_MAX_CHARS`] — `BadInput`.
pub async fn propose_idea(
    db: &Db,
    id: Id,
    text: Option<String>,
    task_space: Option<Id>,
    request_solution: Option<Id>,
    attrib: &Attribution,
) -> Result<(Entry, Entry), IdeaError> {
    let text = text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    if text
        .as_deref()
        .is_some_and(|t| t.chars().count() > PROPOSE_TEXT_MAX_CHARS)
    {
        return Err(StoreError::BadInput(format!(
            "text: не больше {PROPOSE_TEXT_MAX_CHARS} символов"
        ))
        .into());
    }
    let attrib = attrib.clone();
    db.write(move |tx| {
        atomic(tx, |tx| {
            let idea = match live_idea(tx, id)? {
                Ok(e) => e,
                Err(e) => return Ok(Err(e)),
            };
            let mut d = IdeaData::from_stored(&idea.data);
            if d.origin == IdeaOrigin::Agent {
                return Ok(Err(IdeaError::AgentOrigin));
            }
            let status = idea.status.unwrap_or(EntryStatus::New);
            if status == EntryStatus::Proposed
                && let Some(q) = open_question(tx, &d)?
            {
                return Ok(Ok((idea, q)));
            }
            // `proposed` без открытого вопроса (рассогласование) предлагается заново
            if !matches!(
                status,
                EntryStatus::New | EntryStatus::Deferred | EntryStatus::Proposed
            ) {
                return Ok(Err(IdeaError::Transition {
                    from: status,
                    to: EntryStatus::Proposed.as_str(),
                }));
            }
            let space = match resolve_task_space(tx, &idea, task_space, None, request_solution)? {
                Ok(s) => s,
                Err(e) => return Ok(Err(e)),
            };
            let now_ts = Timestamp::now();
            let now = fmt_ts(now_ts);
            let options: Vec<Value> = IDEA_OPTIONS
                .iter()
                .map(|(key, text)| json!({"key": key, "text": text}))
                .collect();
            let q = NewEntry {
                refs: None,
                space_id: idea.space_id,
                kind: EntryKind::Question,
                title: proposal_title(&idea.title),
                body: joined(&[text.as_deref().unwrap_or(""), &idea_line(idea.id)]),
                data: json!({
                    "options": options,
                    "idea": idea.id.to_string(),
                    "task_space": space.to_string(),
                }),
                area: idea.area.clone(),
                tags: idea.tags.clone(),
                status: None,
                certainty: None,
                idempotency_key: None,
                triggers: Vec::new(),
                links: vec![(EdgeType::RelatesTo, entry_ref(idea.id))],
            };
            let qid = insert_entry_as(tx, q, None, &attrib, None, &now, Maker::Server)?;
            d.question_id = Some(qid);
            d.proposed_at = Some(now_ts);
            revise_with(
                tx,
                &idea,
                Some(EntryStatus::Proposed),
                d.to_value(),
                None,
                &attrib,
                &now,
            )?;
            Ok(Ok((load_entry(tx, id)?, load_entry(tx, qid)?)))
        })
    })
    .await?
}

/// Переход идеи в транзакции писателя (§5.2): проверка [`check_idea_transition`], у `accept` —
/// задача (`insert_entry`, системное ребро `derived_from` задача → идея) в пространстве по
/// §5.2, п. 1, затем новая ревизия идеи (`decided`, `task_id`, `horizon`, `prev_task_ids`).
/// `answered`: `Some(вопрос)` — действующий вопрос-предложение уже отвечен вызывающим
/// (`answer_question`, его `task_space` выбирает пространство задачи); `None` — открытый
/// вопрос-предложение `data.question_id` отвечается здесь (`answer_tx`) вариантом решения.
/// Возвращает `(идея, задача)`. Логическая ошибка — до записи или под [`atomic`] вызывающего.
pub(crate) fn apply_decision_tx(
    tx: &Connection,
    idea: &Entry,
    d: &NewDecision,
    answered: Option<&Entry>,
    attrib: &Attribution,
    now: &str,
) -> Result<Result<(Entry, Option<Entry>), IdeaError>, StoreError> {
    let mut data = IdeaData::from_stored(&idea.data);
    if d.expected_origin.is_some_and(|o| o != data.origin) {
        return Ok(Err(IdeaError::OriginChanged));
    }
    let cur = idea.status.unwrap_or(EntryStatus::New);
    let task = if matches!(cur, EntryStatus::Accepted | EntryStatus::Done) {
        task_state(tx, &data)?
    } else {
        None
    };
    let reason = d
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(str::to_string);
    if reason
        .as_deref()
        .is_some_and(|r| r.chars().count() > REASON_MAX_CHARS)
    {
        return Err(StoreError::BadInput(format!(
            "reason: не больше {REASON_MAX_CHARS} символов"
        )));
    }
    let to = match check_idea_transition(cur, &data, task, d.decision, reason.as_deref()) {
        Ok(s) => s,
        Err(code) => return Ok(Err(transition_error(code, cur, &data, d.decision))),
    };
    let today = utc_today();
    let horizon = match d.decision {
        IdeaDecision::Defer => {
            let h = match d.horizon {
                Some(h) => h,
                None => match today.checked_add(Span::new().days(i64::from(d.defer_days.max(1)))) {
                    Ok(h) => h,
                    Err(e) => return Ok(Err(IdeaError::BadHorizon(format!("horizon: {e}")))),
                },
            };
            if let Err(e) = check_horizon(h, today) {
                return Ok(Err(IdeaError::BadHorizon(e)));
            }
            Some(h)
        }
        _ if d.horizon.is_some() => {
            return Ok(Err(IdeaError::BadHorizon(
                "horizon: только у decision defer".into(),
            )));
        }
        _ => None,
    };
    // вопрос-предложение: отвеченный вызывающим или открытый
    let open_q = match answered {
        Some(_) => None,
        None => open_question(tx, &data)?,
    };
    let mut task_id = None;
    if d.decision == IdeaDecision::Accept {
        let from_question = answered
            .or(open_q.as_ref())
            .and_then(|q| QuestionData::from_stored(&q.data).task_space);
        let space =
            match resolve_task_space(tx, idea, d.task_space, from_question, d.request_solution)? {
                Ok(s) => s,
                Err(e) => return Ok(Err(e)),
            };
        let t = NewEntry {
            refs: None,
            space_id: space,
            kind: EntryKind::Task,
            title: idea.title.clone(),
            body: joined(&[&idea.body, &idea_line(idea.id)]),
            data: TaskData {
                links: data.links.clone(),
                ..TaskData::default()
            }
            .to_value(),
            area: idea.area.clone(),
            tags: idea.tags.clone(),
            status: None,
            certainty: None,
            idempotency_key: None,
            triggers: Vec::new(),
            links: vec![(EdgeType::DerivedFrom, entry_ref(idea.id))],
        };
        let tid = insert_entry_as(tx, t, None, attrib, None, now, Maker::Server)?;
        data.task_id = Some(tid);
        task_id = Some(tid);
    }
    match d.decision {
        IdeaDecision::Defer => data.horizon = horizon,
        IdeaDecision::Reopen => {
            data.horizon = None;
            if cur == EntryStatus::Accepted
                && let Some(t) = data.task_id.take()
            {
                data.prev_task_ids.push(t);
            }
        }
        // «не раньше» имеет смысл только у отложенной идеи (§5.2)
        IdeaDecision::Accept | IdeaDecision::Decline => data.horizon = None,
    }
    data.decided = Some(Decided {
        decision: d.decision,
        by: d.by,
        channel: d.channel,
        at: parse_ts(now)?,
    });
    revise_with(
        tx,
        idea,
        Some(to),
        data.to_value(),
        reason.as_deref(),
        attrib,
        now,
    )?;
    if let (Some(q), Some(key)) = (open_q, d.decision.option_key()) {
        let a = NormalizedAnswer {
            text: None,
            option: Some(key.to_string()),
            by: d.by,
            channel: d.channel,
        };
        match answer_tx(tx, &q, &a, attrib, now)? {
            Ok(_) => {}
            Err(QuestionError::Store(e)) => return Err(e),
            // вопрос проверен открытым выше: иное — рассогласование, откат всего
            Err(e) => return Err(StoreError::Conflict(e.to_string())),
        }
    }
    let task = task_id.map(|t| load_entry(tx, t)).transpose()?;
    Ok(Ok((load_entry(tx, idea.id)?, task)))
}

/// Решение по идее одной транзакцией писателя (§5.2): [`apply_decision_tx`]; открытый
/// вопрос-предложение отвечается вариантом решения (ровно одно решение-`decision`). Ошибка на
/// любом шаге откатывает всё: нет ни идеи `accepted` без задачи, ни задачи без ребра.
pub async fn decide_idea(
    db: &Db,
    id: Id,
    d: NewDecision,
    attrib: &Attribution,
) -> Result<Decision, IdeaError> {
    let attrib = attrib.clone();
    db.write(move |tx| {
        atomic(tx, |tx| {
            let idea = match live_idea(tx, id)? {
                Ok(e) => e,
                Err(e) => return Ok(Err(e)),
            };
            let open = if d.decision.option_key().is_some() {
                open_question(tx, &IdeaData::from_stored(&idea.data))?.map(|q| q.id)
            } else {
                None
            };
            let now = fmt_ts(Timestamp::now());
            let (idea, task) = match apply_decision_tx(tx, &idea, &d, None, &attrib, &now)? {
                Ok(v) => v,
                Err(e) => return Ok(Err(e)),
            };
            let question = open.map(|q| load_entry(tx, q)).transpose()?;
            Ok(Ok(Decision {
                idea,
                task,
                question,
            }))
        })
    })
    .await?
}

/// «Принять как свою» (§5.4): идея «от агента» в `new` или `deferred` получает
/// `origin: human`, `adopted_at`, `adopted_by` и новую ревизию; автор записи не меняется.
/// Уже человека — [`IdeaError::AlreadyAdopted`]; другой статус — [`IdeaError::Transition`].
pub async fn adopt_idea(
    db: &Db,
    id: Id,
    user: Id,
    attrib: &Attribution,
) -> Result<Entry, IdeaError> {
    let attrib = attrib.clone();
    db.write(move |tx| {
        let idea = match live_idea(tx, id)? {
            Ok(e) => e,
            Err(e) => return Ok(Err(e)),
        };
        let mut d = IdeaData::from_stored(&idea.data);
        if d.origin == IdeaOrigin::Human {
            return Ok(Err(IdeaError::AlreadyAdopted));
        }
        let status = idea.status.unwrap_or(EntryStatus::New);
        if !matches!(status, EntryStatus::New | EntryStatus::Deferred) {
            return Ok(Err(IdeaError::Transition {
                from: status,
                to: "adopt",
            }));
        }
        let now_ts = Timestamp::now();
        let now = fmt_ts(now_ts);
        d.origin = IdeaOrigin::Human;
        d.adopted_at = Some(now_ts);
        d.adopted_by = Some(user);
        revise_with(tx, &idea, idea.status, d.to_value(), None, &attrib, &now)?;
        Ok(Ok(load_entry(tx, id)?))
    })
    .await?
}

/// Идея, на которую указывает системное ребро `derived_from` задачи, если она живая.
fn idea_of_task(c: &Connection, task: Id) -> Result<Option<Entry>, StoreError> {
    let ids: Vec<Id> = {
        let mut st = c.prepare_cached(
            "SELECT dst_id FROM edges
             WHERE src_kind = 'entry' AND src_id = ?1 AND dst_kind = 'entry'
               AND type = 'derived_from' AND origin = 'system' AND deleted_at IS NULL",
        )?;
        st.query_map([task], |r| r.get(0))?
            .collect::<Result<_, _>>()?
    };
    for id in ids {
        if let Some(e) = live_opt(c, id)?
            && e.kind == EntryKind::Idea
        {
            return Ok(Some(e));
        }
    }
    Ok(None)
}

/// Смена статуса задачи (из `update_entry`, та же транзакция; §5.5): задача идеи `accepted`
/// → `done` — идея `done` с `done_at`; задача из `done` переоткрыта — идея снова `accepted`.
/// Ревизия идеи — с атрибуцией правки задачи. `dropped` идею не меняет (§5).
pub(crate) fn on_task_status_tx(
    tx: &Connection,
    task: &Entry,
    from: EntryStatus,
    to: EntryStatus,
    attrib: &Attribution,
    now: &str,
) -> Result<(), StoreError> {
    if from == to || !(to == EntryStatus::Done || from == EntryStatus::Done) {
        return Ok(());
    }
    let Some(idea) = idea_of_task(tx, task.id)? else {
        return Ok(());
    };
    let mut d = IdeaData::from_stored(&idea.data);
    if d.task_id != Some(task.id) {
        return Ok(());
    }
    let next = match (idea.status, to) {
        (Some(EntryStatus::Accepted), EntryStatus::Done) => {
            d.done_at = Some(parse_ts(now)?);
            EntryStatus::Done
        }
        (Some(EntryStatus::Done), s) if s != EntryStatus::Done => {
            d.done_at = None;
            EntryStatus::Accepted
        }
        _ => return Ok(()),
    };
    revise_with(tx, &idea, Some(next), d.to_value(), None, attrib, now)
}

/// Вопрос-предложение устарел или удалён (из `update_entry` и `delete_entry`; §5.3, §5.6):
/// идея `proposed` с этим `question_id` возвращается в `new`, `question_id` снимается.
pub(crate) fn on_proposal_closed_tx(
    tx: &Connection,
    q: &Entry,
    attrib: &Attribution,
    now: &str,
) -> Result<(), StoreError> {
    let Some(iid) = QuestionData::from_stored(&q.data).idea else {
        return Ok(());
    };
    let Some(idea) = live_opt(tx, iid)?.filter(|e| e.kind == EntryKind::Idea) else {
        return Ok(());
    };
    let mut d = IdeaData::from_stored(&idea.data);
    if idea.status != Some(EntryStatus::Proposed) || d.question_id != Some(q.id) {
        return Ok(());
    }
    d.question_id = None;
    revise_with(
        tx,
        &idea,
        Some(EntryStatus::New),
        d.to_value(),
        None,
        attrib,
        now,
    )
}

/// Причина устаревания вопроса-предложения удалённой идеи.
const IDEA_DELETED_REASON: &str = "идея удалена";

/// Удаление записи (из `delete_entry`, та же транзакция; §5.6): у идеи — её открытый
/// вопрос-предложение `obsolete` с причиной «идея удалена»; у вопроса-предложения — идея
/// `proposed` снова `new`. Задача из «начать» идею не меняет; восстановление не откатывает.
pub(crate) fn on_entry_deleted_tx(
    tx: &Connection,
    e: &Entry,
    attrib: &Attribution,
    now: &str,
) -> Result<(), StoreError> {
    match e.kind {
        EntryKind::Idea => {
            if let Some(q) = open_question(tx, &IdeaData::from_stored(&e.data))? {
                revise_with(
                    tx,
                    &q,
                    Some(EntryStatus::Obsolete),
                    q.data.clone(),
                    Some(IDEA_DELETED_REASON),
                    attrib,
                    now,
                )?;
            }
            Ok(())
        }
        EntryKind::Question => on_proposal_closed_tx(tx, e, attrib, now),
        _ => Ok(()),
    }
}

/// Причина устаревания восстановленного вопроса-предложения.
const STALE_PROPOSAL_REASON: &str = "предложение устарело";

/// Восстановление записи из корзины (из `restore_entry`, та же транзакция; §5.6). Связанные
/// статусы не откатываются, но и рассогласования не остаётся:
/// - открытый вопрос-предложение, который идея уже не считает своим (`question_id` другой,
///   идея не `proposed`, удалена), становится `obsolete` с причиной «предложение устарело»;
/// - идея `proposed` без открытого вопроса-предложения возвращается в `new` без `question_id`.
pub(crate) fn on_entry_restored_tx(
    tx: &Connection,
    e: &Entry,
    attrib: &Attribution,
    now: &str,
) -> Result<(), StoreError> {
    match e.kind {
        EntryKind::Question if e.status == Some(EntryStatus::Open) => {
            let Some(iid) = QuestionData::from_stored(&e.data).idea else {
                return Ok(());
            };
            let current = live_opt(tx, iid)?.is_some_and(|i| {
                i.kind == EntryKind::Idea
                    && i.status == Some(EntryStatus::Proposed)
                    && IdeaData::from_stored(&i.data).question_id == Some(e.id)
            });
            if !current {
                revise_with(
                    tx,
                    e,
                    Some(EntryStatus::Obsolete),
                    e.data.clone(),
                    Some(STALE_PROPOSAL_REASON),
                    attrib,
                    now,
                )?;
            }
            Ok(())
        }
        EntryKind::Idea if e.status == Some(EntryStatus::Proposed) => {
            let mut d = IdeaData::from_stored(&e.data);
            if open_question(tx, &d)?.is_some() {
                return Ok(());
            }
            d.question_id = None;
            revise_with(
                tx,
                e,
                Some(EntryStatus::New),
                d.to_value(),
                None,
                attrib,
                now,
            )
        }
        _ => Ok(()),
    }
}

/// Кандидат проактивности §6: живая идея `origin: human` в `new` или в `deferred` с
/// `horizon ≤ today` (UTC). Тот же предикат — у дайджеста и подсказки `prompt`.
pub fn idea_candidate(e: &Entry, today: Date) -> bool {
    if e.kind != EntryKind::Idea || e.deleted_at.is_some() {
        return false;
    }
    let d = IdeaData::from_stored(&e.data);
    d.origin == IdeaOrigin::Human
        && match e.status {
            Some(EntryStatus::New) => true,
            Some(EntryStatus::Deferred) => d.horizon.is_some_and(|h| h <= today),
            _ => false,
        }
}

/// Есть ли среди `ids` запись вида `idea` — любого статуса, в том числе в корзине (показ идеи
/// сессии, §6.1). Пачками по [`ANY_IDEA_CHUNK`] параметров.
pub async fn any_idea(db: &Db, ids: &[Id]) -> Result<bool, StoreError> {
    if ids.is_empty() {
        return Ok(false);
    }
    let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
    db.read(move |c| {
        for chunk in ids.chunks(ANY_IDEA_CHUNK) {
            let mut st = c.prepare(&format!(
                "SELECT 1 FROM entries WHERE kind = 'idea' AND id IN ({}) LIMIT 1",
                in_list(chunk.len())
            ))?;
            if st.exists(params_from_iter(chunk))? {
                return Ok(true);
            }
        }
        Ok(false)
    })
    .await
}

/// Параметров в одном запросе [`any_idea`].
const ANY_IDEA_CHUNK: usize = 500;

/// Кандидаты §6 пространств `spaces` ([`idea_candidate`]), кроме записей из `seen`: по
/// `priority` (`high`, `normal`, `low`), затем `updated_at` новые первыми; не больше `limit`.
pub async fn proactive_ideas(
    db: &Db,
    spaces: &[Id],
    today: Date,
    seen: &HashSet<Id>,
    limit: usize,
) -> Result<Vec<Entry>, StoreError> {
    if spaces.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let n = spaces.len();
    let mut args: Vec<String> = spaces.iter().map(ToString::to_string).collect();
    args.push(today.to_string());
    let seen = seen.clone();
    // в выдаче SQL не больше `seen` лишних: остальное отсекает тот же предикат
    let fetch = limit.saturating_add(seen.len());
    db.read(move |c| {
        let mut st = c.prepare(&format!(
            "SELECT {ENTRY_COLS} FROM entries e INDEXED BY entries_feed
             WHERE e.space_id IN ({}) AND e.kind = 'idea' AND e.deleted_at IS NULL
               AND json_extract(e.data, '$.origin') = 'human'
               AND (e.status = 'new'
                    OR (e.status = 'deferred' AND json_extract(e.data, '$.horizon') <= ?{}))
             ORDER BY CASE json_extract(e.data, '$.priority')
                          WHEN 'high' THEN 2 WHEN 'low' THEN 0 ELSE 1 END DESC,
                      e.updated_at DESC, e.id DESC
             LIMIT {fetch}",
            in_list(n),
            n + 1
        ))?;
        let rows = st.query_map(params_from_iter(args), entry_row)?;
        let mut out = Vec::new();
        for e in rows {
            let e = e?;
            if !seen.contains(&e.id) && idea_candidate(&e, today) {
                out.push(e);
                if out.len() == limit {
                    break;
                }
            }
        }
        Ok(out)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposal_title_fits() {
        assert_eq!(proposal_title("Кэш"), "Начать идею «Кэш»?");
        let t = proposal_title(&"я".repeat(600));
        assert_eq!(t.chars().count(), TITLE_MAX_CHARS);
        assert!(t.ends_with("…»?"));
    }

    #[test]
    fn decision_options_match() {
        for (k, _) in IDEA_OPTIONS {
            let d = IdeaDecision::from_option(k).unwrap();
            assert_eq!(d.option_key(), Some(k));
        }
        assert_eq!(IdeaDecision::from_option("reopen"), None);
    }

    /// Ошибка хранилища внутри шага откатывает его записи до точки сохранения: внешняя
    /// транзакция, продолженная после ошибки, их не фиксирует.
    #[test]
    fn atomic_rolls_back_on_store_error() {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t (x INTEGER)").unwrap();
        let tx = c.transaction().unwrap();
        let r: Result<Result<(), ()>, StoreError> = atomic(&tx, |c| {
            c.execute("INSERT INTO t VALUES (1)", [])?;
            Err(StoreError::Conflict("сбой".into()))
        });
        assert!(r.is_err());
        let ok: Result<Result<(), ()>, StoreError> = atomic(&tx, |c| {
            c.execute("INSERT INTO t VALUES (2)", [])?;
            Ok(Ok(()))
        });
        assert!(matches!(ok, Ok(Ok(()))));
        tx.commit().unwrap();
        let xs: Vec<i64> = c
            .prepare("SELECT x FROM t")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(xs, vec![2]);
    }

    /// Нормализация запроса для сверки `Idempotency-Key` не зависит от часов: `horizon`, который к
    /// повтору стал «сегодня» (или прошёл), даёт тот же вид, что при первой записи.
    #[test]
    fn request_value_ignores_clock() {
        for h in [utc_today(), Date::new(2020, 1, 1).unwrap()] {
            let data = json!({"origin": "human", "horizon": h.to_string(), "priority": "high"});
            assert!(idea_from_request(&data).is_err(), "{h}");
            let v = idea_request_value(&data).unwrap();
            assert_eq!(v["horizon"], json!(h.to_string()));
            assert_eq!(
                (v["origin"].clone(), v["priority"].clone()),
                (json!("human"), json!("high"))
            );
        }
        // остальные правила те же
        assert!(idea_request_value(&json!({"horizon": "2099-01-02"})).is_err());
        assert!(idea_request_value(&json!({"origin": "human", "task_id": null})).is_err());
        let future = json!({"origin": "agent", "horizon": "2099-01-02"});
        assert_eq!(
            idea_request_value(&future).unwrap(),
            idea_from_request(&future).unwrap().to_value()
        );
    }

    #[test]
    fn origin_required_on_request() {
        let e = idea_from_request(&json!({})).unwrap_err();
        assert!(e.contains("data.origin"), "{e}");
        assert_eq!(
            idea_from_request(&json!({"origin": "agent"}))
                .unwrap()
                .origin,
            IdeaOrigin::Agent
        );
    }
}
