//! Виды внутренних работ и типы предложений человеку (M7.3 §6 «Общее», §6.3 п. 1, §6.7 п. 1;
//! миграция 0027).
//!
//! В схеме у `works.kind` и `proposals.type` CHECK нет: допустимые значения — эти перечисления.
//! Строку неизвестного значения читатель пропускает с WARN ([`WorkKind::parse`] — `None`), так
//! виды уборки M7.5 добавятся без миграции. Тестовый вид `echo` есть только с фичей
//! `test-hooks` (решение координатора 73): `test` занят видом строки журнала отправок проверки
//! подключения (§6.2 п. 9).
//!
//! **Очередь работ** (§6.3 п. 1, задача 3 плана): работа `j:<id>` — строка `works`
//! ([`Work`]). Состояния `queued → running → done | failed`; `expired` — работа агента без итога
//! [`AGENT_TTL_DAYS`] суток; `cancelled` — снял человек, сменился режим вида или исполнитель
//! отказался (`skip`). Захват агентом ([`claim_next`]) — аренда (2 ч у сервера): истекла
//! ([`expire`]) — снова `queued`, `attempts + 1`, после [`LEASE_ATTEMPTS`]-й — `failed`
//! (`lease_expired`). Незакрытой работы вида на пространство — не больше одной
//! ([`enqueue`] — `None`): долг не копится. `items` — кандидаты пакета ([`WorkItem`]: ссылка и
//! ревизия, вид, заголовок, текст вида агента, сигналы); `result` — итог (принято, отклонено с
//! причинами, явные `skip` исполнителя, пропущено); `result_key` — `Idempotency-Key` принятого
//! итога. Время — `crate::time::now()` (в тестах — замороженные часы).

use std::collections::{HashMap, HashSet};

use jiff::{SignedDuration, Timestamp, civil};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zam_core::Id;

use crate::time::{fmt_ts, now, ts_at, ts_opt};
use crate::{Db, StoreError};

/// Вид внутренней работы (`works.kind`, настройка `works.kind.<вид>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkKind {
    /// Пробные вопросы ночной проверки (§6.4 п. 2).
    Probes,
    /// Кандидаты в Rules (§6.7 п. 3).
    Rules,
    /// Тестовый вид: пакет и итог без смысла вида (только `test-hooks`).
    #[cfg(feature = "test-hooks")]
    Echo,
}

impl WorkKind {
    /// Все виды в порядке ночи (пробы, затем кандидаты в Rules; §6.3 п. 2).
    #[cfg(not(feature = "test-hooks"))]
    pub const ALL: &'static [WorkKind] = &[WorkKind::Probes, WorkKind::Rules];
    /// Все виды в порядке ночи (пробы, затем кандидаты в Rules; §6.3 п. 2) и тестовый `echo`.
    #[cfg(feature = "test-hooks")]
    pub const ALL: &'static [WorkKind] = &[WorkKind::Probes, WorkKind::Rules, WorkKind::Echo];

    pub fn as_str(self) -> &'static str {
        match self {
            WorkKind::Probes => "probes",
            WorkKind::Rules => "rules",
            #[cfg(feature = "test-hooks")]
            WorkKind::Echo => "echo",
        }
    }

    /// Вид по строке; неизвестная строка (в том числе `tidy` до M7.5 и тип предложения
    /// `rule`) — `None`.
    pub fn parse(s: &str) -> Option<WorkKind> {
        WorkKind::ALL.iter().copied().find(|k| k.as_str() == s)
    }
}

/// Тип предложения человеку (`proposals.type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProposalType {
    /// Кандидат в Rules: `payload {path, content}` (§6.7 п. 1).
    Rule,
}

impl ProposalType {
    pub const ALL: &'static [ProposalType] = &[ProposalType::Rule];

    pub fn as_str(self) -> &'static str {
        match self {
            ProposalType::Rule => "rule",
        }
    }

    /// Тип по строке; неизвестная — `None`.
    pub fn parse(s: &str) -> Option<ProposalType> {
        ProposalType::ALL.iter().copied().find(|t| t.as_str() == s)
    }
}

// ---------- очередь работ (§6.3 п. 1) ----------

/// Работа агента без итога дольше этого — `expired` (§6.3 п. 1), суток от постановки.
pub const AGENT_TTL_DAYS: i64 = 7;
/// Столько истёкших аренд — работа `failed` (`lease_expired`; §6.3 п. 1).
pub const LEASE_ATTEMPTS: u32 = 3;
/// `error_class` работы, у которой истекла последняя аренда.
pub const LEASE_EXPIRED: &str = "lease_expired";
/// `error_class` работы агента без итога [`AGENT_TTL_DAYS`] суток.
pub const EXPIRED: &str = "expired";
/// `error_class` работы, снятой сменой действующего режима вида (§6.1 п. 2).
pub const MODE_CHANGED: &str = "mode_changed";
/// `error_class` работы, от которой отказался исполнитель (`skip` без элементов, §6.3 п. 4).
pub const SKIPPED: &str = "skip";
/// Причина отказа элемента, выброшенного вторым сканом (§6.2 п. 6): запись не берётся в пакеты до
/// новой ревизии ([`excluded_refs`]).
pub const SECRET_IN_PAYLOAD: &str = "secret_in_payload";
/// `error_class` работы, все кандидаты которой к выдаче удалены или исправлены ([`live_refs`]).
pub const STALE: &str = "stale";
/// Сколько суток назад итоги работ ещё отсеивают кандидатов ([`excluded_refs`],
/// [`accepted_refs`]): чтение итогов не растёт без предела; отсев старше — снова отбирается и, если
/// повод остался, отсеивается заново.
pub const REFS_HORIZON_DAYS: i64 = 365;
/// `part` отметки показа строки внутренних работ дайджеста в `session_seen` (§6.3 п. 5): цель —
/// первая работа строки (`target_kind = entry`, id работы), ревизия не используется (1) — важен
/// сам факт показа: строка — раз за сессию (любая эпоха). Подсказки записей
/// ([`crate::RECALL_PART`]) такая отметка не задевает.
pub const WORKS_LINE_PART: &str = "works_line";

