//! Журнал задач (спецификация M4.5 §3): записи `task` — план в `body`, ход работы в `data`.
//!
//! - [`TaskData`] — `data` задачи: `next_step`, `blocked_by`, `owner`, `links`, `estimate_min`,
//!   `priority` (M6.5 §5: по нему агент выбирает следующий пункт плана) ведёт клиент;
//!   `claimed_by`, `started_at`, `done_at` — только сервер (клиентская `data` с этими полями
//!   отклоняется). Правка `data` задачи — слияние на верхнем уровне (`null` снимает поле);
//! - [`check_task_transition`] — переходы статусов: между `open`, `in_progress`, `blocked` —
//!   любые; в `done`/`dropped` — из любого; из `done`/`dropped` — только в `open` и с причиной.
//!   `started_at` ставится при первом переходе в `in_progress`, `done_at` — при `done` (снимается
//!   при переоткрытии), закрытие (`done`/`dropped`) снимает захват, `next_step` и `blocked_by`
//!   (явные значения той же правки сильнее; M6.6, замечание 35);
//! - захват ([`claim_task`], [`release_task`]) — одна транзакция писателя с проверкой текущего
//!   `claimed_by`; ревизию записи не создаёт (меняется только `entries.data`), аудит ведёт
//!   вызывающий (сервер: `entry.claim`, `entry.release` в `system.db`);
//! - [`claimed_tasks`] — задачи, захваченные сессией (для `GET /sessions/{id}` и хука
//!   `checkpoint`);
//! - [`open_tasks`] — число открытых задач и заголовки незахваченных для строки дайджеста
//!   «Открыто задач: N — …» (M6.6 §4.2);
//! - [`task_tree`] — дерево подзадач `part_of` пространства (M7 §5.4 п. 1, задача 5 M7.2):
//!   одним запросом, циклы — компонентами сильной связности ([`TaskTree::build`]).

use std::cmp::Reverse;
use std::collections::hash_map::Entry as MapEntry;
use std::collections::{HashMap, HashSet};

use jiff::{SignedDuration, Timestamp};
use rusqlite::{params, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use zam_core::Id;

use crate::entries::{EntryKind, EntryStatus, load_entry};
use crate::ideas::{IdeaPriority, priority_opt};
use crate::time::{fmt_ts, ser_ts};
use crate::{Db, StoreError};

/// Предел `next_step` и `blocked_by`, символов.
pub const TASK_TEXT_MAX_CHARS: usize = 1000;
/// Ссылок в `links` и длина ссылки **[решение исполнителя]**: пределы в спецификации не заданы.
pub const TASK_LINKS_MAX: usize = 50;
pub const TASK_LINK_MAX_CHARS: usize = 1000;
/// Предел срока захвата, минут.
pub const CLAIM_TTL_MAX_MIN: u32 = 1440;
/// Срок захвата по умолчанию, минут.
pub const CLAIM_TTL_DEFAULT_MIN: u32 = 120;
/// Предел `estimate_min` (минут в году) **[решение исполнителя]**.
pub const ESTIMATE_MAX_MIN: u32 = 525_600;

/// Кто ведёт задачу.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Owner {
    Agent,
    Human,
}

/// Захват задачи сессией до момента `until`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub session: String,
    pub agent: Option<String>,
    #[serde(serialize_with = "ser_ts")]
    pub until: Timestamp,
}

impl Claim {
    /// Захват действует на момент `now`.
    pub fn live(&self, now: Timestamp) -> bool {
        self.until > now
    }
}

/// Разобранная `data` задачи.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskData {
    pub next_step: Option<String>,
    pub blocked_by: Option<String>,
    pub owner: Option<Owner>,
    pub links: Vec<String>,
    pub claimed_by: Option<Claim>,
    pub started_at: Option<Timestamp>,
    pub done_at: Option<Timestamp>,
    pub estimate_min: Option<u32>,
    /// Приоритет пункта плана (те же значения, что у идеи); нет — без приоритета.
    pub priority: Option<IdeaPriority>,
}

/// Поля, которые ведёт клиент.
const CLIENT_KEYS: [&str; 6] = [
    "next_step",
    "blocked_by",
    "owner",
    "links",
    "estimate_min",
    "priority",
];
/// Поля, которые ведёт сервер.
const SERVER_KEYS: [&str; 3] = ["claimed_by", "started_at", "done_at"];

/// Текстовое поле: `null` и пустая строка — снять, иначе до [`TASK_TEXT_MAX_CHARS`] символов.
fn text_field(v: &Value, field: &str) -> Result<Option<String>, String> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => {
            let s = s.trim();
            if s.chars().count() > TASK_TEXT_MAX_CHARS {
                return Err(format!(
                    "data.{field}: не больше {TASK_TEXT_MAX_CHARS} символов"
                ));
            }
            Ok((!s.is_empty()).then(|| s.to_string()))
        }
        _ => Err(format!("data.{field}: ожидается строка")),
    }
}

pub(crate) fn links_field(v: &Value) -> Result<Vec<String>, String> {
    if v.is_null() {
        return Ok(Vec::new());
    }
    let a = v
        .as_array()
        .ok_or_else(|| "data.links: ожидается массив строк".to_string())?;
    let mut out: Vec<String> = Vec::with_capacity(a.len());
    for x in a {
        let s = x
            .as_str()
            .ok_or_else(|| "data.links: ожидается массив строк".to_string())?
            .trim();
        if s.chars().count() > TASK_LINK_MAX_CHARS {
            return Err(format!(
                "data.links: строки до {TASK_LINK_MAX_CHARS} символов"
            ));
        }
        if !s.is_empty() && !out.iter().any(|o| o == s) {
            out.push(s.to_string());
        }
    }
    if out.len() > TASK_LINKS_MAX {
        return Err(format!("data.links: не больше {TASK_LINKS_MAX} элементов"));
    }
    Ok(out)
}

