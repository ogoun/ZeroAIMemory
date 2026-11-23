//! Журнал отправок подключённой модели (`work_calls`, спецификация M7 §6.2 п. 8, миграция 0027):
//! метаданные каждого обращения — время, работа, вид (`test` — проверка подключения), хост,
//! модель, токены (оценка — `estimated`), длительность, исход (`ok`, класс ошибки или
//! `aborted`), код HTTP, «вне LAN», число элементов пакета. **Текста пакета и ответа нет.**
//!
//! - Расход суток и месяца пользователя — сумма по журналу ([`tokens_since`]): отдельного
//!   счётчика нет.
//! - «Стоп до следующей ночи» и аудит `works.model.key_use` «раз за ночь» сервер считает по
//!   строкам ночи или ручного запуска ([`calls_between`]; решение координатора 77): перезапуск в
//!   окне стоп не теряет.
//! - Строка без отправки — `ms = NULL` (запрос не ушёл: предел `limit`, имя не разрешилось).
//! - Хранение — 180 суток от `ts` ([`gc`], воркер обслуживания).
use jiff::Timestamp;
use rusqlite::params;
use zam_core::Id;

use crate::time::{fmt_ts, ts_at};
use crate::{Db, StoreError};

/// Вид строки проверки подключения (`POST /works/model/test`).
pub const KIND_TEST: &str = "test";
/// Исход удачного обращения.
pub const OUTCOME_OK: &str = "ok";

/// Строка журнала отправок.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallRow {
    pub ts: Timestamp,
    /// Работа `j:<id>` (у проверки подключения — нет).
    pub work_id: Option<Id>,
    /// Вид работы или [`KIND_TEST`].
    pub kind: String,
    pub host: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated: bool,
    /// Длительность обращения, мс; `None` — запрос не отправлялся.
    pub ms: Option<u64>,
    /// `ok`, класс ошибки или `aborted`.
    pub outcome: String,
    pub http_status: Option<u16>,
    pub external: bool,
    /// Элементов в пакете (у проверки подключения — нет).
    pub items: Option<u32>,
}

impl CallRow {
    /// Запрос ушёл к модели (есть длительность обращения).
    pub fn sent(&self) -> bool {
        self.ms.is_some()
    }

    /// Токены строки (вход + выход).
    pub fn tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

const COLS: &str = "ts, work_id, kind, host, model, input_tokens, output_tokens, estimated, ms, \
                    outcome, http_status, external, items";

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<CallRow> {
    let u =
        |i: usize| -> rusqlite::Result<u64> { Ok(u64::try_from(r.get::<_, i64>(i)?).unwrap_or(0)) };
    Ok(CallRow {
        ts: ts_at(r, 0)?,
        work_id: r.get(1)?,
        kind: r.get(2)?,
        host: r.get(3)?,
        model: r.get(4)?,
        input_tokens: u(5)?,
        output_tokens: u(6)?,
        estimated: r.get::<_, i64>(7)? != 0,
        ms: r
            .get::<_, Option<i64>>(8)?
            .map(|v| u64::try_from(v).unwrap_or(0)),
        outcome: r.get(9)?,
        http_status: r
            .get::<_, Option<i64>>(10)?
            .and_then(|v| u16::try_from(v).ok()),
        external: r.get::<_, i64>(11)? != 0,
        items: r
            .get::<_, Option<i64>>(12)?
            .and_then(|v| u32::try_from(v).ok()),
    })
}

/// Дописывает строку журнала.
pub async fn record_call(db: &Db, c: &CallRow) -> Result<(), StoreError> {
    let c = c.clone();
    db.write(move |t| {
        t.execute(
            &format!(
                "INSERT INTO work_calls ({COLS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"
            ),
            params![
                fmt_ts(c.ts),
                c.work_id,
                c.kind,
                c.host,
                c.model,
                to_i64(c.input_tokens),
                to_i64(c.output_tokens),
                i64::from(c.estimated),
                c.ms.map(to_i64),
                c.outcome,
                c.http_status.map(i64::from),
                i64::from(c.external),
                c.items.map(i64::from),
            ],
        )?;
        Ok(())
    })
    .await
}

/// Токены (вход + выход) всех строк с `ts >= since` — расход суток или месяца.
pub async fn tokens_since(db: &Db, since: Timestamp) -> Result<u64, StoreError> {
    let since = fmt_ts(since);
    db.read(move |c| {
        let n: i64 = c.query_row(
            "SELECT coalesce(sum(input_tokens + output_tokens), 0) FROM work_calls WHERE ts >= ?1",
            [since],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(n).unwrap_or(0))
    })
    .await
}

/// Последние строки с `ts >= since`, новые первыми, не больше `limit`.
pub async fn list_calls(
    db: &Db,
    since: Timestamp,
    limit: usize,
) -> Result<Vec<CallRow>, StoreError> {
    let since = fmt_ts(since);
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT {COLS} FROM work_calls WHERE ts >= ?1 ORDER BY ts DESC, id DESC LIMIT ?2"
        ))?;
        let rows = st.query_map(params![since, limit], row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .await
}

/// Строки с `from <= ts < until` (`until = None` — без верхней границы) по порядку записи:
/// строки одной ночи или одного ручного запуска.
pub async fn calls_between(
    db: &Db,
    from: Timestamp,
    until: Option<Timestamp>,
) -> Result<Vec<CallRow>, StoreError> {
    let from = fmt_ts(from);
    let until = until.map(fmt_ts);
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT {COLS} FROM work_calls
             WHERE ts >= ?1 AND (?2 IS NULL OR ts < ?2)
             ORDER BY ts, id"
        ))?;
        let rows = st.query_map(params![from, until], row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .await
}

/// Удаляет строки старше `cutoff` (по `ts`); число удалённых.
pub async fn gc(db: &Db, cutoff: Timestamp) -> Result<u64, StoreError> {
    let cutoff = fmt_ts(cutoff);
    db.write(move |t| {
        let n = t.execute("DELETE FROM work_calls WHERE ts < ?1", [cutoff])?;
        Ok(n as u64)
    })
    .await
}
