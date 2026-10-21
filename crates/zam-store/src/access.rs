//! Реестр решений и доступ участников в `system.db` (§6.2, §11.6 ТЗ).
//!
//! Память решения хранится в базе владельца (`users/<owner_id>.db`, пространство
//! `space_id`); здесь — только реестр (GUID → владелец, пространство) и разрешения
//! участников. Проверка доступа на запросе — у сервера; кэша решений о доступе нет: каждое
//! чтение роли идёт в `system.db`, поэтому отзыв действует со следующего запроса.
//!
//! Переходы состояния запроса (иначе `Conflict`):
//! - `pending → approved | rejected`;
//! - `approved → revoked`, `approved → approved` (смена режима);
//! - `rejected → approved`, `revoked → approved`.
//!
//! Повторный запрос после `rejected`/`revoked` состояние не меняет **[решение исполнителя]**:
//! снова открыть доступ может только владелец или администратор. Каждое изменение пишет
//! аудит в той же транзакции: `solution.register`, `solution.access.request|approve|reject|
//! revoke|mode`, `solution.guid_conflict`.

use jiff::Timestamp;
use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use serde_json::json;
use zam_core::{Id, SolutionGuid};

use crate::system::{audit_tx, is_unique_violation};
use crate::time::{fmt_ts, parse_ts, ts_at, ts_opt};
use crate::{Access, StoreError, SystemStore};

/// Режим, который записывается в запрос без токена: при одобрении владелец всё равно задаёт
/// режим явно (по умолчанию CLI/REST — `rw`, см. журнал M3).
const DEFAULT_REQUESTED: Access = Access::Rw;

/// Решение в реестре.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SolutionInfo {
    pub guid: SolutionGuid,
    pub owner_id: Id,
    /// Пространство `solution` в базе владельца.
    pub space_id: Id,
    pub name: String,
    #[serde(serialize_with = "crate::time::ser_ts")]
    pub created_at: Timestamp,
}

/// Состояние запроса доступа участника.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessState {
    Pending,
    Approved,
    Rejected,
    Revoked,
}

impl AccessState {
    pub fn as_str(self) -> &'static str {
        match self {
            AccessState::Pending => "pending",
            AccessState::Approved => "approved",
            AccessState::Rejected => "rejected",
            AccessState::Revoked => "revoked",
        }
    }
}

impl ToSql for AccessState {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl FromSql for AccessState {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        match v.as_str()? {
            "pending" => Ok(AccessState::Pending),
            "approved" => Ok(AccessState::Approved),
            "rejected" => Ok(AccessState::Rejected),
            "revoked" => Ok(AccessState::Revoked),
            other => Err(rusqlite::types::FromSqlError::Other(
                format!("неизвестное состояние доступа: {other}").into(),
            )),
        }
    }
}

/// Роль пользователя в решении.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolutionRole {
    Owner,
    /// Одобренный участник с режимом.
    Member(Access),
    Pending,
    Rejected,
    Revoked,
}

/// Решение владельца или администратора по запросу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessDecision {
    Approve(Access),
    Reject { reason: Option<String> },
}

/// Запрос доступа участника (для владельца и администратора).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccessEntry {
    pub user_id: Id,
    pub user_name: String,
    pub access: Access,
    pub state: AccessState,
    #[serde(serialize_with = "crate::time::ser_ts")]
    pub requested_at: Timestamp,
    #[serde(serialize_with = "crate::time::ser_ts_opt")]
    pub decided_at: Option<Timestamp>,
    /// `admin` | id владельца.
    pub decided_by: Option<String>,
}

const SOLUTION_COLS: &str = "guid, owner_id, space_id, name, created_at";

fn solution_row(r: &Row) -> rusqlite::Result<SolutionInfo> {
    Ok(SolutionInfo {
        guid: r.get(0)?,
        owner_id: r.get(1)?,
        space_id: r.get(2)?,
        name: r.get(3)?,
        created_at: ts_at(r, 4)?,
    })
}

