//! Предложения человеку — кандидаты в Rules (M7.3 §6.7 п. 1) и кандидаты уборки (M7.5 §8.7;
//! миграция 0027, таблица `proposals`).
//!
//! Предложение `o:<id>` — служебный шаг до решения человека, а не запись памяти: у него нет
//! ревизий, корзины, поиска и экспорта (решение координатора 51). Тип — `proposals.type` без
//! CHECK ([`ProposalType`]: `rule`, с M7.5 — `tidy`): **выборки и счёт** ([`list`], [`counts`],
//! предел [`PROPOSALS_OPEN_MAX`] — на тип) — с условием `type = ?` **в SQL**, поэтому строки
//! других типов (тестовый `echo` очереди работ) их не задевают; строка неизвестного типа, статуса
//! или вида автора, прочитанная по id ([`get`]), пропускается с WARN (так сервер 0.0.17 после
//! отката пропускает строки `tidy`).
//!
//! **Отсев повторов** — `dedup_key` (`blake3`, [`key_source`], [`key_source_rev`],
//! [`key_text`]; решение координатора 64): у предложения из работы — по ссылке источника (его
//! отказ человеком — навсегда, в любой ревизии источника), у отказа исполнителя (`skipped`) — по
//! ссылке и ревизии (изменённый источник оценивается заново), у спонтанного кандидата агента — по
//! пути и тексту (тот же путь и текст — прежнее предложение, [`InsertOutcome::Existing`]). Строка
//! с ключом — повтор в любом статусе, **кроме `withdrawn`**: отзыв автором — не отказ человека
//! (решение 64 «навсегда» — только у человека; ревью r1 задачи 7, Minor 5), отозванный источник
//! снова кандидат, тот же текст — новое предложение.
//!
//! Статусы: `proposed → accepted | declined | withdrawn | stale` ([`decide`]); `skipped` —
//! исполнитель решил «правила здесь нет» (у уборки — операция `keep` или отказ `{n, skip}`),
//! человеку не показывается (только отсев повторов); `stale` — у уборки: запись кандидата
//! изменилась после предложения (ревизия не та) — при «Применить» или сборщиком ночи
//! ([`mark_stale_tidy`]). Предложение само ничего не применяет: заметку Rules и операцию уборки
//! выполняет человек, принимая его (сервер, `POST /proposals/{id}/accept`), здесь — только
//! `result_ref`.

use std::collections::HashSet;

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zam_core::Id;

use crate::time::{fmt_ts, now, ts_at, ts_opt};
pub use crate::works::ProposalType;
use crate::{Db, StoreError};

/// Открытых (`proposed`) предложений одного типа в базе пользователя — не больше (§6.7 п. 2:
/// 409 `/problems/proposals-full`).
pub const PROPOSALS_OPEN_MAX: u32 = 50;
/// Предложений в одной выборке [`list`] — не больше.
pub const LIST_MAX: usize = 200;

/// Статус предложения (`proposals.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProposalStatus {
    Proposed,
    Accepted,
    Declined,
    Withdrawn,
    Stale,
    Skipped,
}

impl ProposalStatus {
    pub const ALL: [ProposalStatus; 6] = [
        ProposalStatus::Proposed,
        ProposalStatus::Accepted,
        ProposalStatus::Declined,
        ProposalStatus::Withdrawn,
        ProposalStatus::Stale,
        ProposalStatus::Skipped,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ProposalStatus::Proposed => "proposed",
            ProposalStatus::Accepted => "accepted",
            ProposalStatus::Declined => "declined",
            ProposalStatus::Withdrawn => "withdrawn",
            ProposalStatus::Stale => "stale",
            ProposalStatus::Skipped => "skipped",
        }
    }

    pub fn parse(s: &str) -> Option<ProposalStatus> {
        ProposalStatus::ALL.into_iter().find(|x| x.as_str() == s)
    }
}

/// Автор предложения (`proposals.author_kind`): агент (сессия, токен) или подключённая модель.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProposalAuthor {
    Agent,
    Model,
}

