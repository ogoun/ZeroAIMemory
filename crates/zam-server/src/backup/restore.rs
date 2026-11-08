//! `zam-server restore <каталог|архив.zip> [--replace] [--no-reindex]` (M6 §12б.3).
//!
//! 1. Лок `.zam.lock` с меткой `restore` (`DataDir::open_for_restore`): сервер запущен — отказ.
//! 2. Каталог данных пуст (нет `system.db`, баз и блобов), иначе отказ; с `--replace` прежние
//!    `system.db`, `users/`, `blobs/` переносятся в `backups/pre-restore-<время>/`.
//! 3. **До записи хоть одного файла**: пути описи (только базы и блобы), размер и SHA-256
//!    каждого файла, версия схемы каждой базы (новее программы — `SchemaTooNew`; старше —
//!    миграция вперёд с `bak-v<N>` при открытии), место на диске.
//! 4. Базы и блобы **копируются** (не ссылки: данные не делят файлы с копией).
//! 5. Открытие баз (миграции), очистка `web_sessions`, отметка «пересобрать индекс из кэша»
//!    (`meta.reindex_pending`) в каждой базе и `.zam-reindex` в `qdrant/` — `reindex` выполнит
//!    сервер при первом открытии базы (для любого Qdrant).
//! 6. Прогноз векторов ([`VectorPlan`]) и проверка мастер-ключа Vault.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use zam_store::{DataDir, Db, LockMark, StoreError, SystemStore, USER_MIGRATIONS};

use super::manifest::{self, Kind, Manifest};
use super::{BackupError, MANIFEST_FILE, with_margin};
use crate::config::ServerConfig;

/// Параметры восстановления.
#[derive(Debug, Clone)]
pub struct RestoreOpts {
    /// Непустой каталог данных: прежние данные — в `backups/pre-restore-…`.
    pub replace: bool,
    /// Ставить отметки пересборки индекса (`--no-reindex` — `false`).
    pub reindex: bool,
    /// `model_id` модели этой машины для прогноза векторов; `None` — по конфигурации
    /// ([`crate::ml::expected_model_id`]).
    pub model_id: Option<String>,
}

impl Default for RestoreOpts {
    fn default() -> Self {
        Self {
            replace: false,
            reindex: true,
            model_id: None,
        }
    }
}

/// Что ждёт векторы после восстановления.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VectorPlan {
    /// Модель и нарезка совпали: Qdrant пересобирается из кэша без инференса.
    FromCache,
    /// Пересчёт: `"model"` — другая модель (или её нет), `"chunker"` — другая нарезка;
    /// `chunks` — сколько чанков посчитается заново.
    Recompute { reason: &'static str, chunks: u64 },
}

impl VectorPlan {
    /// Строка для человека.
    pub fn text(&self) -> String {
        match self {
            VectorPlan::FromCache => "векторы из кэша".into(),
            VectorPlan::Recompute { reason, chunks } => {
                let why = if *reason == "model" {
                    "другая модель"
                } else {
                    "другая нарезка"
                };
                format!("пересчёт: {why}, {chunks} чанков")
            }
        }
    }
}

/// Итог восстановления.
#[derive(Debug, Clone)]
pub struct RestoreReport {
    pub dbs: usize,
    pub blobs: usize,
    pub schemas: BTreeMap<String, u32>,
    pub drift: Vec<String>,
    pub vectors: VectorPlan,
    /// Мастер-ключ этой машины подходит к ключам Vault копии (или их нет).
    pub vault_ok: bool,
    /// Куда перенесены прежние данные (`--replace`).
    pub moved_to: Option<PathBuf>,
}

/// Предупреждение о ключе Vault (§12б.3) — дословно.
pub const VAULT_WARNING: &str =
    "Vault: ключ не подходит — секреты недоступны, положите прежний master.key";

