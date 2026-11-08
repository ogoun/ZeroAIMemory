//! `zam-server backup` и `restore`, автокопии (M6 §12б.2–§12б.4).
//!
//! **Копия** ([`backup`]) — отдельный процесс без остановки сервера:
//! 1. место в каталоге копий (базы + новые блобы + 10 %) — до записи;
//! 2. признак `.zam-backup` в каталоге данных: поток контрольных точек сервера перестаёт
//!    делать `RESTART` (`zam_store::Db`), писатель не ждёт читателя копии;
//! 3. `system.db` → `VACUUM INTO`; список пользователей — **из копии** `system.db`;
//! 4. `users/<id>.db` каждого пользователя → `VACUUM INTO`;
//! 5. блобы по таблице `blobs` копии каждой базы: жёсткая ссылка на тот же файл предыдущей
//!    копии, иначе копия с проверкой SHA-256;
//! 6. проверка копий баз (`integrity_check`, `foreign_key_check`), опись `manifest.json`;
//! 7. признак снимается при любом исходе.
//!
//! Каждая база — свой снимок; расхождения между ними (пользователь создан после снимка
//! `system.db`, база ещё не создана) — в `drift` описи. Каталог копии пишется под именем
//! `<имя>.partial` и переименовывается в `<yyyy-MM-dd_HHmmss>` только целиком готовым.
//!
//! Без сервера (`.zam.lock` свободен) копия берёт лок сама и читает базы `immutable=1`; при
//! занятом локе — `READ_WRITE` + `query_only` (может создать `-shm` закрытой базы).
//!
//! **Восстановление** — [`restore`], **расписание** — [`schedule`].

pub mod manifest;
pub mod restore;
pub mod schedule;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use zam_store::{LockMark, OpenMode, StoreError};

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
    #[error(
        "каталог данных не пуст (есть system.db или базы пользователей): укажите пустой \
         каталог или --replace (прежние данные переносятся в backups/pre-restore-…)"
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
    #[error("в каталоге данных {} нет system.db", .0.display())]
    NoData(PathBuf),
    #[error("копия повреждена: {0}")]
    BadArchive(String),
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
/// (их интервал — 1 с): `RESTART`, начатый до этого, задержал бы писателя.
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

/// Готовые копии каталога копий (с описью), от старых к новым.
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
    //! Тестовые точки копии (фича `test-hooks`): подмена свободного места и отказ жёсткой
    //! ссылки — по каталогу данных, чтобы параллельные тесты не мешали друг другу.
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static FREE: Mutex<Option<HashMap<PathBuf, u64>>> = Mutex::new(None);
    static NO_LINK: Mutex<Option<HashMap<PathBuf, ()>>> = Mutex::new(None);

    /// Свободное место для копий каталога данных `data_dir` (`None` — настоящее).
    pub fn set_free_space(data_dir: &Path, free: Option<u64>) {
        let mut g = FREE.lock().unwrap_or_else(|e| e.into_inner());
        let m = g.get_or_insert_with(HashMap::new);
        match free {
            Some(f) => m.insert(data_dir.to_path_buf(), f),
            None => m.remove(data_dir),
        };
    }

    /// Жёсткие ссылки копии каталога данных `data_dir` «не поддерживаются».
    pub fn set_link_fails(data_dir: &Path, fails: bool) {
        let mut g = NO_LINK.lock().unwrap_or_else(|e| e.into_inner());
        let m = g.get_or_insert_with(HashMap::new);
        if fails {
            m.insert(data_dir.to_path_buf(), ());
        } else {
            m.remove(data_dir);
        }
    }

    pub(super) fn free_space(data_dir: &Path) -> Option<u64> {
        FREE.lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()?
            .get(data_dir)
            .copied()
    }

    pub(super) fn link_fails(data_dir: &Path) -> bool {
        NO_LINK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|m| m.contains_key(data_dir))
    }
}

/// Свободное место тома `dir` (тестовая точка — по каталогу данных).
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

/// Нужно места: сумма и 10 % запаса.
fn with_margin(n: u64) -> u64 {
    n.saturating_add(n / 10)
}