/// Кто выполняет работу (`works.mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkMode {
    /// Агент в своей сессии (`memory_recall event=work`).
    Agent,
    /// Подключённая модель (сервер, сразу после постановки).
    Model,
}

impl WorkMode {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkMode::Agent => "agent",
            WorkMode::Model => "model",
        }
    }

    pub fn parse(s: &str) -> Option<WorkMode> {
        [WorkMode::Agent, WorkMode::Model]
            .into_iter()
            .find(|m| m.as_str() == s)
    }
}

/// Постановка работы (`works.origin`): ночь или ручной запуск человека.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Nightly,
    Manual,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Nightly => "nightly",
            Origin::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Option<Origin> {
        [Origin::Nightly, Origin::Manual]
            .into_iter()
            .find(|o| o.as_str() == s)
    }
}

/// Состояние работы (`works.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkState {
    Queued,
    Running,
    Done,
    Failed,
    Expired,
    Cancelled,
}

impl WorkState {
    pub const ALL: [WorkState; 6] = [
        WorkState::Queued,
        WorkState::Running,
        WorkState::Done,
        WorkState::Failed,
        WorkState::Expired,
        WorkState::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            WorkState::Queued => "queued",
            WorkState::Running => "running",
            WorkState::Done => "done",
            WorkState::Failed => "failed",
            WorkState::Expired => "expired",
            WorkState::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<WorkState> {
        WorkState::ALL.into_iter().find(|w| w.as_str() == s)
    }

    /// Незакрытая работа: `queued` или `running`.
    pub fn is_open(self) -> bool {
        matches!(self, WorkState::Queued | WorkState::Running)
    }
}

/// Кандидат пакета в `works.items`: номер элемента `n` (как в `<record n="…">`), ссылка и
/// ревизия записи, её вид, заголовок (исходник), текст вида агента (маска — до обрезки) и сигналы
/// вида.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItem {
    pub n: usize,
    pub target: String,
    pub rev: i64,
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub signals: Vec<(String, String)>,
}

/// Работа — строка `works`.
#[derive(Debug, Clone, PartialEq)]
pub struct Work {
    pub id: Id,
    pub kind: WorkKind,
    pub space_id: Id,
    pub mode: WorkMode,
    pub origin: Origin,
    /// Ночь ночной постановки (дата слота в поясе `[works] tz`).
    pub night: Option<civil::Date>,
    pub state: WorkState,
    pub items: Vec<WorkItem>,
    pub claimed_session: Option<String>,
    pub claimed_until: Option<Timestamp>,
    pub attempts: u32,
    pub created_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub result: Option<Value>,
    pub result_key: Option<String>,
    pub error_class: Option<String>,
}

impl Work {
    /// Элемент пакета номер `n`.
    pub fn item(&self, n: usize) -> Option<&WorkItem> {
        self.items.iter().find(|i| i.n == n)
    }
}

/// Новая работа ([`enqueue`]).
#[derive(Debug, Clone)]
pub struct NewWork {
    pub kind: WorkKind,
    pub space: Id,
    pub mode: WorkMode,
    pub origin: Origin,
    pub night: Option<civil::Date>,
    pub items: Vec<WorkItem>,
}

const COLS: &str = "id, kind, space_id, mode, origin, night, state, items, claimed_session, \
                    claimed_until, attempts, created_at, started_at, finished_at, result, \
                    result_key, error_class";

