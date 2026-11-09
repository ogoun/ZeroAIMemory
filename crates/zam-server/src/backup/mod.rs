//! `zam-server backup` и `restore`, автокопии (M6 §12б.2–§12б.4).
//!
//! **Копия** ([`backup`]) — отдельный процесс без остановки сервера:
//! 1. место в каталоге копий (базы + новые блобы + 10 %; архив — вдвое: сборка и zip) — до
//!    записи; брошенные `*.partial` старше `backup.flag_max_hours` удаляются;
//! 2. признак `.zam-backup` в каталоге данных: поток контрольных точек сервера перестаёт
//!    делать `RESTART` (`zam_store::Db`), писатель не ждёт читателя копии;
//! 3. первый снимок `system.db` → **список пользователей**;
//! 4. `users/<id>.db` каждого пользователя из списка → `VACUUM INTO`;
//! 5. второй снимок `system.db` — он и идёт в копию: всё, на что ссылаются снимки баз
//!    (ключи Vault `user_keys` первого секрета, решения, доступы), в нём уже есть; признак
//!    снимается (блобы неизменяемы — копирование файлов писателю не мешает);
//! 6. блобы по таблице `blobs` копии каждой базы: жёсткая ссылка на тот же файл предыдущей
//!    копии, иначе копия с проверкой SHA-256 при записи;
//! 7. проверка копий баз (`integrity_check`, `foreign_key_check`), `fsync`, опись
//!    `manifest.json`, переименование `<имя>.partial` → `<yyyy-MM-dd_HHmmss>`.
//!
//! Каждая база — свой снимок; расхождения между ними (пользователь создан после первого
//! снимка `system.db`, база ещё не создана) — в `drift` описи.
//!
//! **Режим чтения баз.** При занятом локе (сервер запущен) — `READ_WRITE` без записи
//! (`OpenMode::QueryOnly`, может создать `-shm` закрытой базы). Без сервера копия берёт лок
//! сама; база без `-wal` (или с пустым) читается `immutable=1`, а база с непустым `-wal`
//! (аварийная остановка) — `QueryOnly`: SQLite восстанавливает кадры WAL, `immutable=1` их бы
//! не увидел. Каталог только для чтения с таким WAL — отказ с причиной.
//!
//! **Жёсткие ссылки** на блобы предыдущей копии не перепроверяются: блоб адресуется своей
//! SHA-256, неизменяем и был проверен при записи в ту копию; суммы всех файлов проверяет
//! `restore` до записи **[решение исполнителя 18a, ревью M3]**.
//!
//! На Linux копия получает права только владельца (каталоги 0700, файлы 0600): в ней токены
//! и шифротексты Vault.
//!
//! **Восстановление** — [`restore`], **расписание** — [`schedule`].

pub mod manifest;
pub mod restore;
pub mod schedule;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use zam_store::{BackupFlagHold, LockMark, OpenMode, StoreError};

pub use manifest::{FileEntry, MANIFEST_FILE, MANIFEST_FORMAT, Manifest};
pub use restore::{RestoreOpts, RestoreReport, VectorPlan, restore};

use crate::config::ServerConfig;

/// Ошибки копии и восстановления; все — код выхода 1.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("каталог данных занят: zam-server запущен — остановите сервер")]
    Locked,
    #[error("идёт восстановление (zam-server restore): копия сейчас невозможна")]
    RestoreInProgress,
    #[error("идёт копия (zam-server backup): дождитесь её конца")]
    BackupInProgress,
    #[error("копия в {} уже идёт (её сборку держит другой процесс zam-server backup)", .0.display())]
    StageBusy(PathBuf),
    #[error(
        "каталог данных не пуст (есть system.db, её -wal/-shm, базы или блобы): укажите \
         пустой каталог или --replace (прежние данные переносятся в backups/pre-restore-…)"
    )]
    NotEmpty,
    #[error("мало места: нужно {need} байт, свободно {free}")]
    NoSpace { need: u64, free: u64 },
    #[error("схема {file} новее программы: {have}, поддерживается до {max} — обновите zam-server")]
    SchemaTooNew { file: String, have: u32, max: u32 },
    #[error("контрольная сумма не совпала: {path}")]
    BadSum { path: String },
    #[error("нет файла блоба {sha} пользователя {user} (все отсутствующие — в описи копии)")]
    MissingBlob { user: String, sha: String },
    #[error(
        "базу {} не открыть для чтения: каталог только для чтения, а лок каталога данных занят \
         (сервер запущен) — копия невозможна",
        .path.display()
    )]
    ReadOnly { path: PathBuf },
    #[error(
        "у базы {} незавершённый WAL (сервер остановлен аварийно), а каталог только для \
         чтения: дайте права записи или запустите и штатно остановите сервер, затем повторите \
         копию",
        .path.display()
    )]
    UnrecoveredWal { path: PathBuf },
    #[error("в каталоге данных {} нет system.db", .0.display())]
    NoData(PathBuf),
    #[error("копия повреждена: {0}")]
    BadArchive(String),
    /// Сбой после начала записи в каталог данных: каталог помечен незавершённым
    /// восстановлением, сервер его не примет до повтора `restore --replace`.
    #[error(
        "восстановление прервано: {reason}. Каталог данных помечен незавершённым — повторите \
         zam-server restore <копия> --replace{}",
        .moved_to.as_ref().map(|p| format!("; прежние данные — в {}", p.display())).unwrap_or_default()
    )]
    RestoreFailed {
        reason: String,
        moved_to: Option<PathBuf>,
    },
    #[error("ошибка ввода-вывода: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Параметры копии: каталог копий вместо `backup.dir` или один архив zip.
