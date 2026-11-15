//! Журнал «Идеи» (спецификация M5.7 §4–§6): записи `idea` — замысел человека в `title`/`body`,
//! решение по нему в `status` и `data`.
//!
//! - [`IdeaData`] — `data` идеи: `priority`, `horizon`, `links` ведёт клиент; `origin` задаётся
//!   при создании (меняет его только [`adopt_idea`]); `task_id`, `prev_task_ids`,
//!   `question_id`, `proposed_at`, `decided`, `adopted_at`, `adopted_by`, `done_at` и поля
//!   разбора агентом M6.5 `review`, `reviews` ([`Review`]), `returned_at` — сервер;
//! - [`check_idea_transition`] — таблица переходов §5 (единственное место правил, в том числе
//!   выхода из `accepted` при снятой или удалённой задаче — ждёт ответа владельца);
//! - операции — по одной транзакции писателя: [`propose_idea`] (вопрос с вариантами
//!   [`IDEA_OPTIONS`]), [`decide_idea`] (переход и задача при `accept`), [`adopt_idea`],
//!   разбор агентом [`review_idea`] (M6.5 §5: исходы [`ReviewOutcome`], задача плана, история
//!   разборов); общий шаг перехода [`apply_decision_tx`] вызывает и ответ на
//!   вопрос-предложение ([`crate::answer_question`]). Логическая ошибка после записи откатывает
//!   всё ([`atomic`]);
//! - хуки транзакций журналов: смена статуса задачи ([`on_task_status_tx`]; у задачи может быть
//!   несколько идей — [`ideas_of_task`], M6.5 §5), устаревание и
//!   удаление вопроса-предложения ([`on_proposal_closed_tx`]), удаление идеи
//!   ([`on_entry_deleted_tx`]);
//! - отбор для проактивности: [`proactive_ideas`] и предикат [`idea_candidate`] (§6).

use std::collections::HashSet;

use jiff::civil::Date;
use jiff::{Span, Timestamp};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use zam_core::{Id, TargetKind, TargetRef};

use crate::entries::{
    ENTRY_COLS, Entry, EntryKind, EntryStatus, Maker, NewEntry, REASON_MAX_CHARS, TITLE_MAX_CHARS,
    check_key, ensure_not_report, entry_row, insert_entry_as, load_entry, revise_with,
};
use crate::file_refs::{normalize_ref_path, shown_ref};
use crate::graph::{EdgeOrigin, EdgeType, insert_entry_links};
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
/// Варианты вопроса «вернуть» идею со снятой или удалённой задачей (§16, п. 2).
pub const IDEA_REOPEN_OPTIONS: [(&str, &str); 2] =
    [("reopen", "вернуть"), ("decline", "отказаться")];
/// `summary` разбора не короче, символов, — умолчание `ideas.review_min_chars` сервера (M6.5 §5).
pub const REVIEW_MIN_CHARS_DEFAULT: usize = 200;
/// Предел `summary` разбора, символов.
pub const REVIEW_SUMMARY_MAX_CHARS: usize = 8000;
/// Предел `missing` разбора (что дописать человеку), символов.
pub const REVIEW_MISSING_MAX_CHARS: usize = 2000;
/// Ссылок в `basis` и в `similar` одного разбора **[решение исполнителя]**.
pub const REVIEW_REFS_MAX: usize = 50;
/// Разборов в истории `data.reviews` (новые в начале).
pub const REVIEWS_MAX: usize = 10;

/// Вид вопроса-предложения идеи (`data.proposal` вопроса; нет поля — [`ProposalKind::Start`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ProposalKind {
    /// «Начать идею?» — варианты [`IDEA_OPTIONS`] (§5.1).
    #[default]
    Start,
    /// «Вернуть идею?» — идея `accepted` со снятой задачей, варианты [`IDEA_REOPEN_OPTIONS`]
    /// (§16).
    Reopen,
}

impl ProposalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProposalKind::Start => "start",
            ProposalKind::Reopen => "reopen",
        }
    }

    /// Вид из `data.proposal`: `reopen` — «вернуть», прочее и нет поля — «начать».
    pub fn from_stored(v: Option<&Value>) -> ProposalKind {
        match v.and_then(Value::as_str) {
            Some("reopen") => ProposalKind::Reopen,
            _ => ProposalKind::Start,
        }
    }

    /// Варианты вопроса этого вида (фиксирует сервер).
    pub fn options(self) -> &'static [(&'static str, &'static str)] {
        match self {
            ProposalKind::Start => &IDEA_OPTIONS,
            ProposalKind::Reopen => &IDEA_REOPEN_OPTIONS,
        }
    }

    /// Решение по ключу варианта вопроса этого вида; чужой ключ — `None`.
    pub fn decision(self, key: &str) -> Option<IdeaDecision> {
        self.options()
            .iter()
            .any(|(k, _)| *k == key)
            .then(|| IdeaDecision::parse(key))
            .flatten()
    }

    /// Ключи вариантов для сообщений: «accept, defer или decline».
    pub fn keys_text(self) -> &'static str {
        match self {
            ProposalKind::Start => "accept, defer или decline",
            ProposalKind::Reopen => "reopen или decline",
        }
    }
}

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
}

/// Решение в записи `data.decided.decision`: решения `decide` и ответа на вопрос-предложение
/// ([`IdeaDecision`]) и исходы разбора агентом (M6.5 §5; `plan` записывается как `accept`).
/// Отдельный тип, а не новые варианты [`IdeaDecision`]: `return` и `done` — не решения запроса
/// `decide` **[решение исполнителя задачи 2 M6.5]**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecidedAs {
    Accept,
    Defer,
    Decline,
    Reopen,
    /// Разбор: на доработку.
    Return,
    /// Разбор: уже сделано.
    Done,
}

impl DecidedAs {
    pub fn as_str(self) -> &'static str {
        match self {
            DecidedAs::Accept => "accept",
            DecidedAs::Defer => "defer",
            DecidedAs::Decline => "decline",
            DecidedAs::Reopen => "reopen",
            DecidedAs::Return => "return",
            DecidedAs::Done => "done",
        }
    }
}

impl From<IdeaDecision> for DecidedAs {
    fn from(d: IdeaDecision) -> DecidedAs {
        match d {
            IdeaDecision::Accept => DecidedAs::Accept,
            IdeaDecision::Defer => DecidedAs::Defer,
            IdeaDecision::Decline => DecidedAs::Decline,
            IdeaDecision::Reopen => DecidedAs::Reopen,
        }
    }
}

impl From<ReviewOutcome> for DecidedAs {
    fn from(o: ReviewOutcome) -> DecidedAs {
        match o {
            ReviewOutcome::Plan => DecidedAs::Accept,
            ReviewOutcome::Return => DecidedAs::Return,
            ReviewOutcome::Decline => DecidedAs::Decline,
            ReviewOutcome::Done => DecidedAs::Done,
        }
    }
}

/// Канал в записи `data.decided.channel`: канал решения или ответа ([`Channel`]) или разбор
/// агентом `review` (M6.5 §5). Отдельный тип: `review` не канал ответа, клиент его не присылает.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecidedVia {
    Webui,
    Chat,
    Cli,
    Review,
}

impl DecidedVia {
    pub fn as_str(self) -> &'static str {
        match self {
            DecidedVia::Webui => "webui",
            DecidedVia::Chat => "chat",
            DecidedVia::Cli => "cli",
            DecidedVia::Review => "review",
        }
    }
}

impl From<Channel> for DecidedVia {
    fn from(c: Channel) -> DecidedVia {
        match c {
            Channel::Webui => DecidedVia::Webui,
            Channel::Chat => DecidedVia::Chat,
            Channel::Cli => DecidedVia::Cli,
        }
    }
}

