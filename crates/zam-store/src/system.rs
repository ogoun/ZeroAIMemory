//! `system.db`: пользователи, токены, веб-сессии, аудит, настройки (§6.2 ТЗ).
//! Реестр решений и доступ участников — в [`crate::access`].

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jiff::{Span, Timestamp};
use rand::TryRngCore;
use rand::rngs::OsRng;
use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use zam_core::{Id, generate_token, hash_token};

use crate::time::{conv_err, fmt_ts, ts_at, ts_opt};
use crate::{DataDir, Db, Migration, StoreError};

/// Миграции `system.db`.
pub static SYSTEM_MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("../migrations/system/0001_init.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("../migrations/system/0002_index_skipped.sql"),
    },
    Migration {
        version: 3,
        sql: include_str!("../migrations/system/0003_solutions.sql"),
    },
    Migration {
        version: 4,
        sql: include_str!("../migrations/system/0004_human_tokens.sql"),
    },
    Migration {
        version: 5,
        sql: include_str!("../migrations/system/0005_vault_keys.sql"),
    },
];

/// База пользователя, исключённая из пересборки векторного индекса (`index_skipped`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedIndex {
    pub user_id: Id,
    pub error: String,
    pub attempts: u32,
    pub skipped_at: Timestamp,
}

/// Срок жизни веб-сессии.
const SESSION_TTL_HOURS: i64 = 12;
/// Не чаще одного обновления `last_used_at`/`last_ip` на токен.
const LAST_USED_THROTTLE_SECS: i64 = 60;

/// Пользователь.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: Id,
    pub name: String,
    pub is_admin: bool,
    pub created_at: Timestamp,
    pub disabled_at: Option<Timestamp>,
}

/// Режим доступа токена или участника решения.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Rw,
    Ro,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Rw => "rw",
            Access::Ro => "ro",
        }
    }
}

impl ToSql for Access {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl FromSql for Access {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        match v.as_str()? {
            "rw" => Ok(Access::Rw),
            "ro" => Ok(Access::Ro),
            other => Err(rusqlite::types::FromSqlError::Other(
                format!("неизвестный режим доступа: {other}").into(),
            )),
        }
    }
}

/// Описание токена (без секрета).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenInfo {
    pub id: Id,
    pub user_id: Id,
    pub name: String,
    pub prefix: String,
    pub access: Access,
    pub created_at: Timestamp,
    pub last_used_at: Option<Timestamp>,
    pub last_ip: Option<String>,
    pub revoked_at: Option<Timestamp>,
}

/// Результат успешной проверки токена.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthToken {
    pub token_id: Id,
    pub user_id: Id,
    pub access: Access,
}

/// Веб-сессия администратора (идентификатор сессии в ответ не входит).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSession {
    pub csrf_token: String,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
    pub ip: Option<String>,
}

/// Запись аудита.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditRow {
    pub id: i64,
    pub ts: Timestamp,
    pub actor: String,
    pub user_id: Option<Id>,
    pub action: String,
    pub target: Option<String>,
    pub ip: Option<String>,
    pub details: serde_json::Value,
}

/// Хранилище `system.db`; дёшево клонируется.
#[derive(Clone, Debug)]
pub struct SystemStore {
    pub(crate) db: Db,
}

fn user_row(r: &Row) -> rusqlite::Result<User> {
    Ok(User {
        id: r.get(0)?,
        name: r.get(1)?,
        is_admin: r.get(2)?,
        created_at: ts_at(r, 3)?,
        disabled_at: ts_opt(r, 4)?,
    })
}

const USER_COLS: &str = "id, name, is_admin, created_at, disabled_at";

fn token_row(r: &Row) -> rusqlite::Result<TokenInfo> {
    Ok(TokenInfo {
        id: r.get(0)?,
        user_id: r.get(1)?,
        name: r.get(2)?,
        prefix: r.get(3)?,
        access: r.get(4)?,
        created_at: ts_at(r, 5)?,
        last_used_at: ts_opt(r, 6)?,
        last_ip: r.get(7)?,
        revoked_at: ts_opt(r, 8)?,
    })
}