#[derive(Debug, Clone, Default)]
pub struct BackupOpts {
    pub dir: Option<PathBuf>,
    pub zip: Option<PathBuf>,
}

/// Итог копии.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BackupReport {
    /// Каталог копии или архив.
    pub path: PathBuf,
    pub dbs: usize,
    pub blobs: usize,
    /// Блобов связано жёсткой ссылкой с предыдущей копией.
    pub linked: usize,
    /// Блобов скопировано.
    pub copied: usize,
    /// Байт в копии.
    pub bytes: u64,
    pub ms: u64,
    pub drift: Vec<String>,
}

impl BackupReport {
    /// Итог одной строкой для человека.
    pub fn line(&self) -> String {
        let mut s = format!(
            "копия {}: баз {}, блобов {} (связано {}, скопировано {}), {} байт, {} мс",
            self.path.display(),
            self.dbs,
            self.blobs,
            self.linked,
            self.copied,
            self.bytes,
            self.ms
        );
        if !self.drift.is_empty() {
            s.push_str(&format!("; расхождения снимков: {}", self.drift.join("; ")));
        }
        s
    }
}

/// Предупреждение о мастер-ключе Vault (§12б.2) — дословно, путь — файл ключа конфигурации.
pub fn master_key_warning(cfg: &ServerConfig) -> String {
    format!(
        "Мастер-ключ Vault не входит в копию: сохраните {} или ZAM_MASTER_KEY отдельно; без \
         него секреты Vault на новой машине не раскрыть",
        crate::vault::keys::master_key_path(cfg).display()
    )
}

/// Сколько ждать после записи признака, пока его заметят потоки контрольных точек сервера
/// (их интервал — 1 с). Страховка писателя проверяет и сам файл признака.
const FLAG_SETTLE: Duration = Duration::from_millis(1500);

/// Имя каталога копии: `yyyy-MM-dd_HHmmss` (UTC).
const NAME_FORMAT: &str = "%Y-%m-%d_%H%M%S";

/// Время копии по имени каталога (`yyyy-MM-dd_HHmmss`, возможно с суффиксом `-N`).
pub fn copy_time(name: &str) -> Option<jiff::Timestamp> {
    let head = name.get(..17)?;
    let rest = &name[17..];
    if !(rest.is_empty()
        || rest
            .strip_prefix('-')
            .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit())))
    {
        return None;
    }
    jiff::civil::DateTime::strptime(NAME_FORMAT, head)
        .ok()?
        .to_zoned(jiff::tz::TimeZone::UTC)
        .ok()
        .map(|z| z.timestamp())
}

/// Готовые копии каталога копий (с описью), от старых к новым. Сюда входят и копии,
/// снятые вручную `zam-server backup` в тот же каталог: ротация автокопий (`keep`) считает
/// и удаляет их наравне с автокопиями.
pub fn list_copies(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<(jiff::Timestamp, String, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let t = copy_time(&name)?;
            let p = e.path();
            p.join(MANIFEST_FILE).is_file().then_some((t, name, p))
        })
        .collect();
    v.sort();
    v.into_iter().map(|(_, _, p)| p).collect()
}

/// Время последней готовой копии каталога копий.
pub fn last_copy_at(dir: &Path) -> Option<jiff::Timestamp> {
    list_copies(dir)
        .last()
        .and_then(|p| p.file_name()?.to_str().and_then(copy_time))
}

/// Поле `backup` ответа `GET /index/status` (§12б.4): время последней копии, расписание и
/// предупреждение `no_recent_backup` (копии нет или она старше `backup.warn_days`).
pub fn status_json(cfg: &ServerConfig, now: jiff::Timestamp) -> serde_json::Value {
    let last = last_copy_at(&cfg.backup.dir(&cfg.data_dir));
    let limit = jiff::SignedDuration::from_hours(24 * i64::from(cfg.backup.warn_days));
    let stale = last.is_none_or(|t| now.duration_since(t) > limit);
    serde_json::json!({
        "last_at": last.map(|t| t.to_string()),
        "schedule": cfg.backup.schedule,
        "warning": stale.then_some("no_recent_backup"),
    })
}