/// Последнее решение по идее (`data.decided`): человека или агента (`decide`, ответ на
/// вопрос-предложение) либо разбор агентом (`by: agent`, `channel: review`, M6.5 §5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decided {
    pub decision: DecidedAs,
    pub by: AnswerBy,
    pub channel: DecidedVia,
    #[serde(serialize_with = "ser_ts")]
    pub at: Timestamp,
}

/// Исход разбора идеи агентом (M6.5 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewOutcome {
    /// В план: новая задача или привязка к открытой.
    Plan,
    /// На доработку: человек дописывает (`missing`).
    Return,
    /// Отклонить: разбор и есть причина.
    Decline,
    /// Уже сделано: в `basis` ссылка на сделанное.
    Done,
}

impl ReviewOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewOutcome::Plan => "plan",
            ReviewOutcome::Return => "return",
            ReviewOutcome::Decline => "decline",
            ReviewOutcome::Done => "done",
        }
    }

    pub fn parse(s: &str) -> Option<ReviewOutcome> {
        match s {
            "plan" => Some(ReviewOutcome::Plan),
            "return" => Some(ReviewOutcome::Return),
            "decline" => Some(ReviewOutcome::Decline),
            "done" => Some(ReviewOutcome::Done),
            _ => None,
        }
    }
}

/// Разбор идеи агентом (`data.review` — последний, `data.reviews` — история; M6.5 §5). Поля
/// без значения в `data` не пишутся.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub outcome: ReviewOutcome,
    /// Разбор для человека (Markdown).
    pub summary: String,
    /// Что дописать (у `return`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing: Option<String>,
    /// На что опирался разбор: ссылки с префиксом (`k:…`, `file:…`, `commit:…`).
    #[serde(default)]
    pub basis: Vec<String>,
    /// Похожие идеи (`w:…`), в том числе отклонённые.
    #[serde(default)]
    pub similar: Vec<String>,
    /// Задача плана (`k:<id>`) у исхода `plan`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Пользователь, от имени которого разбирал агент (владелец токена).
    pub by_user: Id,
    /// Сессия агента.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Ключ повтора (`Idempotency-Key`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
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
    /// Последний разбор агентом (M6.5 §5).
    pub review: Option<Review>,
    /// История разборов: не больше 10, новые в начале (предел держит запись разбора).
    pub reviews: Vec<Review>,
    /// Когда идея ушла «На доработку» (`returned`); снимается при любом выходе из `returned`
    /// ([`returned_exit_data`]).
    pub returned_at: Option<Timestamp>,
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
            review: None,
            reviews: Vec::new(),
            returned_at: None,
        }
    }
}

/// Поля, которые ведёт клиент.
const CLIENT_KEYS: [&str; 3] = ["priority", "horizon", "links"];
/// Поля, которые ведёт сервер.
const SERVER_KEYS: [&str; 11] = [
    "task_id",
    "prev_task_ids",
    "question_id",
    "proposed_at",
    "decided",
    "adopted_at",
    "adopted_by",
    "done_at",
    "review",
    "reviews",
    "returned_at",
];
/// Подсказка обязательного `data.origin` (по образцу обязательного `by` ответа, §4).
pub const IDEA_ORIGIN_REQUIRED: &str = "data.origin: укажите human — идея человека (записана по его \
                               словам) или agent — своя";

/// `data.priority` идеи и задачи (M6.5 §5 — одно правило и один текст ошибки): `null` — нет
/// значения, строка `low | normal | high`, иное — ошибка поля.
pub(crate) fn priority_opt(v: &Value) -> Result<Option<IdeaPriority>, String> {
    const ERR: &str = "data.priority: low, normal или high";
    match v {
        Value::Null => Ok(None),
        Value::String(s) => IdeaPriority::parse(s.trim())
            .map(Some)
            .ok_or_else(|| ERR.into()),
        _ => Err(ERR.into()),
    }
}

