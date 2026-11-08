//! `zam-server restore <каталог|архив.zip> [--replace] [--no-reindex]` (M6 §12б.3).
//!
//! 1. Лок `.zam.lock` с меткой `restore` (`DataDir::open_for_restore`): сервер запущен — отказ.
//! 2. Каталог данных пуст (нет `system.db`, баз и блобов), иначе отказ; с `--replace` прежние
//!    `system.db`, `users/`, `blobs/` переносятся в `backups/pre-restore-<время>/`.
//! 3. **До записи в каталог данных**: пути описи (только базы и блобы), место (с копиями
//!    `bak-v<N>` мигрирующих баз); файлы копии **копируются** (не ссылки: данные не делят файлы
//!    с копией) в сборку `<data_dir>/.restore-staging/` с SHA-256 при записи (одно чтение),
//!    там же сверяются размеры, суммы и версии схем (новее программы — `SchemaTooNew`; старше
//!    — миграция вперёд с `bak-v<N>` при открытии). Любой сбой — сборка удаляется.
//! 4. Признак [`zam_store::RESTORE_INCOMPLETE_FILE`] (переживает сбой: сервер и `backup`
//!    отказывают, пока его нет), перенос прежних данных (`--replace`) и сборки на место
//!    переименованием.
//! 5. Открытие баз (миграции), очистка `web_sessions`, отметка «пересобрать индекс из кэша»
//!    (`meta.reindex_pending`) в каждой базе и `.zam-reindex` в `qdrant/` — `reindex` выполнит
//!    сервер при первом открытии базы (для любого Qdrant).
//! 6. Прогноз векторов ([`VectorPlan`]) и проверка мастер-ключа Vault; признак незавершённого
//!    восстановления снимается последним.

use std::collections::BTreeMap;
use std::io::{Read, Write};
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
    /// Замечания для человека (внешний Qdrant после `--replace`).
    pub notes: Vec<String>,
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
        for n in &self.notes {
            s.push_str(&format!("\n{n}"));
        }
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

/// Имя каталога сборки восстановления в каталоге данных: файлы копии пишутся сюда (с проверкой
/// сумм при записи), а в каталог данных попадают только переименованием целиком проверенными.
const STAGING: &str = ".restore-staging";

