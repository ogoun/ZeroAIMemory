//! Прогоны ночной проверки памяти и их результаты по пробам (`check_runs`, `check_results`,
//! спецификация M7 §6.5 п. 7, миграция 0027).
//!
//! - **Повод** — колонка `trigger_kind` (`nightly` | `manual`; TRIGGER — ключевое слово SQL,
//!   решение координатора 75). У ночного прогона `night` — дата ночи (слот в поясе `[works] tz`),
//!   у ручного — `NULL`: ночной прогон ночи один ([`has_nightly`] — кроме `failed`, в том числе
//!   прерванных остановкой).
//! - **Состояния** — `running` → `done` | `skipped` (переиндексация) | `timeout` (предел
//!   `check_max_min`, метрики по пройденным, без вердикта) | `failed` (`error = "interrupted"` —
//!   оборван остановкой сервера, [`mark_interrupted`] при следующем старте).
//! - **JSON** — `fingerprint`, `context`, `metrics`, `verdict` (вердикт — `{kind, flags, …}` сервера);
//!   `flags` — массив строк (`backup_overlap`, …); `episode` — id первого прогона эпизода регрессии;
//!   `accepted_at`/`accepted_by` — норма, принятая человеком ([`accept_run`]).
//! - **Результаты** — ключ `(run_id, probe_id, space_id)`: посторонний вопрос идёт в области каждого
//!   решения пользователя, `space_id = ''` — без решения (решение координатора 69). Ранги — номер
//!   карточки цели с 1, `NULL` — не нашлась.
//! - **Хранение** — 365 суток от `started_at` ([`gc`], воркер обслуживания), результаты — вместе с
//!   прогоном.
use std::collections::HashMap;

use jiff::{Timestamp, civil};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use zam_core::Id;

use crate::time::{fmt_ts, ts_at, ts_opt};
use crate::{Db, StoreError};

/// `error` прогона, оборванного остановкой сервера.
pub const ERROR_INTERRUPTED: &str = "interrupted";

/// Повод прогона.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Trigger {
    Nightly,
    Manual,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Nightly => "nightly",
            Trigger::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Option<Trigger> {
        [Trigger::Nightly, Trigger::Manual]
            .into_iter()
            .find(|t| t.as_str() == s)
    }
}

/// Состояние прогона.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunStatus {
    Running,
    Done,
    Skipped,
    Timeout,
    Failed,
}

impl RunStatus {
    pub const ALL: [RunStatus; 5] = [
        RunStatus::Running,
        RunStatus::Done,
        RunStatus::Skipped,
        RunStatus::Timeout,
        RunStatus::Failed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Done => "done",
            RunStatus::Skipped => "skipped",
            RunStatus::Timeout => "timeout",
            RunStatus::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<RunStatus> {
        RunStatus::ALL.into_iter().find(|x| x.as_str() == s)
    }
}

/// Прогон проверки (колонки §6.5 п. 7).
#[derive(Debug, Clone, PartialEq)]
pub struct CheckRun {
    pub id: Id,
    pub trigger: Trigger,
    /// Дата ночи у ночного прогона.
    pub night: Option<civil::Date>,
    pub status: RunStatus,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    pub fingerprint: Option<Value>,
    pub fp_hash: Option<String>,
    pub context: Option<Value>,
    pub metrics: Option<Value>,
    pub verdict: Option<Value>,
    pub flags: Vec<String>,
    pub episode: Option<Id>,
    pub accepted_at: Option<Timestamp>,
    pub accepted_by: Option<String>,
    pub error: Option<String>,
}

impl CheckRun {
    /// Вид вердикта (`baseline`, `ok`, `regression`, `changed`, `degraded`); нет вердикта —
    /// `None`.
    pub fn verdict_kind(&self) -> Option<&str> {
        self.verdict.as_ref().and_then(|v| v["kind"].as_str())
    }
}

/// Результат пробы в прогоне.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    pub probe_id: Id,
    /// Решение запроса постороннего вопроса или пространство пробы; `None` — без решения
    /// (`space_id = ''`).
    pub space: Option<Id>,
    pub search_rank: Option<u32>,
    /// Карточек в бюджете основного поиска.
    pub search_cut: Option<u32>,
    pub deep_rank: Option<u32>,
    pub prompt_rank: Option<u32>,
    pub search_ms: Option<u32>,
    pub prompt_ms: Option<u32>,
}

