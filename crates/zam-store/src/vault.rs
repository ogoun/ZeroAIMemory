//! Vault (§7.6 ТЗ): записи секретов пользователя (`vault_items`, ревизии `vault_revisions` —
//! только дописываются) и ключи данных пользователей в `system.db` (`user_keys`, проверочный
//! шифротекст мастер-ключа `settings['vault.key_check']`).
//!
//! Криптографии здесь нет: значение (`secret`) приходит и уходит шифротекстом, id записи
//! создаёт сервер и шифрует значение до вызова (AAD связывает шифротекст с пользователем,
//! записью и ревизией). Поэтому каждая новая ревизия получает свой шифротекст: правка без
//! нового значения — тоже перешифрованное прежнее значение (`secret_blob` у
//! [`vault_update`] сервер передаёт всегда).
//!
//! Открытые поля — `title` (имя, уникально среди живых записей одной области) и `data`
//! (`{type, username?, url?, description?}`, проверяет сервер). Значения нигде не
//! индексируются: у Vault нет чанков, FTS и заданий индексации.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use zam_core::{Id, SolutionGuid};

use crate::entries::{AuthorView, json_col};
use crate::nodes::Attribution;
use crate::system::{is_unique_violation, json_err};
use crate::time::{fmt_ts, ts_at};
use crate::{Db, StoreError, SystemStore};

/// Ключ настройки с проверочным шифротекстом мастер-ключа (base64 в JSON-строке).
pub const KEY_CHECK_SETTING: &str = "vault.key_check";
/// Предел длины имени записи, символов. [решение исполнителя]
pub const VAULT_TITLE_MAX_CHARS: usize = 200;

/// Область записи Vault: личный Vault (`knowledge`) или привязка к решению.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultScope {
    Knowledge,
    Solution(SolutionGuid),
}

impl VaultScope {
    /// `scope` в базе.
    pub fn as_str(&self) -> &'static str {
        match self {
            VaultScope::Knowledge => "knowledge",
            VaultScope::Solution(_) => "solution",
        }
    }

    fn guid(&self) -> Option<&str> {
        match self {
            VaultScope::Knowledge => None,
            VaultScope::Solution(g) => Some(g.as_str()),
        }
    }
}

impl Serialize for VaultScope {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// Метаданные записи Vault (без значения).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VaultMeta {
    pub id: Id,
    pub scope: VaultScope,
    pub title: String,
    pub data: serde_json::Value,
    /// Текущая ревизия (`current_rev`).
    pub rev: i64,
    /// Автор записи (создатель).
    pub author: AuthorView,
    #[serde(serialize_with = "crate::time::ser_ts")]
    pub created_at: Timestamp,
    #[serde(serialize_with = "crate::time::ser_ts")]
    pub updated_at: Timestamp,
}

impl VaultMeta {
    /// GUID решения записи с привязкой к решению.
    pub fn agent_solution_id(&self) -> Option<&SolutionGuid> {
        match &self.scope {
            VaultScope::Knowledge => None,
            VaultScope::Solution(g) => Some(g),
        }
    }
}

/// Ревизия записи Vault (без значения).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VaultRevision {
    pub rev: i64,
    pub title: String,
    pub data: serde_json::Value,
    #[serde(serialize_with = "crate::time::ser_ts")]
    pub ts: Timestamp,
    pub author: AuthorView,
}

const ITEM_COLS: &str = "id, scope, agent_solution_id, title, data, current_rev, author_kind, \
     user_id, token_id, session_id, machine, created_at, updated_at";