#[cfg(feature = "test-hooks")]
pub mod test_hooks {
    //! Тестовые точки копии и восстановления (фича `test-hooks`) — по каталогу данных, чтобы
    //! параллельные тесты не мешали друг другу: подмена свободного места, отказ жёсткой
    //! ссылки, действие после первого снимка `system.db`, сбой в точке `zip` (запись
    //! архива) или `restore_place` (середина переноса файлов в каталог данных).
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    type Hook = Arc<dyn Fn() + Send + Sync>;

    static FREE: Mutex<Option<HashMap<PathBuf, u64>>> = Mutex::new(None);
    static NO_LINK: Mutex<Option<HashMap<PathBuf, ()>>> = Mutex::new(None);
    static AFTER_FIRST: Mutex<Option<HashMap<PathBuf, Hook>>> = Mutex::new(None);
    static FAIL: Mutex<Option<HashMap<PathBuf, &'static str>>> = Mutex::new(None);

    fn set<T>(m: &Mutex<Option<HashMap<PathBuf, T>>>, k: &Path, v: Option<T>) {
        let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
        let h = g.get_or_insert_with(HashMap::new);
        match v {
            Some(v) => h.insert(k.to_path_buf(), v),
            None => h.remove(k),
        };
    }

    fn get<T: Clone>(m: &Mutex<Option<HashMap<PathBuf, T>>>, k: &Path) -> Option<T> {
        m.lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()?
            .get(k)
            .cloned()
    }

    /// Свободное место для копий и импорта каталога данных `data_dir` (`None` — настоящее).
    pub fn set_free_space(data_dir: &Path, free: Option<u64>) {
        set(&FREE, data_dir, free);
    }

    /// Жёсткие ссылки копии каталога данных `data_dir` «не поддерживаются».
    pub fn set_link_fails(data_dir: &Path, fails: bool) {
        set(&NO_LINK, data_dir, fails.then_some(()));
    }

    /// Действие после первого снимка `system.db` копии каталога `data_dir` (до баз).
    pub fn set_after_first_snapshot(data_dir: &Path, f: Option<Hook>) {
        set(&AFTER_FIRST, data_dir, f);
    }

    /// Сбой в точке `point` (`zip`, `restore_place`) для каталога данных `data_dir`.
    pub fn set_fail_point(data_dir: &Path, point: Option<&'static str>) {
        set(&FAIL, data_dir, point);
    }

    pub(super) fn free_space(data_dir: &Path) -> Option<u64> {
        get(&FREE, data_dir)
    }

    pub(super) fn link_fails(data_dir: &Path) -> bool {
        get(&NO_LINK, data_dir).is_some()
    }

    pub(super) fn after_first(data_dir: &Path) {
        if let Some(f) = get(&AFTER_FIRST, data_dir) {
            f();
        }
    }

    pub(super) fn fails(data_dir: &Path, point: &str) -> bool {
        get(&FAIL, data_dir) == Some(point)
    }
}

/// Свободное место тома `dir` (копии, импорт; тестовая точка — по каталогу данных).
pub(crate) fn free_space(data_dir: &Path, dir: &Path) -> std::io::Result<u64> {
    #[cfg(feature = "test-hooks")]
    if let Some(f) = test_hooks::free_space(data_dir) {
        return Ok(f);
    }
    let _ = data_dir;
    fs4::available_space(dir)
}

fn link_fails(data_dir: &Path) -> bool {
    #[cfg(feature = "test-hooks")]
    if test_hooks::link_fails(data_dir) {
        return true;
    }
    let _ = data_dir;
    false
}

/// Тестовая точка сбоя `point` (без фичи `test-hooks` — никогда).
pub(crate) fn fail_point(data_dir: &Path, point: &str) -> Result<(), BackupError> {
    #[cfg(feature = "test-hooks")]
    if test_hooks::fails(data_dir, point) {
        return Err(BackupError::Io(std::io::Error::other(format!(
            "тестовый сбой в точке {point}"
        ))));
    }
    let _ = (data_dir, point);
    Ok(())
}

fn after_first_snapshot(data_dir: &Path) {
    #[cfg(feature = "test-hooks")]
    test_hooks::after_first(data_dir);
    let _ = data_dir;
}

/// Нужно места: сумма и 10 % запаса.
fn with_margin(n: u64) -> u64 {
    n.saturating_add(n / 10)
}

/// fsync каталога: имена новых файлов переживают сбой питания (Linux). На Windows каталог так
/// не синхронизируется — NTFS журналирует метаданные сама.
pub(crate) fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    std::fs::File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// fsync файла.
fn sync_file(p: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new().write(true).open(p)?.sync_all()
}

