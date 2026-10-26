//! Мастер-ключ сервера и ключи данных пользователей Vault (§7.6).
//!
//! **Мастер-ключ** (32 байта, base64):
//! 1. `ZAM_MASTER_KEY`, иначе
//! 2. файл `[vault] master_key_file` (по умолчанию `<data_dir>/master.key`, одна строка base64).
//!
//! Файл создаётся только при первом запуске Vault — когда в `system.db` нет ни
//! `user_keys`, ни `vault.key_check`, — атомарно и без перезаписи ([`publish_key_file`]) до
//! записи `vault.key_check`; на Linux с правами 0600, на Windows с DACL только владельцу,
//! SYSTEM и администраторам.
//!
//! **Блокировка** ([`VaultState::Locked`]): ключ не прошёл `vault.key_check` (подменён или
//! расходится с `ZAM_MASTER_KEY`), файла нет при существующих ключах, ключ не base64 32 байт,
//! файл не читается. Причина — одной строкой WARN в журнал и в ответ 503
//! `/problems/vault-locked`; остальной сервер работает, новый ключ молча не создаётся.
//! Ошибка старта — только сбой `system.db`.
//!
//! **Ключ пользователя** — 32 случайных байта, обёрнутые мастер-ключом (`user_keys`);
//! создаётся при первом шифровании, развёрнутые ключи кэшируются в памяти ([`Zeroizing`]).
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use zam_core::Id;
use zam_store::{StoreError, SystemStore};
use zeroize::Zeroizing;

use super::crypto::{
    self, SecretKey, item_aad, key_check, key_check_ok, unwrap_user_key, wrap_user_key,
};
use crate::config::ServerConfig;
use crate::error::ApiError;

/// Переменная окружения мастер-ключа.
pub const MASTER_KEY_ENV: &str = "ZAM_MASTER_KEY";
/// Имя файла мастер-ключа по умолчанию в каталоге данных.
pub const MASTER_KEY_FILE: &str = "master.key";

/// Состояние Vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultState {
    Ready,
    /// Заблокирован: причина без путей и ключей (уходит в ответ 503).
    Locked {
        reason: String,
    },
}

/// Ключи Vault: мастер-ключ (если Vault готов) и кэш ключей пользователей.
pub struct VaultKeys {
    state: VaultState,
    master: Option<SecretKey>,
    sys: SystemStore,
    cache: Mutex<HashMap<Id, SecretKey>>,
}

// Вручную: ключи не попадают в отладочный вывод.
impl std::fmt::Debug for VaultKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultKeys")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// Откуда взят мастер-ключ (для причины блокировки).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Env,
    File,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Source::Env => "ZAM_MASTER_KEY",
            Source::File => "файл мастер-ключа ([vault] master_key_file)",
        }
    }
}

/// Итог получения мастер-ключа: ключ или причина блокировки.
type Resolved = Result<(SecretKey, Source), String>;

/// Путь файла мастер-ключа: из конфигурации или `<data_dir>/master.key`.
pub fn master_key_path(cfg: &ServerConfig) -> PathBuf {
    cfg.vault
        .master_key_file
        .clone()
        .unwrap_or_else(|| cfg.data_dir.join(MASTER_KEY_FILE))
}

/// base64 (стандартный алфавит, заполнение необязательно) ровно 32 байт.
fn decode_key(text: &str) -> Option<SecretKey> {
    let t = text.trim();
    let raw = Zeroizing::new(
        STANDARD
            .decode(t)
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(t))
            .ok()?,
    );
    crypto::key_from(&raw)
}

