//! Пробы ночной проверки памяти (`probes`, спецификация M7 §6.4 п. 1, миграция 0027).
//!
//! Проба `x:<id>` — вопрос о записи пространства: вид `probe` (исполнитель внутренних работ),
//! `cache` (запрос действующей выжимки кэша), `negative` (посторонний вопрос, общий набор сервера)
//! и `manual` (ручной ввод — M7.4). Пробы вне поиска, дайджеста, подсказок, выжимок и экспорта:
//! таблицу читают только ночная проверка и `GET /check/probes`.
//!
//! - **Пределы.** Активных проб с целью — не больше [`PROBES_MAX`] на базу (посторонние вопросы в
//!   предел не входят: их набор задаёт сервер); новые пробы вида `probe` считаются по
//!   происхождению работы ([`new_probes_since`]: ночь — `nightly`, ручной запуск — `manual`),
//!   пробы из кэша — своим счётом ([`kind_since`]).
//! - **Повтор.** Та же нормализованная строка вопроса (NFKC, нижний регистр, пробелы —
//!   [`crate::normalize_query`]) у активной пробы того же пространства — [`InsertOutcome::Duplicate`].
//! - **Цель ушла.** Все цели пробы удалены, замещены или отозваны — проба `retired`
//!   (`target_gone`, [`retire_gone`]) при следующем прогоне; изменённая цель пробу не снимает.
//! - **Ссылки целей** — `<буква>:<id>` (`n:` заметки, `i:` insight, буквы журналов —
//!   `zam_store::EntryKind::ref_letter`), ревизия — на момент пробы.
use std::collections::HashSet;

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use zam_core::Id;

pub use zam_core::split::Split;

use crate::time::{fmt_ts, ts_at, ts_opt};
use crate::{Db, EntryKind, StoreError, normalize_query};

/// Активных проб с целью на пользователя (§6.4 п. 1).
pub const PROBES_MAX: u32 = 500;
/// Причина снятия: все цели пробы удалены, замещены или отозваны.
pub const RETIRED_TARGET_GONE: &str = "target_gone";
/// Причина снятия постороннего вопроса, которого больше нет в наборе сервера.
pub const RETIRED_SET_CHANGED: &str = "set_changed";

/// Вид пробы.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProbeKind {
    /// Вопрос исполнителя внутренних работ (вид работы `probes`).
    Probe,
    /// Запрос действующей выжимки кэша, цели — её источники.
    Cache,
    /// Посторонний вопрос (цели нет).
    Negative,
    /// Ручная проба (ввод — M7.4).
    Manual,
}