/// Linux: права только владельца на дерево копии (каталоги 0700, файлы 0600).
pub(crate) fn restrict_tree(p: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let md = std::fs::metadata(p)?;
        if md.is_dir() {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700))?;
            for e in std::fs::read_dir(p)? {
                restrict_tree(&e?.path())?;
            }
        } else {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    #[cfg(not(unix))]
    let _ = p;
    Ok(())
}

/// Ставит признак копии (лок держится до уничтожения результата). Не вышло — WARN: копия
/// идёт и без него, но сервер может задержать запись на время копии.
fn hold_flag(data_dir: &Path) -> Option<BackupFlagHold> {
    match zam_store::hold_backup_flag(data_dir) {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::warn!(
                path = %zam_store::backup_flag_path(data_dir).display(),
                error = %e,
                "признак копии не записан: сервер может задержать запись на время копии"
            );
            None
        }
    }
}

/// Размер файла и его `-wal` (0 — нет).
fn db_size(p: &Path) -> u64 {
    std::fs::metadata(p).map_or(0, |m| m.len()) + zam_store::backup::wal_size(p)
}

/// Уникальное имя каталога копии в `dir` на момент `now`.
fn copy_name(dir: &Path, now: jiff::Timestamp) -> String {
    let base = now
        .to_zoned(jiff::tz::TimeZone::UTC)
        .strftime(NAME_FORMAT)
        .to_string();
    let free = |n: &str| !dir.join(n).exists() && !dir.join(format!("{n}.partial")).exists();
    if free(&base) {
        return base;
    }
    (2..)
        .map(|k| format!("{base}-{k}"))
        .find(|n| free(n))
        .expect("бесконечный ряд")
}

/// Файл-лок сборки копии внутри `<сборка>.partial/`: пока `backup` жив, он держит на нём
/// исключительный лок ОС (снимается ОС при любой смерти процесса).
const STAGE_LOCK: &str = ".zam-building";

/// Лок сборки копии; при уничтожении лок снимается и файл удаляется (до переименования или
/// удаления сборки — в копию он не попадает).
struct StageLock {
    path: PathBuf,
    file: Option<std::fs::File>,
}

impl StageLock {
    fn take(stage: &Path) -> std::io::Result<StageLock> {
        let path = stage.join(STAGE_LOCK);
        let file = std::fs::File::create(&path)?;
        fs4::FileExt::lock(&file)?;
        Ok(StageLock {
            path,
            file: Some(file),
        })
    }
}

impl Drop for StageLock {
    fn drop(&mut self) {
        if let Some(f) = self.file.take() {
            let _ = fs4::FileExt::unlock(&f);
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Сборку `stage` сейчас собирает живой `backup`: лок её файла-лока занят.
fn stage_alive(stage: &Path) -> Option<bool> {
    let f = std::fs::File::open(stage.join(STAGE_LOCK)).ok()?;
    Some(match fs4::FileExt::try_lock(&f) {
        Ok(()) => {
            let _ = fs4::FileExt::unlock(&f);
            false
        }
        Err(_) => true,
    })
}

/// Удаляет брошенную сборку `p` (каталог `*.partial` или `<архив>.tmp`). Решение — по локу
/// сборки ([`STAGE_LOCK`]): лок занят — сборку собирает живая копия, свободен — копия умерла.
/// Сборка без файла-лока (не успел появиться, копия старой версии) — только старше `max_age`
/// **[решение исполнителя, ревью P4]**. `lock_of` — каталог сборки, чей лок решает
/// (у `<архив>.tmp` — `<архив>.partial`).
fn remove_stale(p: &Path, lock_of: &Path, max_age: Duration) {
    let old = || {
        std::fs::symlink_metadata(p)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|a| a > max_age)
    };
    let abandoned = match stage_alive(lock_of) {
        Some(alive) => !alive,
        None => old(),
    };
    if !abandoned {
        return;
    }
    let r = if p.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    };
    match r {
        Ok(()) => tracing::info!(path = %p.display(), "удалена брошенная сборка копии"),
        Err(err) => {
            tracing::warn!(path = %p.display(), error = %err, "брошенная сборка копии не удалена")
        }
    }
}

/// Брошенные сборки каталога копий `dir`: только свои — `yyyy-MM-dd_HHmmss[-N].partial`;
/// прочие файлы и каталоги (в том числе чужие `*.partial`) не трогаются.
fn clean_partials(dir: &Path, max_age: Duration) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name
            .strip_suffix(".partial")
            .is_some_and(|n| copy_time(n).is_some())
        {
            let p = e.path();
            remove_stale(&p, &p, max_age);
        }
    }
}