/// Строка `works`; `Ok(None)` — неизвестный вид, режим, постановка или состояние (строка
/// пропускается с WARN: виды уборки M7.5 добавятся без миграции).
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Option<Work>> {
    let id: Id = r.get(0)?;
    let kind_s: String = r.get(1)?;
    let Some(kind) = WorkKind::parse(&kind_s) else {
        tracing::warn!(work = %id, kind = %kind_s, "внутренние работы: строка неизвестного вида пропущена");
        return Ok(None);
    };
    let (mode_s, origin_s, state_s): (String, String, String) = (r.get(3)?, r.get(4)?, r.get(6)?);
    let (Some(mode), Some(origin), Some(state)) = (
        WorkMode::parse(&mode_s),
        Origin::parse(&origin_s),
        WorkState::parse(&state_s),
    ) else {
        tracing::warn!(work = %id, "внутренние работы: строка с неизвестным режимом или состоянием пропущена");
        return Ok(None);
    };
    let night: Option<String> = r.get(5)?;
    let items: String = r.get(7)?;
    let result: Option<String> = r.get(14)?;
    Ok(Some(Work {
        id,
        kind,
        space_id: r.get(2)?,
        mode,
        origin,
        night: night.and_then(|s| s.parse().ok()),
        state,
        items: serde_json::from_str(&items).unwrap_or_else(|e| {
            tracing::warn!(work = %id, error = %e, "внутренние работы: items не разобраны");
            Vec::new()
        }),
        claimed_session: r.get(8)?,
        claimed_until: ts_opt(r, 9)?,
        attempts: u32::try_from(r.get::<_, i64>(10)?).unwrap_or(0),
        created_at: ts_at(r, 11)?,
        started_at: ts_opt(r, 12)?,
        finished_at: ts_opt(r, 13)?,
        result: result.and_then(|s| serde_json::from_str(&s).ok()),
        result_key: r.get(15)?,
        error_class: r.get(16)?,
    }))
}

fn get_tx(c: &rusqlite::Connection, id: Id) -> Result<Option<Work>, StoreError> {
    let mut st = c.prepare_cached(&format!("SELECT {COLS} FROM works WHERE id = ?1"))?;
    Ok(st.query_row([id], row).optional()?.flatten())
}

/// Работа `id`; нет или неизвестного вида — `None`.
pub async fn get(db: &Db, id: Id) -> Result<Option<Work>, StoreError> {
    db.read(move |c| get_tx(c, id)).await
}

/// Ставит работу в очередь (`queued`); у вида в пространстве уже есть незакрытая работа — `None`
/// (§6.3 п. 1: долг не копится).
pub async fn enqueue(db: &Db, w: NewWork) -> Result<Option<Work>, StoreError> {
    let items =
        serde_json::to_string(&w.items).map_err(|e| StoreError::BadInput(format!("items: {e}")))?;
    db.write(move |t| {
        let open: i64 = t.query_row(
            "SELECT count(*) FROM works
             WHERE space_id = ?1 AND kind = ?2 AND state IN ('queued', 'running')",
            params![w.space, w.kind.as_str()],
            |r| r.get(0),
        )?;
        if open > 0 {
            return Ok(None);
        }
        let id = Id::new();
        t.execute(
            "INSERT INTO works (id, kind, space_id, mode, origin, night, state, items, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7, ?8)",
            params![
                id,
                w.kind.as_str(),
                w.space,
                w.mode.as_str(),
                w.origin.as_str(),
                w.night.map(|d| d.to_string()),
                items,
                fmt_ts(now()),
            ],
        )?;
        get_tx(t, id)
    })
    .await
}

/// Захват работы агентом сессии `session` в пространствах `spaces` (вид `kind`, `None` — любой):
/// своя живая аренда этой сессии — та же работа без продления (агент потерял пакет — получает его
/// снова); иначе — старейшая `queued` работа агента: `running`, аренда `lease`. Строки
/// неизвестного вида пропускаются с WARN. Истёкшие аренды снимает [`expire`] (зовёт вызывающий).
pub async fn claim_next(
    db: &Db,
    spaces: &[Id],
    kind: Option<WorkKind>,
    session: &str,
    lease: SignedDuration,
) -> Result<Option<Work>, StoreError> {
    let spaces = spaces.to_vec();
    let session = session.to_string();
    db.write(move |t| {
        let now = now();
        let in_scope = |w: &Work| spaces.contains(&w.space_id) && kind.is_none_or(|k| w.kind == k);
        let mine = {
            let mut st = t.prepare_cached(&format!(
                "SELECT {COLS} FROM works
                 WHERE state = 'running' AND mode = 'agent' AND claimed_session = ?1
                   AND claimed_until > ?2
                 ORDER BY created_at, id"
            ))?;
            let rows = st.query_map(params![session, fmt_ts(now)], row)?;
            let mut found = None;
            for w in rows {
                if let Some(w) = w?
                    && in_scope(&w)
                {
                    found = Some(w);
                    break;
                }
            }
            found
        };
        if mine.is_some() {
            return Ok(mine);
        }
        let next = {
            let mut st = t.prepare_cached(&format!(
                "SELECT {COLS} FROM works WHERE state = 'queued' AND mode = 'agent'
                 ORDER BY created_at, id"
            ))?;
            let rows = st.query_map([], row)?;
            let mut found = None;
            for w in rows {
                if let Some(w) = w?
                    && in_scope(&w)
                {
                    found = Some(w);
                    break;
                }
            }
            found
        };
        let Some(w) = next else {
            return Ok(None);
        };
        let until = now.checked_add(lease).unwrap_or(now);
        t.execute(
            "UPDATE works SET state = 'running', claimed_session = ?2, claimed_until = ?3,
                    started_at = coalesce(started_at, ?4)
             WHERE id = ?1",
            params![w.id, session, fmt_ts(until), fmt_ts(now)],
        )?;
        get_tx(t, w.id)
    })
    .await
}

