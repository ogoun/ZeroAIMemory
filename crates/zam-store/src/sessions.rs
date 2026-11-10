//! Сессии агентов и выданное им (§6.3, §9.3, §10.3; задача 5 M4).
//!
//! - `sessions` — в базе пользователя запроса: внешний id сессии агента (`X-Zam-Session`,
//!   `session_id` хука, до [`SESSION_ID_MAX_CHARS`] символов), токен, решение, область, машина,
//!   агент, время начала, последнего обращения и конца, счётчик записей в память
//!   `memory_writes` (решение контроллера R1: читает хук `checkpoint`). Сессия создаётся
//!   первым касанием ([`touch_session`], [`add_memory_write`], [`mark_seen`]);
//! - `session_seen` — что сессии уже выдано: цель в базе владельца `owner_id` (решение может
//!   быть чужим), часть цели `part` (решение контроллера R11: `chunk_id` чанка, отданного
//!   целиком, или [`WHOLE_TARGET`] — полный текст цели; [`RECALL_PART`] — подсказка `/recall`)
//!   и её ревизия. Вложение (M5) — цель [`SeenTarget::Blob`] (sha256), часть — `locator`,
//!   отданный `/read` целиком, или `chunk_id` фрагмента `mode=chunks`, ревизия 0. Хранится большая из выданных ревизий: ревизии только растут, и повторная
//!   выдача старой не откатывает отметку;
//! - эпоха (спецификация M6 §7.2, миграция 0025): `sessions.seen_epoch` растёт [`bump_epoch`]
//!   (сжатие контекста, `resume`, `clear`, восстановление), отметка пишется в эпоху касания
//!   ([`SessionTouch::epoch`]), [`seen_revs`] читает только её. Отметки прежних эпох хранятся до
//!   срока сессии (аналитика). Показ идей — без эпохи (§7.2, «одна идея — один показ за
//!   сессию»): раздел идей дайджеста читает [`seen_entry_ids`], подсказка идей к запросу —
//!   [`seen_revs_any_epoch`];
//! - `holdout` — сессия контрольной группы агентного A/B ([`set_holdout`], M6 §12); признак с
//!   наследованием от родителя — [`session_holdout`];
//! - `parent_id`, `agent_type` — сессия субагента (спецификация M4.5 §9.4): хук
//!   `subagent-start` создаёт `<session>:<agent_id>` с id родителя;
//! - `session_tokens` — выданное сессии по событию (§9.5): число выдач `calls`, оценка размера в
//!   токенах `tokens` и число повторных выдач `reread_calls` (M6 §7.3: `/read` с пометкой «уже
//!   выдавалось» — у события `read`); `/recall` прибавляет свои события
//!   ([`mark_seen_with_tokens`]), хуки с локальным выводом — пачкой через `POST /sessions`
//!   ([`add_token_counts`]);
//! - срок хранения — [`gc_sessions`]: сессии без обращений дольше срока удаляются вместе с
//!   выданным им, учётом и снимком (воркер обслуживания сервера, раз в сутки, 30 дней).
use std::collections::HashMap;

use jiff::Timestamp;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use zam_core::{Id, SolutionGuid, TargetKind, TargetRef};

use crate::time::{fmt_ts, ser_ts, ser_ts_opt, ts_at, ts_opt};
use crate::{Db, StoreError};

/// Предел длины id сессии и строковых полей сессии, символов (как у заголовков атрибуции).
pub const SESSION_ID_MAX_CHARS: usize = 128;
/// `part` выданного: полный текст цели (а не отдельный чанк).
pub const WHOLE_TARGET: &str = "";
/// `part` выданного: цель показана подсказкой `/recall` (§10.3; заголовок и ссылка, а не текст),
/// поэтому поиск (`skip_seen`) и `/read` такую отметку выдачей текста не считают
/// **[решение исполнителя, задача 8 M4]**. Подсказка показывается не больше раза за сессию.
pub const RECALL_PART: &str = "recall";