/// Брошенная сборка архива `zip`: только `<архив>.partial` и `<архив>.tmp` (любое расширение
/// архива) — каталог архива может быть чужим.
fn clean_zip_leftovers(zip: &Path, max_age: Duration) {
    let with = |suffix: &str| {
        let mut s = zip.as_os_str().to_owned();
        s.push(suffix);
        PathBuf::from(s)
    };
    let (partial, tmp) = (with(".partial"), with(".tmp"));
    // `.tmp` пишется, пока сборка `.partial` жива и залочена; сборки нет — `.tmp` брошен
    if tmp.exists() {
        if partial.exists() {
            remove_stale(&tmp, &partial, max_age);
        } else {
            remove_stale(&tmp, &tmp, Duration::ZERO);
        }
    }
    if partial.exists() {
        remove_stale(&partial, &partial, max_age);
    }
}

/// `rename` с повтором на Windows ([`zam_core::fsx::rename_retry`]); каждая пауза — WARN с
/// номером попытки и кодом.
pub(crate) fn rename_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    zam_core::fsx::rename_retry(from, to, |attempt, code, pause| {
        tracing::warn!(
            from = %from.display(),
            to = %to.display(),
            attempt,
            code,
            pause_ms = pause.as_millis() as u64,
            "переименование не удалось (файл занят), повтор"
        );
    })
}

/// Как читать базу `src`: при чужом локе — `QueryOnly`; под своим — `immutable=1`, если WAL
/// пуст, иначе `QueryOnly` (восстановление кадров WAL).
fn mode_for(src: &Path, own_lock: bool) -> OpenMode {
    if own_lock && zam_store::backup::wal_size(src) == 0 {
        OpenMode::Immutable
    } else {
        OpenMode::QueryOnly
    }
}

/// `VACUUM INTO` с переводом «каталог только для чтения» в понятную ошибку; копия — `fsync`.
fn snapshot(src: &Path, dst: &Path, own_lock: bool) -> Result<(), BackupError> {
    if let Some(d) = dst.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mode = mode_for(src, own_lock);
    match zam_store::vacuum_into(src, dst, mode) {
        Ok(()) => {}
        Err(StoreError::Sqlite(rusqlite::Error::SqliteFailure(f, _)))
            if mode == OpenMode::QueryOnly
                && matches!(
                    f.code,
                    rusqlite::ErrorCode::CannotOpen | rusqlite::ErrorCode::ReadOnly
                ) =>
        {
            let path = src.to_path_buf();
            return Err(if own_lock {
                BackupError::UnrecoveredWal { path }
            } else {
                BackupError::ReadOnly { path }
            });
        }
        Err(e) => return Err(e.into()),
    }
    sync_file(dst)?;
    Ok(())
}

/// Копирует `src` в `dst`, считая SHA-256 при записи (одно чтение); `(sha256, размер)`.
pub(crate) fn copy_hashed(src: &mut dyn Read, dst: &Path) -> std::io::Result<(String, u64)> {
    use sha2::Digest;
    if let Some(d) = dst.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut w = std::fs::File::create(dst)?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let k = src.read(&mut buf)?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        w.write_all(&buf[..k])?;
        n += k as u64;
    }
    w.sync_all()?;
    Ok((manifest::hex(&h.finalize()), n))
}

/// Собранная копия в каталоге `stage` (до описи и переименования).
struct Staged {
    manifest: Manifest,
    dbs: usize,
    linked: usize,
    copied: usize,
    bytes: u64,
}