/// Итог прогона ([`finish_run`]).
#[derive(Debug, Clone, PartialEq)]
pub struct RunFinish {
    pub status: RunStatus,
    pub fingerprint: Option<Value>,
    pub fp_hash: Option<String>,
    pub context: Option<Value>,
    pub metrics: Option<Value>,
    pub verdict: Option<Value>,
    pub flags: Vec<String>,
    pub episode: Option<Id>,
    pub error: Option<String>,
}

const COLS: &str = "id, trigger_kind, night, status, started_at, finished_at, fingerprint, fp_hash, \
                    context, metrics, verdict, flags, episode, accepted_at, accepted_by, error";

fn json_col(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<Option<Value>> {
    let s: Option<String> = r.get(i)?;
    s.map(|s| serde_json::from_str(&s).map_err(|e| crate::time::conv_err(i, e)))
        .transpose()
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<CheckRun> {
    let bad = |i: usize, what: &str| {
        crate::time::conv_err(i, StoreError::Corrupt(format!("check_runs: {what}")))
    };
    let trigger: String = r.get(1)?;
    let night: Option<String> = r.get(2)?;
    let status: String = r.get(3)?;
    let flags: String = r.get(11)?;
    Ok(CheckRun {
        id: r.get(0)?,
        trigger: Trigger::parse(&trigger).ok_or_else(|| bad(1, "trigger_kind"))?,
        night: night
            .map(|n| n.parse::<civil::Date>().map_err(|_| bad(2, "night")))
            .transpose()?,
        status: RunStatus::parse(&status).ok_or_else(|| bad(3, "status"))?,
        started_at: ts_at(r, 4)?,
        finished_at: ts_opt(r, 5)?,
        fingerprint: json_col(r, 6)?,
        fp_hash: r.get(7)?,
        context: json_col(r, 8)?,
        metrics: json_col(r, 9)?,
        verdict: json_col(r, 10)?,
        flags: serde_json::from_str(&flags).map_err(|e| crate::time::conv_err(11, e))?,
        episode: r.get(12)?,
        accepted_at: ts_opt(r, 13)?,
        accepted_by: r.get(14)?,
        error: r.get(15)?,
    })
}

fn to_text(v: &Option<Value>) -> Option<String> {
    v.as_ref().map(Value::to_string)
}

fn u32_col(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<Option<u32>> {
    Ok(r.get::<_, Option<i64>>(i)?
        .map(|v| u32::try_from(v).unwrap_or(u32::MAX)))
}

/// Начинает прогон `id` (`running`, начало — `started_at`).
pub async fn start_run(
    db: &Db,
    id: Id,
    trigger: Trigger,
    night: Option<civil::Date>,
    started_at: Timestamp,
) -> Result<(), StoreError> {
    db.write(move |t| {
        t.execute(
            "INSERT INTO check_runs (id, trigger_kind, night, status, started_at)
             VALUES (?1, ?2, ?3, 'running', ?4)",
            params![
                id,
                trigger.as_str(),
                night.map(|d| d.to_string()),
                fmt_ts(started_at)
            ],
        )?;
        Ok(())
    })
    .await
}

/// Заканчивает прогон `id` итогом `f` и результатами `results` — одной транзакцией.
pub async fn finish_run(
    db: &Db,
    id: Id,
    f: RunFinish,
    results: Vec<CheckResult>,
) -> Result<(), StoreError> {
    db.write(move |t| {
        let n = t.execute(
            "UPDATE check_runs SET status = ?2, finished_at = ?3, fingerprint = ?4, fp_hash = ?5,
                    context = ?6, metrics = ?7, verdict = ?8, flags = ?9, episode = ?10,
                    error = ?11
             WHERE id = ?1",
            params![
                id,
                f.status.as_str(),
                fmt_ts(crate::time::now()),
                to_text(&f.fingerprint),
                f.fp_hash,
                to_text(&f.context),
                to_text(&f.metrics),
                to_text(&f.verdict),
                Value::from(f.flags.clone()).to_string(),
                f.episode,
                f.error,
            ],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound);
        }
        let mut ins = t.prepare_cached(
            "INSERT OR REPLACE INTO check_results (run_id, probe_id, space_id, search_rank,
                 search_cut, deep_rank, prompt_rank, search_ms, prompt_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        for r in results {
            ins.execute(params![
                id,
                r.probe_id,
                r.space.map(|s| s.to_string()).unwrap_or_default(),
                r.search_rank,
                r.search_cut,
                r.deep_rank,
                r.prompt_rank,
                r.search_ms,
                r.prompt_ms,
            ])?;
        }
        Ok(())
    })
    .await
}

/// Прогоны `running`, кроме `except` (идут в этом процессе), — `failed` с `error =
/// "interrupted"` (оборваны остановкой сервера, §6.5 п. 1); число помеченных.
pub async fn mark_interrupted(db: &Db, except: Vec<Id>) -> Result<u64, StoreError> {
    db.write(move |t| {
        let ids: Vec<Id> = {
            let mut st = t.prepare_cached("SELECT id FROM check_runs WHERE status = 'running'")?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let now = fmt_ts(crate::time::now());
        let mut n = 0;
        for id in ids.into_iter().filter(|i| !except.contains(i)) {
            n += t.execute(
                "UPDATE check_runs SET status = 'failed', error = ?2, finished_at = ?3
                 WHERE id = ?1 AND status = 'running'",
                params![id, ERROR_INTERRUPTED, now],
            )? as u64;
        }
        Ok(n)
    })
    .await
}

/// У базы за ночь `night` уже есть ночной прогон — любой, кроме `failed` (в том числе
/// прерванных): по `(trigger_kind, night)`, индекс `check_runs_night`.
pub async fn has_nightly(db: &Db, night: civil::Date) -> Result<bool, StoreError> {
    db.read(move |c| {
        let n: Option<i64> = c
            .query_row(
                "SELECT 1 FROM check_runs
                 WHERE trigger_kind = 'nightly' AND night = ?1 AND status != 'failed' LIMIT 1",
                [night.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(n.is_some())
    })
    .await
}

/// Последние прогоны базы, новые первыми, не больше `limit`.
pub async fn runs(db: &Db, limit: usize) -> Result<Vec<CheckRun>, StoreError> {
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT {COLS} FROM check_runs ORDER BY started_at DESC, rowid DESC LIMIT ?1"
        ))?;
        let rows = st.query_map([i64::try_from(limit).unwrap_or(i64::MAX)], row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .await
}

/// Прогоны повода `trigger` и статуса `status` (`None` — любые), новые первыми, не больше
/// `limit`: `GET /check/runs?trigger=&status=` (WebUI ищет последний законченный ночной прогон
/// независимо от числа ручных — задача 8 M7.3, ревью r1).
pub async fn runs_of(
    db: &Db,
    trigger: Option<Trigger>,
    status: Option<RunStatus>,
    limit: usize,
) -> Result<Vec<CheckRun>, StoreError> {
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT {COLS} FROM check_runs
             WHERE (?2 IS NULL OR trigger_kind = ?2) AND (?3 IS NULL OR status = ?3)
             ORDER BY started_at DESC, rowid DESC LIMIT ?1"
        ))?;
        let rows = st.query_map(
            params![
                i64::try_from(limit).unwrap_or(i64::MAX),
                trigger.map(Trigger::as_str),
                status.map(RunStatus::as_str)
            ],
            row,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .await
}

/// Последние законченные (`done`) ночные прогоны, новые первыми, не больше `limit`.
pub async fn nightly_done(db: &Db, limit: usize) -> Result<Vec<CheckRun>, StoreError> {
    db.read(move |c| nightly_done_in(c, limit)).await
}

/// [`nightly_done`] на соединении `c` (в том числе только для чтения — `zam-server doctor`).
pub fn nightly_done_in(c: &Connection, limit: usize) -> Result<Vec<CheckRun>, StoreError> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {COLS} FROM check_runs WHERE trigger_kind = 'nightly' AND status = 'done'
         ORDER BY started_at DESC, rowid DESC LIMIT ?1"
    ))?;
    let rows = st.query_map([i64::try_from(limit).unwrap_or(i64::MAX)], row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Прогон по id.
pub async fn get_run(db: &Db, id: Id) -> Result<Option<CheckRun>, StoreError> {
    db.read(move |c| {
        Ok(
            c.prepare_cached(&format!("SELECT {COLS} FROM check_runs WHERE id = ?1"))?
                .query_row([id], row)
                .optional()?,
        )
    })
    .await
}

fn result_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(Id, CheckResult)> {
    let space: String = r.get(2)?;
    Ok((
        r.get(0)?,
        CheckResult {
            probe_id: r.get(1)?,
            space: if space.is_empty() {
                None
            } else {
                Some(space.parse().map_err(|_| {
                    crate::time::conv_err(2, StoreError::Corrupt("check_results: space_id".into()))
                })?)
            },
            search_rank: u32_col(r, 3)?,
            search_cut: u32_col(r, 4)?,
            deep_rank: u32_col(r, 5)?,
            prompt_rank: u32_col(r, 6)?,
            search_ms: u32_col(r, 7)?,
            prompt_ms: u32_col(r, 8)?,
        },
    ))
}

/// Результаты прогона `run` по пробам (у посторонних вопросов — строка на решение).
pub async fn results(db: &Db, run: Id) -> Result<Vec<CheckResult>, StoreError> {
    Ok(results_of(db, vec![run])
        .await?
        .remove(&run)
        .unwrap_or_default())
}

/// Результаты прогонов `runs`, по прогону.
pub async fn results_of(
    db: &Db,
    runs: Vec<Id>,
) -> Result<HashMap<Id, Vec<CheckResult>>, StoreError> {
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT run_id, probe_id, space_id, search_rank, search_cut, deep_rank, prompt_rank,
                    search_ms, prompt_ms
             FROM check_results WHERE run_id = ?1 ORDER BY probe_id, space_id",
        )?;
        let mut out: HashMap<Id, Vec<CheckResult>> = HashMap::new();
        for run in runs {
            let rows = st.query_map([run], result_row)?;
            let v = out.entry(run).or_default();
            for r in rows {
                v.push(r?.1);
            }
        }
        Ok(out)
    })
    .await
}