fn meta_row(r: &Row) -> rusqlite::Result<VaultMeta> {
    let scope: String = r.get(1)?;
    let guid: Option<SolutionGuid> = r.get(2)?;
    let scope = match (scope.as_str(), guid) {
        ("knowledge", None) => VaultScope::Knowledge,
        ("solution", Some(g)) => VaultScope::Solution(g),
        (s, _) => {
            return Err(rusqlite::types::FromSqlError::Other(
                format!("неизвестная область Vault: {s}").into(),
            )
            .into());
        }
    };
    Ok(VaultMeta {
        id: r.get(0)?,
        scope,
        title: r.get(3)?,
        data: json_col(r, 4)?,
        rev: r.get(5)?,
        author: AuthorView {
            kind: r.get(6)?,
            user_id: r.get(7)?,
            token_id: r.get(8)?,
            session_id: r.get(9)?,
            machine: r.get(10)?,
        },
        created_at: ts_at(r, 11)?,
        updated_at: ts_at(r, 12)?,
    })
}

fn bad(msg: impl Into<String>) -> StoreError {
    StoreError::BadInput(msg.into())
}

/// Имя без пробелов по краям: непустое, печатное, до [`VAULT_TITLE_MAX_CHARS`] символов.
pub fn check_vault_title(t: &str) -> Result<String, StoreError> {
    let t = t.trim();
    if t.is_empty() {
        return Err(bad("пустое имя записи Vault"));
    }
    if t.chars().count() > VAULT_TITLE_MAX_CHARS || t.chars().any(char::is_control) {
        return Err(bad(format!(
            "имя записи Vault: до {VAULT_TITLE_MAX_CHARS} печатных символов"
        )));
    }
    Ok(t.to_string())
}

fn check_data(d: &serde_json::Value) -> Result<String, StoreError> {
    if !d.is_object() {
        return Err(bad("data должно быть JSON-объектом"));
    }
    serde_json::to_string(d).map_err(json_err)
}

fn busy_title(title: &str) -> StoreError {
    StoreError::Conflict(format!("запись Vault с именем «{title}» уже есть"))
}

/// Живая запись и её текущий шифротекст; удалённой или нет — `NotFound`.
fn load_live(c: &Connection, id: Id) -> Result<(VaultMeta, Vec<u8>), StoreError> {
    c.prepare_cached(&format!(
        "SELECT {ITEM_COLS}, secret FROM vault_items WHERE id = ?1 AND deleted_at IS NULL"
    ))?
    .query_row([id], |r| Ok((meta_row(r)?, r.get(13)?)))
    .optional()?
    .ok_or(StoreError::NotFound)
}

/// Ревизия `rev` записи `id` — полный снимок с автором `a`.
#[allow(clippy::too_many_arguments)]
fn insert_revision(
    c: &Connection,
    id: Id,
    rev: i64,
    title: &str,
    data: &str,
    secret: &[u8],
    ts: &str,
    a: &Attribution,
) -> Result<(), StoreError> {
    c.prepare_cached(
        "INSERT INTO vault_revisions (item_id, rev, title, data, secret, ts, author_kind,
             user_id, token_id, session_id, machine)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?
    .execute(params![
        id,
        rev,
        title,
        data,
        secret,
        ts,
        a.author_kind,
        a.user_id,
        a.token_id,
        a.session_id,
        a.machine
    ])?;
    Ok(())
}

/// Живые записи области по имени; `q` — подстрока (без учёта регистра) имени, `data.url`
/// или `data.description`.
pub async fn vault_list(
    db: &Db,
    scope: &VaultScope,
    q: Option<&str>,
) -> Result<Vec<VaultMeta>, StoreError> {
    let (kind, guid) = (scope.as_str(), scope.guid().map(str::to_string));
    let items = db
        .read(move |c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT {ITEM_COLS} FROM vault_items
                 WHERE scope = ?1 AND coalesce(agent_solution_id, '') = coalesce(?2, '')
                   AND deleted_at IS NULL
                 ORDER BY title, id"
            ))?;
            let rows = st.query_map(params![kind, guid], meta_row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    let Some(q) = q.map(str::trim).filter(|q| !q.is_empty()) else {
        return Ok(items);
    };
    let needle = q.to_lowercase();
    let hit = |s: Option<&str>| s.is_some_and(|s| s.to_lowercase().contains(&needle));
    Ok(items
        .into_iter()
        .filter(|m| {
            hit(Some(&m.title))
                || hit(m.data.get("url").and_then(serde_json::Value::as_str))
                || hit(m
                    .data
                    .get("description")
                    .and_then(serde_json::Value::as_str))
        })
        .collect())
}