/// `data.priority` идеи: без значения — `normal`.
fn priority_field(v: &Value) -> Result<IdeaPriority, String> {
    Ok(priority_opt(v)?.unwrap_or(IdeaPriority::Normal))
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
            review: get("review").and_then(|v| serde_json::from_value(v.clone()).ok()),
            // битые элементы истории пропускаются по одному, остальные сохраняются
            reviews: get("reviews")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| serde_json::from_value(v.clone()).ok())
                        .collect()
                })
                .unwrap_or_default(),
            returned_at: ts("returned_at"),
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

    /// Действующее предложение «вернуть» у идеи `accepted` (§16): `question_id` задан и
    /// предложен позже последнего решения. У принятой «начать» идеи `question_id` — отвеченный
    /// вопрос «начать» (предложен раньше решения). Согласованность держат хуки транзакций:
    /// вопрос «вернуть» устарел, удалён или задача снова живая — `question_id` снимается
    /// ([`on_proposal_closed_tx`], [`on_task_status_tx`], [`on_entry_restored_tx`]).
    pub fn reopen_proposed(&self) -> bool {
        match (self.question_id, self.proposed_at) {
            (Some(_), Some(p)) => self.decided.as_ref().is_none_or(|d| p > d.at),
            _ => false,
        }
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
        if let Some(r) = &self.review {
            o.insert("review".into(), json!(r));
        }
        if !self.reviews.is_empty() {
            o.insert("reviews".into(), json!(self.reviews));
        }
        if let Some(t) = self.returned_at {
            o.insert("returned_at".into(), json!(fmt_ts(t)));
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

/// Задача идеи `accepted` снята (`dropped`), удалена или её нет: можно выйти из `accepted`
/// вручную (`reopen`, `decline`, §5) и агент может предложить вернуть идею (§16).
/// **Единственное место правила** «задача снята или удалена» (ответ владельца на вопрос 9 —
/// А; задача 7).
pub fn task_released(task: Option<TaskState>) -> bool {
    matches!(task, None | Some(TaskState::Dropped | TaskState::Deleted))
}

/// Допустимость перехода §5 (без прав «кто»). `Ok` — новый статус; `Err` — машинный код:
/// `transition-not-allowed`, `already-decided` (только `accept` из `accepted`|`done`),
/// `idea-agent-origin` (`accept` идеи «от агента»), `reason-required` (`reopen` без причины).
/// Повторный `defer` по `deferred` разрешён. `task` — состояние задачи `data.task_id`.
/// Идею `returned` (M6.5 §6) человек решает сам, как открытую (`accept`, `defer`, `decline`);
/// `reopen` из неё не переход — на разбор её возвращает правка человека.
pub fn check_idea_transition(
    cur: EntryStatus,
    d: &IdeaData,
    task: Option<TaskState>,
    decision: IdeaDecision,
    reason: Option<&str>,
) -> Result<EntryStatus, &'static str> {
    use EntryStatus::*;
    const TNA: &str = "transition-not-allowed";
    let open = matches!(cur, New | Proposed | Deferred | Returned);
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

/// `data` новой ревизии идеи при смене статуса `from → to`: выход из `returned` любым путём
/// (правка человеком, решение человека, разбор агентом) снимает `returned_at` (M6.5 §6).
/// `None` — `data` без изменений. Единственное место правила — запись ревизии
/// (`entries::write::save`), через которую идут и `update_entry`, и [`revise_with`].
pub(crate) fn returned_exit_data(
    from: Option<EntryStatus>,
    to: Option<EntryStatus>,
    data: &Value,
) -> Option<Value> {
    if from != Some(EntryStatus::Returned)
        || to == Some(EntryStatus::Returned)
        || data.get("returned_at").is_none()
    {
        return None;
    }
    let mut d = data.clone();
    d.as_object_mut()?.remove("returned_at");
    Some(d)
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
    /// Задача идеи снова живая, а вопрос «вернуть» был открыт: вопрос устарел («задача
    /// возобновлена», запись зафиксирована), предлагать нечего — 409 (финальное ревью M5.7).
    #[error("задача идеи возобновлена — предлагать вернуть нечего; вопрос q:{question} устарел")]
    TaskRevived { question: Id },
    /// Предложение вернуть идею без объяснения (`text`): 422 (§16, п. 2).
    #[error("text: объясните, почему идею пора вернуть")]
    ReopenTextRequired,
    /// Поле разбора вне правил (M6.5 §5): 422; текст начинается с имени поля (`summary:`,
    /// `missing:`, `basis:`, `similar:`, `task:`, `task.id:`, `task.title:`).
    #[error("{0}")]
    ReviewInput(String),
    /// Разбор `plan` с `task.id` задачи не в `open`, `in_progress` или `blocked`: 422.
    #[error(
        "task.id: задача плана не открыта — привязать можно задачу open, in_progress или blocked"
    )]
    ReviewTaskNotOpen,
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

/// Время, когда задача `data.task_id` стала `done`: первая ревизия её последней непрерывной
/// серии ревизий `done` (поздняя правка сделанной задачи время не сдвигает; после переоткрытия и
/// повторного `done` — время второго перехода). Задачи в данных нет или история не кончается
/// ревизией `done` — `None`.
fn task_done_ts(c: &Connection, d: &IdeaData) -> Result<Option<Timestamp>, StoreError> {
    let Some(task) = d.task_id else {
        return Ok(None);
    };
    let ts: Option<String> = c
        .query_row(
            "SELECT r.ts FROM entry_revisions r
             WHERE r.entry_id = ?1 AND r.status = 'done'
               AND r.rev > coalesce((SELECT max(x.rev) FROM entry_revisions x
                                     WHERE x.entry_id = ?1
                                       AND (x.status IS NULL OR x.status <> 'done')), 0)
             ORDER BY r.rev LIMIT 1",
            [task],
            |r| r.get(0),
        )
        .optional()?;
    ts.as_deref().map(parse_ts).transpose()
}

/// Есть ли в пространствах `spaces` кандидат первого прохода подсказки к `prompt`: живая идея
/// человека `new` или `deferred` с наступившим `horizon` (один SQL `LIMIT 1`). Нет — поиск идей
/// не нужен (финальное ревью M5.7, Minor 5).
pub async fn any_new_idea(db: &Db, spaces: &[Id], today: Date) -> Result<bool, StoreError> {
    if spaces.is_empty() {
        return Ok(false);
    }
    let n = spaces.len();
    let mut args: Vec<String> = spaces.iter().map(ToString::to_string).collect();
    args.push(today.to_string());
    db.read(move |c| {
        let mut st = c.prepare(&format!(
            "SELECT 1 FROM entries e
             WHERE e.space_id IN ({}) AND e.kind = 'idea' AND e.deleted_at IS NULL
               AND json_extract(e.data, '$.origin') = 'human'
               AND (e.status = 'new'
                    OR (e.status = 'deferred' AND json_extract(e.data, '$.horizon') <= ?{}))
             LIMIT 1",
            in_list(n),
            n + 1
        ))?;
        Ok(st.exists(params_from_iter(args))?)
    })
    .await
}

/// Id живых идей человека `accepted` со снятой или удалённой задачей и без открытого вопроса
/// «вернуть» в пространствах `spaces`, кроме `exclude` (показанное сессии): новые первыми, не
/// больше `limit`. Второй проход подсказки к `prompt` (ревью задачи 7, I1′) — без поиска по
/// всем принятым идеям.
pub async fn released_ideas(
    db: &Db,
    spaces: &[Id],
    exclude: &[Id],
    limit: usize,
) -> Result<Vec<Id>, StoreError> {
    if spaces.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let args: Vec<String> = spaces.iter().map(ToString::to_string).collect();
    let exclude: HashSet<Id> = exclude.iter().copied().collect();
    // в выдаче SQL не больше `exclude` лишних: их отсекает фильтр ниже
    let fetch = limit.saturating_add(exclude.len());
    db.read(move |c| {
        let mut st = c.prepare(&format!(
            "SELECT e.id FROM entries e
             WHERE e.space_id IN ({}) AND e.kind = 'idea' AND e.deleted_at IS NULL
               AND e.status = 'accepted' AND json_extract(e.data, '$.origin') = 'human'
               AND {RELEASED_SQL} AND {NOT_REOPEN_PROPOSED_SQL}
             ORDER BY e.updated_at DESC, e.id DESC
             LIMIT {fetch}",
            in_list(args.len())
        ))?;
        let ids = st.query_map(params_from_iter(args), |r| r.get::<_, Id>(0))?;
        let mut out = Vec::new();
        for id in ids {
            let id = id?;
            if !exclude.contains(&id) {
                out.push(id);
                if out.len() == limit {
                    break;
                }
            }
        }
        Ok(out)
    })
    .await
}

/// SQL «задача идеи `e` снята или удалена» — то же, что [`task_released`] (нет живой задачи
/// `data.task_id` не в `dropped`).
const RELEASED_SQL: &str = "NOT EXISTS (
    SELECT 1 FROM entries t
    WHERE t.id = json_extract(e.data, '$.task_id')
      AND t.deleted_at IS NULL AND t.status IS NOT 'dropped')";

/// SQL «нет открытого предложения «вернуть»» — то же, что `!IdeaData::reopen_proposed`
/// (`question_id` задан и `proposed_at` позже `decided.at`). Время в `data` пишется
/// [`fmt_ts`] фиксированной ширины (`…T…​.ffffffZ`), поэтому строки сравнимы лексикографически
/// (ревью задачи 7, I2).
const NOT_REOPEN_PROPOSED_SQL: &str = "NOT (json_extract(e.data, '$.question_id') IS NOT NULL
    AND json_extract(e.data, '$.proposed_at') IS NOT NULL
    AND json_extract(e.data, '$.proposed_at')
        > COALESCE(json_extract(e.data, '$.decided.at'), ''))";