/// Касание сессии: id и то, что о ней известно из запроса (`None` — не менять прежнее).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionTouch {
    pub id: String,
    pub token_id: Option<Id>,
    pub solution_guid: Option<SolutionGuid>,
    pub area: Option<String>,
    pub machine: Option<String>,
    pub agent: Option<String>,
    /// Id сессии родителя (сессия субагента).
    pub parent_id: Option<String>,
    /// Тип субагента (`Explore`, `Plan`, свои).
    pub agent_type: Option<String>,
    /// Эпоха `session_seen` (M6 §7.2): в неё пишет [`mark_seen`]. Заполняет сессия запроса
    /// сервера (`request_session`); `sessions.seen_epoch` касание не меняет.
    pub epoch: u32,
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
    pub parent_id: Option<String>,
    pub agent_type: Option<String>,
    #[serde(serialize_with = "ser_ts")]
    pub started_at: Timestamp,
    #[serde(serialize_with = "ser_ts")]
    pub last_seen_at: Timestamp,
    #[serde(serialize_with = "ser_ts_opt")]
    pub ended_at: Option<Timestamp>,
    pub memory_writes: i64,
    /// Текущая эпоха `session_seen` (M6 §7.2, [`bump_epoch`]).
    pub seen_epoch: u32,
    /// Сессия контрольной группы агентного A/B ([`set_holdout`], M6 §12).
    pub holdout: bool,
}

/// Предел имени события `session_tokens`, символов.
pub const TOKEN_EVENT_MAX_CHARS: usize = 32;

/// Прибавка к учёту выданного сессии по событию.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenCount {
    /// Событие: `session_start`, `recovery`, `rules` (резерв Rules этих двух, M6 §8), `prompt`,
    /// `post_tool`, `brief` (`/recall`), `search`, `read` (REST и CLI с явной сессией), `mcp`
    /// (инструменты MCP) или `pre_tool`, `post_tool_local`, `subagent_local` (хуки с локальным
    /// выводом).
    pub event: String,
    pub calls: u64,
    pub tokens: u64,
    /// Повторных выдач (M6 §7.3: вызов `/read` с пометкой «уже выдавалось»).
    pub reread_calls: u64,
}

/// Выданное сессии по одному событию.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TokenStat {
    pub calls: i64,
    pub tokens: i64,
    /// Повторных выдач (M6 §7.3); 0 — поля в JSON нет (прежняя форма `{calls, tokens}`).
    #[serde(skip_serializing_if = "is_zero")]
    pub reread_calls: i64,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

/// Цель выданного: узел или запись журнала ([`TargetRef`]) либо блоб вложения по sha256 (M5,
/// задача 7: показ вложения учитывается по содержимому файла, а не по документу — один файл в
/// двух заметках выдаётся один раз).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SeenTarget {
    Ref(TargetRef),
    /// sha256 блоба: 64 hex в нижнем регистре.
    Blob(String),
}

impl From<TargetRef> for SeenTarget {
    fn from(r: TargetRef) -> Self {
        SeenTarget::Ref(r)
    }
}

/// Выданное сессии: владелец базы цели, цель и её часть (`chunk_id`, [`WHOLE_TARGET`]; у блоба —
/// ещё `locator`, отданный целиком через `/read`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SeenKey {
    pub owner_id: Id,
    pub target: SeenTarget,
    pub part: String,
}

/// Выданная часть цели и ревизия цели.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenItem {
    pub key: SeenKey,
    pub rev: i64,
}