/// Живая запись и шифротекст текущей ревизии; удалённой или нет — `NotFound`.
pub async fn vault_get(db: &Db, id: Id) -> Result<(VaultMeta, Vec<u8>), StoreError> {
    db.read(move |c| load_live(c, id)).await
}

/// Новая запись (ревизия 1) с готовым шифротекстом. Имя занято живой записью той же
/// области → `Conflict`; пустое имя или `data` не объект → `BadInput`.
pub async fn vault_put(
    db: &Db,
    id: Id,
    scope: &VaultScope,
    title: &str,
    data: serde_json::Value,
    secret_blob: Vec<u8>,
    a: &Attribution,
) -> Result<VaultMeta, StoreError> {
    let title = check_vault_title(title)?;
    let data = check_data(&data)?;
    let (kind, guid) = (scope.as_str(), scope.guid().map(str::to_string));
    let a = a.clone();
    db.write(move |tx| {
        let now = fmt_ts(Timestamp::now());
        let r = tx
            .prepare_cached(
                "INSERT INTO vault_items (id, scope, agent_solution_id, title, data, secret,
                     current_rev, author_kind, user_id, token_id, session_id, machine,
                     created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
            )?
            .execute(params![
                id,
                kind,
                guid,
                title,
                data,
                secret_blob,
                a.author_kind,
                a.user_id,
                a.token_id,
                a.session_id,
                a.machine,
                now
            ]);
        match r {
            Ok(_) => {}
            Err(e) if is_unique_violation(&e) => {
                // занято имя (vault_live_title) или, что невероятно, id
                return Err(busy_title(&title));
            }
            Err(e) => return Err(e.into()),
        }
        insert_revision(tx, id, 1, &title, &data, &secret_blob, &now, &a)?;
        Ok(load_live(tx, id)?.0)
    })
    .await
}

/// Новая ревизия `expected_rev + 1`: `None` — поле не меняется. Текущая ревизия не
/// `expected_rev` → `Conflict`; имя занято → `Conflict`; удалённой или нет → `NotFound`.
/// `secret_blob` — шифротекст новой ревизии (`None` — прежний шифротекст: годится, только
/// если AAD шифротекста не зависит от ревизии; сервер передаёт значение всегда).
pub async fn vault_update(
    db: &Db,
    id: Id,
    expected_rev: i64,
    title: Option<String>,
    data: Option<serde_json::Value>,
    secret_blob: Option<Vec<u8>>,
    a: &Attribution,
) -> Result<VaultMeta, StoreError> {
    let title = title.as_deref().map(check_vault_title).transpose()?;
    let data = data.as_ref().map(check_data).transpose()?;
    let a = a.clone();
    db.write(move |tx| {
        let (cur, cur_secret) = load_live(tx, id)?;
        if cur.rev != expected_rev {
            return Err(StoreError::Conflict(format!(
                "ревизия записи Vault {}, а не {expected_rev}",
                cur.rev
            )));
        }
        let title = title.unwrap_or(cur.title);
        let data = match data {
            Some(d) => d,
            None => serde_json::to_string(&cur.data).map_err(json_err)?,
        };
        let secret = secret_blob.unwrap_or(cur_secret);
        let rev = cur.rev + 1;
        let now = fmt_ts(Timestamp::now());
        let r = tx
            .prepare_cached(
                "UPDATE vault_items SET title = ?2, data = ?3, secret = ?4, current_rev = ?5,
                     updated_at = ?6
                 WHERE id = ?1",
            )?
            .execute(params![id, title, data, secret, rev, now]);
        match r {
            Ok(_) => {}
            Err(e) if is_unique_violation(&e) => return Err(busy_title(&title)),
            Err(e) => return Err(e.into()),
        }
        insert_revision(tx, id, rev, &title, &data, &secret, &now, &a)?;
        Ok(load_live(tx, id)?.0)
    })
    .await
}