fn solution_in(c: &Connection, guid: &SolutionGuid) -> Result<Option<SolutionInfo>, StoreError> {
    Ok(c.query_row(
        &format!("SELECT {SOLUTION_COLS} FROM solutions WHERE guid = ?1"),
        [guid],
        solution_row,
    )
    .optional()?)
}

fn access_in(
    c: &Connection,
    guid: &SolutionGuid,
    user: Id,
) -> Result<Option<(AccessState, Access)>, StoreError> {
    Ok(c.query_row(
        "SELECT state, access FROM solution_access WHERE guid = ?1 AND user_id = ?2",
        params![guid, user],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

fn user_exists(c: &Connection, id: Id) -> Result<bool, StoreError> {
    Ok(
        c.query_row("SELECT 1 FROM users WHERE id = ?1", [id], |_| Ok(()))
            .optional()?
            .is_some(),
    )
}

fn transition_err(from: AccessState, to: AccessState) -> StoreError {
    StoreError::Conflict(format!(
        "переход доступа {} → {} недопустим",
        from.as_str(),
        to.as_str()
    ))
}

/// Регистрация в транзакции вызывающего; см. [`SystemStore::register_solution`].
fn register_in(
    tx: &Connection,
    guid: &SolutionGuid,
    owner: Id,
    space_id: Id,
    name: &str,
) -> Result<(SolutionInfo, bool), StoreError> {
    if let Some(i) = solution_in(tx, guid)? {
        return Ok((i, false));
    }
    if !user_exists(tx, owner)? {
        return Err(StoreError::NotFound);
    }
    // время — с точностью хранения (микросекунды): возврат равен последующему чтению
    let now = fmt_ts(Timestamp::now());
    let info = SolutionInfo {
        guid: guid.clone(),
        owner_id: owner,
        space_id,
        name: name.to_string(),
        created_at: parse_ts(&now)?,
    };
    tx.execute(
        "INSERT INTO solutions (guid, owner_id, space_id, name, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![info.guid, info.owner_id, info.space_id, info.name, now],
    )
    .map_err(|e| {
        if is_unique_violation(&e) {
            StoreError::Conflict(format!(
                "пространство {space_id} уже зарегистрировано за другим решением"
            ))
        } else {
            e.into()
        }
    })?;
    audit_tx(
        tx,
        "system",
        Some(owner),
        "solution.register",
        Some(guid.as_str()),
        None,
        &json!({ "space_id": space_id.to_string() }),
    )?;
    Ok((info, true))
}

/// Запрос доступа в транзакции вызывающего; см. [`SystemStore::request_access`].
fn request_in(
    tx: &Connection,
    guid: &SolutionGuid,
    user: Id,
    token: Option<Id>,
) -> Result<AccessState, StoreError> {
    let sol = solution_in(tx, guid)?.ok_or(StoreError::NotFound)?;
    if sol.owner_id == user {
        return Err(StoreError::BadInput(
            "владелец решения не может быть его участником".into(),
        ));
    }
    if !user_exists(tx, user)? {
        return Err(StoreError::NotFound);
    }
    if let Some((state, _)) = access_in(tx, guid, user)? {
        return Ok(state);
    }
    // запрошенный режим — режим токена запроса (только токен этого же пользователя)
    let access: Access = match token {
        Some(t) => tx
            .query_row(
                "SELECT access FROM tokens WHERE id = ?1 AND user_id = ?2",
                params![t, user],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(DEFAULT_REQUESTED),
        None => DEFAULT_REQUESTED,
    };
    tx.execute(
        "INSERT INTO solution_access (guid, user_id, access, state, requested_at, token_id)
         VALUES (?1, ?2, ?3, 'pending', ?4, ?5)",
        params![guid, user, access, fmt_ts(Timestamp::now()), token],
    )?;
    let actor = token.map_or_else(|| "system".to_string(), |t| t.to_string());
    audit_tx(
        tx,
        &actor,
        Some(user),
        "solution.access.request",
        Some(guid.as_str()),
        None,
        &json!({ "access": access.as_str() }),
    )?;
    Ok(AccessState::Pending)
}

impl SystemStore {
    /// Регистрирует решение за владельцем. Идемпотентно: GUID уже в реестре — возвращается
    /// существующая запись (`false`), владелец, пространство и имя не меняются. Пространство
    /// уже за другим GUID — `Conflict`; нет пользователя — `NotFound`.
    pub async fn register_solution(
        &self,
        guid: &SolutionGuid,
        owner: Id,
        space_id: Id,
        name: &str,
    ) -> Result<(SolutionInfo, bool), StoreError> {
        let (guid, name) = (guid.clone(), name.to_string());
        self.db
            .write(move |tx| register_in(tx, &guid, owner, space_id, &name))
            .await
    }

    pub async fn get_solution(
        &self,
        guid: &SolutionGuid,
    ) -> Result<Option<SolutionInfo>, StoreError> {
        let guid = guid.clone();
        self.db.read(move |c| solution_in(c, &guid)).await
    }

    /// Решение по пространству базы владельца.
    pub async fn solution_by_space(
        &self,
        space_id: Id,
    ) -> Result<Option<SolutionInfo>, StoreError> {
        self.db
            .read(move |c| {
                Ok(c.query_row(
                    &format!("SELECT {SOLUTION_COLS} FROM solutions WHERE space_id = ?1"),
                    [space_id],
                    solution_row,
                )
                .optional()?)
            })
            .await
    }

    /// Все решения по времени регистрации.
    pub async fn list_solutions(&self) -> Result<Vec<SolutionInfo>, StoreError> {
        self.db
            .read(|c| {
                let mut st = c.prepare(&format!(
                    "SELECT {SOLUTION_COLS} FROM solutions ORDER BY created_at, guid"
                ))?;
                Ok(st.query_map([], solution_row)?.collect::<Result<_, _>>()?)
            })
            .await
    }

    /// Роль пользователя в решении; `None` — ни владелец, ни запроса (или решения нет).
    /// Без кэша: отзыв и отклонение видны со следующего вызова.
    pub async fn solution_role(
        &self,
        guid: &SolutionGuid,
        user: Id,
    ) -> Result<Option<SolutionRole>, StoreError> {
        let guid = guid.clone();
        self.db
            .read(move |c| {
                let Some(sol) = solution_in(c, &guid)? else {
                    return Ok(None);
                };
                if sol.owner_id == user {
                    return Ok(Some(SolutionRole::Owner));
                }
                Ok(
                    access_in(c, &guid, user)?.map(|(state, access)| match state {
                        AccessState::Approved => SolutionRole::Member(access),
                        AccessState::Pending => SolutionRole::Pending,
                        AccessState::Rejected => SolutionRole::Rejected,
                        AccessState::Revoked => SolutionRole::Revoked,
                    }),
                )
            })
            .await
    }

    /// Запрос доступа участника. Идемпотентно: есть запрос — возвращается его состояние
    /// без изменений (в том числе `rejected`/`revoked`). Новый — `pending` с режимом токена
    /// `token` (без токена — `rw`). Владелец — `BadInput`; нет решения или пользователя —
    /// `NotFound`.
    pub async fn request_access(
        &self,
        guid: &SolutionGuid,
        user: Id,
        token: Option<Id>,
    ) -> Result<AccessState, StoreError> {
        let guid = guid.clone();
        self.db
            .write(move |tx| request_in(tx, &guid, user, token))
            .await
    }

    /// Одобряет (с режимом) или отклоняет запрос; `by` — `admin` или id владельца.
    /// Недопустимый переход — `Conflict`; нет решения или запроса — `NotFound`.
    pub async fn decide_access(
        &self,
        guid: &SolutionGuid,
        user: Id,
        d: AccessDecision,
        by: &str,
    ) -> Result<(), StoreError> {
        let (guid, by) = (guid.clone(), by.to_string());
        self.db
            .write(move |tx| {
                solution_in(tx, &guid)?.ok_or(StoreError::NotFound)?;
                let (from, _) = access_in(tx, &guid, user)?.ok_or(StoreError::NotFound)?;
                let now = fmt_ts(Timestamp::now());
                let (action, details) = match &d {
                    AccessDecision::Approve(a) => {
                        tx.execute(
                            "UPDATE solution_access SET state = 'approved', access = ?3,
                                 decided_at = ?4, decided_by = ?5, reason = NULL
                             WHERE guid = ?1 AND user_id = ?2",
                            params![guid, user, a, now, by],
                        )?;
                        let action = if from == AccessState::Approved {
                            "solution.access.mode"
                        } else {
                            "solution.access.approve"
                        };
                        (
                            action,
                            json!({ "access": a.as_str(), "from": from.as_str() }),
                        )
                    }
                    AccessDecision::Reject { reason } => {
                        if from != AccessState::Pending {
                            return Err(transition_err(from, AccessState::Rejected));
                        }
                        tx.execute(
                            "UPDATE solution_access SET state = 'rejected',
                                 decided_at = ?3, decided_by = ?4, reason = ?5
                             WHERE guid = ?1 AND user_id = ?2",
                            params![guid, user, now, by, reason],
                        )?;
                        ("solution.access.reject", json!({ "reason": reason }))
                    }
                };
                audit_tx(
                    tx,
                    &by,
                    Some(user),
                    action,
                    Some(guid.as_str()),
                    None,
                    &details,
                )
            })
            .await
    }

    /// Отзывает одобренный доступ; иначе `Conflict`, нет решения или запроса — `NotFound`.
    pub async fn revoke_access(
        &self,
        guid: &SolutionGuid,
        user: Id,
        by: &str,
    ) -> Result<(), StoreError> {
        let (guid, by) = (guid.clone(), by.to_string());
        self.db
            .write(move |tx| {
                solution_in(tx, &guid)?.ok_or(StoreError::NotFound)?;
                let (from, _) = access_in(tx, &guid, user)?.ok_or(StoreError::NotFound)?;
                if from != AccessState::Approved {
                    return Err(transition_err(from, AccessState::Revoked));
                }
                tx.execute(
                    "UPDATE solution_access SET state = 'revoked',
                         decided_at = ?3, decided_by = ?4, reason = NULL
                     WHERE guid = ?1 AND user_id = ?2",
                    params![guid, user, fmt_ts(Timestamp::now()), by],
                )?;
                audit_tx(
                    tx,
                    &by,
                    Some(user),
                    "solution.access.revoke",
                    Some(guid.as_str()),
                    None,
                    &json!({}),
                )
            })
            .await
    }

    /// Запросы доступа к решению по времени запроса; нет решения — `NotFound`.
    pub async fn list_access(&self, guid: &SolutionGuid) -> Result<Vec<AccessEntry>, StoreError> {
        let guid = guid.clone();
        self.db
            .read(move |c| {
                solution_in(c, &guid)?.ok_or(StoreError::NotFound)?;
                let mut st = c.prepare(
                    "SELECT a.user_id, u.name, a.access, a.state, a.requested_at, a.decided_at,
                            a.decided_by
                     FROM solution_access a JOIN users u ON u.id = a.user_id
                     WHERE a.guid = ?1 ORDER BY a.requested_at, a.user_id",
                )?;
                let rows = st.query_map([&guid], |r| {
                    Ok(AccessEntry {
                        user_id: r.get(0)?,
                        user_name: r.get(1)?,
                        access: r.get(2)?,
                        state: r.get(3)?,
                        requested_at: ts_at(r, 4)?,
                        decided_at: ts_opt(r, 5)?,
                        decided_by: r.get(6)?,
                    })
                })?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await
    }

    /// Решения, доступные пользователю: свои (`Owner`) и одобренные (`Member`), по времени
    /// регистрации.
    pub async fn accessible_solutions(
        &self,
        user: Id,
    ) -> Result<Vec<(SolutionInfo, SolutionRole)>, StoreError> {
        self.db
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT s.guid, s.owner_id, s.space_id, s.name, s.created_at, NULL
                     FROM solutions s WHERE s.owner_id = ?1
                     UNION ALL
                     SELECT s.guid, s.owner_id, s.space_id, s.name, s.created_at, a.access
                     FROM solutions s JOIN solution_access a ON a.guid = s.guid
                     WHERE a.user_id = ?1 AND a.state = 'approved'
                     ORDER BY 5, 1",
                )?;
                let rows = st.query_map([user], |r| {
                    let role = match r.get::<_, Option<Access>>(5)? {
                        Some(a) => SolutionRole::Member(a),
                        None => SolutionRole::Owner,
                    };
                    Ok((solution_row(r)?, role))
                })?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await
    }

    /// Решения, к которым у пользователя есть запрос доступа в любом состоянии (`Member`,
    /// `Pending`, `Rejected`, `Revoked`), по времени регистрации. Свои решения не входят.
    /// Нужен серверу, чтобы на узел отозванного решения ответить 403, а не 404.
    pub async fn member_solutions(
        &self,
        user: Id,
    ) -> Result<Vec<(SolutionInfo, SolutionRole)>, StoreError> {
        self.db
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT s.guid, s.owner_id, s.space_id, s.name, s.created_at, a.state, a.access
                     FROM solutions s JOIN solution_access a ON a.guid = s.guid
                     WHERE a.user_id = ?1 AND s.owner_id <> ?1
                     ORDER BY s.created_at, s.guid",
                )?;
                let rows = st.query_map([user], |r| {
                    let role = match r.get::<_, AccessState>(5)? {
                        AccessState::Approved => SolutionRole::Member(r.get(6)?),
                        AccessState::Pending => SolutionRole::Pending,
                        AccessState::Rejected => SolutionRole::Rejected,
                        AccessState::Revoked => SolutionRole::Revoked,
                    };
                    Ok((solution_row(r)?, role))
                })?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await
    }

    /// Число ожидающих запросов ко всем решениям владельца.
    pub async fn pending_requests_for_owner(&self, owner: Id) -> Result<u64, StoreError> {
        self.db
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM solution_access a JOIN solutions s ON s.guid = a.guid
                     WHERE s.owner_id = ?1 AND a.state = 'pending'",
                    [owner],
                    |r| r.get::<_, i64>(0),
                )? as u64)
            })
            .await
    }

    /// Конфликт GUID при регистрации решений M1–M2 (одной транзакцией): `new` регистрируется
    /// за `owner` с его пространством, к исходному `orig` ставится запрос доступа, аудит
    /// `solution.guid_conflict` — при первой регистрации `new`. Идемпотентно.
    pub(crate) async fn register_guid_conflict(
        &self,
        orig: &SolutionGuid,
        new: &SolutionGuid,
        owner: Id,
        space_id: Id,
        name: &str,
    ) -> Result<(), StoreError> {
        let (orig, new, name) = (orig.clone(), new.clone(), name.to_string());
        self.db
            .write(move |tx| {
                let (_, created) = register_in(tx, &new, owner, space_id, &name)?;
                request_in(tx, &orig, owner, None)?;
                if created {
                    audit_tx(
                        tx,
                        "system",
                        Some(owner),
                        "solution.guid_conflict",
                        Some(orig.as_str()),
                        None,
                        &json!({ "new_guid": new.as_str(), "space_id": space_id.to_string() }),
                    )?;
                }
                Ok(())
            })
            .await
    }
}