/// Работа модели — `running` (выполнение началось; при старте сервера такие возвращаются в
/// очередь — [`requeue_model_running`]). `false` — работы нет или она не `queued`.
pub async fn start_model(db: &Db, id: Id) -> Result<bool, StoreError> {
    db.write(move |t| {
        let n = t.execute(
            "UPDATE works SET state = 'running', started_at = coalesce(started_at, ?2)
             WHERE id = ?1 AND state = 'queued' AND mode = 'model'",
            params![id, fmt_ts(now())],
        )?;
        Ok(n > 0)
    })
    .await
}

/// Закрывает незакрытую работу: состояние `state`, итог `result`, класс ошибки, `finished_at`.
/// `false` — работа уже закрыта (или её нет).
pub async fn finish(
    db: &Db,
    id: Id,
    state: WorkState,
    result: Value,
    error_class: Option<&str>,
) -> Result<bool, StoreError> {
    let error_class = error_class.map(str::to_string);
    let result = (!result.is_null()).then(|| result.to_string());
    db.write(move |t| {
        let n = t.execute(
            "UPDATE works SET state = ?2, result = coalesce(?3, result), error_class = ?4,
                    finished_at = ?5
             WHERE id = ?1 AND state IN ('queued', 'running')",
            params![id, state.as_str(), result, error_class, fmt_ts(now())],
        )?;
        Ok(n > 0)
    })
    .await
}

/// Запись принятого элемента итога (пробы, кандидаты) — в **той же транзакции**, что закрытие
/// работы ([`finish_claimed`]): работу сняли, она истекла или закрыта другим итогом — не
/// записано ничего. Варианты добавляют виды работ (задачи 4 и 7 плана M7.3).
#[derive(Debug, Clone, PartialEq)]
pub enum ItemWrite {
    /// Проба вида `probe` (вид работы `probes`, §6.4 п. 2): пространство и работа пробы — этой
    /// работы. При записи ещё раз, в транзакции, сверяются предел новых проб постановки работы
    /// (`works.origin`) с момента `since` — не больше `cap` — и [`crate::probes::insert_probe`]
    /// (повтор вопроса, предел активных): проверка элемента шла до транзакции, а итоги разных
    /// работ пользователя принимаются параллельно. Не прошедшая сверку проба не пишется (WARN).
    Probe {
        probe: crate::probes::NewProbe,
        since: Timestamp,
        cap: u32,
    },
    /// Тестовый вид `echo` (фича `test-hooks`): строка `proposals` типа [`ECHO_PROPOSAL_TYPE`]
    /// (читатели предложений пропускают неизвестный тип с WARN).
    #[cfg(feature = "test-hooks")]
    Echo {
        target: String,
        text: String,
        /// `agent` | `model`.
        author_kind: &'static str,
        author: String,
        session: Option<String>,
    },
}

/// `proposals.type` записей тестового вида `echo`.
#[cfg(feature = "test-hooks")]
pub const ECHO_PROPOSAL_TYPE: &str = "echo";

/// Одна запись принятого элемента работы `w` (транзакция закрытия).
fn apply_write(t: &rusqlite::Transaction<'_>, w: &Work, x: &ItemWrite) -> Result<(), StoreError> {
    let _ = (t, w);
    match *x {
        ItemWrite::Probe {
            ref probe,
            since,
            cap,
        } => {
            let made = crate::probes::new_since_tx(t, w.origin.as_str(), since)?;
            if made >= cap {
                tracing::warn!(work = %w.id, made, cap, "внутренние работы: предел новых проб исчерпан при записи — проба не записана");
                return Ok(());
            }
            let p = crate::probes::NewProbe {
                space: Some(w.space_id),
                work_id: Some(w.id),
                ..probe.clone()
            };
            match crate::probes::insert_tx(t, &p)? {
                crate::probes::InsertOutcome::Inserted(_) => {}
                o => {
                    tracing::warn!(work = %w.id, outcome = ?o, "внутренние работы: проба не записана");
                }
            }
            Ok(())
        }
        #[cfg(feature = "test-hooks")]
        ItemWrite::Echo {
            ref target,
            ref text,
            author_kind,
            ref author,
            ref session,
        } => {
            t.execute(
                "INSERT INTO proposals (id, space_id, type, targets, payload, author_kind, author,
                                        work_id, session_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    Id::new(),
                    w.space_id,
                    ECHO_PROPOSAL_TYPE,
                    serde_json::json!([target]).to_string(),
                    serde_json::json!({"text": text}).to_string(),
                    author_kind,
                    author,
                    w.id,
                    session,
                    fmt_ts(now()),
                ],
            )?;
            Ok(())
        }
    }
}

/// Исход [`finish_claimed`].
#[derive(Debug, Clone, PartialEq)]
pub enum Closed {
    /// Работа закрыта итогом, записи принятого — в той же транзакции.
    Closed,
    /// Работа уже закрыта итогом с этим `Idempotency-Key`: прежний `result` (сверку тела и
    /// прежний ответ берёт вызывающий), ничего не записано.
    Replay(Value),
    /// Работа не `running` у этого исполнителя (снята, истекла, закрыта, захвачена другой
    /// сессией) или её нет: ничего не записано.
    NotClaimed(Option<Box<Work>>),
}