/// `session_seen.target_kind` и `target_id` цели. Показ вложения — вид `blob` с sha256 блоба
/// (CHECK `session_seen`); документ вложения (`TargetKind::Attachment`) — `BadInput`: из его id
/// sha256 без запроса не получить, а ключ по документу разделил бы один файл двух заметок.
fn seen_cols(t: &SeenTarget) -> Result<(&'static str, String), StoreError> {
    match t {
        SeenTarget::Ref(r) => match r.kind {
            TargetKind::Node => Ok(("node", r.id.to_string())),
            TargetKind::Entry => Ok(("entry", r.id.to_string())),
            TargetKind::Attachment => Err(StoreError::BadInput(
                "показ вложения учитывается видом blob (sha256), не документом".into(),
            )),
        },
        SeenTarget::Blob(sha) => {
            let ok = sha.len() == 64
                && sha
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            if !ok {
                return Err(StoreError::BadInput(
                    "sha256 блоба: 64 hex в нижнем регистре".into(),
                ));
            }
            Ok(("blob", sha.clone()))
        }
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

/// Id, занятые путями REST `/sessions/…` (M6 §7.1: `GET /sessions/unattributed`).
pub const RESERVED_SESSION_IDS: [&str; 1] = ["unattributed"];

/// Id сессии: непустой, до [`SESSION_ID_MAX_CHARS`] печатных символов, не из
/// [`RESERVED_SESSION_IDS`].
pub fn check_session_id(id: &str) -> Result<(), StoreError> {
    if id.trim().is_empty() {
        return Err(StoreError::BadInput("id сессии пуст".into()));
    }
    if RESERVED_SESSION_IDS.contains(&id.trim()) {
        return Err(StoreError::BadInput(format!(
            "id сессии «{}» зарезервирован",
            id.trim()
        )));
    }
    check_field("id сессии", id)
}

fn check_touch(s: &SessionTouch) -> Result<(), StoreError> {
    check_session_id(&s.id)?;
    for (what, v) in [
        ("area", &s.area),
        ("machine", &s.machine),
        ("agent", &s.agent),
        ("parent_id", &s.parent_id),
        ("agent_type", &s.agent_type),
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
    let now = fmt_ts(crate::time::now());
    t.execute(
        "INSERT INTO sessions (id, token_id, solution_guid, area, machine, agent,
                               started_at, last_seen_at, memory_writes, parent_id, agent_type)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9, ?10)
         ON CONFLICT (id) DO UPDATE SET
           token_id = coalesce(excluded.token_id, token_id),
           solution_guid = coalesce(excluded.solution_guid, solution_guid),
           area = coalesce(excluded.area, area),
           machine = coalesce(excluded.machine, machine),
           agent = coalesce(excluded.agent, agent),
           parent_id = coalesce(excluded.parent_id, parent_id),
           agent_type = coalesce(excluded.agent_type, agent_type),
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
            writes,
            s.parent_id,
            s.agent_type
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

/// Регистрация сессии (`POST /sessions`: хук `session-start` шлёт её на каждом старте — и на
/// `resume`): обращение, как [`touch_session`], и в той же транзакции снятие отметки конца —
/// сессию, завершённую `SessionEnd`, продолжили (M6 §8а). `true` — отметка была и снята.
///
/// Только здесь, а не в общем [`upsert`]: поздние обращения после `SessionEnd` (хвостовой запрос,
/// учёт токенов, `PUT` снимка) завершённую сессию не «оживляют».
pub async fn register_session(db: &Db, s: &SessionTouch) -> Result<bool, StoreError> {
    check_touch(s)?;
    let s = s.clone();
    db.write(move |t| {
        upsert(t, &s, 0)?;
        let n = t.execute(
            "UPDATE sessions SET ended_at = NULL WHERE id = ?1 AND ended_at IS NOT NULL",
            [&s.id],
        )?;
        Ok(n > 0)
    })
    .await
}

/// Конец сессии (`ended: true` — сейчас, `false` — снять отметку). `false` — сессии нет.
pub async fn end_session(db: &Db, id: &str, ended: bool) -> Result<bool, StoreError> {
    let id = id.to_string();
    db.write(move |t| {
        let now = fmt_ts(crate::time::now());
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
                            last_seen_at, ended_at, memory_writes, parent_id, agent_type, \
                            seen_epoch, holdout";

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
        parent_id: r.get(10)?,
        agent_type: r.get(11)?,
        seen_epoch: r.get(12)?,
        holdout: r.get(13)?,
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

/// Последняя незавершённая сессия базы с обращением не раньше `since`; `machine` задана —
/// только сессии этой машины, `solution` задано — только сессии этого решения. Нужна MCP без
/// `X-Zam-Session` (решение контроллера R13, задача 10 M4): `.mcp.json` не знает `session_id`
/// агента, и запись через MCP учитывается в сессии хуков.
pub async fn latest_session(
    db: &Db,
    machine: Option<&str>,
    solution: Option<&SolutionGuid>,
    since: Timestamp,
) -> Result<Option<Session>, StoreError> {
    let machine = machine.map(str::to_string);
    let solution = solution.map(|g| g.as_str().to_string());
    let since = fmt_ts(since);
    db.read(move |c| {
        Ok(c.query_row(
            &format!(
                "SELECT {SESSION_COLS} FROM sessions
                 WHERE ended_at IS NULL AND last_seen_at >= ?1
                   AND (?2 IS NULL OR machine = ?2)
                   AND (?3 IS NULL OR solution_guid = ?3)
                 ORDER BY last_seen_at DESC, id DESC LIMIT 1"
            ),
            params![since, machine, solution],
            session_row,
        )
        .optional()?)
    })
    .await
}

/// Касание сессии (как [`touch_session`]) и отметка выданного ей — одной транзакцией; у части
/// цели остаётся большая ревизия.
pub async fn mark_seen(db: &Db, s: &SessionTouch, items: &[SeenItem]) -> Result<(), StoreError> {
    mark_seen_with_tokens(db, s, items, &[]).await
}

/// Имя события учёта: 1..=[`TOKEN_EVENT_MAX_CHARS`] символов `[a-z0-9_]`.
pub(crate) fn check_event(e: &str) -> Result<(), StoreError> {
    let ok = !e.is_empty()
        && e.chars().count() <= TOKEN_EVENT_MAX_CHARS
        && e.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !ok {
        return Err(StoreError::BadInput(format!(
            "событие учёта токенов: до {TOKEN_EVENT_MAX_CHARS} символов [a-z0-9_]"
        )));
    }
    Ok(())
}

/// Прибавки учёта в транзакции записи (сессия уже есть: внешний ключ `session_tokens`).
fn add_tokens_tx(
    t: &rusqlite::Transaction,
    session: &str,
    counts: &[TokenCount],
) -> Result<(), StoreError> {
    let mut st = t.prepare_cached(
        "INSERT INTO session_tokens (session_id, event, calls, tokens, reread_calls)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (session_id, event) DO UPDATE SET
           calls = calls + excluded.calls, tokens = tokens + excluded.tokens,
           reread_calls = reread_calls + excluded.reread_calls",
    )?;
    let big = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
    for c in counts
        .iter()
        .filter(|c| c.calls > 0 || c.tokens > 0 || c.reread_calls > 0)
    {
        st.execute(params![
            session,
            c.event,
            big(c.calls),
            big(c.tokens),
            big(c.reread_calls)
        ])?;
    }
    Ok(())
}

/// Одна выдача события `event` размером `tokens` сессии `s` (сессия создаётся, если её нет).
pub async fn add_tokens(
    db: &Db,
    s: &SessionTouch,
    event: &str,
    tokens: u64,
) -> Result<(), StoreError> {
    let c = TokenCount {
        event: event.to_string(),
        calls: 1,
        tokens,
        reread_calls: 0,
    };
    add_token_counts(db, s, &[c]).await
}

/// Прибавки учёта выданного сессии `s` пачкой (с касанием сессии, одной транзакцией).
pub async fn add_token_counts(
    db: &Db,
    s: &SessionTouch,
    counts: &[TokenCount],
) -> Result<(), StoreError> {
    mark_seen_with_tokens(db, s, &[], counts).await
}

/// Выданное сессии по событиям (событий без выдач в ответе нет).
pub async fn session_tokens(
    db: &Db,
    session: &str,
) -> Result<std::collections::BTreeMap<String, TokenStat>, StoreError> {
    let session = session.to_string();
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT event, calls, tokens, reread_calls FROM session_tokens WHERE session_id = ?1",
        )?;
        let rows = st.query_map([session], |r| {
            Ok((
                r.get::<_, String>(0)?,
                TokenStat {
                    calls: r.get(1)?,
                    tokens: r.get(2)?,
                    reread_calls: r.get(3)?,
                },
            ))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}

/// Выданное сессиям `sessions` по событиям ([`session_tokens`] пачкой; сессии без выдач — без
/// ключа).
pub async fn sessions_tokens(
    db: &Db,
    sessions: &[String],
) -> Result<HashMap<String, std::collections::BTreeMap<String, TokenStat>>, StoreError> {
    let sessions = sessions.to_vec();
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT event, calls, tokens, reread_calls FROM session_tokens WHERE session_id = ?1",
        )?;
        let mut out: HashMap<String, std::collections::BTreeMap<String, TokenStat>> =
            HashMap::new();
        for id in sessions {
            let rows = st.query_map([&id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    TokenStat {
                        calls: r.get(1)?,
                        tokens: r.get(2)?,
                        reread_calls: r.get(3)?,
                    },
                ))
            })?;
            let m: std::collections::BTreeMap<String, TokenStat> =
                rows.collect::<Result<_, _>>()?;
            if !m.is_empty() {
                out.insert(id, m);
            }
        }
        Ok(out)
    })
    .await
}

/// [`mark_seen`] и прибавки учёта выданного (`/recall`: показ и его размер) — одной
/// транзакцией, без лишней синхронизации диска на горячем пути.
pub async fn mark_seen_with_tokens(
    db: &Db,
    s: &SessionTouch,
    items: &[SeenItem],
    counts: &[TokenCount],
) -> Result<(), StoreError> {
    check_touch(s)?;
    for c in counts {
        check_event(&c.event)?;
    }
    let (s, items, counts) = (s.clone(), items.to_vec(), counts.to_vec());
    db.write(move |t| {
        upsert(t, &s, 0)?;
        add_tokens_tx(t, &s.id, &counts)?;
        let now = fmt_ts(crate::time::now());
        let mut st = t.prepare_cached(
            "INSERT INTO session_seen (session_id, owner_id, target_kind, target_id, part, rev,
                                       seen_at, epoch)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (session_id, owner_id, target_kind, target_id, part, epoch) DO UPDATE SET
               rev = max(rev, excluded.rev), seen_at = excluded.seen_at",
        )?;
        for it in &items {
            let (kind, id) = seen_cols(&it.key.target)?;
            st.execute(params![
                s.id,
                it.key.owner_id,
                kind,
                id,
                it.key.part,
                it.rev,
                now,
                s.epoch
            ])?;
        }
        Ok(())
    })
    .await
}

/// Ревизии частей целей, уже выданных сессии в эпоху `epoch` (того, что сессия в ней не
/// получала, в ответе нет): отметки прежних эпох выданными не считаются (M6 §7.2).
pub async fn seen_revs(
    db: &Db,
    session: &str,
    epoch: u32,
    targets: &[SeenKey],
) -> Result<HashMap<SeenKey, i64>, StoreError> {
    seen_revs_in(db, session, Some(epoch), targets).await
}

/// Как [`seen_revs`], но по **всем** эпохам сессии (большая ревизия): показ идей учитывается
/// без эпохи (M6 §7.2, подсказка идей `prompt`).
pub async fn seen_revs_any_epoch(
    db: &Db,
    session: &str,
    targets: &[SeenKey],
) -> Result<HashMap<SeenKey, i64>, StoreError> {
    seen_revs_in(db, session, None, targets).await
}

/// [`seen_revs`] эпохи `epoch` или всех эпох (`None`).
async fn seen_revs_in(
    db: &Db,
    session: &str,
    epoch: Option<u32>,
    targets: &[SeenKey],
) -> Result<HashMap<SeenKey, i64>, StoreError> {
    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    let (session, targets) = (session.to_string(), targets.to_vec());
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT max(rev) FROM session_seen
             WHERE session_id = ?1 AND owner_id = ?2 AND target_kind = ?3 AND target_id = ?4
               AND part = ?5 AND (?6 IS NULL OR epoch = ?6)",
        )?;
        let mut out = HashMap::new();
        for k in targets {
            let (kind, id) = seen_cols(&k.target)?;
            let rev: Option<i64> = st
                .query_row(params![session, k.owner_id, kind, id, k.part, epoch], |r| {
                    r.get(0)
                })?;
            if let Some(rev) = rev {
                out.insert(k, rev);
            }
        }
        Ok(out)
    })
    .await
}