/// Состояние задачи идеи `accepted` или `done` для отбора кандидатов (§6, §16): у прочих
/// статусов — `None`. Задачи нет в данных или она удалена — [`TaskState::Deleted`].
pub async fn idea_task_state(db: &Db, idea: &Entry) -> Result<Option<TaskState>, StoreError> {
    if !matches!(idea.status, Some(EntryStatus::Accepted | EntryStatus::Done)) {
        return Ok(None);
    }
    let d = IdeaData::from_stored(&idea.data);
    db.read(move |c| Ok(Some(task_state(c, &d)?.unwrap_or(TaskState::Deleted))))
        .await
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

/// Заголовок вопроса-предложения «Начать идею «…»?» («Вернуть идею «…»?» у [`ProposalKind::Reopen`])
/// не длиннее [`TITLE_MAX_CHARS`].
fn proposal_title(title: &str, kind: ProposalKind) -> String {
    let head = match kind {
        ProposalKind::Start => "Начать идею «",
        ProposalKind::Reopen => "Вернуть идею «",
    };
    proposal_title_with(title, head)
}

fn proposal_title_with(title: &str, head: &str) -> String {
    const TAIL: &str = "»?";
    let head_len = head.chars().count();
    let room = TITLE_MAX_CHARS - head_len - TAIL.chars().count();
    format!("{head}{}{TAIL}", clip(title, room))
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
///
/// Идея `accepted` со снятой или удалённой задачей ([`task_released`], §16) получает вопрос
/// «вернуть» ([`ProposalKind::Reopen`], варианты [`IDEA_REOPEN_OPTIONS`], без `task_space`):
/// `text` обязателен ([`IdeaError::ReopenTextRequired`]), статус остаётся `accepted`, идея
/// получает `question_id` и `proposed_at`. `task_space` и решение запроса здесь не нужны и не
/// проверяются: задачу выберет следующий «начать» **[решение исполнителя задачи 7]**.
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
            if matches!(status, EntryStatus::Proposed | EntryStatus::Accepted)
                && let Some(q) = open_question(tx, &d)?
            {
                // вопрос «вернуть», а задача снова живая (рассогласование мимо хуков): вопрос
                // устаревает «задача возобновлена», ответ — 409 (ревью задачи 7, M6)
                if status == EntryStatus::Accepted && !task_released(task_state(tx, &d)?) {
                    let now = fmt_ts(crate::time::now());
                    revive_task_tx(tx, idea, &attrib, &now)?;
                    return Ok(Ok(Err(q.id)));
                }
                return Ok(Ok(Ok((idea, q))));
            }
            let kind = if status == EntryStatus::Accepted && task_released(task_state(tx, &d)?) {
                ProposalKind::Reopen
            } else {
                ProposalKind::Start
            };
            // `proposed` без открытого вопроса (рассогласование) предлагается заново
            if kind == ProposalKind::Start
                && !matches!(
                    status,
                    EntryStatus::New | EntryStatus::Deferred | EntryStatus::Proposed
                )
            {
                return Ok(Err(IdeaError::Transition {
                    from: status,
                    to: EntryStatus::Proposed.as_str(),
                }));
            }
            if kind == ProposalKind::Reopen && text.is_none() {
                return Ok(Err(IdeaError::ReopenTextRequired));
            }
            let mut qdata = json!({
                "options": kind.options()
                    .iter()
                    .map(|(key, text)| json!({"key": key, "text": text}))
                    .collect::<Vec<Value>>(),
                "idea": idea.id.to_string(),
            });
            match kind {
                ProposalKind::Start => {
                    let space =
                        match resolve_task_space(tx, &idea, task_space, None, request_solution)? {
                            Ok(s) => s,
                            Err(e) => return Ok(Err(e)),
                        };
                    qdata["task_space"] = json!(space.to_string());
                }
                ProposalKind::Reopen => qdata["proposal"] = json!(kind.as_str()),
            }
            let now_ts = crate::time::now();
            let now = fmt_ts(now_ts);
            let q = NewEntry {
                refs: None,
                space_id: idea.space_id,
                kind: EntryKind::Question,
                title: proposal_title(&idea.title, kind),
                body: joined(&[text.as_deref().unwrap_or(""), &idea_line(idea.id)]),
                data: qdata,
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
            // «вернуть» статус не меняет: `proposed` значит «предложено начать» (§16, п. 2)
            let next = match kind {
                ProposalKind::Start => EntryStatus::Proposed,
                ProposalKind::Reopen => EntryStatus::Accepted,
            };
            revise_with(tx, &idea, Some(next), d.to_value(), None, &attrib, &now)?;
            Ok(Ok(Ok((load_entry(tx, id)?, load_entry(tx, qid)?))))
        })
    })
    .await??
    .map_err(|question| IdeaError::TaskRevived { question })
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
        decision: d.decision.into(),
        by: d.by,
        channel: d.channel.into(),
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
    if let Some(q) = open_q {
        let a = NormalizedAnswer {
            text: None,
            option: Some(d.decision.as_str().to_string()),
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
            let open = open_question(tx, &IdeaData::from_stored(&idea.data))?.map(|q| q.id);
            let now = fmt_ts(crate::time::now());
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

// ---------- разбор агентом (M6.5 §5) ----------

/// Задача плана разбора.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewTask {
    /// Без задачи: исходы `return`, `decline`, `done`; у `plan` — новая задача с умолчаниями.
    None,
    /// Новая задача `open` в пространстве идеи: заголовок — `title` или заголовок идеи, тело —
    /// `body` (план шагов; нет — тело идеи) и строка «Идея: w:<id>», `data.priority` —
    /// `priority` или приоритет идеи.
    New {
        title: Option<String>,
        body: Option<String>,
        priority: Option<IdeaPriority>,
    },
    /// Уже открытая задача плана (`open`, `in_progress`, `blocked`) в пространстве идеи.
    Existing(Id),
}

/// Разбор, как его присылает агент (M6.5 §5).
#[derive(Debug, Clone)]
pub struct NewReview {
    pub outcome: ReviewOutcome,
    /// Разбор для человека: от `min_chars` до [`REVIEW_SUMMARY_MAX_CHARS`] символов.
    pub summary: String,
    /// Что дописать: обязательно у `return` (до [`REVIEW_MISSING_MAX_CHARS`] символов), у
    /// прочих исходов — ошибка.
    pub missing: Option<String>,
    /// На что опирался разбор: `n: i: c: r: t: k: q: w:` (живая запись этого вида или заметка
    /// пространства идеи), `file:<путь от корня решения>`, `commit:<sha>`.
    pub basis: Vec<String>,
    /// Похожие идеи (`w:`) пространства идеи, в том числе отклонённые.
    pub similar: Vec<String>,
    /// Задача плана: только у `plan`.
    pub task: ReviewTask,
    /// Пользователь, от имени которого разбирает агент (владелец токена).
    pub by_user: Id,
    /// Сессия агента; нет — из атрибуции.
    pub session: Option<String>,
    /// Ключ повтора (`Idempotency-Key`).
    pub key: Option<String>,
    /// Нижний предел `summary`, символов (`ideas.review_min_chars` сервера,
    /// [`REVIEW_MIN_CHARS_DEFAULT`]).
    pub min_chars: usize,
}

/// Итог разбора: идея, задача плана (у `plan`) и признак повтора по ключу — `replay: true`
/// значит «этот разбор уже записан, ничего не записано заново».
#[derive(Debug, Clone)]
pub struct Reviewed {
    pub idea: Entry,
    pub task: Option<Entry>,
    pub replay: bool,
}

/// Ошибка поля разбора: `<поле>: <текст>`.
fn review_input(field: &str, msg: impl std::fmt::Display) -> IdeaError {
    IdeaError::ReviewInput(format!("{field}: {msg}"))
}

/// Буквы ссылок на записи в ссылках разбора: все виды записей, кроме отчёта (`p:`), без общей
/// `e:` — буква называет вид, и он сверяется с записью.
const REVIEW_ENTRY_LETTERS: [&str; 7] = ["i", "c", "r", "t", "k", "q", "w"];
/// Префиксы ссылок разбора — для текста ошибки.
const REVIEW_REF_PREFIXES: &str = "n:, i:, c:, r:, t:, k:, q:, w:, file: или commit:";
/// Длина sha коммита в `commit:`, символов.
const REVIEW_COMMIT_LEN: std::ops::RangeInclusive<usize> = 7..=64;

/// Ссылка разбора, разобранная по форме ([`parse_review_ref`]).
enum ReviewRef {
    Entry(EntryKind, Id),
    Node(Id),
    File(String),
    Commit(String),
}

impl ReviewRef {
    /// Вид для `data.review`: канонический id с префиксом (такие ссылки переписывает импорт),
    /// путь с `/`, sha в нижнем регистре.
    fn text(&self) -> String {
        match self {
            ReviewRef::Entry(k, id) => format!("{}:{id}", k.ref_letter()),
            ReviewRef::Node(id) => format!("n:{id}"),
            ReviewRef::File(p) => format!("file:{p}"),
            ReviewRef::Commit(c) => format!("commit:{c}"),
        }
    }
}

/// Форма ссылки поля `field` без обращения к базе: префикс из [`REVIEW_REF_PREFIXES`]; у
/// `file:` — путь относительно корня решения без сегмента `..` ([`normalize_ref_path`]); у
/// `commit:` — от 7 до 64 шестнадцатеричных символов.
fn parse_review_ref(field: &str, raw: &str) -> Result<ReviewRef, IdeaError> {
    let s = raw.trim();
    let shown = shown_ref(s);
    let bad_prefix = || {
        review_input(
            field,
            format!("«{shown}» — нужна ссылка с префиксом {REVIEW_REF_PREFIXES}"),
        )
    };
    let Some((prefix, rest)) = s.split_once(':') else {
        return Err(bad_prefix());
    };
    let id = || {
        rest.trim()
            .parse::<Id>()
            .map_err(|_| review_input(field, format!("{shown} — неверный id")))
    };
    match prefix {
        "file" => {
            if rest.replace('\\', "/").split('/').any(|seg| seg == "..") {
                return Err(review_input(
                    field,
                    format!("{shown} — путь относительно корня решения без «..»"),
                ));
            }
            normalize_ref_path(rest)
                .map(ReviewRef::File)
                .map_err(|e| review_input(field, format!("{shown} — {e}")))
        }
        "commit" => {
            let c = rest.trim();
            if REVIEW_COMMIT_LEN.contains(&c.len()) && c.bytes().all(|b| b.is_ascii_hexdigit()) {
                Ok(ReviewRef::Commit(c.to_ascii_lowercase()))
            } else {
                Err(review_input(
                    field,
                    format!("{shown} — sha коммита: от 7 до 64 шестнадцатеричных символов"),
                ))
            }
        }
        "n" => Ok(ReviewRef::Node(id()?)),
        l if REVIEW_ENTRY_LETTERS.contains(&l) => {
            let kind = EntryKind::from_ref_letter(l).ok_or_else(bad_prefix)?;
            Ok(ReviewRef::Entry(kind, id()?))
        }
        _ => Err(bad_prefix()),
    }
}

/// Ссылки поля `field` разбора идеи `idea`: форма ([`parse_review_ref`]), затем живая цель в
/// пространстве идеи — запись того вида, что у буквы, или заметка; `ideas_only` (`similar`) —
/// только другие идеи `w:`. Итог — нормализованные ссылки без повторов в присланном порядке и
/// есть ли среди них ссылка на сделанное: changelog, релиз, задача `done`, коммит (M6.5 §5).
fn review_refs(
    c: &Connection,
    field: &str,
    raw: &[String],
    idea: &Entry,
    ideas_only: bool,
) -> Result<Result<(Vec<String>, bool), IdeaError>, StoreError> {
    if raw.len() > REVIEW_REFS_MAX {
        return Ok(Err(review_input(
            field,
            format!("не больше {REVIEW_REFS_MAX} ссылок"),
        )));
    }
    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    let mut done = false;
    for s in raw {
        let r = match parse_review_ref(field, s) {
            Ok(r) => r,
            Err(e) => return Ok(Err(e)),
        };
        let shown = shown_ref(s.trim());
        if ideas_only && !matches!(r, ReviewRef::Entry(EntryKind::Idea, _)) {
            return Ok(Err(review_input(
                field,
                format!("{shown} — только ссылки на идеи w:"),
            )));
        }
        match &r {
            ReviewRef::Entry(kind, id) => {
                if ideas_only && *id == idea.id {
                    return Ok(Err(review_input(
                        field,
                        format!("{shown} — это сама разбираемая идея"),
                    )));
                }
                let row: Option<(EntryKind, Id, Option<EntryStatus>)> = c
                    .prepare_cached(
                        "SELECT kind, space_id, status FROM entries
                         WHERE id = ?1 AND deleted_at IS NULL",
                    )?
                    .query_row([*id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .optional()?;
                match row {
                    Some((k, space, status)) if k == *kind && space == idea.space_id => {
                        done |= matches!(k, EntryKind::Changelog | EntryKind::Release)
                            || (k == EntryKind::Task && status == Some(EntryStatus::Done));
                    }
                    _ => {
                        return Ok(Err(review_input(
                            field,
                            format!("{shown} — нет живой записи этого вида в пространстве идеи"),
                        )));
                    }
                }
            }
            ReviewRef::Node(id) => {
                let live: bool = c
                    .prepare_cached(
                        "SELECT EXISTS (SELECT 1 FROM nodes
                                        WHERE id = ?1 AND space_id = ?2 AND deleted_at IS NULL)",
                    )?
                    .query_row(params![id, idea.space_id], |r| r.get(0))?;
                if !live {
                    return Ok(Err(review_input(
                        field,
                        format!("{shown} — нет живой заметки в пространстве идеи"),
                    )));
                }
            }
            ReviewRef::File(_) => {}
            ReviewRef::Commit(_) => done = true,
        }
        let text = r.text();
        if !out.contains(&text) {
            out.push(text);
        }
    }
    Ok(Ok((out, done)))
}

/// Поля разбора без обращения к базе: `summary` (обрезанный, от `min_chars` до
/// [`REVIEW_SUMMARY_MAX_CHARS`] символов), `missing` (только и обязательно у `return`) и
/// задача плана (только у `plan`; без неё — новая задача с умолчаниями).
fn check_review_fields(r: &NewReview) -> Result<(String, Option<String>, ReviewTask), IdeaError> {
    let summary = r.summary.trim().to_string();
    let n = summary.chars().count();
    // разбор обязателен и при нулевом пределе вызывающего
    let min = r.min_chars.max(1);
    if n < min {
        return Err(review_input(
            "summary",
            format!("разбор для человека не короче {min} символов, сейчас {n}"),
        ));
    }
    if n > REVIEW_SUMMARY_MAX_CHARS {
        return Err(review_input(
            "summary",
            format!("не длиннее {REVIEW_SUMMARY_MAX_CHARS} символов, сейчас {n}"),
        ));
    }
    let trimmed = |s: &Option<String>| {
        s.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let missing = trimmed(&r.missing);
    match (r.outcome, &missing) {
        (ReviewOutcome::Return, None) => {
            return Err(review_input(
                "missing",
                "у исхода return обязательно — что дописать человеку",
            ));
        }
        (ReviewOutcome::Return, Some(m)) if m.chars().count() > REVIEW_MISSING_MAX_CHARS => {
            return Err(review_input(
                "missing",
                format!("не длиннее {REVIEW_MISSING_MAX_CHARS} символов"),
            ));
        }
        (ReviewOutcome::Return, Some(_)) | (_, None) => {}
        (_, Some(_)) => return Err(review_input("missing", "только у исхода return")),
    }
    let task = match (r.outcome, &r.task) {
        (
            ReviewOutcome::Plan,
            ReviewTask::New {
                title,
                body,
                priority,
            },
        ) => {
            let title = trimmed(title);
            if title
                .as_deref()
                .is_some_and(|t| t.chars().count() > TITLE_MAX_CHARS)
            {
                return Err(review_input(
                    "task.title",
                    format!("не длиннее {TITLE_MAX_CHARS} символов"),
                ));
            }
            ReviewTask::New {
                title,
                body: trimmed(body),
                priority: *priority,
            }
        }
        (ReviewOutcome::Plan, ReviewTask::None) => ReviewTask::New {
            title: None,
            body: None,
            priority: None,
        },
        (ReviewOutcome::Plan, t @ ReviewTask::Existing(_)) => t.clone(),
        (_, ReviewTask::None) => ReviewTask::None,
        (_, _) => {
            return Err(review_input("task", "задача плана — только у исхода plan"));
        }
    };
    Ok((summary, missing, task))
}

/// Живое ребро `derived_from` задача → идея, явное или системное (как уникальный индекс
/// `edges_explicit`): привязка задачи к идее ребра не дублирует.
fn derived_edge_exists(c: &Connection, task: Id, idea: Id) -> Result<bool, StoreError> {
    Ok(c.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM edges
                        WHERE src_kind = 'entry' AND src_id = ?1 AND dst_kind = 'entry'
                          AND dst_id = ?2 AND type = 'derived_from'
                          AND origin IN ('explicit', 'system') AND deleted_at IS NULL)",
    )?
    .query_row(params![task, idea], |r| r.get(0))?)
}

/// Задача плана разбора `plan` в транзакции разбора: новая — как у «начать»
/// ([`apply_decision_tx`]) в пространстве идеи, с `data.priority` (`task.priority` или
/// приоритет идеи); существующая — живая открытая задача пространства идеи, ей ставится
/// системное ребро `derived_from` на идею (уже есть — не дублируется). Итог — id задачи.
fn review_task_tx(
    tx: &Connection,
    idea: &Entry,
    data: &IdeaData,
    task: ReviewTask,
    attrib: &Attribution,
    now: &str,
) -> Result<Result<Option<Id>, IdeaError>, StoreError> {
    match task {
        ReviewTask::None => Ok(Ok(None)),
        ReviewTask::New {
            title,
            body,
            priority,
        } => {
            let t = NewEntry {
                refs: None,
                space_id: idea.space_id,
                kind: EntryKind::Task,
                title: title.unwrap_or_else(|| idea.title.clone()),
                body: joined(&[body.as_deref().unwrap_or(&idea.body), &idea_line(idea.id)]),
                data: TaskData {
                    links: data.links.clone(),
                    priority: Some(priority.unwrap_or(data.priority)),
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
            Ok(Ok(Some(insert_entry_as(
                tx,
                t,
                None,
                attrib,
                None,
                now,
                Maker::Server,
            )?)))
        }
        ReviewTask::Existing(tid) => {
            match live_opt(tx, tid)? {
                Some(t) if t.kind == EntryKind::Task && t.space_id == idea.space_id => {
                    if !matches!(
                        t.status,
                        Some(EntryStatus::Open | EntryStatus::InProgress | EntryStatus::Blocked)
                    ) {
                        return Ok(Err(IdeaError::ReviewTaskNotOpen));
                    }
                }
                Some(t) if t.kind == EntryKind::Task => {
                    return Ok(Err(review_input(
                        "task.id",
                        format!(
                            "k:{tid} — задача другого пространства: задача плана — в пространстве идеи"
                        ),
                    )));
                }
                _ => {
                    return Ok(Err(review_input(
                        "task.id",
                        format!("k:{tid} — нет живой задачи"),
                    )));
                }
            }
            if !derived_edge_exists(tx, tid, idea.id)? {
                insert_entry_links(
                    tx,
                    tid,
                    &[(EdgeType::DerivedFrom, entry_ref(idea.id))],
                    attrib,
                    now,
                    EdgeOrigin::System,
                )?;
            }
            Ok(Ok(Some(tid)))
        }
    }
}

/// Текст ошибки исхода `done` без ссылки на сделанное.
const REVIEW_DONE_REF_REQUIRED: &str =
    "у исхода done нужна ссылка на сделанное — c:, r:, commit: или k: задачи done";

/// Разбор идеи агентом одной транзакцией писателя (M6.5 §5):
/// 1. повтор — `key` совпал с ключом последнего разбора того же пользователя и исхода: текущая
///    идея и её задача плана, `replay: true`, ничего не записано;
/// 2. условия — живая идея ([`IdeaError::IdeaDeleted`], [`IdeaError::NotIdea`]) человека
///    ([`IdeaError::AgentOrigin`]) в статусе `new` (иначе [`IdeaError::Transition`] с `to:
///    "review"`; второй разбор той же идеи получает его же — захват не нужен), поля
///    ([`check_review_fields`]), ссылки `basis` и `similar` ([`review_refs`]), у `done` —
///    ссылка на сделанное; ошибка — [`IdeaError::ReviewInput`] с именем поля;
/// 3. `plan` — задача плана ([`review_task_tx`]; не открытая — [`IdeaError::ReviewTaskNotOpen`]);
/// 4. новая ревизия идеи: статус по исходу (`accepted`, `returned` с `returned_at`, `declined`,
///    `done` с `done_at`), `task_id` (прежняя снятая задача идеи `new` — в `prev_task_ids`),
///    `review` и `reviews` (новый в начале, не больше [`REVIEWS_MAX`]), `decided` агента канала
///    `review`.
///
/// Ошибка на любом шаге откатывает всё ([`atomic`]): нет ни задачи без идеи `accepted`, ни
/// ребра без задачи. Права «кто разбирает» (только токен), Knowledge и режим `ideas.mode`
/// проверяет вызывающий.
pub async fn review_idea(
    db: &Db,
    id: Id,
    r: NewReview,
    attrib: &Attribution,
) -> Result<Reviewed, IdeaError> {
    let attrib = attrib.clone();
    db.write(move |tx| atomic(tx, |tx| review_tx(tx, id, &r, &attrib)))
        .await?
}

/// Шаги [`review_idea`] в транзакции писателя.
fn review_tx(
    tx: &Connection,
    id: Id,
    r: &NewReview,
    attrib: &Attribution,
) -> Result<Result<Reviewed, IdeaError>, StoreError> {
    let idea = load_entry(tx, id)?;
    if idea.deleted_at.is_some() {
        return Ok(Err(IdeaError::IdeaDeleted));
    }
    ensure_not_report(&idea)?;
    if idea.kind != EntryKind::Idea {
        return Ok(Err(IdeaError::NotIdea));
    }
    let mut data = IdeaData::from_stored(&idea.data);
    let key = match check_key(r.key.clone()) {
        Ok(k) => k,
        Err(StoreError::BadInput(m)) => return Ok(Err(IdeaError::ReviewInput(m))),
        Err(e) => return Err(e),
    };
    if let Some(prev) = data.review.as_ref().filter(|p| {
        key.is_some() && p.key == key && p.by_user == r.by_user && p.outcome == r.outcome
    }) {
        let task = prev
            .task
            .as_deref()
            .and_then(|t| t.strip_prefix("k:")?.parse::<Id>().ok())
            .map(|t| live_opt(tx, t))
            .transpose()?
            .flatten();
        return Ok(Ok(Reviewed {
            idea,
            task,
            replay: true,
        }));
    }
    if data.origin != IdeaOrigin::Human {
        return Ok(Err(IdeaError::AgentOrigin));
    }
    let cur = idea.status.unwrap_or(EntryStatus::New);
    if cur != EntryStatus::New {
        return Ok(Err(IdeaError::Transition {
            from: cur,
            to: "review",
        }));
    }
    let (summary, missing, task) = match check_review_fields(r) {
        Ok(v) => v,
        Err(e) => return Ok(Err(e)),
    };
    let (basis, done_ref) = match review_refs(tx, "basis", &r.basis, &idea, false)? {
        Ok(v) => v,
        Err(e) => return Ok(Err(e)),
    };
    let (similar, _) = match review_refs(tx, "similar", &r.similar, &idea, true)? {
        Ok(v) => v,
        Err(e) => return Ok(Err(e)),
    };
    if r.outcome == ReviewOutcome::Done && !done_ref {
        return Ok(Err(review_input("basis", REVIEW_DONE_REF_REQUIRED)));
    }
    let now = fmt_ts(crate::time::now());
    let at = parse_ts(&now)?;
    let task_id = match review_task_tx(tx, &idea, &data, task, attrib, &now)? {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    // у идеи `new` действующей задачи нет: оставшаяся от прежнего решения (снятая задача
    // отклонённой и возвращённой идеи) — в `prev_task_ids`, иначе хуки её статуса меняли бы идею
    if let Some(old) = data.task_id.take()
        && Some(old) != task_id
        && !data.prev_task_ids.contains(&old)
    {
        data.prev_task_ids.push(old);
    }
    data.prev_task_ids.retain(|p| Some(*p) != task_id);
    data.task_id = task_id;
    // «не раньше» имеет смысл только у отложенной идеи (как у `accept` и `decline`, §5.2)
    data.horizon = None;
    let to = match r.outcome {
        ReviewOutcome::Plan => EntryStatus::Accepted,
        ReviewOutcome::Return => {
            data.returned_at = Some(at);
            EntryStatus::Returned
        }
        ReviewOutcome::Decline => EntryStatus::Declined,
        ReviewOutcome::Done => {
            data.done_at = Some(at);
            EntryStatus::Done
        }
    };
    let session = r
        .session
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| attrib.session_id.clone());
    let review = Review {
        outcome: r.outcome,
        summary,
        missing,
        basis,
        similar,
        task: task_id.map(|t| format!("k:{t}")),
        by_user: r.by_user,
        session,
        key,
        at,
    };
    data.reviews.insert(0, review.clone());
    data.reviews.truncate(REVIEWS_MAX);
    data.review = Some(review);
    data.decided = Some(Decided {
        decision: r.outcome.into(),
        by: AnswerBy::Agent,
        channel: DecidedVia::Review,
        at,
    });
    revise_with(tx, &idea, Some(to), data.to_value(), None, attrib, &now)?;
    let task = task_id.map(|t| load_entry(tx, t)).transpose()?;
    Ok(Ok(Reviewed {
        idea: load_entry(tx, id)?,
        task,
        replay: false,
    }))
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
        let now_ts = crate::time::now();
        let now = fmt_ts(now_ts);
        d.origin = IdeaOrigin::Human;
        d.adopted_at = Some(now_ts);
        d.adopted_by = Some(user);
        revise_with(tx, &idea, idea.status, d.to_value(), None, &attrib, &now)?;
        Ok(Ok(load_entry(tx, id)?))
    })
    .await?
}

/// Все живые идеи, на которые указывают системные рёбра `derived_from` задачи: у задачи плана их
/// может быть несколько (разбор `plan` с открытой задачей, M6.5 §5). Порядок — по id идеи.
pub(crate) fn ideas_of_task(c: &Connection, task: Id) -> Result<Vec<Entry>, StoreError> {
    let ids: Vec<Id> = {
        let mut st = c.prepare_cached(
            "SELECT DISTINCT dst_id FROM edges
             WHERE src_kind = 'entry' AND src_id = ?1 AND dst_kind = 'entry'
               AND type = 'derived_from' AND origin = 'system' AND deleted_at IS NULL
             ORDER BY dst_id",
        )?;
        st.query_map([task], |r| r.get(0))?
            .collect::<Result<_, _>>()?
    };
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(e) = live_opt(c, id)?
            && e.kind == EntryKind::Idea
        {
            out.push(e);
        }
    }
    Ok(out)
}