/// Временный файл публикации ключа: `<имя>.tmp` в том же каталоге.
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Остаток `<имя>.tmp` прошлого сбоя публикации: ключ из него ещё нигде не использован
/// (`key_check` пишется после публикации) — удаляется.
fn remove_stale_tmp(path: &Path) {
    let tmp = tmp_path(path);
    match std::fs::remove_file(&tmp) {
        Ok(()) => tracing::warn!(
            path = %tmp.display(),
            "удалён остаток прерванного создания мастер-ключа Vault"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(
            path = %tmp.display(),
            error = %e.kind(),
            "не удалось удалить остаток создания мастер-ключа Vault"
        ),
    }
}

/// Файловая система не поддерживает жёсткие ссылки (FAT, часть сетевых ФС).
fn link_unsupported(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::{PermissionDenied, Unsupported};
    // Windows: ERROR_INVALID_FUNCTION (1) / ERROR_NOT_SUPPORTED (50) на FAT и сетевых ФС;
    // Linux: EPERM (vfat), EOPNOTSUPP
    matches!(e.kind(), Unsupported | PermissionDenied)
        || (cfg!(windows) && matches!(e.raw_os_error(), Some(1 | 50)))
}

/// fsync каталога: запись имени нового файла переживает сбой питания (Linux). На Windows
/// каталог так не синхронизируется — NTFS журналирует метаданные сама.
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Атомарная публикация нового файла ключа **без перезаписи**: `<имя>.tmp` (на Linux сразу
/// 0600, на Windows — DACL только владельцу, SYSTEM и администраторам, до записи
/// содержимого — [`create_private`]), `write_all` +
/// `sync_all`, затем `hard_link(tmp, path)` — занятое имя даёт `AlreadyExists` (ключ
/// создаётся один раз), `tmp` удаляется, каталог синхронизируется. Без жёстких ссылок —
/// `rename` после проверки отсутствия `path` (между проверкой и `rename` окна защищает
/// только лок каталога данных: ограничение записано в журнале M3). Читатель никогда не
/// видит пустой или обрезанный ключ.
fn publish_key_file(path: &Path, key: &SecretKey) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&dir)?;
    let tmp = tmp_path(path);
    let _ = std::fs::remove_file(&tmp);
    let written = (|| {
        let mut f = create_private(&tmp)?;
        let text = Zeroizing::new(format!("{}\n", STANDARD.encode(key.as_slice())));
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        Ok::<_, std::io::Error>(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let published = match std::fs::hard_link(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) if link_unsupported(&e) => {
            if path.exists() {
                Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
            } else {
                std::fs::rename(&tmp, path)
            }
        }
        Err(e) => Err(e),
    };
    let _ = std::fs::remove_file(&tmp);
    published?;
    sync_dir(&dir)
}

/// Создаёт новый пустой файл (`create_new`, без перезаписи) сразу с правами только владельца:
/// на Linux — `mode(0o600)` при создании, на Windows — защищённый DACL в самом `CreateFileW`
/// ([`super::acl::create_private_file`]). Файла с унаследованными правами каталога не бывает
/// ни мгновения — ни содержимого, ни дескриптора, открытого с этими правами.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(windows)]
    {
        super::acl::create_private_file(path)
    }
    #[cfg(not(windows))]
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        o.open(path)
    }
}

/// Кто, кроме владельца, может прочитать существующий файл ключа: на Linux — права группы
/// и прочих (`mode & 0o077`), на Windows — SID из DACL ([`super::acl::foreign_readers`]).
/// `None` — только владелец (и SYSTEM, администраторы на Windows).
fn key_file_exposure(path: &Path) -> std::io::Result<Option<String>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
        Ok((mode & 0o077 != 0).then(|| format!("права {mode:03o}")))
    }
    #[cfg(windows)]
    {
        let readers = super::acl::foreign_readers(path)?;
        Ok((!readers.is_empty()).then(|| format!("DACL разрешает чтение: {}", readers.join(", "))))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(None)
    }
}

/// Предупреждение при старте, если файл ключа читают не только владелец (ключ из бэкапа с
/// правами 0644, файл в каталоге с наследуемым доступом «Пользователи»).
/// **[решение исполнителя]** предупреждение, а не блокировка Vault: блокировка не отзывает
/// уже открытый доступ к файлу и к копиям, а лишь отключает секреты до правки прав;
/// права, выставленные администратором намеренно (например, 0640 для агента бэкапа),
/// не меняются сами. Права файла, созданного сервером, — всегда только владелец.
fn warn_if_exposed(path: &Path) {
    match key_file_exposure(path) {
        Ok(None) => {}
        Ok(Some(who)) => tracing::warn!(
            path = %path.display(),
            access = %who,
            "файл мастер-ключа Vault доступен не только владельцу: ограничьте права \
             (Linux: chmod 600; Windows: только владелец, SYSTEM и администраторы)"
        ),
        Err(e) => tracing::warn!(
            path = %path.display(),
            error = %e.kind(),
            "не удалось проверить права файла мастер-ключа Vault"
        ),
    }
}