/// Записи журналов (`target_kind = entry`) базы владельца `owner`, выданные сессии с частью
/// `part` (любая ревизия, **любая эпоха** — показ идей, M6 §7.2): по ним вызывающий узнаёт,
/// показывалась ли сессии запись нужного вида (раздел идей M5.7 §6.1 — только на первом
/// `session_start` сессии). Запись, отмеченная в нескольких эпохах, — один id.
pub async fn seen_entry_ids(
    db: &Db,
    session: &str,
    owner: Id,
    part: &str,
) -> Result<Vec<Id>, StoreError> {
    let (session, part) = (session.to_string(), part.to_string());
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT DISTINCT target_id FROM session_seen
             WHERE session_id = ?1 AND owner_id = ?2 AND target_kind = 'entry' AND part = ?3",
        )?;
        let rows = st.query_map(params![session, owner, part], |r| r.get::<_, String>(0))?;
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

/// Таблицы с `session_id → sessions(id)` без каскада: [`gc_sessions`] чистит их до сессий.
const SESSION_CHILDREN: [&str; 3] = ["session_seen", "session_tokens", "session_snapshots"];

/// Удаляет сессии без обращений с `cutoff` (`last_seen_at < cutoff`) и выданное им (отметки,
/// учёт, снимок — [`SESSION_CHILDREN`]); число удалённых сессий.
pub async fn gc_sessions(db: &Db, cutoff: Timestamp) -> Result<u64, StoreError> {
    let cutoff = fmt_ts(cutoff);
    db.write(move |t| {
        for table in SESSION_CHILDREN {
            t.execute(
                &format!(
                    "DELETE FROM {table} WHERE session_id IN
                       (SELECT id FROM sessions WHERE last_seen_at < ?1)"
                ),
                [&cutoff],
            )?;
        }
        let n = t.execute("DELETE FROM sessions WHERE last_seen_at < ?1", [&cutoff])?;
        Ok(n as u64)
    })
    .await
}