/// Причина устаревания вопроса «вернуть», когда задача идеи снова живая (§16, п. 4).
const TASK_REVIVED_REASON: &str = "задача возобновлена";

/// Задача идеи снова живая (переоткрыта из `dropped`, восстановлена из корзины; §16, п. 4):
/// открытый вопрос «вернуть» идеи `accepted` — `obsolete` с причиной «задача возобновлена»,
/// у идеи снимается `question_id` (новая ревизия). Возвращает идею после шага.
fn revive_task_tx(
    tx: &Connection,
    idea: Entry,
    attrib: &Attribution,
    now: &str,
) -> Result<Entry, StoreError> {
    let mut d = IdeaData::from_stored(&idea.data);
    if idea.status != Some(EntryStatus::Accepted) {
        return Ok(idea);
    }
    let Some(q) = open_question(tx, &d)? else {
        return Ok(idea);
    };
    if ProposalKind::from_stored(q.data.get("proposal")) != ProposalKind::Reopen {
        return Ok(idea);
    }
    revise_with(
        tx,
        &q,
        Some(EntryStatus::Obsolete),
        q.data.clone(),
        Some(TASK_REVIVED_REASON),
        attrib,
        now,
    )?;
    d.question_id = None;
    revise_with(tx, &idea, idea.status, d.to_value(), None, attrib, now)?;
    load_entry(tx, idea.id)
}