/// Ставит признак копии (лок держится до уничтожения результата). Не вышло — WARN: копия
/// идёт и без него, но сервер может задержать запись на время копии.
fn hold_flag(data_dir: &Path) -> Option<zam_store::BackupFlagHold> {
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
    let wal = {
        let mut s = p.as_os_str().to_owned();
        s.push("-wal");
        PathBuf::from(s)
    };
    std::fs::metadata(p).map_or(0, |m| m.len()) + std::fs::metadata(wal).map_or(0, |m| m.len())
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

/// `VACUUM INTO` с переводом «каталог только для чтения» в [`BackupError::ReadOnly`].
fn snapshot(src: &Path, dst: &Path, mode: OpenMode) -> Result<(), BackupError> {
    if let Some(d) = dst.parent() {
        std::fs::create_dir_all(d)?;
    }
    match zam_store::vacuum_into(src, dst, mode) {
        Ok(()) => Ok(()),
        Err(StoreError::Sqlite(rusqlite::Error::SqliteFailure(f, _)))
            if mode == OpenMode::QueryOnly
                && matches!(
                    f.code,
                    rusqlite::ErrorCode::CannotOpen | rusqlite::ErrorCode::ReadOnly
                ) =>
        {
            Err(BackupError::ReadOnly {
                path: src.to_path_buf(),
            })
        }
        Err(e) => Err(e.into()),
    }
}

/// Копирует `src` в `dst`, считая SHA-256; `(sha256, размер)`.
pub(crate) fn copy_hashed(src: &Path, dst: &Path) -> std::io::Result<(String, u64)> {
    use sha2::Digest;
    if let Some(d) = dst.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut r = std::fs::File::open(src)?;
    let mut w = std::fs::File::create(dst)?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let k = r.read(&mut buf)?;
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
    // сервер не запущен — лок берёт копия и держит до конца: тогда `immutable=1` безопасен
    let own_lock = zam_store::try_lock_dir(root);
    let mode = if own_lock.is_some() {
        OpenMode::Immutable
    } else {
        // между проверкой метки и попыткой лока мог начаться restore
        if zam_store::lock_mark(root) == Some(LockMark::Restore) {
            return Err(BackupError::RestoreInProgress);
        }
        OpenMode::QueryOnly
    };
    let dest = opts.dir.clone().unwrap_or_else(|| cfg.backup.dir(root));
    std::fs::create_dir_all(&dest)?;
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
    // место: базы и блобы, которых нет в предыдущей копии (те связываются), плюс 10 %
    let stage_dir = stage.parent().unwrap_or(&dest).to_path_buf();
    std::fs::create_dir_all(&stage_dir)?;
    let need = with_margin(estimate(root, prev.as_deref(), opts.zip.is_some())?);
    let free = free_space(root, &stage_dir)?;
    if need > free {
        return Err(BackupError::NoSpace { need, free });
    }
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage)?;
    let flag = if mode == OpenMode::QueryOnly {
        hold_flag(root)
    } else {
        None
    };
    if flag.is_some() {
        std::thread::sleep(FLAG_SETTLE);
    }
    let res = stage_copy(root, &stage, prev.as_deref(), mode, now);
    drop(flag);
    let staged = match res {
        Ok(s) => s,
        Err((e, missing)) => {
            fail_stage(&stage, &target, missing);
            return Err(e);
        }
    };
    let dbs = staged.dbs;
    let blobs = staged.linked + staged.copied;
    let (linked, copied, mut bytes) = (staged.linked, staged.copied, staged.bytes);
    let drift = staged.manifest.drift.clone();
    let text = serde_json::to_string_pretty(&staged.manifest).map_err(std::io::Error::other)?;
    std::fs::write(stage.join(MANIFEST_FILE), &text)?;
    bytes += text.len() as u64;
    let path = match &opts.zip {
        Some(z) => {
            let r = write_zip(&stage, &staged.manifest, z);
            let _ = std::fs::remove_dir_all(&stage);
            r?;
            bytes = std::fs::metadata(z)?.len();
            z.clone()
        }
        None => {
            std::fs::rename(&stage, &target)?;
            target
        }
    };
    drop(own_lock);
    Ok(BackupReport {
        path,
        dbs,
        blobs,
        linked,
        copied,
        bytes,
        ms: t0.elapsed().as_millis() as u64,
        drift,
    })
}