impl ProbeKind {
    pub const ALL: [ProbeKind; 4] = [
        ProbeKind::Probe,
        ProbeKind::Cache,
        ProbeKind::Negative,
        ProbeKind::Manual,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ProbeKind::Probe => "probe",
            ProbeKind::Cache => "cache",
            ProbeKind::Negative => "negative",
            ProbeKind::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Option<ProbeKind> {
        ProbeKind::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// Автор пробы.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProbeAuthor {
    Agent,
    Model,
    Human,
    System,
}

impl ProbeAuthor {
    pub const ALL: [ProbeAuthor; 4] = [
        ProbeAuthor::Agent,
        ProbeAuthor::Model,
        ProbeAuthor::Human,
        ProbeAuthor::System,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ProbeAuthor::Agent => "agent",
            ProbeAuthor::Model => "model",
            ProbeAuthor::Human => "human",
            ProbeAuthor::System => "system",
        }
    }

    pub fn parse(s: &str) -> Option<ProbeAuthor> {
        ProbeAuthor::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// Цель пробы: ссылка `<буква>:<id>` и ревизия записи на момент пробы.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeTarget {
    #[serde(rename = "ref")]
    pub target: String,
    pub rev: i64,
}

impl ProbeTarget {
    /// Id записи цели (буква ссылки не важна: id узлов и записей не пересекаются).
    pub fn id(&self) -> Option<Id> {
        self.target
            .split_once(':')
            .and_then(|(_, id)| id.parse().ok())
    }

    /// Буква ссылки (`n`, `i`, …).
    pub fn letter(&self) -> &str {
        self.target.split_once(':').map_or("", |(l, _)| l)
    }
}

/// Проба.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub id: Id,
    /// Пространство пробы; у постороннего вопроса — `None` (общий для пользователя).
    pub space: Option<Id>,
    pub question: String,
    pub kind: ProbeKind,
    pub targets: Vec<ProbeTarget>,
    pub split: Split,
    pub overlap: Option<f64>,
    pub author_kind: ProbeAuthor,
    pub author: Option<String>,
    pub work_id: Option<Id>,
    pub active: bool,
    pub retired_reason: Option<String>,
    pub created_at: Timestamp,
    pub retired_at: Option<Timestamp>,
}

impl Probe {
    /// Ссылка пробы `x:<id>`.
    pub fn reference(&self) -> String {
        format!("x:{}", self.id)
    }
}

/// Новая проба.
#[derive(Debug, Clone, PartialEq)]
pub struct NewProbe {
    pub space: Option<Id>,
    pub question: String,
    pub kind: ProbeKind,
    pub targets: Vec<ProbeTarget>,
    pub split: Split,
    pub overlap: Option<f64>,
    pub author_kind: ProbeAuthor,
    pub author: Option<String>,
    pub work_id: Option<Id>,
}

/// Итог [`insert_probe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted(Id),
    /// Та же нормализованная строка вопроса уже есть у активной пробы пространства.
    Duplicate,
    /// Активных проб с целью — [`PROBES_MAX`].
    Full,
}

/// Фильтр списка проб (`GET /check/probes`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProbeFilter {
    pub kind: Option<ProbeKind>,
    /// `Some(true)` — активные, `Some(false)` — снятые.
    pub active: Option<bool>,
    pub split: Option<Split>,
    pub limit: usize,
}

const COLS: &str = "id, space_id, question, kind, targets, split, overlap, author_kind, author, \
                    work_id, status, retired_reason, created_at, retired_at";