/// Снимает копию каталога данных `cfg.data_dir` (блокирующая; вызывать вне потоков runtime).
pub fn backup(cfg: &ServerConfig, opts: &BackupOpts) -> Result<BackupReport, BackupError> {
    let t0 = Instant::now();
    let root = cfg.data_dir.as_path();
    if !root.join("system.db").is_file() {
        return Err(BackupError::NoData(root.to_path_buf()));
    }
    if zam_store::lock_mark(root) == Some(LockMark::Restore) {
        return Err(BackupError::RestoreInProgress);
    }
    if zam_store::restore_incomplete(root) {
        return Err(StoreError::RestoreIncomplete(root.to_path_buf()).into());
    }
    // сервер не запущен — лок берёт копия и держит до конца
    let own_lock = zam_store::try_lock_dir(root);
    if own_lock.is_none() && zam_store::lock_mark(root) == Some(LockMark::Restore) {
        // между проверкой метки и попыткой лока мог начаться restore
        return Err(BackupError::RestoreInProgress);
    }
    let own = own_lock.is_some();
    let dest = opts.dir.clone().unwrap_or_else(|| cfg.backup.dir(root));
    std::fs::create_dir_all(&dest)?;
    let max_age = Duration::from_secs(u64::from(cfg.backup.flag_max_hours) * 3600);
    clean_partials(&dest, max_age);
    let prev = list_copies(&dest).pop();
    let now = zam_store::time::now();
    let (stage, target) = match &opts.zip {
        Some(z) => {
            let mut s = z.as_os_str().to_owned();
            s.push(".partial");
            (PathBuf::from(s), z.clone())
        }
        None => {
            let name = copy_name(&dest, now);
            (dest.join(format!("{name}.partial")), dest.join(name))
        }
    };
    let stage_dir = stage.parent().unwrap_or(&dest).to_path_buf();
    std::fs::create_dir_all(&stage_dir)?;
    if let Some(z) = &opts.zip {
        clean_zip_leftovers(z, max_age);
    }
    // место: базы и блобы, которых нет в предыдущей копии (те связываются), плюс 10 %;
    // архив собирается рядом и пишется целиком — вдвое
    let est = estimate(root, prev.as_deref(), opts.zip.is_some())?;
    let need = with_margin(if opts.zip.is_some() {
        est.saturating_mul(2)
    } else {
        est
    });
    let free = free_space(root, &stage_dir)?;
    if need > free {
        return Err(BackupError::NoSpace { need, free });
    }
    // сборку того же архива держит живая копия — её не трогаем
    if stage_alive(&stage) == Some(true) {
        return Err(BackupError::StageBusy(target.clone()));
    }
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage)?;
    let stage_lock = StageLock::take(&stage)?;
    // при своём локе сервера нет — признак не нужен
    let flag = if own { None } else { hold_flag(root) };
    if flag.is_some() {
        std::thread::sleep(FLAG_SETTLE);
    }
    let res = stage_copy(root, &stage, prev.as_deref(), own, flag, now);
    let staged = match res {
        Ok(s) => s,
        Err((e, missing)) => {
            drop(stage_lock);
            fail_stage(&stage, &target, missing);
            return Err(e);
        }
    };
    let finished = finish(
        &staged,
        &stage,
        &target,
        opts.zip.as_deref(),
        root,
        stage_lock,
    );
    let (path, bytes) = match finished {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&stage);
            return Err(e);
        }
    };
    drop(own_lock);
    Ok(BackupReport {
        path,
        dbs: staged.dbs,
        blobs: staged.linked + staged.copied,
        linked: staged.linked,
        copied: staged.copied,
        bytes,
        ms: t0.elapsed().as_millis() as u64,
        drift: staged.manifest.drift.clone(),
    })
}

/// Опись (с `fsync`) и публикация копии: переименование каталога или архив.
fn finish(
    staged: &Staged,
    stage: &Path,
    target: &Path,
    zip: Option<&Path>,
    root: &Path,
    lock: StageLock,
) -> Result<(PathBuf, u64), BackupError> {
    let text = serde_json::to_string_pretty(&staged.manifest).map_err(std::io::Error::other)?;
    {
        let mut f = std::fs::File::create(stage.join(MANIFEST_FILE))?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    sync_dir(stage)?;
    match zip {
        Some(z) => {
            // сборка залочена, пока пишется `<архив>.tmp`: чистильщик его не тронет
            let r = write_zip(stage, &staged.manifest, z, root);
            drop(lock);
            let _ = std::fs::remove_dir_all(stage);
            r?;
            restrict_tree(z)?;
            Ok((z.to_path_buf(), std::fs::metadata(z)?.len()))
        }
        None => {
            // файл-лок — не часть копии: снимается до переименования
            drop(lock);
            restrict_tree(stage)?;
            rename_retry(stage, target)?;
            if let Some(d) = target.parent() {
                sync_dir(d)?;
            }
            Ok((target.to_path_buf(), staged.bytes + text.len() as u64))
        }
    }
}

/// Оценка места копии: размеры баз (с `-wal`) и блобов, которых нет в предыдущей копии
/// (для архива — все блобы).
fn estimate(root: &Path, prev: Option<&Path>, zip: bool) -> std::io::Result<u64> {
    // system.db снимается дважды (первый снимок удаляется после второго)
    let mut n = db_size(&root.join("system.db")).saturating_mul(2);
    if let Ok(rd) = std::fs::read_dir(root.join("users")) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "db") {
                n += db_size(&p);
            }
        }
    }
    fn walk(dir: &Path, rel: &Path, prev: Option<&Path>, n: &mut u64) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let r = rel.join(e.file_name());
            if p.is_dir() {
                walk(&p, &r, prev, n);
            } else if prev.is_none_or(|pv| !pv.join("blobs").join(&r).is_file()) {
                *n += e.metadata().map_or(0, |m| m.len());
            }
        }
    }
    walk(
        &root.join("blobs"),
        Path::new(""),
        if zip { None } else { prev },
        &mut n,
    );
    Ok(n)
}

/// Неудавшаяся копия: при отсутствующих блобах остаётся только опись с их списком
/// (`<имя>.failed/manifest.json`), иначе каталог удаляется.
fn fail_stage(stage: &Path, target: &Path, missing: Option<Box<Manifest>>) {
    let _ = std::fs::remove_dir_all(stage);
    let Some(m) = missing else {
        return;
    };
    let mut failed = target.as_os_str().to_owned();
    failed.push(".failed");
    let failed = PathBuf::from(failed);
    let _ = std::fs::remove_dir_all(&failed);
    if std::fs::create_dir_all(&failed).is_ok()
        && let Ok(t) = serde_json::to_string_pretty(&m)
    {
        let _ = std::fs::write(failed.join(MANIFEST_FILE), t);
    }
}