/// Итог исполнителя (§6.3 п. 4) — одной транзакцией: работа `running`, захваченная сессией
/// `session` (у модели — `None`: работа модели), закрывается с итогом `result` и ключом
/// `Idempotency-Key`, записи принятого `writes` ([`ItemWrite`]) — в той же транзакции. Повтор с
/// тем же ключом после закрытия — [`Closed::Replay`] без записей.
pub async fn finish_claimed(
    db: &Db,
    id: Id,
    session: Option<&str>,
    state: WorkState,
    result: Value,
    key: Option<&str>,
    writes: Vec<ItemWrite>,
) -> Result<Closed, StoreError> {
    let session = session.map(str::to_string);
    let key = key.map(str::to_string);
    let result = result.to_string();
    db.write(move |t| {
        let Some(w) = get_tx(t, id)? else {
            return Ok(Closed::NotClaimed(None));
        };
        if let (Some(k), Some(rk)) = (key.as_deref(), w.result_key.as_deref())
            && k == rk
        {
            return Ok(Closed::Replay(w.result.clone().unwrap_or(Value::Null)));
        }
        let claimed = w.state == WorkState::Running
            && match session.as_deref() {
                None => w.mode == WorkMode::Model,
                Some(s) => w.mode == WorkMode::Agent && w.claimed_session.as_deref() == Some(s),
            };
        if !claimed {
            return Ok(Closed::NotClaimed(Some(Box::new(w))));
        }
        for x in &writes {
            apply_write(t, &w, x)?;
        }
        let error_class = (state == WorkState::Cancelled).then_some(SKIPPED);
        t.execute(
            "UPDATE works SET state = ?2, result = ?3, result_key = ?4, error_class = ?5,
                    finished_at = ?6
             WHERE id = ?1",
            params![id, state.as_str(), result, key, error_class, fmt_ts(now())],
        )?;
        Ok(Closed::Closed)
    })
    .await
}

/// Пары «ссылка, ревизия» из `refs`, которые ещё живы: запись журнала (`i:`, `c:`, `r:`, `t:`,
/// `k:`, `q:`, `w:`, `e:`) или заметка (`n:`) не удалена и её текущая ревизия — та же. Ссылки
/// других видов считаются живыми. Снимок кандидатов работы перед выдачей сверяется по ним:
/// удалённая или исправленная запись в пакет не идёт (сборщик предложит новую ревизию).
pub async fn live_refs(
    db: &Db,
    refs: &[(String, i64)],
) -> Result<HashSet<(String, i64)>, StoreError> {
    let refs = refs.to_vec();
    db.read(move |c| {
        let mut out = HashSet::new();
        for (target, rev) in refs {
            let (letter, raw) = target.split_once(':').unwrap_or(("", ""));
            let table = if letter == "n" {
                Some("nodes")
            } else if crate::is_entry_ref_letter(letter) {
                Some("entries")
            } else {
                None
            };
            let live = match (table, raw.parse::<Id>()) {
                (Some(table), Ok(id)) => {
                    let mut st = c.prepare_cached(&format!(
                        "SELECT current_rev FROM {table} WHERE id = ?1 AND deleted_at IS NULL"
                    ))?;
                    st.query_row([id], |r| r.get::<_, i64>(0))
                        .optional()?
                        .is_some_and(|cur| cur == rev)
                }
                (Some(_), Err(_)) => false,
                (None, _) => true,
            };
            if live {
                out.insert((target, rev));
            }
        }
        Ok(out)
    })
    .await
}

/// Возвращает работу `running` в очередь без итога (`POST /works/{id}/release`, работа модели,
/// не дошедшая до итога). `false` — работа не `running`.
pub async fn release(db: &Db, id: Id) -> Result<bool, StoreError> {
    db.write(move |t| {
        let n = t.execute(
            "UPDATE works SET state = 'queued', claimed_session = NULL, claimed_until = NULL
             WHERE id = ?1 AND state = 'running'",
            [id],
        )?;
        Ok(n > 0)
    })
    .await
}

/// Заменяет кандидатов работы (пакет не вместил хвост — он уходит в следующую работу, §6.3
/// п. 3).
pub async fn set_items(db: &Db, id: Id, items: Vec<WorkItem>) -> Result<(), StoreError> {
    let items =
        serde_json::to_string(&items).map_err(|e| StoreError::BadInput(format!("items: {e}")))?;
    db.write(move |t| {
        t.execute(
            "UPDATE works SET items = ?2 WHERE id = ?1",
            params![id, items],
        )?;
        Ok(())
    })
    .await
}

/// Снимает незакрытые работы вида `kind` (`cancelled`, класс `reason`); число снятых. `mode` —
/// только работы этого исполнителя (`None` — любые).
pub async fn cancel_kind(
    db: &Db,
    kind: WorkKind,
    reason: &str,
    mode: Option<WorkMode>,
) -> Result<u64, StoreError> {
    let reason = reason.to_string();
    db.write(move |t| {
        let n = t.execute(
            "UPDATE works SET state = 'cancelled', error_class = ?2, finished_at = ?3
             WHERE kind = ?1 AND state IN ('queued', 'running')
               AND (?4 IS NULL OR mode = ?4)",
            params![
                kind.as_str(),
                reason,
                fmt_ts(now()),
                mode.map(WorkMode::as_str)
            ],
        )?;
        Ok(n as u64)
    })
    .await
}