/// Сбой хранилища при проверке ключа: повреждённая настройка — блокировка, прочее — ошибка
/// старта.
fn store_or_lock<T>(r: Result<T, StoreError>) -> anyhow::Result<Result<T, String>> {
    match r {
        Ok(v) => Ok(Ok(v)),
        Err(StoreError::Corrupt(m)) => Ok(Err(format!("ключи Vault повреждены: {m}"))),
        Err(e) => Err(e.into()),
    }
}

/// Есть ли уже ключи Vault (обёрнутые ключи пользователей или проверочный шифротекст).
async fn has_keys(sys: &SystemStore) -> anyhow::Result<Result<bool, String>> {
    if sys.vault_any_user_key().await?.is_some() {
        return Ok(Ok(true));
    }
    Ok(store_or_lock(sys.vault_key_check().await)?.map(|k| k.is_some()))
}

/// Мастер-ключ из окружения или файла; файл создаётся только для пустого Vault.
async fn resolve(
    cfg: &ServerConfig,
    sys: &SystemStore,
    env_key: Option<&str>,
) -> anyhow::Result<Resolved> {
    if let Some(raw) = env_key.filter(|k| !k.trim().is_empty()) {
        return Ok(match decode_key(raw) {
            Some(k) => Ok((k, Source::Env)),
            None => Err("ZAM_MASTER_KEY: ожидается base64 ровно 32 байт".into()),
        });
    }
    let path = master_key_path(cfg);
    remove_stale_tmp(&path);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let text = Zeroizing::new(text);
            warn_if_exposed(&path);
            Ok(match decode_key(&text) {
                Some(k) => Ok((k, Source::File)),
                None => Err(format!(
                    "{}: ожидается base64 ровно 32 байт",
                    Source::File.label()
                )),
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match has_keys(sys).await? {
                Err(reason) => return Ok(Err(reason)),
                Ok(true) => {
                    return Ok(Err(format!(
                        "{} не найден, а ключи Vault уже есть: новый ключ не создаётся \
                         (верните файл из резервной копии или задайте ZAM_MASTER_KEY)",
                        Source::File.label()
                    )));
                }
                Ok(false) => {}
            }
            let key = crypto::random_key();
            match publish_key_file(&path, &key) {
                Ok(()) => {
                    tracing::info!(
                        path = %path.display(),
                        "создан мастер-ключ Vault: зарезервируйте файл отдельно от бэкапа каталога данных"
                    );
                    Ok(Ok((key, Source::File)))
                }
                // параллельный первый запуск — читаем записанный
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let text = Zeroizing::new(std::fs::read_to_string(&path)?);
                    Ok(decode_key(&text)
                        .map(|k| (k, Source::File))
                        .ok_or_else(|| format!("{}: ожидается base64", Source::File.label())))
                }
                Err(e) => Ok(Err(format!(
                    "не удалось создать {}: {}",
                    Source::File.label(),
                    e.kind()
                ))),
            }
        }
        Err(e) => Ok(Err(format!(
            "{} не читается: {}",
            Source::File.label(),
            e.kind()
        ))),
    }
}

/// Проверка ключа по `vault.key_check` (или по ключу пользователя, если проверочного
/// шифротекста ещё нет); первая проверка записывает `vault.key_check`.
async fn verify(
    sys: &SystemStore,
    master: &SecretKey,
    src: Source,
) -> anyhow::Result<Result<(), String>> {
    let mismatch = || {
        format!(
            "мастер-ключ ({}) не подходит: проверка vault.key_check не прошла \
             (ключ подменён или расходится с прежним)",
            src.label()
        )
    };
    let check = match store_or_lock(sys.vault_key_check().await)? {
        Ok(c) => c,
        Err(reason) => return Ok(Err(reason)),
    };
    if let Some(blob) = check {
        return Ok(if key_check_ok(master, &blob) {
            Ok(())
        } else {
            Err(mismatch())
        });
    }
    if let Some((user, wrapped)) = sys.vault_any_user_key().await?
        && unwrap_user_key(master, user, &wrapped).is_err()
    {
        return Ok(Err(mismatch()));
    }
    let stored = sys.vault_init_key_check(key_check(master)).await?;
    Ok(if key_check_ok(master, &stored) {
        Ok(())
    } else {
        Err(mismatch())
    })
}