fn space_col(s: Option<Id>) -> String {
    s.map(|i| i.to_string()).unwrap_or_default()
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Probe> {
    let space: String = r.get(1)?;
    let kind: String = r.get(3)?;
    let targets: String = r.get(4)?;
    let split: String = r.get(5)?;
    let author_kind: String = r.get(7)?;
    let status: String = r.get(10)?;
    let bad = |i: usize, what: &str| {
        crate::time::conv_err(i, StoreError::Corrupt(format!("probes: {what}")))
    };
    Ok(Probe {
        id: r.get(0)?,
        space: if space.is_empty() {
            None
        } else {
            Some(space.parse().map_err(|_| bad(1, "space_id"))?)
        },
        question: r.get(2)?,
        kind: ProbeKind::parse(&kind).ok_or_else(|| bad(3, "kind"))?,
        targets: serde_json::from_str(&targets).map_err(|e| crate::time::conv_err(4, e))?,
        split: Split::parse(&split).ok_or_else(|| bad(5, "split"))?,
        overlap: r.get(6)?,
        author_kind: ProbeAuthor::parse(&author_kind).ok_or_else(|| bad(7, "author_kind"))?,
        author: r.get(8)?,
        work_id: r.get(9)?,
        active: status == "active",
        retired_reason: r.get(11)?,
        created_at: ts_at(r, 12)?,
        retired_at: ts_opt(r, 13)?,
    })
}

/// Активных проб с целью в базе.
fn active_with_target(c: &Connection) -> Result<u32, StoreError> {
    let n: i64 = c.query_row(
        "SELECT count(*) FROM probes WHERE status = 'active' AND kind != 'negative'",
        [],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Нормализованные вопросы активных проб пространства `space`.
fn active_questions(c: &Connection, space: &str) -> Result<HashSet<String>, StoreError> {
    let mut st =
        c.prepare_cached("SELECT question FROM probes WHERE status = 'active' AND space_id = ?1")?;
    let rows = st.query_map([space], |r| r.get::<_, String>(0))?;
    let mut out = HashSet::new();
    for q in rows {
        out.insert(normalize_query(&q?));
    }
    Ok(out)
}

fn insert_tx(c: &Connection, p: &NewProbe) -> Result<InsertOutcome, StoreError> {
    let space = space_col(p.space);
    if active_questions(c, &space)?.contains(&normalize_query(&p.question)) {
        return Ok(InsertOutcome::Duplicate);
    }
    if p.kind != ProbeKind::Negative && active_with_target(c)? >= PROBES_MAX {
        return Ok(InsertOutcome::Full);
    }
    let id = Id::new();
    let targets = serde_json::to_string(&p.targets)
        .map_err(|e| StoreError::BadInput(format!("targets: {e}")))?;
    c.execute(
        "INSERT INTO probes (id, space_id, question, kind, targets, split, overlap, author_kind,
                             author, work_id, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'active', ?11)",
        params![
            id,
            space,
            p.question,
            p.kind.as_str(),
            targets,
            p.split.as_str(),
            p.overlap,
            p.author_kind.as_str(),
            p.author,
            p.work_id,
            fmt_ts(crate::time::now()),
        ],
    )?;
    Ok(InsertOutcome::Inserted(id))
}

/// Записывает пробу (см. модуль: повтор вопроса — `Duplicate`, предел — `Full`). Пустой вопрос —
/// `BadInput`; у пробы с целью — хотя бы одна цель.
pub async fn insert_probe(db: &Db, p: NewProbe) -> Result<InsertOutcome, StoreError> {
    if p.question.trim().is_empty() {
        return Err(StoreError::BadInput("question: пустой вопрос".into()));
    }
    if p.kind != ProbeKind::Negative && p.targets.is_empty() {
        return Err(StoreError::BadInput(
            "targets: у пробы с целью — хотя бы одна".into(),
        ));
    }
    db.write(move |t| insert_tx(t, &p)).await
}

/// Активные пробы базы по порядку записи.
pub async fn active_probes(db: &Db) -> Result<Vec<Probe>, StoreError> {
    db.read(|c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT {COLS} FROM probes WHERE status = 'active' ORDER BY created_at, id"
        ))?;
        let rows = st.query_map([], row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .await
}

/// Пробы по фильтру, новые первыми.
pub async fn list_probes(db: &Db, f: &ProbeFilter) -> Result<Vec<Probe>, StoreError> {
    let f = f.clone();
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT {COLS} FROM probes
             WHERE (?1 IS NULL OR kind = ?1)
               AND (?2 IS NULL OR (status = 'active') = ?2)
               AND (?3 IS NULL OR split = ?3)
             ORDER BY created_at DESC, id DESC LIMIT ?4"
        ))?;
        let rows = st.query_map(
            params![
                f.kind.map(ProbeKind::as_str),
                f.active,
                f.split.map(Split::as_str),
                i64::try_from(f.limit).unwrap_or(i64::MAX),
            ],
            row,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .await
}

/// Сводка проб базы: активных всего, активных по видам и частям.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeCounts {
    pub active: u32,
    pub by_kind: Vec<(String, u32)>,
    pub by_split: Vec<(String, u32)>,
}

/// Сводка активных проб ([`ProbeCounts`]).
pub async fn probe_counts(db: &Db) -> Result<ProbeCounts, StoreError> {
    db.read(|c| {
        let group = |col: &str| -> Result<Vec<(String, u32)>, StoreError> {
            let mut st = c.prepare_cached(&format!(
                "SELECT {col}, count(*) FROM probes WHERE status = 'active' GROUP BY {col} \
                 ORDER BY {col}"
            ))?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    u32::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
                ))
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        };
        let by_kind = group("kind")?;
        let by_split = group("split")?;
        Ok(ProbeCounts {
            active: by_kind.iter().map(|(_, n)| n).sum(),
            by_kind,
            by_split,
        })
    })
    .await
}