impl ProposalAuthor {
    pub fn as_str(self) -> &'static str {
        match self {
            ProposalAuthor::Agent => "agent",
            ProposalAuthor::Model => "model",
        }
    }

    pub fn parse(s: &str) -> Option<ProposalAuthor> {
        [ProposalAuthor::Agent, ProposalAuthor::Model]
            .into_iter()
            .find(|x| x.as_str() == s)
    }
}

/// Источник предложения в `targets`: ссылка записи и её ревизия.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    #[serde(rename = "ref")]
    pub target: String,
    pub rev: i64,
}

/// Предложение — строка `proposals`.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub id: Id,
    pub space_id: Id,
    pub ptype: ProposalType,
    pub status: ProposalStatus,
    pub targets: Vec<SourceRef>,
    /// JSON по типу: у `rule` — `{path, content}`.
    pub payload: Value,
    pub reason: Option<String>,
    pub basis: Vec<String>,
    pub dedup_key: Option<String>,
    pub author_kind: ProposalAuthor,
    /// `model:<имя модели>` | id сессии агента.
    pub author: Option<String>,
    pub work_id: Option<Id>,
    pub session_id: Option<String>,
    pub created_at: Timestamp,
    pub decided_at: Option<Timestamp>,
    pub decided_by: Option<String>,
    pub decision_note: Option<String>,
    /// Итог принятия: заметка `n:<id>`.
    pub result_ref: Option<String>,
}

impl Proposal {
    /// `payload.path` кандидата в Rules.
    pub fn path(&self) -> &str {
        self.payload["path"].as_str().unwrap_or("")
    }

    /// `payload.content` кандидата в Rules.
    pub fn content(&self) -> &str {
        self.payload["content"].as_str().unwrap_or("")
    }
}

/// Новое предложение ([`insert`]).
#[derive(Debug, Clone, PartialEq)]
pub struct NewProposal {
    pub space: Id,
    pub ptype: ProposalType,
    /// `proposed` или `skipped` (отказ исполнителя — только для отсева повторов).
    pub status: ProposalStatus,
    pub targets: Vec<SourceRef>,
    pub payload: Value,
    pub reason: Option<String>,
    pub basis: Vec<String>,
    pub dedup_key: Option<String>,
    pub author_kind: ProposalAuthor,
    pub author: Option<String>,
    pub work_id: Option<Id>,
    pub session_id: Option<String>,
}

/// Исход [`insert`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted(Id),
    /// Строка с тем же `dedup_key` уже есть (в любом статусе, кроме `withdrawn`) — новой нет.
    Existing(Id),
    /// Открытых предложений типа уже [`PROPOSALS_OPEN_MAX`] — новой нет.
    Full,
}

/// Ключ отсева по ссылке источника: предложение из работы (отказ человека — навсегда).
pub fn key_source(ptype: ProposalType, target: &str) -> String {
    hash(&[ptype.as_str(), "src", target])
}

/// Ключ отсева по ссылке и ревизии источника: отказ исполнителя `skipped`.
pub fn key_source_rev(ptype: ProposalType, target: &str, rev: i64) -> String {
    hash(&[ptype.as_str(), "src-rev", target, &rev.to_string()])
}

/// Ключ отсева спонтанного кандидата: путь и текст.
pub fn key_text(ptype: ProposalType, path: &str, content: &str) -> String {
    hash(&[ptype.as_str(), "text", path, content])
}

/// Ключ отсева кандидата уборки (M7.5 §8.7 п. 1, `dedup_key`): тип кандидата `kind`, ссылки и
/// ревизии записей (порядок не важен).
pub fn key_refs_rev(ptype: ProposalType, kind: &str, refs: &[(String, i64)]) -> String {
    let mut v: Vec<String> = refs.iter().map(|(r, rev)| format!("{r}@{rev}")).collect();
    v.sort();
    let mut parts = vec![ptype.as_str(), "refs-rev", kind];
    parts.extend(v.iter().map(String::as_str));
    hash(&parts)
}