const TOKEN_COLS: &str =
    "id, user_id, name, prefix, access, created_at, last_used_at, last_ip, revoked_at";

pub(crate) fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _)
        if f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE)
}

pub(crate) fn json_err(e: serde_json::Error) -> StoreError {
    StoreError::Corrupt(format!("некорректный JSON: {e}"))
}

/// 32 случайных байта ОС в base64url без заполнения.
fn random_b64() -> String {
    let mut b = [0u8; 32];
    OsRng
        .try_fill_bytes(&mut b)
        .expect("источник случайности ОС");
    URL_SAFE_NO_PAD.encode(b)
}

/// Пишет запись аудита в транзакции вызывающего (действие и его аудит — атомарно).
/// `details` не должен содержать секретов.
pub(crate) fn audit_tx(
    c: &Connection,
    actor: &str,
    user_id: Option<Id>,
    action: &str,
    target: Option<&str>,
    ip: Option<&str>,
    details: &serde_json::Value,
) -> Result<(), StoreError> {
    let details = serde_json::to_string(details).map_err(json_err)?;
    c.execute(
        "INSERT INTO audit (ts, actor, user_id, action, target, ip, details)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            fmt_ts(Timestamp::now()),
            actor,
            user_id,
            action,
            target,
            ip,
            details
        ],
    )?;
    Ok(())
}

impl SystemStore {
    /// Открывает `system.db` в каталоге данных.
    pub fn open(dir: &DataDir) -> Result<SystemStore, StoreError> {
        Ok(SystemStore {
            db: Db::open(&dir.system_db(), SYSTEM_MIGRATIONS)?,
        })
    }