impl VaultKeys {
    /// Загружает мастер-ключ (`ZAM_MASTER_KEY` из окружения процесса, иначе файл).
    /// Блокировка Vault — не ошибка старта.
    pub async fn load(cfg: &ServerConfig, sys: &SystemStore) -> anyhow::Result<VaultKeys> {
        let env = std::env::var(MASTER_KEY_ENV).ok().map(Zeroizing::new);
        Self::load_with(cfg, sys, env.as_deref().map(String::as_str)).await
    }

    /// [`Self::load`] с явным значением `ZAM_MASTER_KEY` (`None` — переменной нет).
    pub async fn load_with(
        cfg: &ServerConfig,
        sys: &SystemStore,
        env_key: Option<&str>,
    ) -> anyhow::Result<VaultKeys> {
        let checked = match resolve(cfg, sys, env_key).await? {
            Ok((key, src)) => verify(sys, &key, src).await?.map(|()| key),
            Err(reason) => Err(reason),
        };
        let (state, master) = match checked {
            Ok(key) => (VaultState::Ready, Some(key)),
            Err(reason) => {
                tracing::warn!(path = %master_key_path(cfg).display(), "Vault заблокирован: {reason}");
                (VaultState::Locked { reason }, None)
            }
        };
        Ok(VaultKeys {
            state,
            master,
            sys: sys.clone(),
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn state(&self) -> &VaultState {
        &self.state
    }

    /// Vault готов, иначе 503 `/problems/vault-locked` с причиной.
    pub fn ready(&self) -> Result<(), ApiError> {
        self.master_key().map(|_| ())
    }

    fn master_key(&self) -> Result<&SecretKey, ApiError> {
        match (&self.state, &self.master) {
            (VaultState::Ready, Some(k)) => Ok(k),
            (VaultState::Locked { reason }, _) => Err(ApiError::VaultLocked(reason.clone())),
            (VaultState::Ready, None) => {
                Err(ApiError::Internal("Vault готов без мастер-ключа".into()))
            }
        }
    }

    fn cached(&self, user: Id) -> Option<SecretKey> {
        self.cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&user)
            .cloned()
    }

    /// Ключ данных пользователя; `create` — создать, если его ещё нет.
    async fn user_key(&self, user: Id, create: bool) -> Result<SecretKey, ApiError> {
        let master = self.master_key()?;
        if let Some(k) = self.cached(user) {
            return Ok(k);
        }
        let wrapped = match self.sys.vault_user_key(user).await? {
            Some(w) => w,
            None if create => {
                let fresh = crypto::random_key();
                // параллельный запрос мог успеть раньше — действует записанный
                self.sys
                    .vault_insert_user_key(user, wrap_user_key(master, user, &fresh))
                    .await?
            }
            None => {
                return Err(ApiError::Internal(format!(
                    "нет ключа данных Vault пользователя {user}"
                )));
            }
        };
        let key = unwrap_user_key(master, user, &wrapped).map_err(|e| {
            ApiError::Internal(format!("ключ данных Vault пользователя {user}: {e}"))
        })?;
        self.cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(user, key.clone());
        Ok(key)
    }

    /// Шифрует значение ревизии `rev` записи `item` пользователя `user`.
    pub async fn encrypt(
        &self,
        user: Id,
        item: Id,
        rev: i64,
        plain: &[u8],
    ) -> Result<Vec<u8>, ApiError> {
        let key = self.user_key(user, true).await?;
        Ok(crypto::seal(&key, &item_aad(user, item, rev), plain))
    }

    /// Расшифровывает значение ревизии `rev` записи `item` пользователя `user`.
    pub async fn decrypt(
        &self,
        user: Id,
        item: Id,
        rev: i64,
        blob: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, ApiError> {
        let key = self.user_key(user, false).await?;
        crypto::open(&key, &item_aad(user, item, rev), blob)
            .map_err(|e| ApiError::Internal(format!("запись Vault {item} ревизии {rev}: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zam_store::DataDir;

    fn dir(name: &str) -> PathBuf {
        crate::test_util::dir_in_target(&format!("vault-keys-{name}"))
    }

    fn setup(name: &str) -> (ServerConfig, SystemStore, DataDir) {
        let d = dir(name);
        let cfg = ServerConfig {
            data_dir: d.clone(),
            ..ServerConfig::default()
        };
        let dd = DataDir::open(&d).unwrap();
        let sys = SystemStore::open(&dd).unwrap();
        (cfg, sys, dd)
    }

    async fn user(sys: &SystemStore) -> Id {
        sys.create_user("u", false).await.unwrap().id
    }

    /// Файл создаётся с правами только владельца **до** записи ключа: новый пустой файл уже
    /// закрыт (финальная волна M3; на Windows DACL ставился после записи содержимого).
    #[test]
    fn private_file_restricted_before_write() {
        let d = dir("private-before-write");
        let p = d.join("k.tmp");
        let f = create_private(&p).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 0, "ещё пуст");
        assert_eq!(key_file_exposure(&p).unwrap(), None);
        drop(f);
        assert_eq!(
            create_private(&p).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists,
            "без перезаписи"
        );
    }

    /// Существующий ключ, читаемый не только владельцем, замечается при старте (ключ из
    /// бэкапа с правами 0644 или унаследованным доступом каталога): Vault работает,
    /// в журнале — предупреждение.
    #[tokio::test]
    async fn exposed_key_file_detected_vault_still_ready() {
        let (cfg, sys, _dd) = setup("exposed");
        let path = master_key_path(&cfg);
        let v = VaultKeys::load_with(&cfg, &sys, None).await.unwrap();
        assert_eq!(v.state(), &VaultState::Ready);
        assert_eq!(
            key_file_exposure(&path).unwrap(),
            None,
            "созданный сервером"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(
                key_file_exposure(&path).unwrap().as_deref(),
                Some("права 644")
            );
        }
        #[cfg(windows)]
        {
            super::super::acl::set_dacl_for_test(&path, "D:P(A;;FA;;;OW)(A;;FR;;;BU)");
            let who = key_file_exposure(&path).unwrap().unwrap();
            assert!(who.contains("S-1-5-32-545"), "{who}");
        }
        let v = VaultKeys::load_with(&cfg, &sys, None).await.unwrap();
        assert_eq!(
            v.state(),
            &VaultState::Ready,
            "предупреждение, не блокировка"
        );
    }

    #[tokio::test]
    async fn master_key_created_once_with_0600_on_linux() {
        let (cfg, sys, _dd) = setup("created-once");
        let path = master_key_path(&cfg);
        assert!(!path.exists());
        let v = VaultKeys::load_with(&cfg, &sys, None).await.unwrap();
        assert_eq!(v.state(), &VaultState::Ready);
        let first = std::fs::read(&path).unwrap();
        assert!(decode_key(std::str::from_utf8(&first).unwrap()).is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "права master.key: {mode:o}");
        }
        // шифрование создаёт ключ пользователя; второй запуск — тот же файл, данные читаются
        let u = user(&sys).await;
        let item = Id::new();
        let blob = v.encrypt(u, item, 1, b"s3cret").await.unwrap();
        drop(v);
        let v = VaultKeys::load_with(&cfg, &sys, None).await.unwrap();
        assert_eq!(v.state(), &VaultState::Ready);
        assert_eq!(std::fs::read(&path).unwrap(), first, "ключ не пересоздан");
        assert_eq!(
            v.decrypt(u, item, 1, &blob).await.unwrap().as_slice(),
            b"s3cret"
        );
        // файл удалён при существующих ключах — блокировка, новый файл не создаётся
        std::fs::remove_file(&path).unwrap();
        let v = VaultKeys::load_with(&cfg, &sys, None).await.unwrap();
        assert!(
            matches!(v.state(), VaultState::Locked { .. }),
            "{:?}",
            v.state()
        );
        assert!(!path.exists());
        assert!(matches!(v.ready(), Err(ApiError::VaultLocked(_))));
        assert!(matches!(
            v.encrypt(u, item, 2, b"x").await,
            Err(ApiError::VaultLocked(_))
        ));
    }

    #[tokio::test]
    async fn stale_tmp_removed_and_key_created() {
        let (cfg, sys, _dd) = setup("stale-tmp");
        let path = master_key_path(&cfg);
        // остаток прерванной публикации: обрезанный ключ во временном файле
        std::fs::write(tmp_path(&path), "QUFB").unwrap();
        let v = VaultKeys::load_with(&cfg, &sys, None).await.unwrap();
        assert_eq!(v.state(), &VaultState::Ready);
        assert!(!tmp_path(&path).exists(), "остаток .tmp удалён");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(decode_key(&text).is_some(), "полный ключ: {text}");
        assert!(text.ends_with('\n'));
        // остаток рядом с уже опубликованным ключом — тоже удаляется, ключ не меняется
        std::fs::write(tmp_path(&path), "мусор").unwrap();
        let v = VaultKeys::load_with(&cfg, &sys, None).await.unwrap();
        assert_eq!(v.state(), &VaultState::Ready);
        assert!(!tmp_path(&path).exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn publish_never_overwrites_existing_key_file() {
        let d = dir("publish");
        let path = d.join("sub").join("master.key");
        let k1 = crypto::random_key();
        publish_key_file(&path, &k1).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        assert_eq!(decode_key(&first).unwrap().as_slice(), k1.as_slice());
        assert!(!tmp_path(&path).exists());
        // второй ключ не перезаписывает первый: AlreadyExists, файл тот же, .tmp убран
        let e = publish_key_file(&path, &crypto::random_key()).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        assert!(!tmp_path(&path).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn env_master_key_has_priority() {
        let (cfg, sys, _dd) = setup("env-priority");
        let path = master_key_path(&cfg);
        let env = STANDARD.encode([7u8; 32]);
        // первый запуск с ZAM_MASTER_KEY: файл не создаётся
        let v = VaultKeys::load_with(&cfg, &sys, Some(&env)).await.unwrap();
        assert_eq!(v.state(), &VaultState::Ready);
        assert!(!path.exists(), "при ZAM_MASTER_KEY файл не нужен");
        let u = user(&sys).await;
        let item = Id::new();
        let blob = v.encrypt(u, item, 1, b"v").await.unwrap();
        // файл с другим ключом не мешает: окружение важнее
        std::fs::write(&path, format!("{}\n", STANDARD.encode([9u8; 32]))).unwrap();
        let v = VaultKeys::load_with(&cfg, &sys, Some(&env)).await.unwrap();
        assert_eq!(v.state(), &VaultState::Ready);
        assert_eq!(v.decrypt(u, item, 1, &blob).await.unwrap().as_slice(), b"v");
        // без окружения — ключ файла не подходит: блокировка с причиной, файл не тронут
        let v = VaultKeys::load_with(&cfg, &sys, None).await.unwrap();
        let VaultState::Locked { reason } = v.state() else {
            panic!("ожидалась блокировка");
        };
        assert!(reason.contains("vault.key_check"), "{reason}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            STANDARD.encode([9u8; 32])
        );
        // ZAM_MASTER_KEY расходится с прежним — тоже блокировка
        let other = STANDARD.encode([8u8; 32]);
        let v = VaultKeys::load_with(&cfg, &sys, Some(&other))
            .await
            .unwrap();
        let VaultState::Locked { reason } = v.state() else {
            panic!("ожидалась блокировка");
        };
        assert!(reason.contains("ZAM_MASTER_KEY"), "{reason}");
        // не base64 32 байт — блокировка, не ошибка старта
        let v = VaultKeys::load_with(&cfg, &sys, Some("не ключ"))
            .await
            .unwrap();
        assert!(matches!(v.state(), VaultState::Locked { .. }));
        let short = STANDARD.encode([1u8; 16]);
        let v = VaultKeys::load_with(&cfg, &sys, Some(&short))
            .await
            .unwrap();
        assert!(matches!(v.state(), VaultState::Locked { .. }));
    }

    #[tokio::test]
    async fn concurrent_first_encrypt_one_user_key() {
        let (cfg, sys, _dd) = setup("concurrent");
        let v = std::sync::Arc::new(VaultKeys::load_with(&cfg, &sys, None).await.unwrap());
        let u = user(&sys).await;
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let v = v.clone();
            tasks.push(tokio::spawn(async move {
                let item = Id::new();
                (item, v.encrypt(u, item, 1, b"x").await.unwrap())
            }));
        }
        let mut blobs = Vec::new();
        for t in tasks {
            blobs.push(t.await.unwrap());
        }
        // новый экземпляр без кэша расшифровывает все: ключ пользователя один
        let v2 = VaultKeys::load_with(&cfg, &sys, None).await.unwrap();
        for (item, b) in blobs {
            assert_eq!(v2.decrypt(u, item, 1, &b).await.unwrap().as_slice(), b"x");
        }
    }
}