/// Работы модели `running` (оборванные остановкой сервера) — снова `queued` (§6.1 п. 5); число
/// возвращённых.
pub async fn requeue_model_running(db: &Db) -> Result<u64, StoreError> {
    db.write(|t| {
        let n = t.execute(
            "UPDATE works SET state = 'queued', claimed_session = NULL, claimed_until = NULL
             WHERE mode = 'model' AND state = 'running'",
            [],
        )?;
        Ok(n as u64)
    })
    .await
}

/// Сроки работ агента на момент `at` (§6.3 п. 1): незакрытая дольше [`AGENT_TTL_DAYS`] суток —
/// `expired`; истёкшая аренда — `queued` и `attempts + 1`, [`LEASE_ATTEMPTS`]-я — `failed`
/// (`lease_expired`). Число изменённых.
pub async fn expire(db: &Db, at: Timestamp) -> Result<u64, StoreError> {
    db.write(move |t| {
        let ttl = at
            .checked_sub(SignedDuration::from_hours(24 * AGENT_TTL_DAYS))
            .unwrap_or(at);
        let now_s = fmt_ts(at);
        let mut n = t.execute(
            "UPDATE works SET state = 'expired', error_class = ?3, finished_at = ?2
             WHERE mode = 'agent' AND state IN ('queued', 'running') AND created_at < ?1",
            params![fmt_ts(ttl), now_s, EXPIRED],
        )?;
        n += t.execute(
            "UPDATE works SET state = 'failed', attempts = attempts + 1, error_class = ?3,
                    finished_at = ?1
             WHERE mode = 'agent' AND state = 'running' AND claimed_until <= ?1
               AND attempts + 1 >= ?2",
            params![now_s, i64::from(LEASE_ATTEMPTS), LEASE_EXPIRED],
        )?;
        n += t.execute(
            "UPDATE works SET state = 'queued', attempts = attempts + 1, claimed_session = NULL,
                    claimed_until = NULL
             WHERE mode = 'agent' AND state = 'running' AND claimed_until <= ?1",
            [now_s],
        )?;
        Ok(n as u64)
    })
    .await
}

/// Последний итог вида по пространствам (`max(finished_at)`): обход пространств ночи — с давно не
/// обслуженного (§6.3 п. 2).
pub async fn served_at(db: &Db, kind: WorkKind) -> Result<HashMap<Id, Timestamp>, StoreError> {
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT space_id, max(finished_at) FROM works
             WHERE kind = ?1 AND finished_at IS NOT NULL GROUP BY space_id",
        )?;
        let rows = st.query_map([kind.as_str()], |r| Ok((r.get::<_, Id>(0)?, ts_at(r, 1)?)))?;
        Ok(rows.collect::<Result<HashMap<_, _>, _>>()?)
    })
    .await
}

/// Пакетов (работ) вида с постановкой `origin` с момента `since` (`created_at >= since`): предел
/// ночи и ручных запусков суток (§6.3 п. 2).
pub async fn count_packages(
    db: &Db,
    kind: WorkKind,
    origin: Origin,
    since: Timestamp,
) -> Result<u32, StoreError> {
    db.read(move |c| {
        let n: i64 = c.query_row(
            "SELECT count(*) FROM works WHERE kind = ?1 AND origin = ?2 AND created_at >= ?3",
            params![kind.as_str(), origin.as_str(), fmt_ts(since)],
            |r| r.get(0),
        )?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    })
    .await
}

/// Пространства, где у вида уже была ночная постановка ночи `night` (§6.1 п. 4: постановка —
/// если её не было; у режима агента — одна работа пространства за ночь).
pub async fn nightly_spaces(
    db: &Db,
    kind: WorkKind,
    night: civil::Date,
) -> Result<HashSet<Id>, StoreError> {
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT DISTINCT space_id FROM works
             WHERE kind = ?1 AND origin = 'nightly' AND night = ?2",
        )?;
        let rows = st.query_map(params![kind.as_str(), night.to_string()], |r| r.get(0))?;
        Ok(rows.collect::<Result<HashSet<Id>, _>>()?)
    })
    .await
}

/// Старейшая `queued` работа модели вида `kind` в пространствах `spaces` (работы, возвращённые в
/// очередь после остановки или стопа, — первыми в следующем запуске).
pub async fn next_model_queued(
    db: &Db,
    kind: WorkKind,
    spaces: &[Id],
) -> Result<Option<Work>, StoreError> {
    let spaces = spaces.to_vec();
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT {COLS} FROM works WHERE state = 'queued' AND mode = 'model' AND kind = ?1
             ORDER BY created_at, id"
        ))?;
        let rows = st.query_map([kind.as_str()], row)?;
        for w in rows {
            if let Some(w) = w?
                && spaces.contains(&w.space_id)
            {
                return Ok(Some(w));
            }
        }
        Ok(None)
    })
    .await
}

/// Работы: новые первыми, фильтры состояния и вида, не больше `limit`; строки неизвестного вида
/// пропускаются с WARN.
pub async fn list(
    db: &Db,
    state: Option<WorkState>,
    kind: Option<WorkKind>,
    limit: usize,
) -> Result<Vec<Work>, StoreError> {
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT {COLS} FROM works
             WHERE (?1 IS NULL OR state = ?1) AND (?2 IS NULL OR kind = ?2)
             ORDER BY created_at DESC, id DESC"
        ))?;
        let rows = st.query_map(
            params![state.map(WorkState::as_str), kind.map(WorkKind::as_str)],
            row,
        )?;
        let mut out = Vec::new();
        for w in rows {
            if let Some(w) = w? {
                out.push(w);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    })
    .await
}

