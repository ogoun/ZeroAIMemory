//! Сессии агентов и выданное им (§6.3, §9.3, §10.3; задача 5 M4).
//!
//! - `sessions` — в базе пользователя запроса: внешний id сессии агента (`X-Zam-Session`,
//!   `session_id` хука, до [`SESSION_ID_MAX_CHARS`] символов), токен, решение, область, машина,
//!   агент, время начала, последнего обращения и конца, счётчик записей в память
//!   `memory_writes` (решение контроллера R1: читает хук `checkpoint`). Сессия создаётся
//!   первым касанием ([`touch_session`], [`add_memory_write`], [`mark_seen`]);
//! - `session_seen` — что сессии уже выдано: цель в базе владельца `owner_id` (решение может
//!   быть чужим) и её ревизия. Хранится большая из выданных ревизий: ревизии только растут,
//!   и повторная выдача старой не откатывает отметку;
//! - срок хранения — [`gc_sessions`]: сессии без обращений дольше срока удаляются вместе с
//!   выданным им (воркер обслуживания сервера, раз в сутки, 30 дней).
use std::collections::HashMap;

use jiff::Timestamp;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use zam_core::{Id, SolutionGuid, TargetKind, TargetRef};

use crate::time::{fmt_ts, ser_ts, ser_ts_opt, ts_at, ts_opt};
use crate::{Db, StoreError};

/// Предел длины id сессии и строковых полей сессии, символов (как у заголовков атрибуции).
pub const SESSION_ID_MAX_CHARS: usize = 128;

/// Касание сессии: id и то, что о ней известно из запроса (`None` — не менять прежнее).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionTouch {
    pub id: String,
    pub token_id: Option<Id>,
    pub solution_guid: Option<SolutionGuid>,
    pub area: Option<String>,
    pub machine: Option<String>,
    pub agent: Option<String>,
}

/// Сессия агента.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    pub id: String,
    #[serde(skip)]
    pub token_id: Option<Id>,
    /// GUID решения; в JSON API — `solution`.
    #[serde(rename = "solution")]
    pub solution_guid: Option<String>,
    pub area: Option<String>,
    pub machine: Option<String>,
    pub agent: Option<String>,
    #[serde(serialize_with = "ser_ts")]
    pub started_at: Timestamp,
    #[serde(serialize_with = "ser_ts")]
    pub last_seen_at: Timestamp,
    #[serde(serialize_with = "ser_ts_opt")]
    pub ended_at: Option<Timestamp>,
    pub memory_writes: i64,
}

/// Цель, выданная сессии: владелец базы цели и сама цель.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SeenKey {
    pub owner_id: Id,
    pub target: TargetRef,
}

/// Выданная цель и её ревизия.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeenItem {
    pub key: SeenKey,
    pub rev: i64,
}

/// `target_kind` колонок `session_seen` и `usage`.
pub(crate) fn target_kind_str(k: TargetKind) -> &'static str {
    match k {
        TargetKind::Node => "node",
        TargetKind::Entry => "entry",
    }
}

/// Строка поля сессии: до [`SESSION_ID_MAX_CHARS`] символов без управляющих.
fn check_field(what: &str, v: &str) -> Result<(), StoreError> {
    if v.chars().count() > SESSION_ID_MAX_CHARS || v.chars().any(char::is_control) {
        return Err(StoreError::BadInput(format!(
            "{what}: до {SESSION_ID_MAX_CHARS} печатных символов"
        )));
    }
    Ok(())
}

/// Id сессии: непустой, до [`SESSION_ID_MAX_CHARS`] печатных символов.
pub fn check_session_id(id: &str) -> Result<(), StoreError> {
    if id.trim().is_empty() {
        return Err(StoreError::BadInput("id сессии пуст".into()));
    }
    check_field("id сессии", id)
}

fn check_touch(s: &SessionTouch) -> Result<(), StoreError> {
    check_session_id(&s.id)?;
    for (what, v) in [
        ("area", &s.area),
        ("machine", &s.machine),
        ("agent", &s.agent),
    ] {
        if let Some(v) = v {
            check_field(what, v)?;
        }
    }
    Ok(())
}

/// Upsert сессии в транзакции записи: `last_seen_at` — сейчас, известные поля дописываются,
/// `memory_writes` растёт на `writes`.
fn upsert(t: &rusqlite::Transaction, s: &SessionTouch, writes: i64) -> Result<(), StoreError> {
    let now = fmt_ts(Timestamp::now());
    t.execute(
        "INSERT INTO sessions (id, token_id, solution_guid, area, machine, agent,
                               started_at, last_seen_at, memory_writes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)
         ON CONFLICT (id) DO UPDATE SET
           token_id = coalesce(excluded.token_id, token_id),
           solution_guid = coalesce(excluded.solution_guid, solution_guid),
           area = coalesce(excluded.area, area),
           machine = coalesce(excluded.machine, machine),
           agent = coalesce(excluded.agent, agent),
           last_seen_at = excluded.last_seen_at,
           memory_writes = memory_writes + excluded.memory_writes",
        params![
            s.id,
            s.token_id,
            s.solution_guid,
            s.area,
            s.machine,
            s.agent,
            now,
            writes
        ],
    )?;
    Ok(())
}

/// Создаёт сессию или отмечает обращение к ней (`last_seen_at`).
pub async fn touch_session(db: &Db, s: &SessionTouch) -> Result<(), StoreError> {
    check_touch(s)?;
    let s = s.clone();
    db.write(move |t| upsert(t, &s, 0)).await
}