/// Итог [`accept_run`].
#[derive(Debug, Clone, PartialEq)]
pub enum AcceptOutcome {
    /// Норма принята; вердикт прогона.
    Accepted(Value),
    /// Прогон — не последний законченный ночной прогон открытого эпизода регрессии (или эпизода
    /// нет).
    NotLatest,
}

/// Принимает ухудшение как норму (§6.5 п. 5): только последний законченный ночной прогон, и
/// только с вердиктом `regression`, ещё не принятый; прогон получает `accepted_at`/`accepted_by`
/// и дальше входит в базу (база начинается с него). Нет прогона — `NotFound`.
pub async fn accept_run(db: &Db, run: Id, by: &str) -> Result<AcceptOutcome, StoreError> {
    let by = by.to_string();
    db.write(move |t| {
        let exists: Option<i64> = t
            .query_row("SELECT 1 FROM check_runs WHERE id = ?1", [run], |r| {
                r.get(0)
            })
            .optional()?;
        if exists.is_none() {
            return Err(StoreError::NotFound);
        }
        let Some(last) = nightly_done_in(t, 1)?.pop() else {
            return Ok(AcceptOutcome::NotLatest);
        };
        if last.id != run || last.verdict_kind() != Some("regression") || last.accepted_at.is_some()
        {
            return Ok(AcceptOutcome::NotLatest);
        }
        t.execute(
            "UPDATE check_runs SET accepted_at = ?2, accepted_by = ?3 WHERE id = ?1",
            params![run, fmt_ts(crate::time::now()), by],
        )?;
        Ok(AcceptOutcome::Accepted(last.verdict.unwrap_or(Value::Null)))
    })
    .await
}

/// Удаляет прогоны с `started_at < cutoff` вместе с их результатами; число удалённых прогонов.
pub async fn gc(db: &Db, cutoff: Timestamp) -> Result<u64, StoreError> {
    let cutoff = fmt_ts(cutoff);
    db.write(move |t| {
        t.execute(
            "DELETE FROM check_results WHERE run_id IN
               (SELECT id FROM check_runs WHERE started_at < ?1)",
            [&cutoff],
        )?;
        let n = t.execute("DELETE FROM check_runs WHERE started_at < ?1", [&cutoff])?;
        Ok(n as u64)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_parse_inverse() {
        for s in RunStatus::ALL {
            assert_eq!(RunStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(Trigger::parse("nightly"), Some(Trigger::Nightly));
        assert_eq!(Trigger::parse("manual"), Some(Trigger::Manual));
        assert_eq!(Trigger::parse("trigger"), None);
    }
}