/// Ключ «отклонено человеком» кандидата уборки: тип кандидата и ссылки записей **без ревизий**
/// (отклонённое не возвращается ни в какой ревизии, §8.7 п. 1).
pub fn key_refs(ptype: ProposalType, kind: &str, refs: &[String]) -> String {
    let mut v: Vec<&str> = refs.iter().map(String::as_str).collect();
    v.sort_unstable();
    let mut parts = vec![ptype.as_str(), "refs", kind];
    parts.extend(v);
    hash(&parts)
}

fn hash(parts: &[&str]) -> String {
    let mut h = blake3::Hasher::new();
    for p in parts {
        // длина перед частью: границы частей однозначны
        h.update(&(p.len() as u64).to_le_bytes());
        h.update(p.as_bytes());
    }
    h.finalize().to_hex().to_string()
}

const COLS: &str = "id, space_id, type, status, targets, payload, reason, basis, dedup_key, \
                    author_kind, author, work_id, session_id, created_at, decided_at, decided_by, \
                    decision_note, result_ref";

/// Строка `proposals`; `Ok(None)` — неизвестный тип, статус или вид автора (WARN).
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Option<Proposal>> {
    let id: Id = r.get(0)?;
    let (t, s, a): (String, String, String) = (r.get(2)?, r.get(3)?, r.get(9)?);
    let Some(ptype) = ProposalType::parse(&t) else {
        tracing::warn!(proposal = %id, r#type = %t, "предложения: строка неизвестного типа пропущена");
        return Ok(None);
    };
    let (Some(status), Some(author_kind)) = (ProposalStatus::parse(&s), ProposalAuthor::parse(&a))
    else {
        tracing::warn!(proposal = %id, "предложения: строка с неизвестным статусом или автором пропущена");
        return Ok(None);
    };
    let json = |i: usize| -> rusqlite::Result<Value> {
        let s: String = r.get(i)?;
        Ok(serde_json::from_str(&s).unwrap_or(Value::Null))
    };
    let targets = serde_json::from_value(json(4)?).unwrap_or_default();
    let basis = serde_json::from_value(json(7)?).unwrap_or_default();
    Ok(Some(Proposal {
        id,
        space_id: r.get(1)?,
        ptype,
        status,
        targets,
        payload: json(5)?,
        reason: r.get(6)?,
        basis,
        dedup_key: r.get(8)?,
        author_kind,
        author: r.get(10)?,
        work_id: r.get(11)?,
        session_id: r.get(12)?,
        created_at: ts_at(r, 13)?,
        decided_at: ts_opt(r, 14)?,
        decided_by: r.get(15)?,
        decision_note: r.get(16)?,
        result_ref: r.get(17)?,
    }))
}

fn get_tx(c: &Connection, id: Id) -> Result<Option<Proposal>, StoreError> {
    let mut st = c.prepare_cached(&format!("SELECT {COLS} FROM proposals WHERE id = ?1"))?;
    Ok(st.query_row([id], row).optional()?.flatten())
}

/// Открытых (`proposed`) предложений типа `ptype`.
fn open_count(c: &Connection, ptype: ProposalType) -> Result<u32, StoreError> {
    let n: i64 = c.query_row(
        "SELECT count(*) FROM proposals WHERE type = ?1 AND status = 'proposed'",
        [ptype.as_str()],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

/// [`insert`] в транзакции вызывающего (запись принятого элемента работы —
/// `crate::works::finish_claimed`, одна транзакция с закрытием работы): сначала повтор по
/// `dedup_key` (в любом статусе, кроме `withdrawn`, — [`InsertOutcome::Existing`]), затем предел
/// открытых (`proposed`) — [`InsertOutcome::Full`]; у `skipped` предела нет.
pub(crate) fn insert_tx(c: &Connection, p: &NewProposal) -> Result<InsertOutcome, StoreError> {
    if let Some(k) = &p.dedup_key {
        let prev: Option<Id> = c
            .query_row(
                "SELECT id FROM proposals
                 WHERE dedup_key = ?1 AND type = ?2 AND status != 'withdrawn'
                 ORDER BY created_at, id LIMIT 1",
                params![k, p.ptype.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = prev {
            return Ok(InsertOutcome::Existing(id));
        }
    }
    if p.status == ProposalStatus::Proposed && open_count(c, p.ptype)? >= PROPOSALS_OPEN_MAX {
        return Ok(InsertOutcome::Full);
    }
    if !matches!(p.status, ProposalStatus::Proposed | ProposalStatus::Skipped) {
        return Err(StoreError::BadInput(format!(
            "новое предложение — proposed или skipped, не {}",
            p.status.as_str()
        )));
    }
    let id = Id::new();
    c.execute(
        "INSERT INTO proposals (id, space_id, type, status, targets, payload, reason, basis,
                                dedup_key, author_kind, author, work_id, session_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            id,
            p.space,
            p.ptype.as_str(),
            p.status.as_str(),
            json_of(&p.targets),
            p.payload.to_string(),
            p.reason,
            json_of(&p.basis),
            p.dedup_key,
            p.author_kind.as_str(),
            p.author,
            p.work_id,
            p.session_id,
            fmt_ts(now()),
        ],
    )?;
    Ok(InsertOutcome::Inserted(id))
}

/// JSON-массив строкой (`targets`, `basis`): сериализация векторов строк и ссылок не падает.
fn json_of<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".into())
}

/// Новое предложение: повтор по `dedup_key` — прежнее, сверх предела открытых — `Full`.
pub async fn insert(db: &Db, p: NewProposal) -> Result<InsertOutcome, StoreError> {
    db.write(move |t| insert_tx(t, &p)).await
}

/// Предложение `id`; нет или неизвестного типа (WARN) — `None`.
pub async fn get(db: &Db, id: Id) -> Result<Option<Proposal>, StoreError> {
    db.read(move |c| get_tx(c, id)).await
}

/// Фильтр [`list`]: тип (условие SQL), статус (`None` — любой, кроме `skipped`), вид автора;
/// новые первыми, не больше `limit` (до [`LIST_MAX`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListFilter {
    pub ptype: ProposalType,
    pub status: Option<ProposalStatus>,
    pub author_kind: Option<ProposalAuthor>,
    pub limit: usize,
}

/// Предложения по фильтру `f` (тип — в SQL; строки с неизвестным статусом или автором — WARN).
pub async fn list(db: &Db, f: ListFilter) -> Result<Vec<Proposal>, StoreError> {
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT {COLS} FROM proposals
             WHERE type = ?1
               AND (CASE WHEN ?2 IS NULL THEN status != 'skipped' ELSE status = ?2 END)
               AND (?3 IS NULL OR author_kind = ?3)
             ORDER BY created_at DESC, id DESC LIMIT ?4"
        ))?;
        let limit = i64::try_from(f.limit.min(LIST_MAX)).unwrap_or(0);
        let rows = st.query_map(
            params![
                f.ptype.as_str(),
                f.status.map(ProposalStatus::as_str),
                f.author_kind.map(ProposalAuthor::as_str),
                limit
            ],
            row,
        )?;
        let mut out = Vec::new();
        for p in rows {
            if let Some(p) = p? {
                out.push(p);
            }
        }
        Ok(out)
    })
    .await
}