/// Запись в память в этой сессии: `memory_writes + 1` (сессия создаётся, если её нет).
pub async fn add_memory_write(db: &Db, s: &SessionTouch) -> Result<(), StoreError> {
    check_touch(s)?;
    let s = s.clone();
    db.write(move |t| upsert(t, &s, 1)).await
}

/// Конец сессии (`ended: true` — сейчас, `false` — снять отметку). `false` — сессии нет.
pub async fn end_session(db: &Db, id: &str, ended: bool) -> Result<bool, StoreError> {
    let id = id.to_string();
    db.write(move |t| {
        let now = fmt_ts(Timestamp::now());
        let n = t.execute(
            "UPDATE sessions SET ended_at = CASE WHEN ?2 THEN coalesce(ended_at, ?3) END,
                                 last_seen_at = ?3
             WHERE id = ?1",
            params![id, ended, now],
        )?;
        Ok(n > 0)
    })
    .await
}

const SESSION_COLS: &str = "id, token_id, solution_guid, area, machine, agent, started_at, \
                            last_seen_at, ended_at, memory_writes";

fn session_row(r: &rusqlite::Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: r.get(0)?,
        token_id: r.get(1)?,
        solution_guid: r.get(2)?,
        area: r.get(3)?,
        machine: r.get(4)?,
        agent: r.get(5)?,
        started_at: ts_at(r, 6)?,
        last_seen_at: ts_at(r, 7)?,
        ended_at: ts_opt(r, 8)?,
        memory_writes: r.get(9)?,
    })
}

/// Сессия по id.
pub async fn get_session(db: &Db, id: &str) -> Result<Option<Session>, StoreError> {
    let id = id.to_string();
    db.read(move |c| {
        Ok(c.query_row(
            &format!("SELECT {SESSION_COLS} FROM sessions WHERE id = ?1"),
            [id],
            session_row,
        )
        .optional()?)
    })
    .await
}

/// Сессии базы, свежие первыми; `solution` — только сессии этого решения.
pub async fn list_sessions(
    db: &Db,
    solution: Option<&SolutionGuid>,
    limit: usize,
) -> Result<Vec<Session>, StoreError> {
    let solution = solution.map(|g| g.as_str().to_string());
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    db.read(move |c| {
        let mut st = c.prepare(&format!(
            "SELECT {SESSION_COLS} FROM sessions
             WHERE ?1 IS NULL OR solution_guid = ?1
             ORDER BY last_seen_at DESC, id LIMIT ?2"
        ))?;
        let rows = st.query_map(params![solution, limit], session_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .await
}

/// Отмечает выданное сессии (сессия создаётся, если её нет); у цели остаётся большая ревизия.
pub async fn mark_seen(db: &Db, session: &str, items: &[SeenItem]) -> Result<(), StoreError> {
    check_session_id(session)?;
    if items.is_empty() {
        return Ok(());
    }
    let (session, items) = (session.to_string(), items.to_vec());
    db.write(move |t| {
        let now = fmt_ts(Timestamp::now());
        t.execute(
            "INSERT INTO sessions (id, started_at, last_seen_at) VALUES (?1, ?2, ?2)
             ON CONFLICT (id) DO UPDATE SET last_seen_at = excluded.last_seen_at",
            params![session, now],
        )?;
        let mut st = t.prepare_cached(
            "INSERT INTO session_seen (session_id, owner_id, target_kind, target_id, rev, seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (session_id, owner_id, target_kind, target_id) DO UPDATE SET
               rev = max(rev, excluded.rev), seen_at = excluded.seen_at",
        )?;
        for it in &items {
            st.execute(params![
                session,
                it.key.owner_id,
                target_kind_str(it.key.target.kind),
                it.key.target.id,
                it.rev,
                now
            ])?;
        }
        Ok(())
    })
    .await
}

/// Ревизии целей, уже выданных сессии (целей, которых сессия не получала, в ответе нет).
pub async fn seen_revs(
    db: &Db,
    session: &str,
    targets: &[SeenKey],
) -> Result<HashMap<SeenKey, i64>, StoreError> {
    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    let (session, targets) = (session.to_string(), targets.to_vec());
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT rev FROM session_seen
             WHERE session_id = ?1 AND owner_id = ?2 AND target_kind = ?3 AND target_id = ?4",
        )?;
        let mut out = HashMap::new();
        for k in targets {
            let rev: Option<i64> = st
                .query_row(
                    params![
                        session,
                        k.owner_id,
                        target_kind_str(k.target.kind),
                        k.target.id
                    ],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(rev) = rev {
                out.insert(k, rev);
            }
        }
        Ok(out)
    })
    .await
}

/// Удаляет сессии без обращений с `cutoff` (`last_seen_at < cutoff`) и выданное им; число
/// удалённых сессий.
pub async fn gc_sessions(db: &Db, cutoff: Timestamp) -> Result<u64, StoreError> {
    let cutoff = fmt_ts(cutoff);
    db.write(move |t| {
        t.execute(
            "DELETE FROM session_seen WHERE session_id IN
               (SELECT id FROM sessions WHERE last_seen_at < ?1)",
            [&cutoff],
        )?;
        let n = t.execute("DELETE FROM sessions WHERE last_seen_at < ?1", [&cutoff])?;
        Ok(n as u64)
    })
    .await
}