/// Новая эпоха `session_seen` сессии (M6 §7.2): `seen_epoch + 1`, обращение (`last_seen_at`);
/// сессии нет — создаётся с эпохой 1. Возвращает новую эпоху.
///
/// Созданная так сессия — без `token_id`, `machine`, `solution_guid` и прочих полей касания.
/// Это безвредно: поля дописывает первое же касание ([`touch_session`], [`mark_seen`] —
/// `coalesce`), до него сессия ничего не получала; `latest_session` с фильтром по машине или
/// решению её не выберет, а без касаний она уйдёт по сроку [`gc_sessions`].
pub async fn bump_epoch(db: &Db, session: &str) -> Result<u32, StoreError> {
    check_session_id(session)?;
    let id = session.to_string();
    db.write(move |t| {
        let now = fmt_ts(crate::time::now());
        let epoch: i64 = t.query_row(
            "INSERT INTO sessions (id, started_at, last_seen_at, seen_epoch) VALUES (?1, ?2, ?2, 1)
             ON CONFLICT (id) DO UPDATE SET
               seen_epoch = seen_epoch + 1, last_seen_at = excluded.last_seen_at
             RETURNING seen_epoch",
            params![id, now],
            |r| r.get(0),
        )?;
        epoch_u32(epoch)
    })
    .await
}

