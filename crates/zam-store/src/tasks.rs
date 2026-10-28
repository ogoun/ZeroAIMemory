//! Журнал задач (спецификация M4.5 §3): записи `task` — план в `body`, ход работы в `data`.
//!
//! - [`TaskData`] — `data` задачи: `next_step`, `blocked_by`, `owner`, `links`, `estimate_min`
//!   ведёт клиент; `claimed_by`, `started_at`, `done_at` — только сервер (клиентская `data` с
//!   этими полями отклоняется). Правка `data` задачи — слияние на верхнем уровне (`null`
//!   снимает поле);
//! - [`check_task_transition`] — переходы статусов: между `open`, `in_progress`, `blocked` —
//!   любые; в `done`/`dropped` — из любого; из `done`/`dropped` — только в `open` и с причиной.
//!   `started_at` ставится при первом переходе в `in_progress`, `done_at` — при `done` (снимается
//!   при переоткрытии), закрытие (`done`/`dropped`) снимает захват;
//! - захват ([`claim_task`], [`release_task`]) — одна транзакция писателя с проверкой текущего
//!   `claimed_by`; ревизию записи не создаёт (меняется только `entries.data`), аудит ведёт
//!   вызывающий (сервер: `entry.claim`, `entry.release` в `system.db`);
//! - [`claimed_tasks`] — задачи, захваченные сессией (для `GET /sessions/{id}` и хука
//!   `checkpoint`).

use jiff::{SignedDuration, Timestamp};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use zam_core::Id;

use crate::entries::{EntryKind, EntryStatus, load_entry};
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
}

/// Поля, которые ведёт клиент.
const CLIENT_KEYS: [&str; 5] = ["next_step", "blocked_by", "owner", "links", "estimate_min"];
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

fn links_field(v: &Value) -> Result<Vec<String>, String> {
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
        Value::Object(o)
    }

    /// Длительность `done_at − started_at`, целых минут (не меньше 0); нет любой метки — `None`.
    pub fn duration_min(&self) -> Option<i64> {
        let d = self.done_at?.duration_since(self.started_at?);
        Some(d.as_secs().max(0) / 60)
    }

    /// Следы смены статуса: `started_at` при первом `in_progress`, `done_at` при `done` (и снятие
    /// при переоткрытии), снятие захвата при закрытии, `blocked_by` теряется при выходе из
    /// `blocked` (явное значение той же правки накладывается после — [`TaskData::apply_patch`]).
    pub fn on_transition(&mut self, from: Option<EntryStatus>, to: EntryStatus, now: Timestamp) {
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

/// Живая задача по id: нет — `NotFound`, другой вид — `NotTask`.
fn live_task(c: &rusqlite::Connection, id: Id) -> Result<crate::Entry, TaskError> {
    let e = load_entry(c, id)?;
    if e.deleted_at.is_some() {
        return Err(StoreError::NotFound.into());
    }
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
        if c.live(Timestamp::now()) && c.session != session && !human {
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

/// Живые задачи (`open`, `in_progress`, `blocked`) с действующим захватом сессии `session`,
/// новые первыми.
pub async fn claimed_tasks(
    db: &Db,
    session: &str,
    now: Timestamp,
) -> Result<Vec<ClaimedTask>, StoreError> {
    let session = session.to_string();
    let now = fmt_ts(now);
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT id, title, status FROM entries
             WHERE kind = 'task' AND deleted_at IS NULL
               AND status IN ('open', 'in_progress', 'blocked')
               AND json_extract(data, '$.claimed_by.session') = ?1
               AND json_extract(data, '$.claimed_by.until') > ?2
             ORDER BY updated_at DESC, id DESC",
        )?;
        let rows = st.query_map(params![session, now], |r| {
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

#[cfg(test)]
mod tests {
    use super::*;
    use EntryStatus::*;

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
            ..TaskData::default()
        };
        assert_eq!(TaskData::from_stored(&t.to_value()), t);
        assert_eq!(TaskData::from_stored(&json!({})), TaskData::default());
        assert_eq!(TaskData::default().to_value(), json!({}));
    }
}