impl TaskData {
    /// `data` из базы; повреждённые поля пропускаются (запись всегда читается).
    pub fn from_stored(data: &Value) -> TaskData {
        let get = |k: &str| data.get(k).filter(|v| !v.is_null());
        TaskData {
            next_step: get("next_step").and_then(|v| text_field(v, "next_step").ok().flatten()),
            blocked_by: get("blocked_by").and_then(|v| text_field(v, "blocked_by").ok().flatten()),
            owner: get("owner").and_then(|v| serde_json::from_value(v.clone()).ok()),
            links: get("links")
                .and_then(|v| links_field(v).ok())
                .unwrap_or_default(),
            claimed_by: get("claimed_by").and_then(|v| serde_json::from_value(v.clone()).ok()),
            started_at: get("started_at").and_then(|v| v.as_str()?.parse().ok()),
            done_at: get("done_at").and_then(|v| v.as_str()?.parse().ok()),
            estimate_min: get("estimate_min").and_then(|v| u32::try_from(v.as_u64()?).ok()),
            priority: get("priority").and_then(|v| priority_opt(v).ok().flatten()),
        }
    }

    /// Применяет клиентский патч `data`: поля [`CLIENT_KEYS`], `null` снимает поле. Поля сервера
    /// и неизвестные — ошибка с текстом `data.<поле>: …` (422).
    pub fn apply_patch(&mut self, patch: &Value) -> Result<(), String> {
        let o = patch
            .as_object()
            .ok_or_else(|| "data должно быть JSON-объектом".to_string())?;
        for k in o.keys() {
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
                "next_step" => next.next_step = text_field(v, k)?,
                "blocked_by" => next.blocked_by = text_field(v, k)?,
                "owner" => {
                    next.owner = match v {
                        Value::Null => None,
                        Value::String(_) => Some(
                            serde_json::from_value(v.clone())
                                .map_err(|_| "data.owner: agent или human".to_string())?,
                        ),
                        _ => return Err("data.owner: agent или human".into()),
                    }
                }
                "links" => next.links = links_field(v)?,
                "estimate_min" => {
                    next.estimate_min = match v {
                        Value::Null => None,
                        _ => Some(
                            v.as_u64()
                                .and_then(|n| u32::try_from(n).ok())
                                .filter(|n| (1..=ESTIMATE_MAX_MIN).contains(n))
                                .ok_or_else(|| {
                                    format!("data.estimate_min: целое от 1 до {ESTIMATE_MAX_MIN}")
                                })?,
                        ),
                    }
                }
                "priority" => next.priority = priority_opt(v)?,
                _ => unreachable!("ключи проверены выше"),
            }
        }
        *self = next;
        Ok(())
    }

    /// Нормализованная `data` для записи: только заданные поля.
    pub fn to_value(&self) -> Value {
        let mut o = Map::new();
        if let Some(s) = &self.next_step {
            o.insert("next_step".into(), json!(s));
        }
        if let Some(s) = &self.blocked_by {
            o.insert("blocked_by".into(), json!(s));
        }
        if let Some(x) = self.owner {
            o.insert("owner".into(), json!(x));
        }
        if !self.links.is_empty() {
            o.insert("links".into(), json!(self.links));
        }
        if let Some(c) = &self.claimed_by {
            o.insert("claimed_by".into(), json!(c));
        }
        if let Some(t) = self.started_at {
            o.insert("started_at".into(), json!(fmt_ts(t)));
        }
        if let Some(t) = self.done_at {
            o.insert("done_at".into(), json!(fmt_ts(t)));
        }
        if let Some(n) = self.estimate_min {
            o.insert("estimate_min".into(), json!(n));
        }
        if let Some(p) = self.priority {
            o.insert("priority".into(), json!(p.as_str()));
        }
        Value::Object(o)
    }

    /// Длительность `done_at − started_at`, целых минут (не меньше 0); нет любой метки — `None`.
    pub fn duration_min(&self) -> Option<i64> {
        let d = self.done_at?.duration_since(self.started_at?);
        Some(d.as_secs().max(0) / 60)
    }

    /// Следы смены статуса: `started_at` при первом `in_progress`, `done_at` при `done` (и снятие
    /// при переоткрытии), снятие захвата при закрытии, `blocked_by` теряется при выходе из
    /// `blocked`; переход в `done`/`dropped` снимает `next_step` и `blocked_by` (M6.6, замечание
    /// 35: у выполненной задачи нет «далее»; прежнее — в истории ревизий), повтор того же
    /// статуса — не переход. Явное значение той же правки накладывается после —
    /// [`TaskData::apply_patch`].
    pub fn on_transition(&mut self, from: Option<EntryStatus>, to: EntryStatus, now: Timestamp) {
        let from_status = from.unwrap_or(EntryStatus::Open);
        if matches!(to, EntryStatus::Done | EntryStatus::Dropped) && from_status != to {
            self.next_step = None;
            self.blocked_by = None;
        }
        match to {
            EntryStatus::InProgress if self.started_at.is_none() => self.started_at = Some(now),
            EntryStatus::Done => {
                // повторный done → done момент завершения не сдвигает
                if self.done_at.is_none() {
                    self.done_at = Some(now);
                }
                self.claimed_by = None;
            }
            EntryStatus::Dropped => self.claimed_by = None,
            EntryStatus::Open if matches!(from, Some(EntryStatus::Done | EntryStatus::Dropped)) => {
                self.done_at = None;
            }
            _ => {}
        }
        if to != EntryStatus::Blocked && from == Some(EntryStatus::Blocked) {
            self.blocked_by = None;
        }
    }
}

/// Нормализация клиентской `data` новой задачи: `{}` + патч.
pub fn check_task(data: &Value) -> Result<TaskData, String> {
    let mut t = TaskData::default();
    t.apply_patch(data)?;
    Ok(t)
}