type StageErr = (BackupError, Option<Box<Manifest>>);

fn file_entry(stage: &Path, rel: &str) -> Result<FileEntry, BackupError> {
    let (sha256, size) = manifest::sha256_file(&manifest::local(stage, rel))?;
    Ok(FileEntry {
        path: rel.to_string(),
        size,
        sha256,
    })
}

/// Шаги 3–6 копии в каталог `stage`; признак `flag` снимается после последней базы.
fn stage_copy(
    root: &Path,
    stage: &Path,
    prev: Option<&Path>,
    own_lock: bool,
    flag: Option<BackupFlagHold>,
    now: jiff::Timestamp,
) -> Result<Staged, StageErr> {
    let e = |e: BackupError| (e, None);
    let mut files = Vec::new();
    let mut schemas = BTreeMap::new();
    let mut models = BTreeMap::new();
    let mut chunkers = BTreeMap::new();
    let mut drift = Vec::new();
    let mut bytes = 0u64;

    // 1. первый снимок system.db — только список пользователей
    let first = stage.join(".system.first.db");
    snapshot(&root.join("system.db"), &first, own_lock).map_err(e)?;
    let users: Vec<String> =
        zam_store::backup::query_strings(&first, "users", "SELECT id FROM users ORDER BY id")
            .map_err(|x| e(x.into()))?;
    let _ = std::fs::remove_file(&first);
    after_first_snapshot(root);
    let listed: BTreeSet<&str> = users.iter().map(String::as_str).collect();
    // 2. базы пользователей списка
    let mut dbs = Vec::new();
    for id in &users {
        let src = root.join("users").join(format!("{id}.db"));
        if !src.is_file() {
            drift.push(format!("пользователь {id}: базы ещё нет — пропуск"));
            continue;
        }
        let rel = format!("users/{id}.db");
        let dst = manifest::local(stage, &rel);
        snapshot(&src, &dst, own_lock).map_err(e)?;
        dbs.push((rel, dst));
    }
    // 3. второй снимок system.db — в копию: в нём всё, на что ссылаются снимки баз
    let sys_copy = stage.join("system.db");
    snapshot(&root.join("system.db"), &sys_copy, own_lock).map_err(e)?;
    // базы сняты — признак больше не нужен (блобы неизменяемы)
    drop(flag);
    // пользователь, созданный между снимками system.db, в копии system.db есть, а его данных
    // (базы, блобов) в копии нет; база без записи в копии system.db — не входит совсем
    let in_sys: BTreeSet<String> =
        zam_store::backup::query_strings(&sys_copy, "users", "SELECT id FROM users ORDER BY id")
            .map_err(|x| e(x.into()))?
            .into_iter()
            .collect();
    for id in in_sys.iter().filter(|id| !listed.contains(id.as_str())) {
        drift.push(format!(
            "пользователь {id}: создан во время копии — учётная запись в копии system.db есть, \
             его базы и блобов в копии нет"
        ));
    }
    if let Ok(rd) = std::fs::read_dir(root.join("users")) {
        let mut orphans: Vec<String> = rd
            .flatten()
            .filter_map(|d| {
                let n = d.file_name().into_string().ok()?;
                let id = n.strip_suffix(".db")?.to_string();
                (!listed.contains(id.as_str()) && !in_sys.contains(&id)).then_some(id)
            })
            .collect();
        orphans.sort();
        for id in orphans {
            drift.push(format!(
                "база users/{id}.db: пользователя нет в system.db — в копию не входит"
            ));
        }
    }
    dbs.insert(0, ("system.db".to_string(), sys_copy));
    // проверка копий, версии схем, модели и нарезка
    for (rel, p) in &dbs {
        let (ver, ok) = zam_store::integrity_ok(p).map_err(|x| e(x.into()))?;
        if !ok {
            return Err(e(BackupError::BadArchive(format!(
                "копия {rel} не прошла integrity_check/foreign_key_check"
            ))));
        }
        schemas.insert(rel.clone(), ver);
        if rel != "system.db" {
            let m = zam_store::backup::query_strings(
                p,
                "embeddings",
                "SELECT DISTINCT model_id FROM embeddings ORDER BY model_id",
            )
            .map_err(|x| e(x.into()))?;
            models.insert(rel.clone(), m);
            chunkers.insert(
                rel.clone(),
                zam_store::backup::chunkers_of(p).map_err(|x| e(x.into()))?,
            );
        }
        let fe = file_entry(stage, rel).map_err(e)?;
        bytes += fe.size;
        files.push(fe);
    }
    // 4. блобы — по таблице `blobs` копии каждой базы
    let no_link = link_fails(root);
    let (mut linked, mut copied) = (0usize, 0usize);
    let mut missing = Vec::new();
    for (rel, p) in dbs.iter().filter(|(r, _)| r != "system.db") {
        let user = rel
            .strip_prefix("users/")
            .and_then(|r| r.strip_suffix(".db"))
            .unwrap_or_default()
            .to_string();
        let list = zam_store::backup::blob_list(p).map_err(|x| e(x.into()))?;
        for (sha, size) in list {
            let brel = format!("blobs/{user}/{}/{sha}", sha.get(..2).unwrap_or("00"));
            let src = manifest::local(root, &brel);
            if !src.is_file() {
                missing.push((user.clone(), sha));
                continue;
            }
            let dst = manifest::local(stage, &brel);
            let prev_file = prev.map(|pv| manifest::local(pv, &brel));
            let mut done = false;
            if let Some(pf) =
                prev_file.filter(|pf| std::fs::metadata(pf).is_ok_and(|m| m.len() == size))
                && !no_link
            {
                if let Some(d) = dst.parent() {
                    std::fs::create_dir_all(d).map_err(|x| e(x.into()))?;
                }
                match std::fs::hard_link(&pf, &dst) {
                    Ok(()) => {
                        linked += 1;
                        done = true;
                    }
                    Err(err) => tracing::debug!(error = %err, "жёсткая ссылка не создана — копия"),
                }
            }
            if !done {
                let mut r = std::fs::File::open(&src).map_err(|x| e(x.into()))?;
                let (got, _) = copy_hashed(&mut r, &dst).map_err(|x| e(x.into()))?;
                if got != sha {
                    return Err(e(BackupError::BadSum { path: brel }));
                }
                copied += 1;
            }
            bytes += size;
            files.push(FileEntry {
                path: brel,
                size,
                sha256: sha,
            });
        }
    }
    let manifest = Manifest {
        format: MANIFEST_FORMAT,
        server_version: env!("CARGO_PKG_VERSION").to_string(),
        created_at: now.to_string(),
        source_machine: manifest::machine_name(),
        schemas,
        files,
        embedding_models: models,
        chunkers,
        drift,
        master_key_included: false,
        missing_blobs: missing.iter().map(|(u, s)| format!("{u}/{s}")).collect(),
    };
    if let Some((user, sha)) = missing.first().cloned() {
        return Err((
            BackupError::MissingBlob { user, sha },
            Some(Box::new(manifest)),
        ));
    }
    Ok(Staged {
        dbs: dbs.len(),
        manifest,
        linked,
        copied,
        bytes,
    })
}