/// Снимает пробу `id` с причиной `reason`; `false` — пробы нет или она уже снята.
pub async fn retire_probe(db: &Db, id: Id, reason: &str) -> Result<bool, StoreError> {
    let reason = reason.to_string();
    db.write(move |t| {
        let n = t.execute(
            "UPDATE probes SET status = 'retired', retired_reason = ?2, retired_at = ?3
             WHERE id = ?1 AND status = 'active'",
            params![id, reason, fmt_ts(crate::time::now())],
        )?;
        Ok(n > 0)
    })
    .await
}

/// Новые пробы вида `probe` с `since` по происхождению работы `origin` (`nightly` | `manual`,
/// `JOIN works ON works.id = probes.work_id`, §6.3 п. 2): ночной предел — с начала окна по
/// `nightly`, ручной — с начала суток UTC по `manual`. Снятые после записи тоже считаются.
pub async fn new_probes_since(db: &Db, origin: &str, since: Timestamp) -> Result<u32, StoreError> {
    let origin = origin.to_string();
    db.read(move |c| {
        let n: i64 = c.query_row(
            "SELECT count(*) FROM probes p JOIN works w ON w.id = p.work_id
             WHERE p.kind = 'probe' AND w.origin = ?1 AND p.created_at >= ?2",
            params![origin, fmt_ts(since)],
            |r| r.get(0),
        )?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    })
    .await
}

/// Новые пробы вида `kind` с `since` (у проб из кэша — предел за ночь).
pub async fn kind_since(db: &Db, kind: ProbeKind, since: Timestamp) -> Result<u32, StoreError> {
    db.read(move |c| {
        let n: i64 = c.query_row(
            "SELECT count(*) FROM probes WHERE kind = ?1 AND created_at >= ?2",
            params![kind.as_str(), fmt_ts(since)],
            |r| r.get(0),
        )?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    })
    .await
}