/// Переход статуса задачи. `Err` — машинный код: `reason-required` (переоткрытие без причины),
/// `transition-not-allowed` (из `done`/`dropped` — не в `open`) или `not-a-task-status`.
pub fn check_task_transition(
    from: EntryStatus,
    to: EntryStatus,
    reason: Option<&str>,
) -> Result<(), &'static str> {
    use EntryStatus::*;
    if !matches!(to, Open | InProgress | Blocked | Done | Dropped)
        || !matches!(from, Open | InProgress | Blocked | Done | Dropped)
    {
        return Err("not-a-task-status");
    }
    if from == to {
        return Ok(());
    }
    match (from, to) {
        (Done | Dropped, Open) => {
            if reason.is_some_and(|r| !r.trim().is_empty()) {
                Ok(())
            } else {
                Err("reason-required")
            }
        }
        (Done | Dropped, _) => Err("transition-not-allowed"),
        _ => Ok(()),
    }
}

/// Ошибки операций над задачей.
#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    /// Задача захвачена другой сессией: 409 `task-claimed`.
    #[error("задача уже захвачена другой сессией")]
    Claimed(Claim),
    /// Снять чужой захват может только человек: 403.
    #[error("захват принадлежит другой сессии: снять его может только человек")]
    NotClaimedBySession,
    /// Запись не задача: 422.
    #[error("запись не является задачей")]
    NotTask,
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Живая задача по id: нет — `NotFound`, отчёт — `ReportImmutable`, другой вид — `NotTask`.
fn live_task(c: &rusqlite::Connection, id: Id) -> Result<crate::Entry, TaskError> {
    let e = load_entry(c, id)?;
    if e.deleted_at.is_some() {
        return Err(StoreError::NotFound.into());
    }
    crate::entries::ensure_not_report(&e)?;
    if e.kind != EntryKind::Task {
        return Err(TaskError::NotTask);
    }
    Ok(e)
}

fn put_data(c: &rusqlite::Connection, id: Id, t: &TaskData) -> Result<(), StoreError> {
    c.execute(
        "UPDATE entries SET data = ?2 WHERE id = ?1",
        params![id, t.to_value().to_string()],
    )?;
    Ok(())
}

/// Захватывает задачу сессией `session` на `ttl_min` минут (1..=1440): свободна, срок прошлого
/// захвата истёк или захват той же сессии — `claimed_by` ставится или продлевается; иначе
/// [`TaskError::Claimed`] с действующим захватом. Закрытую задачу (`done`, `dropped`) не
/// захватить (`Conflict`). Ревизию не создаёт.
pub async fn claim_task(
    db: &Db,
    id: Id,
    session: &str,
    agent: Option<&str>,
    ttl_min: u32,
    now: Timestamp,
) -> Result<Claim, TaskError> {
    let session = session.trim().to_string();
    if session.is_empty() {
        return Err(StoreError::BadInput("пустая сессия".into()).into());
    }
    if !(1..=CLAIM_TTL_MAX_MIN).contains(&ttl_min) {
        return Err(StoreError::BadInput(format!("ttl_min: от 1 до {CLAIM_TTL_MAX_MIN}")).into());
    }
    let agent = agent
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string);
    let until = now + SignedDuration::from_mins(i64::from(ttl_min));
    db.write(move |tx| {
        let e = match live_task(tx, id) {
            Ok(e) => e,
            Err(e) => return Ok(Err(e)),
        };
        if matches!(e.status, Some(EntryStatus::Done | EntryStatus::Dropped)) {
            return Err(StoreError::Conflict(
                "задача закрыта: захватить её нельзя".into(),
            ));
        }
        let mut t = TaskData::from_stored(&e.data);
        if let Some(c) = &t.claimed_by
            && c.live(now)
            && c.session != session
        {
            return Ok(Err(TaskError::Claimed(c.clone())));
        }
        let claim = Claim {
            session,
            agent,
            until,
        };
        t.claimed_by = Some(claim.clone());
        put_data(tx, id, &t)?;
        Ok(Ok(claim))
    })
    .await?
}

/// Снимает захват: свой — любой сессией-владельцем, чужой действующий — только `human`
/// ([`TaskError::NotClaimedBySession`] иначе). Захвата нет или он истёк — ничего не делает.
/// Возвращает снятый захват.
pub async fn release_task(
    db: &Db,
    id: Id,
    session: &str,
    human: bool,
) -> Result<Option<Claim>, TaskError> {
    let session = session.trim().to_string();
    db.write(move |tx| {
        let e = match live_task(tx, id) {
            Ok(e) => e,
            Err(e) => return Ok(Err(e)),
        };
        let mut t = TaskData::from_stored(&e.data);
        let Some(c) = t.claimed_by.clone() else {
            return Ok(Ok(None));
        };
        if c.live(crate::time::now()) && c.session != session && !human {
            return Ok(Err(TaskError::NotClaimedBySession));
        }
        t.claimed_by = None;
        put_data(tx, id, &t)?;
        Ok(Ok(Some(c)))
    })
    .await?
}

/// Задача, захваченная сессией.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaimedTask {
    pub id: Id,
    pub title: String,
    pub status: EntryStatus,
}