/// Оценка места копии: размеры баз (с `-wal`) и блобов, которых нет в предыдущей копии
/// (для архива — все блобы).
fn estimate(root: &Path, prev: Option<&Path>, zip: bool) -> std::io::Result<u64> {
    let mut n = db_size(&root.join("system.db"));
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
    let Some(m) = missing else {
        let _ = std::fs::remove_dir_all(stage);
        return;
    };
    let _ = std::fs::remove_dir_all(stage);
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

/// Шаги 3–6 копии в каталог `stage`.
fn stage_copy(
    root: &Path,
    stage: &Path,
    prev: Option<&Path>,
    mode: OpenMode,
    now: jiff::Timestamp,
) -> Result<Staged, StageErr> {
    let e = |e: BackupError| (e, None);
    let mut files = Vec::new();
    let mut schemas = BTreeMap::new();
    let mut models = BTreeMap::new();
    let mut chunkers = BTreeMap::new();
    let mut drift = Vec::new();
    let mut bytes = 0u64;

    // 1. system.db
    let sys_copy = stage.join("system.db");
    snapshot(&root.join("system.db"), &sys_copy, mode).map_err(e)?;
    // 2. пользователи — из копии system.db
    let users: Vec<String> =
        zam_store::backup::query_strings(&sys_copy, "users", "SELECT id FROM users ORDER BY id")
            .map_err(|x| e(x.into()))?;
    let listed: BTreeSet<&str> = users.iter().map(String::as_str).collect();
    if let Ok(rd) = std::fs::read_dir(root.join("users")) {
        let mut late: Vec<String> = rd
            .flatten()
            .filter_map(|d| {
                let n = d.file_name().into_string().ok()?;
                let id = n.strip_suffix(".db")?.to_string();
                (!listed.contains(id.as_str())).then_some(id)
            })
            .collect();
        late.sort();
        for id in late {
            drift.push(format!(
                "пользователь {id}: создан после снимка system.db — в копию не входит"
            ));
        }
    }
    let mut dbs = vec![("system.db".to_string(), sys_copy)];
    // 3. базы пользователей
    for id in &users {
        let src = root.join("users").join(format!("{id}.db"));
        if !src.is_file() {
            drift.push(format!("пользователь {id}: базы ещё нет — пропуск"));
            continue;
        }
        let rel = format!("users/{id}.db");
        let dst = manifest::local(stage, &rel);
        snapshot(&src, &dst, mode).map_err(e)?;
        dbs.push((rel, dst));
    }
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
                let (got, n) = copy_hashed(&src, &dst).map_err(|x| e(x.into()))?;
                if got != sha {
                    return Err(e(BackupError::BadSum { path: brel }));
                }
                let _ = n;
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

/// Архив zip64 копии: базы и опись сжаты, блобы — `store`.
fn write_zip(stage: &Path, m: &Manifest, out: &Path) -> Result<(), BackupError> {
    use zip::CompressionMethod;
    use zip::write::SimpleFileOptions;
    let mut tmp = out.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let f = std::fs::File::create(&tmp)?;
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
    for (name, opt) in names {
        z.start_file(name, opt).map_err(zerr)?;
        let mut r = std::fs::File::open(manifest::local(stage, name))?;
        std::io::copy(&mut r, &mut z)?;
    }
    let mut w = z.finish().map_err(zerr)?;
    w.flush()?;
    w.into_inner()
        .map_err(|e| BackupError::Io(e.into_error()))?
        .sync_all()?;
    std::fs::rename(&tmp, out)?;
    Ok(())
}

/// Удаляет копии (каталоги) из `Prune` расписания.
pub fn prune(paths: &[PathBuf]) {
    for p in paths {
        match std::fs::remove_dir_all(p) {
            Ok(()) => tracing::info!(path = %p.display(), "автокопия удалена (сверх backup.keep)"),
            Err(e) => tracing::warn!(path = %p.display(), error = %e, "автокопия не удалена"),
        }
    }
}