    /// Проверка доступности `system.db` (для `/ready`).
    pub async fn ping(&self) -> Result<(), StoreError> {
        self.db
            .read(|c| {
                Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
                    .map(|_| ())?)
            })
            .await
    }

    // ---- пользователи ----

    pub async fn create_user(&self, name: &str, is_admin: bool) -> Result<User, StoreError> {
        let name = name.to_string();
        self.db
            .write(move |tx| {
                let u = User {
                    id: Id::new(),
                    name,
                    is_admin,
                    created_at: Timestamp::now(),
                    disabled_at: None,
                };
                tx.execute(
                    "INSERT INTO users (id, name, is_admin, created_at) VALUES (?1, ?2, ?3, ?4)",
                    params![u.id, u.name, u.is_admin, fmt_ts(u.created_at)],
                )
                .map_err(|e| {
                    if is_unique_violation(&e) {
                        StoreError::Conflict(format!("пользователь «{}» уже существует", u.name))
                    } else {
                        e.into()
                    }
                })?;
                Ok(u)
            })
            .await
    }

    pub async fn list_users(&self) -> Result<Vec<User>, StoreError> {
        self.db
            .read(|c| {
                let mut st = c.prepare(&format!("SELECT {USER_COLS} FROM users ORDER BY id"))?;
                Ok(st.query_map([], user_row)?.collect::<Result<_, _>>()?)
            })
            .await
    }

    pub async fn get_user(&self, id: Id) -> Result<Option<User>, StoreError> {
        self.db
            .read(move |c| {
                Ok(c.query_row(
                    &format!("SELECT {USER_COLS} FROM users WHERE id = ?1"),
                    [id],
                    user_row,
                )
                .optional()?)
            })
            .await
    }

    /// Блокирует или разблокирует пользователя; нет такого — `NotFound`.
    pub async fn set_user_disabled(&self, id: Id, disabled: bool) -> Result<(), StoreError> {
        self.db
            .write(move |tx| {
                let at = disabled.then(|| fmt_ts(Timestamp::now()));
                let n = tx.execute(
                    "UPDATE users SET disabled_at = ?2 WHERE id = ?1",
                    params![id, at],
                )?;
                if n == 0 {
                    return Err(StoreError::NotFound);
                }
                Ok(())
            })
            .await
    }

    /// Переименовывает пользователя; нет такого — `NotFound`, занятое имя — `Conflict`.
    pub async fn rename_user(&self, id: Id, name: &str) -> Result<(), StoreError> {
        let name = name.to_string();
        self.db
            .write(move |tx| {
                let n = tx
                    .execute(
                        "UPDATE users SET name = ?2 WHERE id = ?1",
                        params![id, name],
                    )
                    .map_err(|e| {
                        if is_unique_violation(&e) {
                            StoreError::Conflict(format!("пользователь «{name}» уже существует"))
                        } else {
                            e.into()
                        }
                    })?;
                if n == 0 {
                    return Err(StoreError::NotFound);
                }
                Ok(())
            })
            .await
    }

    /// Первый (по времени создания) администратор.
    pub async fn admin_user(&self) -> Result<Option<User>, StoreError> {
        self.db
            .read(|c| {
                Ok(c.query_row(
                    &format!(
                        "SELECT {USER_COLS} FROM users WHERE is_admin = 1 ORDER BY id LIMIT 1"
                    ),
                    [],
                    user_row,
                )
                .optional()?)
            })
            .await
    }

    // ---- токены ----

    /// Выпускает токен; открытый текст возвращается один раз, в БД — только sha256.
    /// Любой токен — агентский (§11.4): колонка `tokens.human` (M3) получает значение по
    /// умолчанию 0 и не читается.
    pub async fn issue_token(
        &self,
        user: Id,
        name: &str,
        access: Access,
    ) -> Result<(TokenInfo, String), StoreError> {
        let name = name.to_string();
        let nt = generate_token();
        let plain = nt.plaintext.clone();
        let info = self
            .db
            .write(move |tx| {
                let exists = tx
                    .query_row("SELECT 1 FROM users WHERE id = ?1", [user], |_| Ok(()))
                    .optional()?
                    .is_some();
                if !exists {
                    return Err(StoreError::NotFound);
                }
                let info = TokenInfo {
                    id: Id::new(),
                    user_id: user,
                    name,
                    prefix: nt.prefix,
                    access,
                    created_at: Timestamp::now(),
                    last_used_at: None,
                    last_ip: None,
                    revoked_at: None,
                };
                tx.execute(
                    "INSERT INTO tokens (id, user_id, name, prefix, secret_hash, access, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        info.id,
                        info.user_id,
                        info.name,
                        info.prefix,
                        nt.secret_hash,
                        info.access,
                        fmt_ts(info.created_at)
                    ],
                )?;
                Ok(info)
            })
            .await?;
        Ok((info, plain))
    }

    /// Префиксы неотозванных токенов, выпущенных человеческими в M3 (`tokens.human = 1`), по
    /// порядку id: с M4 флаг упразднён и не читается — сервер один раз пишет о них WARN при
    /// старте (§11.4). Префикс — не секрет (отображается в списке токенов).
    pub async fn legacy_human_token_prefixes(&self) -> Result<Vec<String>, StoreError> {
        self.db
            .read(|c| {
                let mut st = c.prepare(
                    "SELECT prefix FROM tokens WHERE human = 1 AND revoked_at IS NULL ORDER BY id",
                )?;
                Ok(st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)
            })
            .await
    }

    pub async fn get_token(&self, id: Id) -> Result<Option<TokenInfo>, StoreError> {
        self.db
            .read(move |c| {
                Ok(c.query_row(
                    &format!("SELECT {TOKEN_COLS} FROM tokens WHERE id = ?1"),
                    [id],
                    token_row,
                )
                .optional()?)
            })
            .await
    }

    pub async fn list_tokens(&self, user: Id) -> Result<Vec<TokenInfo>, StoreError> {
        self.db
            .read(move |c| {
                let mut st = c.prepare(&format!(
                    "SELECT {TOKEN_COLS} FROM tokens WHERE user_id = ?1 ORDER BY id"
                ))?;
                Ok(st.query_map([user], token_row)?.collect::<Result<_, _>>()?)
            })
            .await
    }

    /// Отзывает токен (повторный отзыв сохраняет первоначальное время); нет токена — `NotFound`.
    pub async fn revoke_token(&self, token: Id) -> Result<(), StoreError> {
        self.db
            .write(move |tx| {
                let n = tx.execute(
                    "UPDATE tokens SET revoked_at = COALESCE(revoked_at, ?2) WHERE id = ?1",
                    params![token, fmt_ts(Timestamp::now())],
                )?;
                if n == 0 {
                    return Err(StoreError::NotFound);
                }
                Ok(())
            })
            .await
    }

    /// Проверяет токен по sha256. Без кэша: отзыв и блокировка действуют сразу.
    /// `last_used_at`/`last_ip` обновляются не чаще раза в 60 с на токен.
    pub async fn verify_token(
        &self,
        plaintext: &str,
        ip: Option<&str>,
    ) -> Result<Option<AuthToken>, StoreError> {
        let hash = hash_token(plaintext);
        let found = self
            .db
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT t.id, t.user_id, t.access, t.revoked_at, t.last_used_at, u.disabled_at
                     FROM tokens t JOIN users u ON u.id = t.user_id
                     WHERE t.secret_hash = ?1",
                    [&hash],
                    |r| {
                        let auth = AuthToken {
                            token_id: r.get(0)?,
                            user_id: r.get(1)?,
                            access: r.get(2)?,
                        };
                        let revoked = r.get::<_, Option<String>>(3)?.is_some();
                        let last_used = ts_opt(r, 4).unwrap_or(None);
                        let disabled = r.get::<_, Option<String>>(5)?.is_some();
                        Ok((auth, revoked || disabled, last_used))
                    },
                )
                .optional()?)
            })
            .await?;
        let Some((auth, rejected, last_used)) = found else {
            return Ok(None);
        };
        if rejected {
            return Ok(None);
        }
        let now = Timestamp::now();
        let stale =
            last_used.is_none_or(|t| now.as_second() - t.as_second() >= LAST_USED_THROTTLE_SECS);
        if stale {
            let ip = ip.map(str::to_string);
            let id = auth.token_id;
            let threshold = fmt_ts(
                now.checked_sub(Span::new().seconds(LAST_USED_THROTTLE_SECS))
                    .map_err(|e| StoreError::Corrupt(e.to_string()))?,
            );
            // Сбой обновления статистики не должен отклонять валидный токен.
            let _ = self
                .db
                .write(move |tx| {
                    tx.execute(
                        "UPDATE tokens SET last_used_at = ?2, last_ip = ?3
                         WHERE id = ?1 AND (last_used_at IS NULL OR last_used_at < ?4)",
                        params![id, fmt_ts(now), ip, threshold],
                    )?;
                    Ok(())
                })
                .await;
        }
        Ok(Some(auth))
    }

    // ---- веб-сессии ----

    /// Создаёт сессию: `(session_id для cookie, csrf)`. В БД id хранится как sha256.
    pub async fn create_web_session(
        &self,
        ip: Option<&str>,
    ) -> Result<(String, String), StoreError> {
        let (id, csrf) = (random_b64(), random_b64());
        let (key, csrf_db) = (hash_token(&id), csrf.clone());
        let ip = ip.map(str::to_string);
        self.db
            .write(move |tx| {
                let now = Timestamp::now();
                let exp = now
                    .checked_add(Span::new().hours(SESSION_TTL_HOURS))
                    .map_err(|e| StoreError::Corrupt(e.to_string()))?;
                tx.execute(
                    "DELETE FROM web_sessions WHERE expires_at <= ?1",
                    [fmt_ts(now)],
                )?;
                tx.execute(
                    "INSERT INTO web_sessions (id, csrf_token, created_at, expires_at, ip)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![key, csrf_db, fmt_ts(now), fmt_ts(exp), ip],
                )?;
                Ok(())
            })
            .await?;
        Ok((id, csrf))
    }

    /// Живая сессия по открытому id; просроченная удаляется и не возвращается.
    pub async fn get_web_session(&self, id: &str) -> Result<Option<WebSession>, StoreError> {
        let key = hash_token(id);
        let k = key.clone();
        let row = self
            .db
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT csrf_token, created_at, expires_at, ip FROM web_sessions WHERE id = ?1",
                    [&k],
                    |r| {
                        Ok(WebSession {
                            csrf_token: r.get(0)?,
                            created_at: ts_at(r, 1)?,
                            expires_at: ts_at(r, 2)?,
                            ip: r.get(3)?,
                        })
                    },
                )
                .optional()?)
            })
            .await?;
        match row {
            Some(ws) if ws.expires_at > Timestamp::now() => Ok(Some(ws)),
            Some(_) => {
                self.db
                    .write(move |tx| {
                        tx.execute("DELETE FROM web_sessions WHERE id = ?1", [&key])?;
                        Ok(())
                    })
                    .await?;
                Ok(None)
            }
            None => Ok(None),
        }
    }

    pub async fn delete_web_session(&self, id: &str) -> Result<(), StoreError> {
        let key = hash_token(id);
        self.db
            .write(move |tx| {
                tx.execute("DELETE FROM web_sessions WHERE id = ?1", [&key])?;
                Ok(())
            })
            .await
    }

    // ---- аудит ----

    /// Пишет запись аудита. `details` не должен содержать секретов.
    pub async fn audit(
        &self,
        actor: &str,
        user_id: Option<Id>,
        action: &str,
        target: Option<&str>,
        ip: Option<&str>,
        details: serde_json::Value,
    ) -> Result<(), StoreError> {
        let (actor, action) = (actor.to_string(), action.to_string());
        let (target, ip) = (target.map(str::to_string), ip.map(str::to_string));
        self.db
            .write(move |tx| {
                audit_tx(
                    tx,
                    &actor,
                    user_id,
                    &action,
                    target.as_deref(),
                    ip.as_deref(),
                    &details,
                )
            })
            .await
    }

    /// Записи аудита, новые первыми; `before_id` — курсор (строго меньше).
    pub async fn list_audit(
        &self,
        limit: u32,
        before_id: Option<i64>,
    ) -> Result<Vec<AuditRow>, StoreError> {
        self.db
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT id, ts, actor, user_id, action, target, ip, details FROM audit
                     WHERE (?1 IS NULL OR id < ?1) ORDER BY id DESC LIMIT ?2",
                )?;
                let rows = st.query_map(params![before_id, limit], |r| {
                    let details: String = r.get(7)?;
                    Ok(AuditRow {
                        id: r.get(0)?,
                        ts: ts_at(r, 1)?,
                        actor: r.get(2)?,
                        user_id: r.get(3)?,
                        action: r.get(4)?,
                        target: r.get(5)?,
                        ip: r.get(6)?,
                        details: serde_json::from_str(&details).map_err(|e| conv_err(7, e))?,
                    })
                })?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await
    }

    // ---- исключённые из пересборки индекса базы ----

    /// Отмечает базу пользователя исключённой из пересборки индекса (повторная отметка
    /// обновляет ошибку и попытки). Переживает перезапуск: снимается [`Self::index_unskip`],
    /// когда база снова открылась и ей поставлен reindex.
    pub async fn index_skip(&self, user: Id, error: &str, attempts: u32) -> Result<(), StoreError> {
        let error: String = error.chars().take(1000).collect();
        self.db
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO index_skipped (user_id, error, attempts, skipped_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(user_id) DO UPDATE SET error = excluded.error,
                         attempts = max(attempts, excluded.attempts),
                         skipped_at = excluded.skipped_at",
                    params![user, error, attempts, fmt_ts(Timestamp::now())],
                )?;
                Ok(())
            })
            .await
    }

    /// Снимает отметку исключения базы.
    pub async fn index_unskip(&self, user: Id) -> Result<(), StoreError> {
        self.db
            .write(move |tx| {
                tx.execute("DELETE FROM index_skipped WHERE user_id = ?1", [user])?;
                Ok(())
            })
            .await
    }

    /// Исключённые из пересборки базы, по времени отметки.
    pub async fn index_skipped(&self) -> Result<Vec<SkippedIndex>, StoreError> {
        self.db
            .read(|c| {
                let mut st = c.prepare(
                    "SELECT user_id, error, attempts, skipped_at FROM index_skipped
                     ORDER BY skipped_at, user_id",
                )?;
                let rows = st.query_map([], |r| {
                    Ok(SkippedIndex {
                        user_id: r.get(0)?,
                        error: r.get(1)?,
                        attempts: r.get(2)?,
                        skipped_at: ts_at(r, 3)?,
                    })
                })?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await
    }

    // ---- настройки ----

    pub async fn get_setting<T: DeserializeOwned + Send + 'static>(
        &self,
        key: &str,
    ) -> Result<Option<T>, StoreError> {
        let key = key.to_string();
        let raw: Option<String> = self
            .db
            .read(move |c| {
                Ok(
                    c.query_row("SELECT value FROM settings WHERE key = ?1", [&key], |r| {
                        r.get(0)
                    })
                    .optional()?,
                )
            })
            .await?;
        raw.map(|s| serde_json::from_str(&s).map_err(json_err))
            .transpose()
    }

    /// Пачка строковых настроек со счётчиком версии — одной транзакцией (шаблоны блока
    /// инструкции, §15.5). `edits` — `(ключ, новое значение или None — сброс, значение по
    /// умолчанию)`: действующее значение — сохранённое, иначе по умолчанию. Ключ пишется, только
    /// если действующее значение меняется; тогда `version_key` становится
    /// `max(сохранённая, floor) + 1`. Возвращает новую версию и изменённые ключи (с признаком
    /// сброса) или `None`, если ничего не изменилось.
    pub async fn set_settings_versioned(
        &self,
        edits: Vec<(String, Option<String>, String)>,
        version_key: &str,
        floor: u32,
    ) -> Result<Option<(u32, Vec<(String, bool)>)>, StoreError> {
        let version_key = version_key.to_string();
        self.db
            .write(move |tx| {
                let get = |key: &str| -> Result<Option<String>, StoreError> {
                    Ok(tx
                        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                            r.get(0)
                        })
                        .optional()?)
                };
                let mut changed = Vec::new();
                for (key, new, default) in edits {
                    let stored: Option<String> = match get(&key)? {
                        Some(raw) => {
                            serde_json::from_str::<Option<String>>(&raw).map_err(json_err)?
                        }
                        None => None,
                    };
                    let old_eff = stored.as_deref().unwrap_or(&default);
                    let new_eff = new.as_deref().unwrap_or(&default);
                    if old_eff == new_eff {
                        continue;
                    }
                    let json = serde_json::to_string(&new).map_err(json_err)?;
                    tx.execute(
                        "INSERT INTO settings (key, value) VALUES (?1, ?2)
                         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                        params![key, json],
                    )?;
                    changed.push((key, new.is_none()));
                }
                if changed.is_empty() {
                    return Ok(None);
                }
                let saved = match get(&version_key)? {
                    Some(raw) => serde_json::from_str::<u32>(&raw).map_err(json_err)?,
                    None => 0,
                };
                let next = saved.max(floor).saturating_add(1);
                tx.execute(
                    "INSERT INTO settings (key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![version_key, next.to_string()],
                )?;
                Ok(Some((next, changed)))
            })
            .await
    }

    pub async fn set_setting<T: Serialize + ?Sized>(
        &self,
        key: &str,
        value: &T,
    ) -> Result<(), StoreError> {
        let key = key.to_string();
        let json = serde_json::to_string(value).map_err(json_err)?;
        self.db
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO settings (key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![key, json],
                )?;
                Ok(())
            })
            .await
    }
}