/// Копирует файлы описи в `staging`, считая SHA-256 при записи (одно чтение источника), и
/// проверяет суммы, размеры и версии схем баз. Каталог данных не трогается.
fn stage_files(src: &mut Source, m: &Manifest, staging: &Path) -> Result<Vec<Kind>, BackupError> {
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
        let dst = manifest::local(staging, &f.path);
        let (sha, size) = src.with_reader(&f.path, |r, _| Ok(super::copy_hashed(r, &dst)?))?;
        if sha != f.sha256 || size != f.size {
            return Err(BackupError::BadSum {
                path: f.path.clone(),
            });
        }
        if !matches!(k, Kind::Blob { .. }) {
            let mut head = Vec::with_capacity(100);
            std::fs::File::open(&dst)?
                .take(100)
                .read_to_end(&mut head)?;
            let have = zam_store::backup::header_user_version(&head)?;
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
    for d in ["", "users", "blobs"] {
        let p = staging.join(d);
        if p.is_dir() {
            super::sync_dir(&p)?;
        }
    }
    Ok(kinds)
}

/// Место восстановления: файлы копии и копии `bak-v<N>` баз, которые при открытии мигрируют
/// (схема старше программы), плюс 10 %.
fn need_space(m: &Manifest, kinds: &[Kind]) -> u64 {
    let sys_max = zam_store::SYSTEM_MIGRATIONS.len() as u32;
    let user_max = USER_MIGRATIONS.len() as u32;
    let mut n = 0u64;
    for (f, k) in m.files.iter().zip(kinds) {
        n += f.size;
        let max = match k {
            Kind::SystemDb => sys_max,
            Kind::UserDb(_) => user_max,
            Kind::Blob { .. } => continue,
        };
        if m.schemas.get(&f.path).is_some_and(|v| *v < max) {
            n += f.size;
        }
    }
    with_margin(n)
}

/// Время для имён каталогов (`yyyy-MM-dd_HHmmss`, UTC).
fn ts_name() -> String {
    zam_store::time::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .strftime("%Y-%m-%d_%H%M%S")
        .to_string()
}

/// Новый каталог `backups/pre-restore-<время>/` для прежних данных.
fn new_pre_restore(root: &Path) -> PathBuf {
    let ts = ts_name();
    let backups = root.join("backups");
    let mut dst = backups.join(format!("pre-restore-{ts}"));
    let mut k = 2;
    while dst.exists() {
        dst = backups.join(format!("pre-restore-{ts}-{k}"));
        k += 1;
    }
    dst
}

/// Переносит `system.db` (с `-wal`/`-shm`), `users/`, `blobs/` каталога данных в `dst`; ничего
/// не удаляется. При повторе после сбоя `dst` — каталог прошлой попытки: прежние данные, ещё
/// оставшиеся на месте, дописываются в него, а то, чьё имя в нём уже занято (остатки
/// прерванного переноса сборки), — в `dst/restore-remnants-<время>/`.
fn move_old(root: &Path, dst: &Path) -> Result<(), BackupError> {
    std::fs::create_dir_all(dst)?;
    let remnants = dst.join(format!("restore-remnants-{}", ts_name()));
    let sys = root.join("system.db");
    let mut items = vec![sys.clone(), side(&sys, "-wal"), side(&sys, "-shm")];
    items.push(root.join("users"));
    items.push(root.join("blobs"));
    for p in items {
        if !p.exists() {
            continue;
        }
        let name = p.file_name().unwrap_or_default().to_os_string();
        let mut to = dst.join(&name);
        if to.exists() {
            std::fs::create_dir_all(&remnants)?;
            to = remnants.join(&name);
        }
        super::rename_retry(&p, &to)?;
    }
    super::sync_dir(dst)?;
    Ok(())
}

/// Каталог данных не пуст: `system.db` или её `-wal`/`-shm` (остатки применились бы к
/// восстановленной базе), файлы в `users/` и `blobs/`, след прерванного восстановления.
fn nonempty(root: &Path) -> bool {
    let sys = root.join("system.db");
    sys.exists()
        || side(&sys, "-wal").exists()
        || side(&sys, "-shm").exists()
        || has_files(&root.join("users"))
        || has_files(&root.join("blobs"))
        || zam_store::restore_incomplete(root)
}

/// Признак незавершённого восстановления: пишется до первого изменения каталога данных и
/// хранит каталог прежних данных (`moved_to <путь>`) — повтор после сбоя дописывает в него.
fn mark_incomplete(root: &Path, moved_to: Option<&Path>) -> std::io::Result<()> {
    let p = root.join(zam_store::RESTORE_INCOMPLETE_FILE);
    let mut f = std::fs::File::create(&p)?;
    writeln!(
        f,
        "restore {} {}",
        std::process::id(),
        zam_store::time::now()
    )?;
    if let Some(m) = moved_to {
        writeln!(f, "moved_to {}", m.display())?;
    }
    f.sync_all()?;
    super::sync_dir(root)
}

/// Каталог прежних данных прерванного восстановления (из признака), если он есть.
fn incomplete_moved_to(root: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(root.join(zam_store::RESTORE_INCOMPLETE_FILE)).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("moved_to "))
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

/// Перенос проверенной сборки на место: `system.db`, `users/`, `blobs/` — переименованием.
fn place_staged(staging: &Path, root: &Path) -> Result<(), BackupError> {
    super::rename_retry(&staging.join("system.db"), &root.join("system.db"))?;
    super::fail_point(root, "restore_place")?;
    for d in ["users", "blobs"] {
        let from = staging.join(d);
        let to = root.join(d);
        // пустой каталог (создан открытием каталога данных) уступает место сборке
        if to.is_dir() && !has_files(&to) {
            std::fs::remove_dir_all(&to)?;
        }
        if from.is_dir() {
            super::rename_retry(&from, &to)?;
        } else {
            std::fs::create_dir_all(&to)?;
        }
    }
    super::sync_dir(root)?;
    let _ = std::fs::remove_dir_all(staging);
    Ok(())
}

/// Синхронная часть: проверки и сборка вне каталога данных, затем (признак незавершённого
/// восстановления) перенос прежних данных и сборки на место. Ошибка после признака —
/// [`BackupError::RestoreFailed`] с путём перенесённых данных.
fn place(
    src: &Path,
    root: &Path,
    replace: bool,
) -> Result<(Manifest, Vec<Kind>, Option<PathBuf>), BackupError> {
    let mut source = Source::open(src)?;
    let m = source.manifest()?;
    let busy = nonempty(root);
    if busy && !replace {
        return Err(BackupError::NotEmpty);
    }
    let kinds: Vec<Kind> = m
        .files
        .iter()
        .map(|f| manifest::classify(&f.path))
        .collect::<Result<_, _>>()?;
    let need = need_space(&m, &kinds);
    let free = super::free_space(root, root)?;
    if need > free {
        return Err(BackupError::NoSpace { need, free });
    }
    let staging = root.join(STAGING);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    if let Err(e) = stage_files(&mut source, &m, &staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    // прежние данные — в каталог прерванной попытки (если была), иначе в новый
    let moved = busy.then(|| incomplete_moved_to(root).unwrap_or_else(|| new_pre_restore(root)));
    mark_incomplete(root, moved.as_deref())?;
    let res = (|| {
        if let Some(dst) = &moved {
            move_old(root, dst)?;
        }
        place_staged(&staging, root)
    })();
    if let Err(e) = res {
        return Err(BackupError::RestoreFailed {
            reason: e.to_string(),
            moved_to: moved,
        });
    }
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
    if zam_store::backup_flag_active(&root, zam_store::backup::backup_flag_max_age()) {
        return Err(BackupError::BackupInProgress);
    }
    let (src_path, replace) = (src.to_path_buf(), opts.replace);
    let r2 = root.clone();
    let (m, kinds, moved_to) = tokio::task::spawn_blocking(move || place(&src_path, &r2, replace))
        .await
        .map_err(|e| BackupError::Io(std::io::Error::other(e)))??;
    let failed = |e: &dyn std::fmt::Display| BackupError::RestoreFailed {
        reason: e.to_string(),
        moved_to: moved_to.clone(),
    };
    let res = finish_restore(cfg, &dd, opts, &m, &kinds).await;
    let (vault_ok, vectors) = res.map_err(|e| failed(&e))?;
    // всё на месте: каталог больше не «незавершённый»
    std::fs::remove_file(root.join(zam_store::RESTORE_INCOMPLETE_FILE)).map_err(|e| failed(&e))?;
    let schemas = m.schemas.clone();
    let dbs = kinds
        .iter()
        .filter(|k| !matches!(k, Kind::Blob { .. }))
        .count();
    let blobs = kinds.len() - dbs;
    let mut notes = Vec::new();
    if moved_to.is_some() && cfg.qdrant.mode == crate::config::QdrantMode::External {
        // внешний Qdrant restore не трогает (соединения нет): точки прежних пространств
        // остаются, но поиск их не видит — фильтр по пространствам восстановленных баз
        notes.push(
            "внешний Qdrant: точки прежних данных остаются в коллекции (поиск их не видит); \
             освободить место — пересоздать коллекцию: zam-server reindex на остановленном \
             сервере пересобирает её целиком"
                .to_string(),
        );
    }
    drop(dd);
    Ok(RestoreReport {
        dbs,
        blobs,
        schemas,
        drift: m.drift.clone(),
        vectors,
        vault_ok,
        moved_to,
        notes,
    })
}

/// Шаг 5–6: открытие баз (миграции), сессии WebUI, аудит, отметки пересборки индекса,
/// прогноз векторов и ключ Vault.
async fn finish_restore(
    cfg: &ServerConfig,
    dd: &DataDir,
    opts: &RestoreOpts,
    m: &Manifest,
    kinds: &[Kind],
) -> Result<(bool, VectorPlan), BackupError> {
    // system.db: миграции, сессии WebUI, ключ Vault
    let system = SystemStore::open(dd)?;
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
    for k in kinds {
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
        // управляемый Qdrant стирает прежнее хранилище (после --replace в нём — точки старых
        // данных); базы с отметкой воркер пересобирает из кэша без перенарезки
        zam_index::supervisor::mark_storage_reindex(&dd.qdrant(), "zam-server restore")?;
    }
    // Кэш L1 правил — память процесса: у CLI `restore` (отдельный процесс) он пуст, сброс
    // нужен восстановлению внутри процесса сервера (тесты; страница бэкапа WebUI — M7): пара
    // (id, ревизия) восстановленной базы может совпасть с другим текстом (журнал 8b, «→ 18a»)
    crate::api::index::reset_rules_l1_cache();
    let vectors = plan(cfg, opts, m, &user_dbs).await?;
    Ok((vault_ok, vectors))
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