/// Решение по открытому предложению ([`decide`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionKind {
    /// Принято человеком (заметка создана — `result_ref`).
    Accepted,
    /// Отклонено человеком.
    Declined,
    /// Отозвано автором.
    Withdrawn,
    /// Цель изменилась после предложения (уборка, M7.5 §8.7 п. 3).
    Stale,
}

impl DecisionKind {
    fn status(self) -> ProposalStatus {
        match self {
            DecisionKind::Accepted => ProposalStatus::Accepted,
            DecisionKind::Declined => ProposalStatus::Declined,
            DecisionKind::Withdrawn => ProposalStatus::Withdrawn,
            DecisionKind::Stale => ProposalStatus::Stale,
        }
    }
}

/// Решение: что, кто (`decided_by` — актор: `admin`, `user`, id токена), заметка решения и итог.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub to: DecisionKind,
    pub by: String,
    pub note: Option<String>,
    pub result_ref: Option<String>,
}

/// Исход [`decide`].
#[derive(Debug, Clone, PartialEq)]
pub enum Decided {
    /// Решение записано: предложение после него.
    Done(Proposal),
    /// Предложение уже не `proposed` (решено раньше или `skipped`): как есть, ничего не записано.
    NotOpen(Proposal),
}

/// Решение `d` по предложению `id` (только `proposed`); нет его (или неизвестный тип) —
/// [`StoreError::NotFound`].
pub async fn decide(db: &Db, id: Id, d: Decision) -> Result<Decided, StoreError> {
    db.write(move |t| {
        let p = get_tx(t, id)?.ok_or(StoreError::NotFound)?;
        if p.status != ProposalStatus::Proposed {
            return Ok(Decided::NotOpen(p));
        }
        t.execute(
            "UPDATE proposals SET status = ?2, decided_at = ?3, decided_by = ?4,
                    decision_note = ?5, result_ref = ?6
             WHERE id = ?1 AND status = 'proposed'",
            params![
                id,
                d.to.status().as_str(),
                fmt_ts(now()),
                d.by,
                d.note,
                d.result_ref
            ],
        )?;
        Ok(Decided::Done(get_tx(t, id)?.ok_or(StoreError::NotFound)?))
    })
    .await
}