/// Идеи задачи `task`: живые идеи по рёбрам `derived_from` ([`ideas_of_task`]), у которых
/// `data.task_id` — эта задача (идея с ребром от прежней задачи, вернувшаяся и начатая заново, к
/// ней уже не относится).
fn ideas_with_task(tx: &Connection, task: Id) -> Result<Vec<Entry>, StoreError> {
    let mut ideas = ideas_of_task(tx, task)?;
    ideas.retain(|i| IdeaData::from_stored(&i.data).task_id == Some(task));
    Ok(ideas)
}

/// Смена статуса задачи (из `update_entry`, та же транзакция; §5.5): задача идеи `accepted`
/// → `done` — идея `done` с `done_at`; задача из `done` переоткрыта — идея снова `accepted`.
/// Так — с каждой идеей задачи ([`ideas_with_task`], M6.5 §5). Ревизия идеи — с атрибуцией
/// правки задачи. `dropped` идею не меняет (§5); задача из `dropped` в любой другой статус —
/// открытый вопрос «вернуть» устаревает ([`revive_task_tx`], §16, п. 4).
pub(crate) fn on_task_status_tx(
    tx: &Connection,
    task: &Entry,
    from: EntryStatus,
    to: EntryStatus,
    attrib: &Attribution,
    now: &str,
) -> Result<(), StoreError> {
    let revived = from == EntryStatus::Dropped && to != EntryStatus::Dropped;
    if from == to || !(revived || to == EntryStatus::Done || from == EntryStatus::Done) {
        return Ok(());
    }
    for mut idea in ideas_with_task(tx, task.id)? {
        if revived {
            idea = revive_task_tx(tx, idea, attrib, now)?;
        }
        let mut d = IdeaData::from_stored(&idea.data);
        let next = match (idea.status, to) {
            (Some(EntryStatus::Accepted), EntryStatus::Done) => {
                d.done_at = Some(parse_ts(now)?);
                EntryStatus::Done
            }
            (Some(EntryStatus::Done), s) if s != EntryStatus::Done => {
                d.done_at = None;
                EntryStatus::Accepted
            }
            _ => continue,
        };
        revise_with(tx, &idea, Some(next), d.to_value(), None, attrib, now)?;
    }
    Ok(())
}