impl RestoreReport {
    /// Итог для человека (несколько строк).
    pub fn text(&self) -> String {
        let mut s = format!(
            "восстановлено: баз {}, блобов {}; схемы: {}",
            self.dbs,
            self.blobs,
            self.schemas
                .iter()
                .map(|(f, v)| format!("{f}={v}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        if let Some(p) = &self.moved_to {
            s.push_str(&format!("\nпрежние данные перенесены в {}", p.display()));
        }
        if !self.drift.is_empty() {
            s.push_str(&format!("\nрасхождения снимков: {}", self.drift.join("; ")));
        }
        s.push_str(&format!("\nвекторы: {}", self.vectors.text()));
        s
    }
}

/// Источник: каталог копии или архив zip.
enum Source {
    Dir(PathBuf),
    Zip(Box<zip::ZipArchive<std::fs::File>>),
}

fn zip_err(e: zip::result::ZipError) -> BackupError {
    BackupError::BadArchive(format!("архив: {e}"))
}

impl Source {
    fn open(src: &Path) -> Result<Source, BackupError> {
        if src.is_dir() {
            return Ok(Source::Dir(src.to_path_buf()));
        }
        let f = std::fs::File::open(src)?;
        Ok(Source::Zip(Box::new(
            zip::ZipArchive::new(f).map_err(zip_err)?,
        )))
    }

    fn manifest(&mut self) -> Result<Manifest, BackupError> {
        match self {
            Source::Dir(d) => Manifest::read(d),
            Source::Zip(z) => {
                let mut e = z.by_name(MANIFEST_FILE).map_err(zip_err)?;
                let mut text = String::new();
                e.read_to_string(&mut text)?;
                Manifest::parse(&text)
            }
        }
    }

    /// Поток файла описи `rel` и его размер.
    fn with_reader<T>(
        &mut self,
        rel: &str,
        f: impl FnOnce(&mut dyn Read, u64) -> Result<T, BackupError>,
    ) -> Result<T, BackupError> {
        let missing = || BackupError::BadSum {
            path: rel.to_string(),
        };
        match self {
            Source::Dir(d) => {
                let p = manifest::local(d, rel);
                let mut file = std::fs::File::open(&p).map_err(|_| missing())?;
                let size = file.metadata()?.len();
                f(&mut file, size)
            }
            Source::Zip(z) => {
                let mut e = z.by_name(rel).map_err(|_| missing())?;
                let size = e.size();
                f(&mut e, size)
            }
        }
    }
}

/// Каталог не пуст: есть файлы (рекурсивно).
fn has_files(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    rd.flatten().any(|e| {
        let p = e.path();
        if p.is_dir() { has_files(&p) } else { true }
    })
}

/// Путь `<file>-wal` / `-shm`.
fn side(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Проверки до записи: пути, суммы, размеры, версии схем. Возвращает виды файлов описи.
fn verify(src: &mut Source, m: &Manifest) -> Result<Vec<Kind>, BackupError> {
    let kinds: Vec<Kind> = m
        .files
        .iter()
        .map(|f| manifest::classify(&f.path))
        .collect::<Result<_, _>>()?;
    if !kinds.contains(&Kind::SystemDb) {
        return Err(BackupError::BadArchive("в описи нет system.db".into()));
    }
    let sys_max = zam_store::SYSTEM_MIGRATIONS.len() as u32;
    let user_max = USER_MIGRATIONS.len() as u32;
    for (f, k) in m.files.iter().zip(&kinds) {
        let is_db = !matches!(k, Kind::Blob { .. });
        let (sha, size, version) = src.with_reader(&f.path, |r, _| {
            let mut head = Vec::with_capacity(100);
            if is_db {
                (&mut *r).take(100).read_to_end(&mut head)?;
            }
            let mut chain = std::io::Cursor::new(head.clone()).chain(r);
            let (sha, size) = manifest::sha256_reader(&mut chain)?;
            Ok((sha, size, head))
        })?;
        if sha != f.sha256 || size != f.size {
            return Err(BackupError::BadSum {
                path: f.path.clone(),
            });
        }
        if is_db {
            let have = zam_store::backup::header_user_version(&version)?;
            let max = if *k == Kind::SystemDb {
                sys_max
            } else {
                user_max
            };
            if have > max {
                return Err(BackupError::SchemaTooNew {
                    file: f.path.clone(),
                    have,
                    max,
                });
            }
        }
    }
    Ok(kinds)
}

/// Переносит прежние `system.db` (с `-wal`/`-shm`), `users/`, `blobs/` в
/// `backups/pre-restore-<время>/`; ничего не удаляется.
fn move_old(root: &Path) -> Result<PathBuf, BackupError> {
    let ts = zam_store::time::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .strftime("%Y-%m-%d_%H%M%S")
        .to_string();
    let backups = root.join("backups");
    let mut dst = backups.join(format!("pre-restore-{ts}"));
    let mut k = 2;
    while dst.exists() {
        dst = backups.join(format!("pre-restore-{ts}-{k}"));
        k += 1;
    }
    std::fs::create_dir_all(&dst)?;
    let sys = root.join("system.db");
    for p in [sys.clone(), side(&sys, "-wal"), side(&sys, "-shm")] {
        if p.exists() {
            std::fs::rename(&p, dst.join(p.file_name().unwrap_or_default()))?;
        }
    }
    for d in ["users", "blobs"] {
        let p = root.join(d);
        if p.exists() {
            std::fs::rename(&p, dst.join(d))?;
        }
        std::fs::create_dir_all(&p)?;
    }
    Ok(dst)
}

/// Копирует файлы описи в каталог данных.
fn write_files(src: &mut Source, m: &Manifest, root: &Path) -> Result<(), BackupError> {
    for f in &m.files {
        let dst = manifest::local(root, &f.path);
        if let Some(d) = dst.parent() {
            std::fs::create_dir_all(d)?;
        }
        src.with_reader(&f.path, |r, _| {
            let mut w = std::fs::File::create(&dst)?;
            std::io::copy(r, &mut w)?;
            w.sync_all()?;
            Ok(())
        })?;
    }
    Ok(())
}

/// Синхронная часть: проверки, перенос прежних данных, запись файлов.
fn place(
    src: &Path,
    root: &Path,
    replace: bool,
) -> Result<(Manifest, Vec<Kind>, Option<PathBuf>), BackupError> {
    let mut source = Source::open(src)?;
    let m = source.manifest()?;
    let nonempty = root.join("system.db").exists()
        || has_files(&root.join("users"))
        || has_files(&root.join("blobs"));
    if nonempty && !replace {
        return Err(BackupError::NotEmpty);
    }
    let kinds = verify(&mut source, &m)?;
    let need = with_margin(m.files.iter().map(|f| f.size).sum());
    let free = super::free_space(root, root)?;
    if need > free {
        return Err(BackupError::NoSpace { need, free });
    }
    let moved = if nonempty {
        Some(move_old(root)?)
    } else {
        None
    };
    write_files(&mut source, &m, root)?;
    Ok((m, kinds, moved))
}

/// Восстанавливает копию `src` (каталог или архив zip) в `cfg.data_dir` на остановленном
/// сервере.
pub async fn restore(
    cfg: &ServerConfig,
    src: &Path,
    opts: &RestoreOpts,
) -> Result<RestoreReport, BackupError> {
    let root = cfg.data_dir.clone();
    let dd = match DataDir::open_for_restore(&root) {
        Ok(d) => d,
        Err(StoreError::DataDirLocked(_)) => {
            return Err(if zam_store::lock_mark(&root) == Some(LockMark::Restore) {
                BackupError::RestoreInProgress
            } else {
                BackupError::Locked
            });
        }
        Err(e) => return Err(e.into()),
    };
    let (src_path, replace) = (src.to_path_buf(), opts.replace);
    let r2 = root.clone();
    let (m, kinds, moved_to) = tokio::task::spawn_blocking(move || place(&src_path, &r2, replace))
        .await
        .map_err(|e| BackupError::Io(std::io::Error::other(e)))??;

    // system.db: миграции, сессии WebUI, ключ Vault
    let system = SystemStore::open(&dd)?;
    let cleared = system.clear_web_sessions().await?;
    let env_key = std::env::var(crate::vault::keys::MASTER_KEY_ENV).ok();
    let vault_ok = crate::vault::keys::restored_key_fits(cfg, &system, env_key.as_deref())
        .await
        .map_err(|e| BackupError::Io(std::io::Error::other(e.to_string())))?;
    system
        .audit(
            "cli",
            None,
            "server.restore",
            None,
            None,
            serde_json::json!({
                "dbs": kinds.iter().filter(|k| !matches!(k, Kind::Blob { .. })).count(),
                "created_at": m.created_at, "web_sessions_cleared": cleared,
                "reindex": opts.reindex,
            }),
        )
        .await?;
    drop(system);

    // базы пользователей: миграции вперёд (`bak-v<N>`), отметка пересборки индекса из кэша
    let mut user_dbs = Vec::new();
    for k in &kinds {
        if let Kind::UserDb(id) = k {
            let path = dd.user_db(id);
            let db = Db::open(&path, USER_MIGRATIONS)?;
            if opts.reindex {
                zam_store::set_reindex_pending(
                    &db,
                    &serde_json::json!({ "full": false, "space": null }),
                )
                .await?;
            }
            drop(db);
            user_dbs.push((format!("users/{id}.db"), path));
        }
    }
    if opts.reindex {
        zam_index::supervisor::mark_storage_reindex(&dd.qdrant(), "zam-server restore")?;
    }
    // кэш L1 правил процесса: пара (id, ревизия) восстановленной базы может совпасть с другим
    // текстом (журнал 8b, «→ 18a»)
    crate::api::index::reset_rules_l1_cache();

    let vectors = plan(cfg, opts, &m, &user_dbs).await?;
    let schemas = m.schemas.clone();
    let dbs = kinds
        .iter()
        .filter(|k| !matches!(k, Kind::Blob { .. }))
        .count();
    let blobs = kinds.len() - dbs;
    drop(dd);
    Ok(RestoreReport {
        dbs,
        blobs,
        schemas,
        drift: m.drift.clone(),
        vectors,
        vault_ok,
        moved_to,
    })
}

/// Прогноз векторов: модель этой машины против `model_id` кэша копии, затем нарезка
/// процесса (как у сервера) против `meta.chunker*` баз.
async fn plan(
    cfg: &ServerConfig,
    opts: &RestoreOpts,
    m: &Manifest,
    user_dbs: &[(String, PathBuf)],
) -> Result<VectorPlan, BackupError> {
    let cfg2 = cfg.clone();
    let expected = match &opts.model_id {
        Some(id) => Some(id.clone()),
        None => tokio::task::spawn_blocking(move || crate::ml::expected_model_id(&cfg2))
            .await
            .map_err(|e| BackupError::Io(std::io::Error::other(e)))?,
    };
    let mut model_chunks = 0u64;
    let mut model_differs = false;
    for (rel, path) in user_dbs {
        let have = m.embedding_models.get(rel).cloned().unwrap_or_default();
        if !have.is_empty() && expected.as_ref().is_none_or(|e| !have.contains(e)) {
            model_differs = true;
            model_chunks += zam_store::backup::chunk_count(path)?;
        }
    }
    if model_differs {
        return Ok(VectorPlan::Recompute {
            reason: "model",
            chunks: model_chunks,
        });
    }
    // нарезка — как её поставит сервер этой машины (токенизатор установленной модели)
    crate::ml::setup_chunker(cfg, None);
    let mut stale_chunks = 0u64;
    let mut stale = false;
    for (_, path) in user_dbs {
        let (parts, n) = zam_store::backup::stale_chunkers(path)?;
        if !parts.is_empty() {
            stale = true;
            stale_chunks += n;
        }
    }
    Ok(if stale {
        VectorPlan::Recompute {
            reason: "chunker",
            chunks: stale_chunks,
        }
    } else {
        VectorPlan::FromCache
    })
}