/// Мягкое удаление (имя освобождается); ревизии остаются. Удалённой или нет — `NotFound`.
/// Кто удалил — в аудите сервера (`vault.delete`): в схеме Vault столбцов для этого нет.
pub async fn vault_delete(db: &Db, id: Id, _a: &Attribution) -> Result<(), StoreError> {
    db.write(move |tx| {
        let now = fmt_ts(Timestamp::now());
        let n = tx.execute(
            "UPDATE vault_items SET deleted_at = ?2, updated_at = ?2
             WHERE id = ?1 AND deleted_at IS NULL",
            params![id, now],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    })
    .await
}

/// Ревизии живой записи по возрастанию номера (без значений).
pub async fn vault_revisions(db: &Db, id: Id) -> Result<Vec<VaultRevision>, StoreError> {
    db.read(move |c| {
        load_live(c, id)?;
        let mut st = c.prepare_cached(
            "SELECT rev, title, data, ts, author_kind, user_id, token_id, session_id, machine
             FROM vault_revisions WHERE item_id = ?1 ORDER BY rev",
        )?;
        let rows = st.query_map([id], |r| {
            Ok(VaultRevision {
                rev: r.get(0)?,
                title: r.get(1)?,
                data: json_col(r, 2)?,
                ts: ts_at(r, 3)?,
                author: AuthorView {
                    kind: r.get(4)?,
                    user_id: r.get(5)?,
                    token_id: r.get(6)?,
                    session_id: r.get(7)?,
                    machine: r.get(8)?,
                },
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}

/// Шифротекст ревизии `rev` живой записи; нет — `NotFound`.
pub async fn vault_revision_secret(db: &Db, id: Id, rev: i64) -> Result<Vec<u8>, StoreError> {
    db.read(move |c| {
        load_live(c, id)?;
        c.query_row(
            "SELECT secret FROM vault_revisions WHERE item_id = ?1 AND rev = ?2",
            params![id, rev],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound)
    })
    .await
}

// ---- ключи данных пользователей и проверка мастер-ключа (system.db) ----

impl SystemStore {
    /// Обёрнутый ключ данных пользователя.
    pub async fn vault_user_key(&self, user: Id) -> Result<Option<Vec<u8>>, StoreError> {
        self.db
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT wrapped FROM user_keys WHERE user_id = ?1",
                    [user],
                    |r| r.get(0),
                )
                .optional()?)
            })
            .await
    }

    /// Сохраняет обёрнутый ключ, если у пользователя его ещё нет, и возвращает действующий
    /// (свой или записанный раньше параллельным запросом).
    pub async fn vault_insert_user_key(
        &self,
        user: Id,
        wrapped: Vec<u8>,
    ) -> Result<Vec<u8>, StoreError> {
        self.db
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO user_keys (user_id, wrapped, created_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT(user_id) DO NOTHING",
                    params![user, wrapped, fmt_ts(Timestamp::now())],
                )?;
                Ok(tx.query_row(
                    "SELECT wrapped FROM user_keys WHERE user_id = ?1",
                    [user],
                    |r| r.get(0),
                )?)
            })
            .await
    }

    /// Любой обёрнутый ключ (проверка мастер-ключа без `key_check`): `(user, wrapped)`.
    pub async fn vault_any_user_key(&self) -> Result<Option<(Id, Vec<u8>)>, StoreError> {
        self.db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT user_id, wrapped FROM user_keys ORDER BY created_at, user_id LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?)
            })
            .await
    }

    /// Проверочный шифротекст мастер-ключа (`settings['vault.key_check']`).
    pub async fn vault_key_check(&self) -> Result<Option<Vec<u8>>, StoreError> {
        let Some(b64) = self.get_setting::<String>(KEY_CHECK_SETTING).await? else {
            return Ok(None);
        };
        STANDARD
            .decode(b64.as_bytes())
            .map(Some)
            .map_err(|_| StoreError::Corrupt(format!("{KEY_CHECK_SETTING}: не base64")))
    }

    /// Записывает проверочный шифротекст, если его ещё нет; возвращает действующий.
    pub async fn vault_init_key_check(&self, blob: Vec<u8>) -> Result<Vec<u8>, StoreError> {
        let json = serde_json::to_string(&STANDARD.encode(&blob)).map_err(json_err)?;
        self.db
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO settings (key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO NOTHING",
                    params![KEY_CHECK_SETTING, json],
                )?;
                Ok(())
            })
            .await?;
        self.vault_key_check()
            .await?
            .ok_or_else(|| StoreError::Corrupt(format!("{KEY_CHECK_SETTING} не записан")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::USER_MIGRATIONS;
    use crate::nodes::AuthorKind;

    fn db(name: &str) -> Db {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/unit-tmp/zam-store-vault")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join("u.db"), USER_MIGRATIONS).unwrap()
    }

    fn agent() -> Attribution {
        Attribution::new(AuthorKind::Agent)
    }

    fn guid() -> SolutionGuid {
        "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e31".parse().unwrap()
    }

    #[tokio::test]
    async fn put_update_delete_and_history() {
        let db = db("lifecycle");
        let k = VaultScope::Knowledge;
        let s = VaultScope::Solution(guid());
        let id = Id::new();
        let d = serde_json::json!({"type": "token"});
        let m = vault_put(&db, id, &k, " gh ", d.clone(), vec![1], &agent())
            .await
            .unwrap();
        assert_eq!((m.title.as_str(), m.rev), ("gh", 1));
        // то же имя в другой области — можно; в той же — 409
        vault_put(&db, Id::new(), &s, "gh", d.clone(), vec![2], &agent())
            .await
            .unwrap();
        let e = vault_put(&db, Id::new(), &k, "gh", d.clone(), vec![3], &agent()).await;
        assert!(matches!(e, Err(StoreError::Conflict(_))), "{e:?}");
        // устаревшая ревизия — 409; новая ревизия со своим шифротекстом
        let e = vault_update(&db, id, 7, None, None, Some(vec![9]), &agent()).await;
        assert!(matches!(e, Err(StoreError::Conflict(_))), "{e:?}");
        let m = vault_update(
            &db,
            id,
            1,
            Some("gh2".into()),
            None,
            Some(vec![4]),
            &agent(),
        )
        .await
        .unwrap();
        assert_eq!((m.title.as_str(), m.rev), ("gh2", 2));
        assert_eq!(vault_get(&db, id).await.unwrap().1, vec![4]);
        assert_eq!(vault_revision_secret(&db, id, 1).await.unwrap(), vec![1]);
        let revs = vault_revisions(&db, id).await.unwrap();
        assert_eq!(revs.iter().map(|r| r.rev).collect::<Vec<_>>(), [1, 2]);
        // список и поиск по открытым полям
        assert_eq!(vault_list(&db, &k, None).await.unwrap().len(), 1);
        assert_eq!(vault_list(&db, &s, Some("GH")).await.unwrap().len(), 1);
        assert!(vault_list(&db, &k, Some("нет")).await.unwrap().is_empty());
        // мягкое удаление освобождает имя; ревизии только дописываются
        vault_delete(&db, id, &agent()).await.unwrap();
        assert!(matches!(
            vault_get(&db, id).await,
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            vault_delete(&db, id, &agent()).await,
            Err(StoreError::NotFound)
        ));
        vault_put(&db, Id::new(), &k, "gh2", d, vec![5], &agent())
            .await
            .unwrap();
        let e = db
            .write(move |tx| {
                tx.execute("DELETE FROM vault_revisions WHERE item_id = ?1", [id])?;
                Ok(())
            })
            .await;
        assert!(e.is_err(), "ревизии только дописываются");
    }
}
