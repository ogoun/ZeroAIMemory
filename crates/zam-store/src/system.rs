//! `system.db`: пользователи, токены, веб-сессии, аудит, настройки (§6.2 ТЗ).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jiff::{Span, Timestamp};
use rand::TryRngCore;
use rand::rngs::OsRng;
use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use zam_core::{Id, generate_token, hash_token};

use crate::time::{fmt_ts, parse_ts};
use crate::{DataDir, Db, Migration, StoreError};

/// Миграции `system.db`.
pub static SYSTEM_MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: include_str!("../migrations/system/0001_init.sql"),
}];

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

/// Режим доступа токена.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    db: Db,
}

fn conv_err<E: std::error::Error + Send + Sync + 'static>(i: usize, e: E) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
}

fn ts_at(r: &Row, i: usize) -> rusqlite::Result<Timestamp> {
    let s: String = r.get(i)?;
    parse_ts(&s).map_err(|e| conv_err(i, e))
}

fn ts_opt(r: &Row, i: usize) -> rusqlite::Result<Option<Timestamp>> {
    r.get::<_, Option<String>>(i)?
        .map(|s| parse_ts(&s).map_err(|e| conv_err(i, e)))
        .transpose()
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

fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _)
        if f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE)
}

fn json_err(e: serde_json::Error) -> StoreError {
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
        let details = serde_json::to_string(&details).map_err(json_err)?;
        self.db
            .write(move |tx| {
                tx.execute(
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