/// Цель жива: заметка не удалена; запись журнала не удалена, не замещена и не отозвана.
fn target_alive(c: &Connection, t: &ProbeTarget) -> Result<bool, StoreError> {
    let Some(id) = t.id() else {
        return Ok(false);
    };
    if t.letter() == "n" {
        let live: Option<i64> = c
            .query_row(
                "SELECT 1 FROM nodes WHERE id = ?1 AND deleted_at IS NULL",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        return Ok(live.is_some());
    }
    let live: Option<i64> = c
        .query_row(
            "SELECT 1 FROM entries WHERE id = ?1 AND deleted_at IS NULL
               AND (status IS NULL OR status NOT IN ('superseded', 'retracted'))",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(live.is_some())
}

/// Снимает активные пробы с целью, у которых не осталось ни одной живой цели
/// ([`RETIRED_TARGET_GONE`]); id снятых.
pub async fn retire_gone(db: &Db) -> Result<Vec<Id>, StoreError> {
    db.write(|t| {
        let probes: Vec<Probe> = {
            let mut st = t.prepare_cached(&format!(
                "SELECT {COLS} FROM probes WHERE status = 'active' AND kind != 'negative'"
            ))?;
            let rows = st.query_map([], row)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let now = fmt_ts(crate::time::now());
        let mut out = Vec::new();
        for p in probes {
            let mut alive = false;
            for tg in &p.targets {
                if target_alive(t, tg)? {
                    alive = true;
                    break;
                }
            }
            if !alive {
                t.execute(
                    "UPDATE probes SET status = 'retired', retired_reason = ?2, retired_at = ?3
                     WHERE id = ?1",
                    params![p.id, RETIRED_TARGET_GONE, now],
                )?;
                out.push(p.id);
            }
        }
        Ok(out)
    })
    .await
}

/// Сверяет посторонние вопросы базы с набором сервера `set` (вопрос и часть): активные вопросы
/// вне набора снимаются ([`RETIRED_SET_CHANGED`]), недостающие записываются (автор `system`, без
/// пространства и целей). Возвращает число записанных.
pub async fn sync_negatives(db: &Db, set: Vec<(String, Split)>) -> Result<u32, StoreError> {
    db.write(move |t| {
        let have: Vec<(Id, String)> = {
            let mut st = t.prepare_cached(
                "SELECT id, question FROM probes WHERE status = 'active' AND kind = 'negative'",
            )?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let want: HashSet<String> = set.iter().map(|(q, _)| normalize_query(q)).collect();
        let now = fmt_ts(crate::time::now());
        for (id, q) in &have {
            if !want.contains(&normalize_query(q)) {
                t.execute(
                    "UPDATE probes SET status = 'retired', retired_reason = ?2, retired_at = ?3
                     WHERE id = ?1",
                    params![id, RETIRED_SET_CHANGED, now],
                )?;
            }
        }
        let mut added = 0;
        for (q, split) in set {
            let p = NewProbe {
                space: None,
                question: q,
                kind: ProbeKind::Negative,
                targets: Vec::new(),
                split,
                overlap: None,
                author_kind: ProbeAuthor::System,
                author: None,
                work_id: None,
            };
            if matches!(insert_tx(t, &p)?, InsertOutcome::Inserted(_)) {
                added += 1;
            }
        }
        Ok(added)
    })
    .await
}

/// Действующая выжимка кэша — кандидат пробы из кэша (§6.4 п. 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheCandidate {
    pub space: Id,
    pub query: String,
    /// Источники выжимки (до [`CACHE_PROBE_TARGETS`]): `n:<id>` или ссылка записи по виду.
    pub targets: Vec<ProbeTarget>,
}

/// Целей у пробы из кэша — не больше.
pub const CACHE_PROBE_TARGETS: usize = 5;

/// Действующие выжимки кэша (`state = 'valid'`, не удалённые), новые первыми, не больше `limit`:
/// запрос и источники (`cache_deps`, до [`CACHE_PROBE_TARGETS`]). Выжимка без источников-целей
/// пропускается.
pub async fn cache_candidates(db: &Db, limit: usize) -> Result<Vec<CacheCandidate>, StoreError> {
    db.read(move |c| {
        let entries: Vec<(Id, Id, String)> = {
            let mut st = c.prepare_cached(
                "SELECT id, space_id, query FROM answer_cache
                 WHERE state = 'valid' AND deleted_at IS NULL
                 ORDER BY created_at DESC, id DESC LIMIT ?1",
            )?;
            let rows = st.query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut deps = c.prepare_cached(
            "SELECT d.target_kind, d.target_id, d.rev, e.kind FROM cache_deps d
             LEFT JOIN entries e ON d.target_kind = 'entry' AND e.id = d.target_id
             WHERE d.cache_id = ?1 ORDER BY d.rowid",
        )?;
        let mut out = Vec::new();
        for (id, space, query) in entries {
            let rows = deps.query_map([id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Id>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?;
            let mut targets: Vec<ProbeTarget> = Vec::new();
            for row in rows {
                let (kind, tid, rev, entry_kind) = row?;
                let letter = match (kind.as_str(), entry_kind.as_deref()) {
                    ("node", _) => "n",
                    ("entry", Some(k)) => EntryKind::parse(k).map_or("e", EntryKind::ref_letter),
                    _ => continue,
                };
                let t = ProbeTarget {
                    target: format!("{letter}:{tid}"),
                    rev,
                };
                if !targets.contains(&t) {
                    targets.push(t);
                }
                if targets.len() == CACHE_PROBE_TARGETS {
                    break;
                }
            }
            if !targets.is_empty() {
                out.push(CacheCandidate {
                    space,
                    query,
                    targets,
                });
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
    fn kinds_parse_inverse() {
        for k in ProbeKind::ALL {
            assert_eq!(ProbeKind::parse(k.as_str()), Some(k));
        }
        for a in ProbeAuthor::ALL {
            assert_eq!(ProbeAuthor::parse(a.as_str()), Some(a));
        }
        assert_eq!(ProbeKind::parse("tidy"), None);
        let t = ProbeTarget {
            target: format!("i:{}", Id::new()),
            rev: 2,
        };
        assert_eq!(t.letter(), "i");
        assert!(t.id().is_some());
        let j = serde_json::to_value(&t).unwrap();
        assert_eq!(j["rev"], 2);
        assert!(j["ref"].as_str().unwrap().starts_with("i:"));
    }
}
