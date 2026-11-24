//! Показы и оценки полезности (`usage`, §6.3, §10.5; задача 5 M4).
//!
//! Таблица лежит в базе, где лежит цель: владелец решения видит показы и оценки записей своего
//! решения, в том числе от участников (`user_id` — кто показал или оценил). Хранится бессрочно,
//! только дописывается.
//!
//! Учёт выданного без сессии (`tokens_unattributed`, спецификация M6 §7.1, миграция 0025) —
//! здесь же, в базе пользователя запроса: сутки UTC × событие, [`add_unattributed`] и
//! [`unattributed_tokens`].
//!
//! «Агент открыл подсказку» (`usage_opened`, спецификация M7 §6.6, миграция 0027) — отдельная
//! таблица в базе цели: одно событие на сессию и цель держит первичный ключ ([`record_opened`] —
//! `INSERT OR IGNORE`), счётчики — [`opened_counts_many`] и поле `opened` у [`UsageCounts`],
//! начало учёта — строка `meta.usage_opened_since` ([`usage_opened_since`]), событие показа
//! открытия — [`open_source`], «шумные» записи — [`noisy_ids`]. Внешнего ключа на `sessions` нет:
//! уборка сессий строки не трогает.
use std::collections::HashMap;

use jiff::Timestamp;
use rusqlite::{OptionalExtension, params};
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

    /// Источник по строке `usage.source` ([`UsageSource::as_str`]); неизвестная — `None`.
    pub fn parse(s: &str) -> Option<UsageSource> {
        [
            UsageSource::SessionStart,
            UsageSource::Prompt,
            UsageSource::PreTool,
            UsageSource::PostTool,
            UsageSource::Brief,
            UsageSource::Agent,
            UsageSource::Webui,
        ]
        .into_iter()
        .find(|u| u.as_str() == s)
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

/// Счётчики цели: показы, открытия, «полезно», «бесполезно».
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct UsageCounts {
    pub shown: i64,
    /// Сессии, открывшие цель после её подсказки (`usage_opened`, спецификация M7 §6.6 п. 3;
    /// признак сервера `usage_opened`).
    pub opened: i64,
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

/// Счётчики событий многих целей одного вида одним чтением (столбец «показы / полезно /
/// бесполезно» списка Insights WebUI, задача 13; открытия — `usage_opened`, M7 §6.6 п. 3); цели
/// без событий и открытий в ответе нет.
pub async fn usage_counts_many(
    db: &Db,
    kind: TargetKind,
    ids: &[Id],
) -> Result<HashMap<Id, UsageCounts>, StoreError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let list = id_list(ids);
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
        for (id, n) in opened_many_in(c, kind, &list)? {
            out.entry(id).or_default().opened = n;
        }
        Ok(out)
    })
    .await
}

/// Список id JSON-массивом строк (`json_each` запросов «многих целей»).
fn id_list(ids: &[Id]) -> String {
    serde_json::Value::from(ids.iter().map(Id::to_string).collect::<Vec<_>>()).to_string()
}

/// Ключ `meta` начала учёта открытий (миграция 0027, §6.6 п. 3).
pub const USAGE_OPENED_SINCE: &str = "usage_opened_since";

/// Открытие цели агентом после показа подсказкой (§6.6 п. 2): сессия `session_id` прочитала
/// цель `target`, показанную ей событием `source`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedEvent {
    pub ts: Timestamp,
    /// Кто открыл (пользователь запроса).
    pub user_id: Id,
    pub session_id: String,
    pub target: TargetRef,
    /// Событие показа (`session_start`, `prompt`, …).
    pub source: UsageSource,
}