/// Живые задачи (`open`, `in_progress`, `blocked`) пространств `spaces` с действующим захватом
/// сессии `session`, новые первыми. Пространства — область пользователя запроса (у базы
/// владельца чужого решения — только пространство решения; финальное ревью M4.5, M-1); отбор
/// идёт по индексу `entries_feed (space_id, kind, …)`, а не проходом по всем записям базы —
/// запрос звучит на каждом `Stop` хука `checkpoint`. Пустой список — пусто без запроса.
pub async fn claimed_tasks(
    db: &Db,
    spaces: &[Id],
    session: &str,
    now: Timestamp,
) -> Result<Vec<ClaimedTask>, StoreError> {
    if spaces.is_empty() {
        return Ok(Vec::new());
    }
    let mut args: Vec<String> = spaces.iter().map(ToString::to_string).collect();
    let n = args.len();
    args.push(session.to_string());
    args.push(fmt_ts(now));
    db.read(move |c| {
        let mut st = c.prepare(&claimed_sql(n))?;
        let rows = st.query_map(params_from_iter(args), |r| {
            Ok(ClaimedTask {
                id: r.get(0)?,
                title: r.get(1)?,
                status: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .await
}

/// Открытая задача строки дайджеста ([`open_tasks`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTask {
    pub id: Id,
    pub title: String,
    pub priority: Option<IdeaPriority>,
    pub created_at: Timestamp,
}

impl OpenTask {
    /// Ключ порядка [`open_tasks`] — для слияния заголовков нескольких баз, по возрастанию:
    /// `priority` по [`IdeaPriority::rank`] (`high` → `normal` → `low`; без приоритета —
    /// последней: `None` меньше любого ранга), затем новые первыми. Тот же ранг — в SQL
    /// `open_tasks` (`priority_rank_sql`), равенство порядков проверяет тест хранилища.
    pub fn order_key(&self) -> Reverse<(Option<u8>, Timestamp, Id)> {
        let rank = self.priority.map(IdeaPriority::rank);
        Reverse((rank, self.created_at, self.id))
    }
}

/// Ранг `data.priority` в SQL — те же числа, что [`IdeaPriority::rank`]; без приоритета —
/// `NULL` (в `ORDER BY … DESC` — последним, как `None` в [`OpenTask::order_key`]).
fn priority_rank_sql() -> String {
    let arms: String = IdeaPriority::ALL
        .iter()
        .map(|p| format!("WHEN '{}' THEN {} ", p.as_str(), p.rank()))
        .collect();
    format!("CASE json_extract(data, '$.priority') {arms}END")
}

/// Отбор [`open_tasks`]: живые задачи `open` пространств `?1..?n`.
fn open_tasks_where(n: usize) -> String {
    format!(
        "FROM entries INDEXED BY entries_feed
         WHERE space_id IN ({}) AND kind = 'task' AND status = 'open' AND deleted_at IS NULL",
        crate::questions::in_list(n)
    )
}

/// Открытые задачи пространств `spaces` (M6.6 §4.2, строка дайджеста «Открыто задач: N — …»):
/// число всех живых задач `open` и до `titles` задач без живого захвата на текущий момент
/// ([`crate::time::now`]; захваченные дайджест уже показывает строками раздела задач) в порядке
/// [`OpenTask::order_key`] — `data.priority` (`high` → `normal` → `low` → нет), затем новые
/// первыми. Захват — то же сравнение `claimed_by.until`, что у [`claimed_tasks`]. Вызывающий,
/// объединяющий несколько баз, сортирует заголовки по [`OpenTask::order_key`] сам. Пустой
/// список пространств — `(0, [])` без запроса.
pub async fn open_tasks(
    db: &Db,
    spaces: &[Id],
    titles: usize,
) -> Result<(usize, Vec<OpenTask>), StoreError> {
    if spaces.is_empty() {
        return Ok((0, Vec::new()));
    }
    let n = spaces.len();
    let mut args: Vec<String> = spaces.iter().map(ToString::to_string).collect();
    db.read(move |c| {
        let from = open_tasks_where(n);
        let count: i64 = c
            .prepare(&format!("SELECT COUNT(*) {from}"))?
            .query_row(params_from_iter(args.iter()), |r| r.get(0))?;
        // захват жив — `until` позже момента запроса (нет захвата — NULL, задача свободна)
        args.push(fmt_ts(crate::time::now()));
        let mut st = c.prepare(&format!(
            "SELECT id, title, data, created_at {from}
               AND NOT ifnull(json_extract(data, '$.claimed_by.until') > ?{}, 0)
             ORDER BY {} DESC, created_at DESC, id DESC
             LIMIT {titles}",
            n + 1,
            priority_rank_sql()
        ))?;
        let rows = st
            .query_map(params_from_iter(args), |r| {
                let data: String = r.get(2)?;
                let data = serde_json::from_str(&data).unwrap_or(Value::Null);
                Ok(OpenTask {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    priority: TaskData::from_stored(&data).priority,
                    created_at: crate::time::ts_at(r, 3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((usize::try_from(count).unwrap_or(0), rows))
    })
    .await
}

/// SQL [`claimed_tasks`] для `n` пространств: `?1..?n` — пространства, затем сессия и момент.
fn claimed_sql(n: usize) -> String {
    format!(
        "SELECT id, title, status FROM entries INDEXED BY entries_feed
         WHERE space_id IN ({}) AND kind = 'task' AND deleted_at IS NULL
           AND status IN ('open', 'in_progress', 'blocked')
           AND json_extract(data, '$.claimed_by.session') = ?{}
           AND json_extract(data, '$.claimed_by.until') > ?{}
         ORDER BY updated_at DESC, id DESC",
        crate::questions::in_list(n),
        n + 1,
        n + 2
    )
}

/// Задача в дереве подзадач ([`TaskTree`]): то, что нужно полям ответа о родителе и подзадачах
/// (`parent`, `subtasks`, `last_activity_at`, строки подзадач карточки) без запроса на задачу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskNode {
    pub title: String,
    /// Статус задачи (запись без статуса — `open`).
    pub status: EntryStatus,
    pub updated_at: Timestamp,
    /// Срок записанного захвата `data.claimed_by.until` (в том числе истёкшего).
    pub claim_until: Option<Timestamp>,
    /// Агент записанного захвата `data.claimed_by.agent` (строки подзадач: «в работе у …»)
    /// **[решение исполнителя, задача 5 M7.2]**: без него `claimed {agent, until}` строки
    /// потребовал бы второго запроса.
    pub claim_agent: Option<String>,
}

impl TaskNode {
    /// Захват действует на момент `now` (то же сравнение, что [`Claim::live`]).
    pub fn claimed(&self, now: Timestamp) -> bool {
        self.claim_until.is_some_and(|u| u > now)
    }
}

/// Сводка прямых подзадач общей задачи (M7 §5.4 п. 2): всего и по статусам; прогресс —
/// `done/(total − dropped)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SubtaskSummary {
    pub total: usize,
    pub done: usize,
    pub dropped: usize,
    pub open: usize,
    pub in_progress: usize,
    pub blocked: usize,
}

/// Дерево подзадач `part_of` пространства (M7 §5.4 п. 1): подзадача — живая задача `S` с ребром
/// `S part_of P` к живой задаче `P` того же пространства. Родитель — самое раннее ребро
/// ([`TaskTree::parent`]); задача с двумя родителями — в списках обоих
/// ([`TaskTree::children`]). Рёбра задачи на цикле `part_of` любой длины не учитываются: она —
/// верхнего уровня и ничья не подзадача (решение 21). Верхний уровень — ровно задачи без
/// учтённого ребра к родителю.
#[derive(Debug, Clone, Default)]
pub struct TaskTree {
    /// Живые задачи пространства.
    pub nodes: HashMap<Id, TaskNode>,
    /// Подзадача → родитель (самое раннее учтённое ребро).
    pub parent: HashMap<Id, Id>,
    /// Общая задача → прямые подзадачи в порядке создания ребра (порядок шагов плана).
    pub children: HashMap<Id, Vec<Id>>,
}

impl TaskTree {
    /// Дерево по задачам `nodes` и рёбрам `edges` — пары `(подзадача, родитель)` в порядке
    /// создания ребра. Рёбра, у которых конец — не задача из `nodes`, и повторы пропускаются.
    /// Циклы ищутся по всем оставшимся рёбрам — компоненты сильной связности (Тарьян, без
    /// рекурсии и без предела длины), O(V + E): рёбра задач компоненты больше одной задачи (и
    /// петли) в дерево не входят.
    pub fn build(nodes: HashMap<Id, TaskNode>, edges: &[(Id, Id)]) -> TaskTree {
        let mut seen: HashSet<(Id, Id)> = HashSet::new();
        let edges: Vec<(Id, Id)> = edges
            .iter()
            .copied()
            .filter(|(s, p)| {
                nodes.contains_key(s) && nodes.contains_key(p) && seen.insert((*s, *p))
            })
            .collect();
        let mut adj: HashMap<Id, Vec<Id>> = HashMap::new();
        for (s, p) in &edges {
            adj.entry(*s).or_default().push(*p);
        }
        let cyclic = on_cycles(&adj);
        let mut parent: HashMap<Id, Id> = HashMap::new();
        let mut children: HashMap<Id, Vec<Id>> = HashMap::new();
        for (s, p) in edges {
            if cyclic.contains(&s) {
                continue;
            }
            parent.entry(s).or_insert(p);
            children.entry(p).or_default().push(s);
        }
        TaskTree {
            nodes,
            parent,
            children,
        }
    }

    /// Прямые подзадачи `id` в порядке шагов (нет — пусто).
    pub fn children_of(&self, id: Id) -> &[Id] {
        self.children.get(&id).map_or(&[], Vec::as_slice)
    }

    /// Сводка прямых подзадач `id`; подзадач нет — `None`.
    pub fn summary(&self, id: Id) -> Option<SubtaskSummary> {
        let kids = self.children_of(id);
        if kids.is_empty() {
            return None;
        }
        let mut s = SubtaskSummary {
            total: kids.len(),
            ..SubtaskSummary::default()
        };
        for k in kids {
            match self.nodes.get(k).map(|n| n.status) {
                Some(EntryStatus::Done) => s.done += 1,
                Some(EntryStatus::Dropped) => s.dropped += 1,
                Some(EntryStatus::InProgress) => s.in_progress += 1,
                Some(EntryStatus::Blocked) => s.blocked += 1,
                _ => s.open += 1,
            }
        }
        Some(s)
    }

    /// Последняя активность задачи `id` с собственным `updated_at` `own`: наибольшее из него и
    /// `updated_at` её прямых подзадач (захват ревизии не создаёт и `updated_at` не меняет).
    pub fn last_activity(&self, id: Id, own: Timestamp) -> Timestamp {
        self.children_of(id)
            .iter()
            .filter_map(|k| self.nodes.get(k).map(|n| n.updated_at))
            .fold(own, Timestamp::max)
    }

    /// Все подзадачи пространства (задачи с учтённым родителем) — `top_level=1` ленты их
    /// исключает.
    pub fn subtask_ids(&self) -> Vec<Id> {
        self.parent.keys().copied().collect()
    }
}

/// Число подзадач по рёбрам `edges` — парам `(подзадача, родитель)` между живыми задачами одного
/// пространства (M7.3 §6.14 п. 10, `task_steps` [`crate::space_counts`]): то же правило, что у
/// [`TaskTree::build`] — повторы не в счёт, рёбра задач на циклах `part_of` (и петли) не
/// учитываются, — то есть `TaskTree::build(…).parent.len()` без чтения самих задач.
pub(crate) fn subtask_count(edges: &[(Id, Id)]) -> usize {
    let mut seen: HashSet<(Id, Id)> = HashSet::new();
    let mut adj: HashMap<Id, Vec<Id>> = HashMap::new();
    for &(s, p) in edges {
        if seen.insert((s, p)) {
            adj.entry(s).or_default().push(p);
        }
    }
    let cyclic = on_cycles(&adj);
    adj.keys().filter(|s| !cyclic.contains(*s)).count()
}

/// Задачи на циклах графа `adj` (задача → её родители): компоненты сильной связности больше
/// одной вершины и петли — алгоритм Тарьяна с явным стеком (глубина графа стек потока не
/// расходует), O(V + E).
fn on_cycles(adj: &HashMap<Id, Vec<Id>>) -> HashSet<Id> {
    let mut index: HashMap<Id, usize> = HashMap::new();
    let mut low: HashMap<Id, usize> = HashMap::new();
    let mut stack: Vec<Id> = Vec::new();
    let mut on_stack: HashSet<Id> = HashSet::new();
    let mut out: HashSet<Id> = HashSet::new();
    let mut next = 0usize;
    let succ = |v: &Id| adj.get(v).map_or(&[][..], Vec::as_slice);
    for &root in adj.keys() {
        if index.contains_key(&root) {
            continue;
        }
        // кадры обхода: вершина и номер её следующего ребра
        let mut work: Vec<(Id, usize)> = vec![(root, 0)];
        index.insert(root, next);
        low.insert(root, next);
        next += 1;
        stack.push(root);
        on_stack.insert(root);
        while let Some(top) = work.last_mut() {
            let v = top.0;
            if let Some(&w) = succ(&v).get(top.1) {
                top.1 += 1;
                match index.entry(w) {
                    MapEntry::Vacant(slot) => {
                        slot.insert(next);
                        low.insert(w, next);
                        next += 1;
                        stack.push(w);
                        on_stack.insert(w);
                        work.push((w, 0));
                    }
                    MapEntry::Occupied(iw) if on_stack.contains(&w) => {
                        let lv = low.get_mut(&v).expect("вершина обхода");
                        *lv = (*lv).min(*iw.get());
                    }
                    MapEntry::Occupied(_) => {}
                }
                continue;
            }
            work.pop();
            let lv = low[&v];
            if let Some(&(u, _)) = work.last() {
                let lu = low.get_mut(&u).expect("вершина обхода");
                *lu = (*lu).min(lv);
            }
            if lv == index[&v] {
                // `v` — корень компоненты: она — вершины стека до `v` включительно
                let mut comp: Vec<Id> = Vec::new();
                while let Some(w) = stack.pop() {
                    on_stack.remove(&w);
                    comp.push(w);
                    if w == v {
                        break;
                    }
                }
                if comp.len() > 1 || succ(&v).contains(&v) {
                    out.extend(comp);
                }
            }
        }
    }
    out
}

/// Дерево подзадач пространства `space_id` (M7 §5.4 п. 1) одним запросом: живые задачи
/// пространства (индекс `entries_feed`) с их рёбрами `part_of` на записи (`edges_src`) в порядке
/// создания ребра; разбор — [`TaskTree::build`]. Рёбра на запись другого пространства или базы,
/// на удалённую задачу и удалённые рёбра в дерево не входят. Цена на 2 000 задач — журнал этапа
/// («M7.2 · Задача 5»).
pub async fn task_tree(db: &Db, space_id: Id) -> Result<TaskTree, StoreError> {
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT t.id, t.title, t.status, t.updated_at,
                    json_extract(t.data, '$.claimed_by.until'),
                    json_extract(t.data, '$.claimed_by.agent'), g.dst_id
             FROM entries t INDEXED BY entries_feed
             LEFT JOIN edges g ON g.src_kind = 'entry' AND g.src_id = t.id
                  AND g.type = 'part_of' AND g.dst_kind = 'entry' AND g.deleted_at IS NULL
             WHERE t.space_id = ?1 AND t.kind = 'task' AND t.deleted_at IS NULL
             ORDER BY g.created_at, g.rowid",
        )?;
        let mut rows = st.query([space_id])?;
        let mut nodes: HashMap<Id, TaskNode> = HashMap::new();
        let mut edges: Vec<(Id, Id)> = Vec::new();
        // повреждённые `data` и ссылки читаются как отсутствующие (как `TaskData::from_stored`)
        let text = |r: &rusqlite::Row<'_>, i: usize| -> rusqlite::Result<Option<String>> {
            Ok(r.get_ref(i)?.as_str().ok().map(str::to_string))
        };
        while let Some(r) = rows.next()? {
            let id: Id = r.get(0)?;
            // строка на каждое ребро задачи: узел — по первой
            if let MapEntry::Vacant(slot) = nodes.entry(id) {
                let status: Option<EntryStatus> = r.get(2)?;
                slot.insert(TaskNode {
                    title: r.get(1)?,
                    status: status.unwrap_or(EntryStatus::Open),
                    updated_at: crate::time::ts_at(r, 3)?,
                    claim_until: text(r, 4)?.and_then(|s| s.parse().ok()),
                    claim_agent: text(r, 5)?,
                });
            }
            if let Some(dst) = text(r, 6)?.and_then(|s| s.parse::<Id>().ok()) {
                edges.push((id, dst));
            }
        }
        Ok(TaskTree::build(nodes, &edges))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use EntryStatus::*;

    fn node(status: EntryStatus) -> TaskNode {
        TaskNode {
            title: "t".into(),
            status,
            updated_at: "2026-11-22T10:00:00Z".parse().unwrap(),
            claim_until: None,
            claim_agent: None,
        }
    }

    /// Задачи `n` штук со статусом `open`.
    fn tasks(n: usize) -> (Vec<Id>, HashMap<Id, TaskNode>) {
        let ids: Vec<Id> = (0..n).map(|_| Id::new()).collect();
        let nodes = ids.iter().map(|i| (*i, node(Open))).collect();
        (ids, nodes)
    }

    /// Родитель — самое раннее ребро, подзадача с двумя родителями — в списках обоих; порядок
    /// подзадач — порядок рёбер; повтор ребра и ребро на задачу вне пространства не в счёт.
    #[test]
    fn tree_parents_and_order() {
        let (t, nodes) = tasks(5);
        let outside = Id::new();
        let tree = TaskTree::build(
            nodes,
            &[
                (t[3], t[0]),
                (t[1], t[0]),
                (t[1], t[4]),
                (t[2], t[0]),
                (t[3], t[0]),
                (t[2], outside),
            ],
        );
        assert_eq!(tree.children_of(t[0]), [t[3], t[1], t[2]]);
        assert_eq!(tree.children_of(t[4]), [t[1]]);
        assert_eq!(tree.parent[&t[1]], t[0]);
        assert_eq!(tree.parent.len(), 3);
        assert!(tree.children_of(outside).is_empty());
        let mut subs = tree.subtask_ids();
        subs.sort();
        let mut want = vec![t[1], t[2], t[3]];
        want.sort();
        assert_eq!(subs, want);
    }

    /// Циклы длиной 2, 3 и 10 и петля: задачи цикла — верхнего уровня, их рёбра (и к общей вне
    /// цикла) не учитываются; задача вне цикла — подзадача задачи цикла.
    #[test]
    fn tree_cycles_any_length_stay_top_level() {
        let (t, mut nodes) = tasks(17);
        let lone = Id::new();
        nodes.insert(lone, node(Open));
        let mut edges = vec![(t[0], t[1]), (t[1], t[0])];
        edges.extend([(t[2], t[3]), (t[3], t[4]), (t[4], t[2])]);
        edges.extend((5..15).map(|i| (t[i], t[if i == 14 { 5 } else { i + 1 }])));
        // D (t[15]) — часть A (t[0]); задача цикла t[2] — ещё и часть общей t[16]
        edges.extend([(t[15], t[0]), (t[2], t[16]), (lone, lone)]);
        let tree = TaskTree::build(nodes, &edges);
        for (i, x) in t.iter().enumerate().take(15) {
            assert!(!tree.parent.contains_key(x), "задача цикла {i}");
        }
        assert!(!tree.parent.contains_key(&lone), "петля — цикл");
        assert_eq!(tree.children_of(t[0]), [t[15]]);
        assert_eq!(tree.parent[&t[15]], t[0]);
        assert!(tree.children_of(t[16]).is_empty());
        assert_eq!(tree.subtask_ids(), [t[15]]);
    }

    /// Длинная цепочка `part_of` (10 000 задач) и цикл той же длины — без переполнения стека:
    /// у цепочки родитель есть у всех, кроме корня; у цикла — ни у кого.
    #[test]
    fn tree_long_chain_and_long_cycle() {
        let n = 10_000;
        let (t, nodes) = tasks(n);
        let chain: Vec<(Id, Id)> = (1..n).map(|i| (t[i], t[i - 1])).collect();
        let tree = TaskTree::build(nodes.clone(), &chain);
        assert_eq!(tree.parent.len(), n - 1);
        let mut cycle = chain;
        cycle.push((t[0], t[n - 1]));
        let tree = TaskTree::build(nodes, &cycle);
        assert!(tree.parent.is_empty() && tree.children.is_empty());
    }

    /// `subtask_count` (число `task_steps`, M7.3 §6.14 п. 10) равен числу подзадач дерева
    /// [`TaskTree::build`] на тех же рёбрах: циклы разной длины, петли, повторы, два родителя,
    /// цепочки — 200 псевдослучайных графов (детерминированный ЛКГ) и графы тестов выше.
    #[test]
    fn subtask_count_matches_tree() {
        let (t, nodes) = tasks(17);
        let fixed: Vec<Vec<(Id, Id)>> = vec![
            vec![],
            vec![
                (t[3], t[0]),
                (t[1], t[0]),
                (t[1], t[4]),
                (t[2], t[0]),
                (t[3], t[0]),
            ],
            vec![(t[0], t[1]), (t[1], t[0]), (t[15], t[0]), (t[2], t[2])],
            (5..15)
                .map(|i| (t[i], t[if i == 14 { 5 } else { i + 1 }]))
                .chain([(t[16], t[5]), (t[4], t[16])])
                .collect(),
        ];
        for edges in &fixed {
            let tree = TaskTree::build(nodes.clone(), edges);
            assert_eq!(subtask_count(edges), tree.parent.len(), "{edges:?}");
        }
        let mut x: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = |m: usize| {
            x = x
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(x >> 33).unwrap() % m
        };
        for _ in 0..200 {
            let n = 2 + next(15);
            let edges: Vec<(Id, Id)> = (0..next(3 * n)).map(|_| (t[next(n)], t[next(n)])).collect();
            let tree = TaskTree::build(nodes.clone(), &edges);
            assert_eq!(subtask_count(&edges), tree.parent.len(), "{edges:?}");
        }
    }

    /// Сводка по статусам прямых подзадач и последняя активность — наибольшее `updated_at`.
    #[test]
    fn tree_summary_and_last_activity() {
        let (t, mut nodes) = tasks(7);
        for (i, s) in [Done, Done, Dropped, InProgress, Blocked]
            .into_iter()
            .enumerate()
        {
            nodes.get_mut(&t[i + 1]).unwrap().status = s;
        }
        let late: Timestamp = "2026-11-22T12:00:00Z".parse().unwrap();
        nodes.get_mut(&t[4]).unwrap().updated_at = late;
        let edges: Vec<(Id, Id)> = (1..7).map(|i| (t[i], t[0])).collect();
        let tree = TaskTree::build(nodes, &edges);
        assert_eq!(
            tree.summary(t[0]),
            Some(SubtaskSummary {
                total: 6,
                done: 2,
                dropped: 1,
                open: 1,
                in_progress: 1,
                blocked: 1,
            })
        );
        assert_eq!(tree.summary(t[1]), None);
        let own: Timestamp = "2026-11-22T11:00:00Z".parse().unwrap();
        assert_eq!(tree.last_activity(t[0], own), late);
        assert_eq!(tree.last_activity(t[1], own), own);
    }

    #[test]
    fn transitions() {
        for (f, t) in [
            (Open, InProgress),
            (InProgress, Blocked),
            (Blocked, Open),
            (Open, Done),
            (Blocked, Dropped),
            (InProgress, Done),
            (Done, Done),
        ] {
            assert_eq!(check_task_transition(f, t, None), Ok(()), "{f:?}→{t:?}");
        }
        assert_eq!(
            check_task_transition(Done, Open, None),
            Err("reason-required")
        );
        assert_eq!(
            check_task_transition(Dropped, Open, Some("  ")),
            Err("reason-required")
        );
        assert_eq!(check_task_transition(Done, Open, Some("нашли баг")), Ok(()));
        for (f, t) in [(Done, InProgress), (Dropped, Blocked), (Done, Dropped)] {
            assert_eq!(
                check_task_transition(f, t, Some("r")),
                Err("transition-not-allowed")
            );
        }
        assert_eq!(
            check_task_transition(Open, Active, None),
            Err("not-a-task-status")
        );
    }

    #[test]
    fn patch_rules() {
        let mut t = TaskData::default();
        t.apply_patch(
            &json!({"next_step": " сделать ", "links": ["a", "a", " "], "owner": "human",
                               "estimate_min": 30}),
        )
        .unwrap();
        assert_eq!(t.next_step.as_deref(), Some("сделать"));
        assert_eq!(t.links, vec!["a"]);
        assert_eq!(t.owner, Some(Owner::Human));
        t.apply_patch(&json!({"next_step": null, "blocked_by": "ждём"}))
            .unwrap();
        assert_eq!(t.next_step, None);
        assert_eq!(t.blocked_by.as_deref(), Some("ждём"));
        for bad in [
            json!({"claimed_by": {}}),
            json!({"started_at": "2026-01-01T00:00:00Z"}),
            json!({"done_at": null}),
            json!({"foo": 1}),
            json!({"next_step": "x".repeat(1001)}),
            json!({"next_step": 5}),
            json!({"owner": "robot"}),
            json!({"links": "a"}),
            json!({"estimate_min": 0}),
            json!({"estimate_min": -1}),
            json!([]),
        ] {
            assert!(TaskData::default().apply_patch(&bad).is_err(), "{bad}");
        }
        // ошибка не оставляет частичного результата
        let mut t = TaskData::default();
        assert!(
            t.apply_patch(&json!({"next_step": "ok", "owner": "robot"}))
                .is_err()
        );
        assert_eq!(t, TaskData::default());
    }

    #[test]
    fn times_and_duration() {
        let t0: Timestamp = "2026-10-28T10:00:00Z".parse().unwrap();
        let t1: Timestamp = "2026-10-28T11:30:30Z".parse().unwrap();
        let mut t = TaskData::default();
        t.on_transition(Some(Open), InProgress, t0);
        assert_eq!(t.started_at, Some(t0));
        // повторный in_progress время не меняет
        t.on_transition(Some(Blocked), InProgress, t1);
        assert_eq!(t.started_at, Some(t0));
        t.claimed_by = Some(Claim {
            session: "s".into(),
            agent: None,
            until: t1,
        });
        t.on_transition(Some(InProgress), Done, t1);
        assert_eq!(t.done_at, Some(t1));
        assert_eq!(t.claimed_by, None);
        // done → done и dropped → dropped не перезаписывают done_at
        let t2: Timestamp = "2026-10-28T12:00:00Z".parse().unwrap();
        t.on_transition(Some(Done), Done, t2);
        assert_eq!(t.done_at, Some(t1));
        t.on_transition(Some(Dropped), Dropped, t2);
        assert_eq!(t.done_at, Some(t1));
        assert_eq!(t.duration_min(), Some(90));
        t.on_transition(Some(Done), Open, t1);
        assert_eq!(t.done_at, None);
        assert_eq!(t.duration_min(), None);
        // выход из blocked снимает блокер
        let mut t = TaskData {
            blocked_by: Some("x".into()),
            ..TaskData::default()
        };
        t.on_transition(Some(Blocked), InProgress, t0);
        assert_eq!(t.blocked_by, None);
    }

    /// M6.6, замечание 35: закрытие (`done`/`dropped`) снимает «следующий шаг» и блокер из
    /// любого живого статуса; повтор того же статуса — не переход, поля не трогает.
    #[test]
    fn closing_clears_next_step_and_blocker() {
        let t0: Timestamp = "2026-10-28T10:00:00Z".parse().unwrap();
        let open = || TaskData {
            next_step: Some("ревью".into()),
            blocked_by: Some("ждём".into()),
            ..TaskData::default()
        };
        for from in [Open, InProgress, Blocked] {
            for to in [Done, Dropped] {
                let mut t = open();
                t.on_transition(Some(from), to, t0);
                assert_eq!(t.next_step, None, "{from:?} → {to:?}");
                assert_eq!(t.blocked_by, None, "{from:?} → {to:?}");
            }
        }
        // задача без статуса в базе читается как open
        let mut t = open();
        t.on_transition(None, Done, t0);
        assert_eq!((t.next_step, t.blocked_by), (None, None));
        // done → done, dropped → dropped: шаг, заданный после закрытия, остаётся
        for s in [Done, Dropped] {
            let mut t = open();
            t.on_transition(Some(s), s, t0);
            assert_eq!(t.next_step.as_deref(), Some("ревью"), "{s:?}");
            assert_eq!(t.blocked_by.as_deref(), Some("ждём"), "{s:?}");
        }
        // прочие переходы шаг не трогают
        let mut t = open();
        t.on_transition(Some(Open), InProgress, t0);
        assert_eq!(t.next_step.as_deref(), Some("ревью"));
    }

    #[test]
    fn value_round_trip() {
        let now: Timestamp = "2026-10-28T10:00:00Z".parse().unwrap();
        let t = TaskData {
            next_step: Some("a".into()),
            owner: Some(Owner::Agent),
            links: vec!["n:1".into()],
            claimed_by: Some(Claim {
                session: "s".into(),
                agent: Some("claude".into()),
                until: now,
            }),
            started_at: Some(now),
            estimate_min: Some(5),
            priority: Some(IdeaPriority::High),
            ..TaskData::default()
        };
        assert_eq!(TaskData::from_stored(&t.to_value()), t);
        assert_eq!(TaskData::from_stored(&json!({})), TaskData::default());
        assert_eq!(TaskData::default().to_value(), json!({}));
    }
}