/// Последняя ночь с ночной постановкой работ (`zam work status`).
pub async fn last_night(db: &Db) -> Result<Option<civil::Date>, StoreError> {
    db.read(|c| {
        let s: Option<String> = c.query_row(
            "SELECT max(night) FROM works WHERE origin = 'nightly'",
            [],
            |r| r.get(0),
        )?;
        Ok(s.and_then(|s| s.parse().ok()))
    })
    .await
}

/// Незакрытые работы агента `queued` по видам в пространствах `spaces` (строка дайджеста, §6.3
/// п. 5): вид, число работ и старейшая работа вида; виды — в порядке [`WorkKind::ALL`].
pub async fn queued_agent_counts(
    db: &Db,
    spaces: &[Id],
) -> Result<Vec<(WorkKind, u32, Id)>, StoreError> {
    let spaces = spaces.to_vec();
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT kind, space_id, id FROM works WHERE state = 'queued' AND mode = 'agent'
             ORDER BY created_at, id",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Id>(1)?,
                r.get::<_, Id>(2)?,
            ))
        })?;
        let mut counts: Vec<(WorkKind, u32, Id)> = Vec::new();
        for r in rows {
            let (k, space, id) = r?;
            let Some(k) = WorkKind::parse(&k) else {
                continue;
            };
            if !spaces.contains(&space) {
                continue;
            }
            match counts.iter_mut().find(|(x, _, _)| *x == k) {
                Some((_, n, _)) => *n += 1,
                None => counts.push((k, 1, id)),
            }
        }
        counts.sort_by_key(|(k, _, _)| WorkKind::ALL.iter().position(|x| x == k));
        Ok(counts)
    })
    .await
}

/// Пары «ссылка, ревизия», которые сборщик вида не берёт до новой ревизии записи (решение 72),
/// из итогов за последние [`REFS_HORIZON_DAYS`] суток: отклонённые с причиной
/// [`SECRET_IN_PAYLOAD`] (`result.rejected`) — у работ `done` **и** `cancelled` (модель отказалась
/// от всей работы `skip`, а второй скан элемент уже выбросил — отсев остаётся; ревью r2 задачи 3,
/// N1) и, у вида `probes`, явные `skip` исполнителя (`result.skip`) — только у `done`.
pub async fn excluded_refs(db: &Db, kind: WorkKind) -> Result<HashSet<(String, i64)>, StoreError> {
    let mut out = HashSet::new();
    for (state, r) in item_results(db, kind, &[WorkState::Done, WorkState::Cancelled]).await? {
        for x in r["rejected"].as_array().into_iter().flatten() {
            if x["reason"] == SECRET_IN_PAYLOAD
                && let (Some(t), Some(rev)) = (x["target"].as_str(), x["rev"].as_i64())
            {
                out.insert((t.to_string(), rev));
            }
        }
        if kind == WorkKind::Probes && state == WorkState::Done {
            for x in r["skip"].as_array().into_iter().flatten() {
                if let (Some(t), Some(rev)) = (x["target"].as_str(), x["rev"].as_i64()) {
                    out.insert((t.to_string(), rev));
                }
            }
        }
    }
    Ok(out)
}

/// Пары «ссылка, ревизия» принятых элементов закрытых итогом (`done`) работ вида за последние
/// [`REFS_HORIZON_DAYS`] суток: элемент с итогом в той же ревизии сборщик не предлагает снова.
pub async fn accepted_refs(db: &Db, kind: WorkKind) -> Result<HashSet<(String, i64)>, StoreError> {
    let mut out = HashSet::new();
    for (_, r) in item_results(db, kind, &[WorkState::Done]).await? {
        for x in r["accepted"].as_array().into_iter().flatten() {
            if let (Some(t), Some(rev)) = (x["target"].as_str(), x["rev"].as_i64()) {
                out.insert((t.to_string(), rev));
            }
        }
    }
    Ok(out)
}

/// Пары «ссылка, ревизия» кандидатов, уже отправленных в пакетах вида в этом запуске
/// постановки (ревью r1 задачи 4, I1; решение координатора): ночью — `items` работ вида с
/// `origin = nightly` той же ночи `night`, у ручного запуска (`night` нет) — работ `origin`
/// с `since` (сутки UTC); **любое** состояние работы (без итога, отклонённые проверкой,
/// `failed`, снятые). Той же ночью (теми же сутками ручных запусков) их сборщик снова не берёт;
/// следующей — они снова кандидаты. Из базы, а не из памяти цикла: перезапуск посреди ночи отсев
/// не снимает. Хвост пакета сверх бюджета снят с работы до выдачи (`set_items`) — он не
/// отправлялся и сюда не входит.
pub async fn offered_refs(
    db: &Db,
    kind: WorkKind,
    origin: Origin,
    night: Option<civil::Date>,
    since: Timestamp,
) -> Result<HashSet<(String, i64)>, StoreError> {
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT items FROM works
             WHERE kind = ?1 AND origin = ?2
               AND (CASE WHEN ?3 IS NOT NULL THEN night = ?3 ELSE created_at >= ?4 END)",
        )?;
        let rows = st.query_map(
            params![
                kind.as_str(),
                origin.as_str(),
                night.map(|d| d.to_string()),
                fmt_ts(since)
            ],
            |r| r.get::<_, String>(0),
        )?;
        let mut out = HashSet::new();
        for items in rows {
            let items: Vec<WorkItem> = serde_json::from_str(&items?).unwrap_or_default();
            out.extend(items.into_iter().map(|i| (i.target, i.rev)));
        }
        Ok(out)
    })
    .await
}