/// Пишет открытие цели; повтор той же сессии и цели ничего не меняет (`INSERT OR IGNORE` по
/// первичному ключу). `true` — новая строка.
pub async fn record_opened(db: &Db, ev: &OpenedEvent) -> Result<bool, StoreError> {
    crate::sessions::check_session_id(&ev.session_id)?;
    let ev = ev.clone();
    db.write(move |t| {
        let n = t
            .prepare_cached(
                "INSERT OR IGNORE INTO usage_opened
                   (ts, user_id, session_id, target_kind, target_id, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![
                fmt_ts(ev.ts),
                ev.user_id,
                ev.session_id,
                kind_str(ev.target.kind),
                ev.target.id,
                ev.source.as_str()
            ])?;
        Ok(n == 1)
    })
    .await
}

/// Число открытий (сессий, открывших цель) многих целей одного вида одним запросом (по индексу
/// `usage_opened_target`); цели без открытий в ответе нет.
pub async fn opened_counts_many(
    db: &Db,
    kind: TargetKind,
    ids: &[Id],
) -> Result<HashMap<Id, i64>, StoreError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let list = id_list(ids);
    db.read(move |c| opened_many_in(c, kind, &list)).await
}

/// [`opened_counts_many`] на соединении `c`; `list` — [`id_list`].
fn opened_many_in(
    c: &rusqlite::Connection,
    kind: TargetKind,
    list: &str,
) -> Result<HashMap<Id, i64>, StoreError> {
    let mut st = c.prepare_cached(
        "SELECT target_id, count(*) FROM usage_opened
         WHERE target_kind = ?1 AND target_id IN (SELECT value FROM json_each(?2))
         GROUP BY target_id",
    )?;
    let rows = st.query_map(params![kind_str(kind), list], |r| {
        Ok((r.get::<_, Id>(0)?, r.get::<_, i64>(1)?))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Событие показа, к которому относится открытие цели `target` сессией `session` (§6.6 п. 2):
/// `source` последнего показа цели этой сессии (`usage.recall_shown`, индекс `usage_target`).
/// `None` — открытие этой сессией уже записано (повтор не берёт запись базы) или показа этой
/// сессии в `usage` нет (сбой учёта показа, неизвестный источник). Одно чтение.
pub async fn open_source(
    db: &Db,
    session: &str,
    target: TargetRef,
) -> Result<Option<UsageSource>, StoreError> {
    crate::sessions::check_session_id(session)?;
    let session = session.to_string();
    let (opened, source): (bool, Option<String>) = db
        .read(move |c| {
            Ok(c.prepare_cached(
                "SELECT EXISTS (SELECT 1 FROM usage_opened
                                WHERE session_id = ?1 AND target_kind = ?2 AND target_id = ?3),
                        (SELECT source FROM usage
                         WHERE target_kind = ?2 AND target_id = ?3 AND event = 'recall_shown'
                           AND session_id = ?1
                         ORDER BY id DESC LIMIT 1)",
            )?
            .query_row(params![session, kind_str(target.kind), target.id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?)
        })
        .await?;
    if opened {
        return Ok(None);
    }
    Ok(source.as_deref().and_then(UsageSource::parse))
}

/// Порог «шумной» записи: показов в окне не меньше (ТЗ §10.5, M7 §6.6 п. 3).
pub const NOISY_MIN_SHOWN: i64 = 10;
/// Окно «шумных», суток: события с `max(now − NOISY_WINDOW_DAYS, usage_opened_since)`.
pub const NOISY_WINDOW_DAYS: i64 = 30;

/// Нижняя граница окна в `usage`: `id` последней строки старше начала окна (`?1`). Обход — с конца
/// по `rowid` до первой такой строки, то есть по строкам окна, а не по всей истории.
const NOISY_LO_SQL: &str = "SELECT id FROM usage WHERE ts < ?1 ORDER BY id DESC LIMIT 1";

/// Отбор «шумных» ([`noisy_ids`]): показы окна — диапазоном `rowid` (`id > ?3`; `+target_kind` не
/// даёт планировщику уйти в `usage_target` и обойти все показы записей за всё время), с точной
/// проверкой `ts`; открытия — по окну (`usage_opened_target`), оценка `useful` — за всё время
/// (`usage_target`).
const NOISY_SQL: &str = "SELECT u.target_id FROM usage u
     WHERE u.id > ?3 AND +u.target_kind = 'entry' AND u.event = 'recall_shown' AND u.ts >= ?1
     GROUP BY u.target_id
     HAVING count(*) >= ?2
        AND NOT EXISTS (SELECT 1 FROM usage x
                        WHERE x.target_kind = 'entry' AND x.target_id = u.target_id
                          AND x.event = 'useful')
        AND NOT EXISTS (SELECT 1 FROM usage_opened o
                        WHERE o.target_kind = 'entry' AND o.target_id = u.target_id
                          AND o.ts >= ?1)";

/// «Шумные» записи журналов базы (M7 §6.6 п. 3, решение координатора 68 и круга правок 1 задачи
/// 6): показов с `since` (включительно) не меньше [`NOISY_MIN_SHOWN`] и ни одного открытия с
/// `since`, а оценки `useful` нет **за всё время** — любая «полезно», в том числе до начала учёта
/// открытий и старше окна, запись исключает. Окно — только условие отбора: счётчики записей
/// вызывающий берёт за всё время ([`usage_counts_many`]). Вид записи и пространство не
/// проверяются — их ставит фильтр ленты. Окно `usage` — по `id`: строки только дописываются, `ts`
/// — время записи, поэтому `id` растёт вместе с ним; стоимость — по строкам окна, а не истории.
pub async fn noisy_ids(db: &Db, since: Timestamp) -> Result<Vec<Id>, StoreError> {
    let since = fmt_ts(since);
    db.read(move |c| {
        let lo: i64 = c
            .prepare_cached(NOISY_LO_SQL)?
            .query_row([&since], |r| r.get(0))
            .optional()?
            .unwrap_or(0);
        let mut st = c.prepare_cached(NOISY_SQL)?;
        let rows = st.query_map(params![since, NOISY_MIN_SHOWN, lo], |r| {
            r.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for id in rows {
            if let Ok(id) = id?.parse::<Id>() {
                out.push(id);
            }
        }
        Ok(out)
    })
    .await
}

/// Начало учёта открытий — момент применения миграции 0027 (`meta.usage_opened_since`); строки
/// нет — `None`.
pub async fn usage_opened_since(db: &Db) -> Result<Option<Timestamp>, StoreError> {
    let raw: Option<String> = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [USAGE_OPENED_SINCE],
                |r| r.get(0),
            )
            .optional()?)
        })
        .await?;
    raw.as_deref().map(crate::time::parse_ts).transpose()
}

/// Счётчики событий цели (по индексу `usage_target`) и её открытия (`usage_opened_target`) —
/// одним чтением.
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
        out.opened = c
            .prepare_cached(
                "SELECT count(*) FROM usage_opened WHERE target_kind = ?1 AND target_id = ?2",
            )?
            .query_row(params![kind_str(target.kind), target.id], |r| r.get(0))?;
        Ok(out)
    })
    .await
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::{NOISY_LO_SQL, NOISY_SQL};

    fn schema() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        for m in crate::USER_MIGRATIONS {
            c.execute_batch(m.sql).unwrap();
        }
        c
    }

    /// Строки `EXPLAIN QUERY PLAN` запроса с `n` параметрами-строками.
    fn plan(c: &Connection, sql: &str, n: usize) -> Vec<String> {
        let mut st = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let args = vec!["x"; n];
        st.query_map(rusqlite::params_from_iter(args), |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// «Шумные» без миграций (круг правок 1 задачи 6, Minor 1): нижняя граница окна — обход
    /// `usage` с конца по `rowid` без сортировки, показы окна — диапазоном `rowid`, а не через
    /// `usage_target` по всей истории; оценка `useful` и открытия — по своим индексам.
    #[test]
    fn noisy_window_by_rowid() {
        let c = schema();
        let lo = plan(&c, NOISY_LO_SQL, 1);
        assert!(
            lo.iter().any(|l| l == "SCAN usage") && !lo.iter().any(|l| l.contains("B-TREE")),
            "{lo:?}"
        );
        let main = plan(&c, NOISY_SQL, 3);
        assert!(
            main.iter()
                .any(|l| l.starts_with("SEARCH u USING INTEGER PRIMARY KEY (rowid>?)")),
            "{main:?}"
        );
        assert!(
            main.iter().any(|l| l.starts_with("SEARCH x USING ")
                && l.contains("usage_target (target_kind=? AND target_id=? AND event=?)")),
            "{main:?}"
        );
        assert!(
            main.iter()
                .any(|l| l.starts_with("SEARCH o USING ") && l.contains("usage_opened_target")),
            "{main:?}"
        );
    }
}