/// Счёт открытых предложений по типам (`GET /proposals/counts`, метка дерева «кандидаты: N»,
/// вкладки «Предложений» WebUI).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ProposalCounts {
    /// Открытых кандидатов в Rules (`type = 'rule'`, `proposed`).
    pub rules: u32,
    /// Открытых кандидатов уборки (`type = 'tidy'`, `proposed`; M7.5 §8.8).
    pub tidy: u32,
}

/// [`ProposalCounts`] базы.
pub async fn counts(db: &Db) -> Result<ProposalCounts, StoreError> {
    db.read(|c| {
        Ok(ProposalCounts {
            rules: open_count(c, ProposalType::Rule)?,
            tidy: open_count(c, ProposalType::Tidy)?,
        })
    })
    .await
}

/// Ключи «отклонено человеком» ([`key_refs`]) предложений уборки `declined` — отсев сборщика по
/// ссылкам в любой ревизии (M7.5 §8.7 пп. 1, 4). Тип кандидата — `payload.candidate`.
pub async fn declined_tidy_keys(db: &Db) -> Result<HashSet<String>, StoreError> {
    db.read(|c| {
        let mut st = c.prepare_cached(
            "SELECT targets, payload FROM proposals WHERE type = ?1 AND status = 'declined'",
        )?;
        let rows = st.query_map([ProposalType::Tidy.as_str()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = HashSet::new();
        for row in rows {
            let (t, p) = row?;
            let targets: Vec<SourceRef> = serde_json::from_str(&t).unwrap_or_default();
            let payload: Value = serde_json::from_str(&p).unwrap_or(Value::Null);
            let kind = payload["candidate"].as_str().unwrap_or("");
            let refs: Vec<String> = targets.into_iter().map(|x| x.target).collect();
            out.insert(key_refs(ProposalType::Tidy, kind, &refs));
        }
        Ok(out)
    })
    .await
}

/// Открытые (`proposed`) предложения уборки, чья запись удалена или сменила ревизию, —
/// `stale` (сборщик ночи, M7.5 §8.7 п. 3: кандидат вернётся с новыми ревизиями). Сколько снято.
pub async fn mark_stale_tidy(db: &Db) -> Result<u64, StoreError> {
    db.write(|t| {
        let open: Vec<(Id, String)> = {
            let mut st = t.prepare_cached(
                "SELECT id, targets FROM proposals WHERE type = ?1 AND status = 'proposed'",
            )?;
            let rows = st.query_map([ProposalType::Tidy.as_str()], |r| {
                Ok((r.get::<_, Id>(0)?, r.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<_, _>>()?
        };
        let mut cur = t.prepare_cached(
            "SELECT current_rev FROM entries WHERE id = ?1 AND deleted_at IS NULL",
        )?;
        let at = fmt_ts(now());
        let mut n = 0u64;
        for (id, targets) in open {
            let targets: Vec<SourceRef> = serde_json::from_str(&targets).unwrap_or_default();
            let mut live = !targets.is_empty();
            for x in &targets {
                let rid = x
                    .target
                    .split_once(':')
                    .and_then(|(_, r)| r.parse::<Id>().ok());
                let rev: Option<i64> = match rid {
                    Some(rid) => cur.query_row([rid], |r| r.get(0)).optional()?,
                    None => None,
                };
                if rev != Some(x.rev) {
                    live = false;
                    break;
                }
            }
            if !live {
                let changed = t.execute(
                    "UPDATE proposals SET status = 'stale', decided_at = ?2,
                            decision_note = 'цель изменилась после предложения'
                     WHERE id = ?1 AND status = 'proposed'",
                    params![id, at],
                )?;
                n += changed as u64;
            }
        }
        Ok(n)
    })
    .await
}

/// Какие из ключей `keys` уже есть у предложений типа `ptype` (в любом статусе, кроме
/// `withdrawn`): сборщик вида
/// `rules` не берёт источник, у которого есть предложение ([`key_source`]) или отказ исполнителя
/// в этой ревизии ([`key_source_rev`]).
pub async fn existing_keys(
    db: &Db,
    ptype: ProposalType,
    keys: &[String],
) -> Result<HashSet<String>, StoreError> {
    if keys.is_empty() {
        return Ok(HashSet::new());
    }
    let list = Value::from(keys.to_vec()).to_string();
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT DISTINCT dedup_key FROM proposals
             WHERE type = ?1 AND status != 'withdrawn'
               AND dedup_key IN (SELECT value FROM json_each(?2))",
        )?;
        let rows = st.query_map(params![ptype.as_str(), list], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<Result<HashSet<_>, _>>()?)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Строки статусов и авторов взаимно обратны; чужие — `None`.
    #[test]
    fn status_and_author_parse() {
        for s in ProposalStatus::ALL {
            assert_eq!(ProposalStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(ProposalStatus::parse("open"), None);
        assert_eq!(ProposalAuthor::parse("agent"), Some(ProposalAuthor::Agent));
        assert_eq!(ProposalAuthor::parse("human"), None);
    }

    /// Ключи отсева различают вид и границы частей.
    #[test]
    fn keys_distinct() {
        let t = ProposalType::Rule;
        assert_ne!(key_source(t, "i:1"), key_source_rev(t, "i:1", 1));
        assert_ne!(key_source_rev(t, "i:1", 1), key_source_rev(t, "i:1", 2));
        assert_ne!(key_text(t, "a", "bc"), key_text(t, "ab", "c"));
        assert_eq!(key_text(t, "a", "b"), key_text(t, "a", "b"));
        // уборка: порядок ссылок не важен, ревизия и тип кандидата — важны
        let u = ProposalType::Tidy;
        let nd = "near_duplicate";
        let ab = [("i:a".to_string(), 1), ("i:b".to_string(), 2)];
        let ba = [("i:b".to_string(), 2), ("i:a".to_string(), 1)];
        assert_eq!(key_refs_rev(u, nd, &ab), key_refs_rev(u, nd, &ba));
        let ab3 = [("i:a".to_string(), 1), ("i:b".to_string(), 3)];
        assert_ne!(key_refs_rev(u, nd, &ab), key_refs_rev(u, nd, &ab3));
        assert_ne!(
            key_refs_rev(u, nd, &ab),
            key_refs_rev(u, "contradiction", &ab)
        );
        let r = ["i:b".to_string(), "i:a".to_string()];
        let r2 = ["i:a".to_string(), "i:b".to_string()];
        assert_eq!(key_refs(u, nd, &r), key_refs(u, nd, &r2));
        assert_ne!(key_refs(u, nd, &r), key_refs_rev(u, nd, &ab));
    }
}