/// Вопрос-предложение устарел или удалён (из `update_entry` и `delete_entry`; §5.3, §5.6):
/// идея `proposed` с этим `question_id` возвращается в `new`, `question_id` снимается. Вопрос
/// «вернуть» (§16, п. 4): у идеи `accepted` снимается только `question_id` — статус прежний,
/// идея снова кандидат.
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
    if d.question_id != Some(q.id) {
        return Ok(());
    }
    let next = match (
        idea.status,
        ProposalKind::from_stored(q.data.get("proposal")),
    ) {
        (Some(EntryStatus::Proposed), ProposalKind::Start) => EntryStatus::New,
        (Some(EntryStatus::Accepted), ProposalKind::Reopen) => EntryStatus::Accepted,
        _ => return Ok(()),
    };
    d.question_id = None;
    revise_with(tx, &idea, Some(next), d.to_value(), None, attrib, now)
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
///   идея не `proposed` — у вопроса «вернуть» не `accepted`, — удалена), становится
///   `obsolete` с причиной «предложение устарело»;
/// - идея `proposed` без открытого вопроса-предложения возвращается в `new` без `question_id`;
/// - идея `accepted` с предложением «вернуть», вопрос которого уже не открыт (устарел при её
///   удалении), теряет `question_id` — снова кандидат (§16, п. 4);
/// - восстановленная живая (не `dropped`) задача идеи — как переоткрытая: открытый вопрос
///   «вернуть» устаревает ([`revive_task_tx`]).
/// - восстановленная идея `accepted` при задаче `done` становится `done`, идея `done` при живой
///   или снятой задаче — снова `accepted` (финальное ревью M5.7): в корзине идея не видела смену
///   статуса задачи. `done_at` — время, когда задача стала `done` ([`task_done_ts`]; M6 §16), а
///   не время восстановления.
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
            let wanted = match ProposalKind::from_stored(e.data.get("proposal")) {
                ProposalKind::Start => EntryStatus::Proposed,
                ProposalKind::Reopen => EntryStatus::Accepted,
            };
            let current = live_opt(tx, iid)?.is_some_and(|i| {
                i.kind == EntryKind::Idea
                    && i.status == Some(wanted)
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
        // идея в корзине не видела смену статуса задачи: `accepted` ↔ `done` сверяется с
        // задачей (финальное ревью M5.7, Minor 1)
        EntryKind::Idea if matches!(e.status, Some(EntryStatus::Accepted | EntryStatus::Done)) => {
            let mut d = IdeaData::from_stored(&e.data);
            let mut status = e.status;
            let mut changed = false;
            if status == Some(EntryStatus::Accepted)
                && d.reopen_proposed()
                && open_question(tx, &d)?.is_none()
            {
                d.question_id = None;
                changed = true;
            }
            match (status, task_state(tx, &d)?) {
                (Some(EntryStatus::Accepted), Some(TaskState::Done)) => {
                    status = Some(EntryStatus::Done);
                    // ревизии `done` нет (статус поставлен мимо ревизий) — время восстановления
                    d.done_at = Some(match task_done_ts(tx, &d)? {
                        Some(ts) => ts,
                        None => parse_ts(now)?,
                    });
                    changed = true;
                }
                (Some(EntryStatus::Done), Some(TaskState::Live | TaskState::Dropped)) => {
                    status = Some(EntryStatus::Accepted);
                    d.done_at = None;
                    changed = true;
                }
                _ => {}
            }
            if !changed {
                return Ok(());
            }
            revise_with(tx, e, status, d.to_value(), None, attrib, now)
        }
        EntryKind::Task if e.status != Some(EntryStatus::Dropped) => {
            for idea in ideas_with_task(tx, e.id)? {
                revive_task_tx(tx, idea, attrib, now)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Кандидат проактивности §6: живая идея `origin: human` в `new`, в `deferred` с
/// `horizon ≤ today` (UTC) или в `accepted`, чья задача снята или удалена
/// ([`task_released`]), без открытого предложения «вернуть» ([`IdeaData::reopen_proposed`];
/// §16, п. 1). `task` — состояние задачи идеи ([`idea_task_state`]); у `accepted` вызывающий
/// обязан его передать (`None` у `accepted` — не кандидат). Тот же предикат — у дайджеста и
/// подсказки `prompt`.
pub fn idea_candidate(e: &Entry, task: Option<TaskState>, today: Date) -> bool {
    if e.kind != EntryKind::Idea || e.deleted_at.is_some() {
        return false;
    }
    let d = IdeaData::from_stored(&e.data);
    d.origin == IdeaOrigin::Human
        && match e.status {
            Some(EntryStatus::New) => true,
            Some(EntryStatus::Deferred) => d.horizon.is_some_and(|h| h <= today),
            // у `accepted` `task: None` — состояние не передано: не кандидат (ревью задачи 7,
            // M2); задачи нет в данных — `Some(Deleted)` от [`idea_task_state`]
            Some(EntryStatus::Accepted) => {
                task.is_some_and(|t| task_released(Some(t))) && !d.reopen_proposed()
            }
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

/// Кандидаты §6 пространств `spaces` ([`idea_candidate`]), кроме записей из `seen`: сначала
/// `new` и `deferred`, затем `accepted` со снятой или удалённой задачей (§16, п. 1); внутри —
/// по `priority` (`high`, `normal`, `low`), затем `updated_at` новые первыми; не больше
/// `limit`.
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
    // в выдаче SQL не больше `seen` лишних: снятая задача и открытый вопрос «вернуть»
    // отсекаются уже в SQL (иначе предложенные идеи с новым `updated_at` вытесняли бы прочие
    // под `LIMIT`), предикат — вторая линия
    let fetch = limit.saturating_add(seen.len());
    db.read(move |c| {
        let mut st = c.prepare(&format!(
            "SELECT {ENTRY_COLS} FROM entries e INDEXED BY entries_feed
             WHERE e.space_id IN ({}) AND e.kind = 'idea' AND e.deleted_at IS NULL
               AND json_extract(e.data, '$.origin') = 'human'
               AND (e.status = 'new'
                    OR (e.status = 'deferred' AND json_extract(e.data, '$.horizon') <= ?{})
                    OR (e.status = 'accepted' AND {RELEASED_SQL}
                        AND {NOT_REOPEN_PROPOSED_SQL}))
             ORDER BY e.status = 'accepted',
                      CASE json_extract(e.data, '$.priority')
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
            if seen.contains(&e.id) {
                continue;
            }
            let task = if e.status == Some(EntryStatus::Accepted) {
                let d = IdeaData::from_stored(&e.data);
                Some(task_state(c, &d)?.unwrap_or(TaskState::Deleted))
            } else {
                None
            };
            if idea_candidate(&e, task, today) {
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
        assert_eq!(
            proposal_title("Кэш", ProposalKind::Start),
            "Начать идею «Кэш»?"
        );
        assert_eq!(
            proposal_title("Кэш", ProposalKind::Reopen),
            "Вернуть идею «Кэш»?"
        );
        let t = proposal_title(&"я".repeat(600), ProposalKind::Reopen);
        assert_eq!(t.chars().count(), TITLE_MAX_CHARS);
        assert!(t.ends_with("…»?"));
    }

    #[test]
    fn decision_options_match() {
        for kind in [ProposalKind::Start, ProposalKind::Reopen] {
            for (k, _) in kind.options() {
                assert_eq!(kind.decision(k).unwrap().as_str(), *k);
                assert!(kind.keys_text().contains(k), "{kind:?}: {k}");
            }
        }
        assert_eq!(ProposalKind::Start.decision("reopen"), None);
        assert_eq!(ProposalKind::Reopen.decision("accept"), None);
        assert_eq!(ProposalKind::Reopen.decision("defer"), None);
        assert_eq!(ProposalKind::from_stored(None), ProposalKind::Start);
        assert_eq!(
            ProposalKind::from_stored(Some(&json!("reopen"))),
            ProposalKind::Reopen
        );
    }

    /// Предложение «вернуть» — `question_id` и `proposed_at` позже последнего решения.
    #[test]
    fn reopen_proposed_by_time() {
        let at = |s: &str| s.parse::<Timestamp>().unwrap();
        let mut d = IdeaData {
            origin: IdeaOrigin::Human,
            question_id: Some(Id::new()),
            proposed_at: Some(at("2026-11-04T10:00:00Z")),
            decided: Some(Decided {
                decision: DecidedAs::Accept,
                by: AnswerBy::Human,
                channel: DecidedVia::Webui,
                at: at("2026-11-04T11:00:00Z"),
            }),
            ..IdeaData::default()
        };
        // вопрос «начать», отвеченный решением, — не предложение «вернуть»
        assert!(!d.reopen_proposed());
        d.proposed_at = Some(at("2026-11-04T12:00:00Z"));
        assert!(d.reopen_proposed());
        d.question_id = None;
        assert!(!d.reopen_proposed());
    }

    #[test]
    fn released_only_dropped_or_deleted() {
        assert!(task_released(None));
        assert!(task_released(Some(TaskState::Dropped)));
        assert!(task_released(Some(TaskState::Deleted)));
        assert!(!task_released(Some(TaskState::Live)));
        assert!(!task_released(Some(TaskState::Done)));
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