/// Архив zip64 копии: базы и опись сжаты, блобы — `store`. Пишется в `<архив>.tmp`, который
/// удаляется при любой ошибке; готовый — `fsync` и переименование.
fn write_zip(stage: &Path, m: &Manifest, out: &Path, root: &Path) -> Result<(), BackupError> {
    let mut tmp = out.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let r = write_zip_to(stage, m, &tmp, root).and_then(|()| {
        rename_retry(&tmp, out)?;
        if let Some(d) = out.parent() {
            sync_dir(d)?;
        }
        Ok(())
    });
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

fn write_zip_to(stage: &Path, m: &Manifest, tmp: &Path, root: &Path) -> Result<(), BackupError> {
    use zip::CompressionMethod;
    use zip::write::SimpleFileOptions;
    let f = std::fs::File::create(tmp)?;
    let mut z = zip::ZipWriter::new(std::io::BufWriter::new(f));
    let zerr = |e: zip::result::ZipError| BackupError::Io(std::io::Error::other(e));
    let deflate = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(true);
    let store = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .large_file(true);
    let mut names: Vec<(&str, SimpleFileOptions)> = m
        .files
        .iter()
        .map(|f| {
            let opt = if f.path.starts_with("blobs/") {
                store
            } else {
                deflate
            };
            (f.path.as_str(), opt)
        })
        .collect();
    names.push((MANIFEST_FILE, deflate));
    for (i, (name, opt)) in names.into_iter().enumerate() {
        if i == 1 {
            fail_point(root, "zip")?;
        }
        z.start_file(name, opt).map_err(zerr)?;
        let mut r = std::fs::File::open(manifest::local(stage, name))?;
        std::io::copy(&mut r, &mut z)?;
    }
    let mut w = z.finish().map_err(zerr)?;
    w.flush()?;
    w.into_inner()
        .map_err(|e| BackupError::Io(e.into_error()))?
        .sync_all()?;
    Ok(())
}

/// Удаляет копии (каталоги) из `Prune` расписания.
pub fn prune(paths: &[PathBuf]) {
    for p in paths {
        match std::fs::remove_dir_all(p) {
            Ok(()) => tracing::info!(path = %p.display(), "копия удалена (сверх backup.keep)"),
            Err(e) => tracing::warn!(path = %p.display(), error = %e, "копия не удалена"),
        }
    }
}
