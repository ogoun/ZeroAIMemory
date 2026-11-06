//! Показы и оценки полезности (`usage`, §6.3, §10.5; задача 5 M4).
//!
//! Таблица лежит в базе, где лежит цель: владелец решения видит показы и оценки записей своего
//! решения, в том числе от участников (`user_id` — кто показал или оценил). Хранится бессрочно,
//! только дописывается.
//!
//! Учёт выданного без сессии (`tokens_unattributed`, спецификация M6 §7.1, миграция 0025) —
//! здесь же, в базе пользователя запроса: сутки UTC × событие, [`add_unattributed`] и
//! [`unattributed_tokens`].
use std::collections::HashMap;

use rusqlite::params;
use serde::Serialize;
use zam_core::{Id, TargetKind, TargetRef};

/// `usage.target_kind` цели (CHECK у столбца нет: показы вложений в выдаче — задача 6 M5).
fn kind_str(k: TargetKind) -> &'static str {
    match k {
        TargetKind::Node => "node",
        TargetKind::Entry => "entry",
        TargetKind::Attachment => "attachment",
    }
}
use crate::time::fmt_ts;
use crate::{Db, StoreError};

/// Событие: показ подсказкой `/recall` или явная оценка.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageKind {
    RecallShown,
    Useful,
    Useless,
}

impl UsageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UsageKind::RecallShown => "recall_shown",
            UsageKind::Useful => "useful",
            UsageKind::Useless => "useless",
        }
    }
}

/// Источник события: событие `/recall` (показ) или кто оценил.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    SessionStart,
    Prompt,
    PreTool,
    PostTool,
    /// Пакет под задачу `/recall` `event: brief` (M4.5 §9.4; в `usage.source` нет CHECK).
    Brief,
    Agent,
    Webui,
}

impl UsageSource {
    pub fn as_str(self) -> &'static str {
        match self {
            UsageSource::SessionStart => "session_start",
            UsageSource::Prompt => "prompt",
            UsageSource::PreTool => "pre_tool",
            UsageSource::PostTool => "post_tool",
            UsageSource::Brief => "brief",
            UsageSource::Agent => "agent",
            UsageSource::Webui => "webui",
        }
    }
}

/// Одно событие `usage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageEvent {
    /// Кто показал или оценил (пользователь запроса).
    pub user_id: Id,
    pub session_id: Option<String>,
    pub target: TargetRef,
    pub event: UsageKind,
    pub source: Option<UsageSource>,
}

/// Счётчики цели: показы, «полезно», «бесполезно».
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct UsageCounts {
    pub shown: i64,
    pub useful: i64,
    pub useless: i64,
}

/// Дописывает событие.
pub async fn record_usage(db: &Db, u: &UsageEvent) -> Result<(), StoreError> {
    record_usages(db, std::slice::from_ref(u)).await
}

/// Дописывает события одной транзакцией (показы подсказок `/recall` — пачкой).
pub async fn record_usages(db: &Db, events: &[UsageEvent]) -> Result<(), StoreError> {
    if events.is_empty() {
        return Ok(());
    }
    for u in events {
        if let Some(s) = &u.session_id {
            crate::sessions::check_session_id(s)?;
        }
    }
    let events = events.to_vec();
    db.write(move |t| {
        let ts = fmt_ts(crate::time::now());
        let mut st = t.prepare_cached(
            "INSERT INTO usage (ts, user_id, session_id, target_kind, target_id, event, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for u in &events {
            st.execute(params![
                ts,
                u.user_id,
                u.session_id,
                kind_str(u.target.kind),
                u.target.id,
                u.event.as_str(),
                u.source.map(UsageSource::as_str)
            ])?;
        }
        Ok(())
    })
    .await
}

/// Выдача без сессии (MCP без явной сессии и прочие, M6 §7.1): прибавляет `calls` и `tokens` к
/// строке `(day, event)` (UPSERT). Событие — как у `session_tokens` (`[a-z0-9_]`, до 32).
pub async fn add_unattributed(
    db: &Db,
    day: jiff::civil::Date,
    event: &str,
    calls: u32,
    tokens: u64,
) -> Result<(), StoreError> {
    crate::sessions::check_event(event)?;
    let (day, event) = (day.strftime("%Y-%m-%d").to_string(), event.to_string());
    let tokens = i64::try_from(tokens).unwrap_or(i64::MAX);
    db.write(move |t| {
        t.prepare_cached(
            "INSERT INTO tokens_unattributed (day, event, calls, tokens) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (day, event) DO UPDATE SET
               calls = calls + excluded.calls, tokens = tokens + excluded.tokens",
        )?
        .execute(params![day, event, calls, tokens])?;
        Ok(())
    })
    .await
}

/// Строка учёта выданного без сессии: сутки UTC (`YYYY-MM-DD`), событие, вызовы и токены.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnattributedRow {
    pub day: String,
    pub event: String,
    pub calls: i64,
    pub tokens: i64,
}

/// Учёт выданного без сессии с суток `since` (включительно): свежие сутки первыми, внутри
/// суток — по событию.
pub async fn unattributed_tokens(
    db: &Db,
    since: jiff::civil::Date,
) -> Result<Vec<UnattributedRow>, StoreError> {
    let since = since.strftime("%Y-%m-%d").to_string();
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT day, event, calls, tokens FROM tokens_unattributed WHERE day >= ?1
             ORDER BY day DESC, event",
        )?;
        let rows = st.query_map([since], |r| {
            Ok(UnattributedRow {
                day: r.get(0)?,
                event: r.get(1)?,
                calls: r.get(2)?,
                tokens: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}

/// Счётчики событий многих целей одного вида одним запросом (столбец «показы / полезно /
/// бесполезно» списка Insights WebUI, задача 13); цели без событий в ответе нет.
pub async fn usage_counts_many(
    db: &Db,
    kind: TargetKind,
    ids: &[Id],
) -> Result<HashMap<Id, UsageCounts>, StoreError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let list =
        serde_json::Value::from(ids.iter().map(Id::to_string).collect::<Vec<_>>()).to_string();
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT target_id, event, count(*) FROM usage
             WHERE target_kind = ?1 AND target_id IN (SELECT value FROM json_each(?2))
             GROUP BY target_id, event",
        )?;
        let rows = st.query_map(params![kind_str(kind), list], |r| {
            Ok((
                r.get::<_, Id>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut out: HashMap<Id, UsageCounts> = HashMap::new();
        for row in rows {
            let (id, event, n) = row?;
            let u = out.entry(id).or_default();
            match event.as_str() {
                "recall_shown" => u.shown = n,
                "useful" => u.useful = n,
                "useless" => u.useless = n,
                _ => {}
            }
        }
        Ok(out)
    })
    .await
}

/// Счётчики событий цели (по индексу `usage_target`).
pub async fn usage_counts(db: &Db, target: TargetRef) -> Result<UsageCounts, StoreError> {
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT event, count(*) FROM usage
             WHERE target_kind = ?1 AND target_id = ?2 GROUP BY event",
        )?;
        let mut out = UsageCounts::default();
        let rows = st.query_map(params![kind_str(target.kind), target.id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (event, n) = row?;
            match event.as_str() {
                "recall_shown" => out.shown = n,
                "useful" => out.useful = n,
                "useless" => out.useless = n,
                _ => {}
            }
        }
        Ok(out)
    })
    .await
}