/// Итоги (`result`) работ вида в состояниях `states`, закрытых за последние
/// [`REFS_HORIZON_DAYS`] суток, с состоянием работы.
async fn item_results(
    db: &Db,
    kind: WorkKind,
    states: &[WorkState],
) -> Result<Vec<(WorkState, Value)>, StoreError> {
    let at = now();
    let since = fmt_ts(
        at.checked_sub(SignedDuration::from_hours(24 * REFS_HORIZON_DAYS))
            .unwrap_or(at),
    );
    let states = states.to_vec();
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT state, result FROM works
             WHERE kind = ?1 AND result IS NOT NULL AND finished_at >= ?2",
        )?;
        let rows = st.query_map(params![kind.as_str(), since], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (state, s) = row?;
            let Some(state) = WorkState::parse(&state).filter(|x| states.contains(x)) else {
                continue;
            };
            if let Ok(v) = serde_json::from_str::<Value>(&s) {
                out.push((state, v));
            }
        }
        Ok(out)
    })
    .await
}

/// Живая запись журнала пространства, все ревизии которой — владельца базы (кандидат сборщика).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerEntry {
    pub id: Id,
    /// Ссылка записи `<буква>:<id>` (`i:`, `c:`, … — `EntryKind::ref_letter`).
    pub target: String,
    pub kind: String,
    pub rev: i64,
    pub title: String,
    pub body: String,
}

/// Живые записи журнала пространства `space` видов `kinds` (пусто — любых; старые первыми), у
/// которых автор **каждой** ревизии — владелец базы `owner` или не указан (ревизии до M3,
/// система, администратор — §6 «Общее»): запись, к которой приложил руку участник общего
/// решения, наружу не уходит. Отчёты не берутся никогда; пары «ссылка, ревизия» из `skip`
/// пропускаются. Не больше `limit`.
pub async fn owner_entries(
    db: &Db,
    owner: Id,
    space: Id,
    kinds: &[&str],
    skip: &HashSet<(String, i64)>,
    limit: usize,
) -> Result<Vec<OwnerEntry>, StoreError> {
    let kinds: Vec<String> = kinds
        .iter()
        .filter(|k| **k != "report")
        .map(|k| (*k).to_string())
        .collect();
    let skip = skip.clone();
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT e.id, e.kind, e.current_rev, e.title, e.body FROM entries e
             WHERE e.space_id = ?1 AND e.deleted_at IS NULL AND e.kind != 'report'
               AND (e.user_id IS NULL OR e.user_id = ?2)
               AND NOT EXISTS (SELECT 1 FROM entry_revisions r WHERE r.entry_id = e.id
                               AND r.user_id IS NOT NULL AND r.user_id != ?2)
             ORDER BY e.created_at, e.id",
        )?;
        let rows = st.query_map(params![space, owner], |r| {
            Ok((
                r.get::<_, Id>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for e in rows {
            let (id, kind, rev, title, body) = e?;
            if !kinds.is_empty() && !kinds.contains(&kind) {
                continue;
            }
            let Some(letter) = crate::EntryKind::parse(&kind).map(crate::EntryKind::ref_letter)
            else {
                continue;
            };
            let target = format!("{letter}:{id}");
            if skip.contains(&(target.clone(), rev)) {
                continue;
            }
            out.push(OwnerEntry {
                id,
                target,
                kind,
                rev,
                title,
                body,
            });
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `parse` и `as_str` взаимно обратны; чужие строки (`tidy` до M7.5, тип предложения `rule`,
    /// вид журнала проверки подключения `test`) — `None`; serde — та же строка.
    #[test]
    fn work_kind_parse_cases() {
        for k in WorkKind::ALL {
            assert_eq!(WorkKind::parse(k.as_str()), Some(*k));
            assert_eq!(
                serde_json::to_value(k).unwrap(),
                serde_json::json!(k.as_str())
            );
        }
        assert_eq!(WorkKind::parse("probes"), Some(WorkKind::Probes));
        assert_eq!(WorkKind::parse("rules"), Some(WorkKind::Rules));
        for bad in ["tidy", "rule", "test", "", "Probes", " probes"] {
            assert_eq!(WorkKind::parse(bad), None, "{bad}");
        }
        for t in ProposalType::ALL {
            assert_eq!(ProposalType::parse(t.as_str()), Some(*t));
        }
        assert_eq!(ProposalType::parse("rule"), Some(ProposalType::Rule));
        for bad in ["rules", "tidy", ""] {
            assert_eq!(ProposalType::parse(bad), None, "{bad}");
        }
    }
}