/// Текущая эпоха `session_seen` сессии; сессии нет — 0 (её отметок тоже нет).
pub async fn session_epoch(db: &Db, session: &str) -> Result<u32, StoreError> {
    let id = session.to_string();
    db.read(move |c| {
        let epoch: Option<i64> = c
            .query_row("SELECT seen_epoch FROM sessions WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        epoch_u32(epoch.unwrap_or(0))
    })
    .await
}

fn epoch_u32(v: i64) -> Result<u32, StoreError> {
    u32::try_from(v).map_err(|_| StoreError::Corrupt(format!("sessions.seen_epoch = {v}")))
}

/// Сессия — в контрольной группе агентного A/B (M6 §12): `holdout` у неё самой, у её родителя
/// (`parent_id` строки) или у `parent` (родитель из запроса: `brief` субагента приходит
/// параллельно с регистрацией его сессии). Сессии нет — по `parent`.
pub async fn session_holdout(
    db: &Db,
    session: &str,
    parent: Option<&str>,
) -> Result<bool, StoreError> {
    let (id, parent) = (session.to_string(), parent.map(str::to_string));
    db.read(move |c| {
        let on: bool = c.query_row(
            "SELECT EXISTS (SELECT 1 FROM sessions WHERE holdout = 1 AND (id = ?1 OR id = ?2
               OR id = (SELECT parent_id FROM sessions WHERE id = ?1)))",
            params![id, parent],
            |r| r.get(0),
        )?;
        Ok(on)
    })
    .await
}

/// Признак `holdout` сессии (контрольная группа агентного A/B, M6 §12) с обращением; сессии
/// нет — создаётся, как в [`bump_epoch`] (без полей касания; это безвредно по тем же причинам).
pub async fn set_holdout(db: &Db, session: &str, on: bool) -> Result<(), StoreError> {
    check_session_id(session)?;
    let id = session.to_string();
    db.write(move |t| {
        let now = fmt_ts(crate::time::now());
        t.execute(
            "INSERT INTO sessions (id, started_at, last_seen_at, holdout) VALUES (?1, ?2, ?2, ?3)
             ON CONFLICT (id) DO UPDATE SET
               holdout = excluded.holdout, last_seen_at = excluded.last_seen_at",
            params![id, now, on],
        )?;
        Ok(())
    })
    .await
}
