//! Сборщик мусора блобов (M7.4 §7.4, ТЗ §6.5) и замок блобов пользователя.
//!
//! **Что удаляется** (у каждого пользователя — свой корень `blobs/<user>/` и своя база):
//! - строка `blobs` с `refcount = 0`, файл которой не менялся `days` суток, — строка и файл;
//! - файл без строки (откат `create_report`, отклонённый multipart, прерванный импорт) того же
//!   возраста;
//! - `.incoming/*.part` старше суток ([`PART_MAX_AGE`]);
//! - строка с `refcount = 0` без файла (после восстановления копии, где таких файлов нет).
//!
//! Не трогаются строки с `refcount > 0` (файлы заметок и отчётов, в том числе в корзине), блоб, на
//! который ещё ссылаются `attachments`, `entry_attachments` или живой документ `attachment_docs`
//! (даже при неверном `refcount`), кэш `blob_extract` и `previews/`. Документы `attachment_docs`
//! открепленных пар (`deleted_at` задан, чанков нет) удаляются вместе со строкой блоба: иначе
//! внешний ключ не дал бы её удалить.
//!
//! **Возраст — `mtime` файла** (решение координатора 103): отвязка последней ссылки
//! ([`crate::attachments::detach`]) и запись уже существующего содержимого
//! ([`crate::BlobStore::put`]) ставят `mtime` на «сейчас» часов хранилища. Блоб, к которому
//! обращались за `days` суток, не удаляется.
//!
//! **Замок блобов пользователя** ([`BlobGate`], решение 104) — в процессе сервера, по корню
//! `blobs/<user>/`: запись держит разделяемый замок ([`BlobShare`]) от записи файла до фиксации
//! строки (`POST /blobs` → `attach`, отчёт → `create_report`, импорт — от распаковки
//! [`crate::import::extract_blobs`] до конца `apply_plan`); сборщик берёт исключительный без
//! ожидания ([`BlobGate::try_exclusive`]) — занят: пользователь пропускается до следующего прохода.
//!
//! **Удаление двухфазное** (решение 105): транзакция `IMMEDIATE` писателя базы — повторная
//! проверка условия, переименование файла в `<root>/.gc/<sha256>`, `DELETE` строки, `COMMIT`;
//! затем файл удаляется из `.gc/`. Сбой между ними: строка жива — файл возвращается из `.gc/`
//! следующим проходом (или `zam-server doctor --fix`), строки нет — файл удаляется. Каждый
//! проход начинает и заканчивает разбором `.gc/` ([`recover`]).
//!
//! [`plan`] — только чтение (тот же отбор считает `doctor`); [`sweep`] — проход: разбор `.gc/`,
//! отбор, удаление под исключительным замком, снова разбор `.gc/`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use jiff::{SignedDuration, Timestamp};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

use crate::blobs::{INCOMING, is_sha256};
use crate::{Db, StoreError};

/// Каталог второй фазы удаления внутри корня блобов пользователя.
pub const GC_DIR: &str = ".gc";
/// Возраст брошенного временного файла загрузки `.incoming/*.part`, после которого он удаляется.
pub const PART_MAX_AGE: SignedDuration = SignedDuration::from_hours(24);

// ---------- замок блобов пользователя ----------

/// Замки процесса по корню блобов (абсолютный путь). Значение — слабая ссылка: замок без
/// держателей никого не защищает, его можно создать заново.
static GATES: LazyLock<Mutex<HashMap<PathBuf, Weak<RwLock<()>>>>> = LazyLock::new(Default::default);

/// Записей реестра, после которых он чистится от мёртвых.
const GATES_PRUNE_AT: usize = 64;

/// Замок блобов одного пользователя (корень `blobs/<user>/`), общий для процесса: один и тот же
/// корень — один замок, как бы ни был получен [`crate::BlobStore`].
#[derive(Debug, Clone)]
pub struct BlobGate {
    lock: Arc<RwLock<()>>,
}

/// Разделяемый замок записи блобов: пока жив, сборщик этого пользователя не работает.
#[derive(Debug)]
#[must_use = "замок держится, пока жив результат"]
pub struct BlobShare {
    _guard: OwnedRwLockReadGuard<()>,
}

/// Исключительный замок сборщика.
#[derive(Debug)]
#[must_use = "замок держится, пока жив результат"]
pub struct BlobExclusive {
    _guard: OwnedRwLockWriteGuard<()>,
}

impl BlobGate {
    /// Замок корня `root` (`<blobs_dir>/<user>`); путь сводится к абсолютному без обращения к
    /// диску — `data_dir` конфигурации и [`crate::DataDir::blobs_dir`] дают один ключ.
    pub fn of(root: &Path) -> BlobGate {
        let key = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
        let mut g = GATES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(lock) = g.get(&key).and_then(Weak::upgrade) {
            return BlobGate { lock };
        }
        if g.len() >= GATES_PRUNE_AT {
            g.retain(|_, w| w.strong_count() > 0);
        }
        let lock: Arc<RwLock<()>> = Arc::default();
        g.insert(key, Arc::downgrade(&lock));
        BlobGate { lock }
    }

    /// Разделяемый замок записи (ждёт, пока идёт удаление сборщика).
    pub async fn share(&self) -> BlobShare {
        BlobShare {
            _guard: self.lock.clone().read_owned().await,
        }
    }

    /// Исключительный замок сборщика сразу или `None` (идёт запись блобов пользователя).
    pub fn try_exclusive(&self) -> Option<BlobExclusive> {
        self.lock
            .clone()
            .try_write_owned()
            .ok()
            .map(|g| BlobExclusive { _guard: g })
    }
}

// ---------- отбор ----------

/// Блоб к удалению: sha256 и размер файла.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GcItem {
    pub sha256: String,
    pub bytes: u64,
}

/// Строка с `refcount > 0` без файла (ошибка данных: отдача вложения — 404).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MissingBlob {
    pub sha256: String,
    /// Файл лежит в `.gc/` (сбой посреди удаления): его вернёт следующий проход или
    /// `doctor --fix`.
    pub in_gc: bool,
}

/// Файл в `.gc/`, оставшийся после сбоя.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GcLeftover {
    pub sha256: String,
    /// Строка блоба жива — файл вернётся на место; иначе — удалится.
    pub restore: bool,
}

/// Итог отбора ([`plan`]): что удалит проход и что видно при проверке (`doctor`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GcPlan {
    /// Строки `refcount = 0` с файлом старше `days` суток — строка и файл.
    pub rows: Vec<GcItem>,
    /// Файлы без строки старше `days` суток.
    pub orphans: Vec<GcItem>,
    /// Строки `refcount = 0` без файла — строка.
    pub dangling: Vec<String>,
    /// `.incoming/*.part` старше суток: имя и размер.
    pub parts: Vec<GcItem>,
    /// Строки `refcount > 0` без файла.
    pub missing: Vec<MissingBlob>,
    /// Файлы `.gc/`.
    pub leftovers: Vec<GcLeftover>,
    /// Все файлы `.incoming/`: число и байты.
    pub incoming_files: u64,
    pub incoming_bytes: u64,
}

impl GcPlan {
    /// Блобов к удалению сборщиком (строки и файлы без `.incoming`).
    pub fn blobs(&self) -> usize {
        self.rows.len() + self.orphans.len() + self.dangling.len()
    }

    /// Байт к удалению сборщиком (файлы блобов и `.part`).
    pub fn bytes(&self) -> u64 {
        self.rows
            .iter()
            .chain(&self.orphans)
            .chain(&self.parts)
            .map(|i| i.bytes)
            .sum()
    }
}

/// Порог возраста `days` суток.
fn min_age(days: u32) -> SignedDuration {
    SignedDuration::from_hours(24 * i64::from(days))
}

/// Файл не менялся не меньше `min` к моменту `now`; `mtime` не читается или в будущем — нет.
fn aged(meta: &std::fs::Metadata, now: Timestamp, min: SignedDuration) -> bool {
    meta.modified()
        .ok()
        .and_then(|t| Timestamp::try_from(t).ok())
        .is_some_and(|t| now.duration_since(t) >= min)
}

/// Путь файла блоба `sha` в корне `root` (как [`crate::BlobStore::path_of`]).
fn blob_path(root: &Path, sha: &str) -> PathBuf {
    root.join(sha.get(..2).unwrap_or("00")).join(sha)
}

/// Файлы каталога `dir` с метаданными; каталога нет — пусто.
fn files_in(dir: &Path) -> Result<Vec<(String, std::fs::Metadata)>, StoreError> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for e in rd {
        let e = e?;
        let Ok(name) = e.file_name().into_string() else {
            continue;
        };
        let meta = e.metadata()?;
        if meta.is_file() {
            out.push((name, meta));
        }
    }
    Ok(out)
}

/// `sha256 → refcount` всех строк `blobs`.
fn load_rows(c: &Connection) -> Result<HashMap<String, i64>, StoreError> {
    let mut st = c.prepare("SELECT sha256, refcount FROM blobs")?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Отбор на момент `now` (только чтение базы `c` и диска): см. [`GcPlan`]. `days = 0` — сборщик
/// выключен: строк и файлов к удалению нет, остальное (`missing`, `.incoming`, `.gc/`) считается.
pub fn plan(c: &Connection, root: &Path, now: Timestamp, days: u32) -> Result<GcPlan, StoreError> {
    let rows = load_rows(c)?;
    let min = min_age(days);
    let on = days > 0;
    let mut p = GcPlan::default();
    let mut seen: HashSet<String> = HashSet::new();
    let dirs = match std::fs::read_dir(root) {
        Ok(rd) => rd.collect::<Result<Vec<_>, _>>()?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    for d in dirs {
        let Ok(aa) = d.file_name().into_string() else {
            continue;
        };
        if aa.len() != 2 || !aa.bytes().all(|b| b.is_ascii_hexdigit()) || !d.path().is_dir() {
            continue;
        }
        for (sha, meta) in files_in(&d.path())? {
            if !is_sha256(&sha) || !sha.starts_with(&aa) {
                continue;
            }
            let item = || GcItem {
                sha256: sha.clone(),
                bytes: meta.len(),
            };
            match rows.get(&sha) {
                Some(0) if on && aged(&meta, now, min) => p.rows.push(item()),
                None if on && aged(&meta, now, min) => p.orphans.push(item()),
                _ => {}
            }
            seen.insert(sha);
        }
    }
    let gc = root.join(GC_DIR);
    for (sha, _) in files_in(&gc)? {
        if is_sha256(&sha) {
            p.leftovers.push(GcLeftover {
                restore: rows.contains_key(&sha),
                sha256: sha,
            });
        }
    }
    let mut absent: Vec<(&String, &i64)> =
        rows.iter().filter(|(s, _)| !seen.contains(*s)).collect();
    absent.sort();
    for (sha, rc) in absent {
        let in_gc = gc.join(sha).is_file();
        if *rc > 0 {
            p.missing.push(MissingBlob {
                sha256: sha.clone(),
                in_gc,
            });
        } else if on && !in_gc {
            p.dangling.push(sha.clone());
        }
    }
    for (name, meta) in files_in(&root.join(INCOMING))? {
        p.incoming_files += 1;
        p.incoming_bytes += meta.len();
        if name.ends_with(".part") && aged(&meta, now, PART_MAX_AGE) {
            p.parts.push(GcItem {
                sha256: name,
                bytes: meta.len(),
            });
        }
    }
    p.rows.sort_by(|a, b| a.sha256.cmp(&b.sha256));
    p.orphans.sort_by(|a, b| a.sha256.cmp(&b.sha256));
    Ok(p)
}

// ---------- проход ----------

/// Итог прохода [`sweep`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GcReport {
    /// Удалено строк `blobs`.
    pub rows: u64,
    /// Удалено файлов: блобы, файлы без строки, `.part`, остатки `.gc/` без строки.
    pub files: u64,
    /// Байт удалённых файлов.
    pub bytes: u64,
    /// Из них `.incoming/*.part`.
    pub parts: u64,
    /// Файлов возвращено из `.gc/` на место.
    pub restored: u64,
    /// Отобранных, которые при повторной проверке оставлены (привязаны, обновлены).
    pub kept: u64,
    /// Блобов, удаление которых не удалось (WARN; повтор — следующим проходом).
    pub errors: u64,
    /// Замок блобов занят (идёт запись): пользователь пропущен.
    pub busy: bool,
}

impl GcReport {
    /// Ничего не изменено.
    pub fn is_empty(&self) -> bool {
        self.rows + self.files + self.restored == 0
    }
}

/// Префикс текста тестового «сбоя процесса» (фича `test-hooks`).
#[cfg(feature = "test-hooks")]
const CRASH: &str = "zam-gc-test-crash:";

/// Тестовая точка сбоя `point` прохода (фича `test-hooks`): ошибка обрывает проход на месте, как
/// смерть процесса, — без итогового разбора `.gc/`. Без фичи — никогда.
fn fail_point(root: &Path, point: &str) -> Result<(), StoreError> {
    #[cfg(feature = "test-hooks")]
    if crate::test_hooks::gc_fails(root, point) {
        return Err(StoreError::Io(std::io::Error::other(format!(
            "{CRASH}{point}"
        ))));
    }
    let _ = (root, point);
    Ok(())
}

/// Ошибка — тестовый «сбой процесса» ([`fail_point`]).
fn is_crash(e: &StoreError) -> bool {
    #[cfg(feature = "test-hooks")]
    if let StoreError::Io(io) = e {
        return io.to_string().starts_with(CRASH);
    }
    let _ = e;
    false
}

/// Ссылаются ли на блоб `sha` пары заметок и отчётов или живой (или с чанками) документ.
fn referenced(c: &Connection, sha: &str) -> Result<bool, StoreError> {
    Ok(c.query_row(
        "SELECT EXISTS (SELECT 1 FROM attachments WHERE blob_sha256 = ?1)
             OR EXISTS (SELECT 1 FROM entry_attachments WHERE blob_sha256 = ?1)
             OR EXISTS (SELECT 1 FROM attachment_docs d WHERE d.blob_sha256 = ?1
                          AND (d.deleted_at IS NULL
                               OR EXISTS (SELECT 1 FROM chunks WHERE attachment_id = d.id)))",
        [sha],
        |r| r.get(0),
    )?)
}

/// Документы открепленных пар блоба `sha` (без чанков): иначе строку блоба не удалить.
fn drop_detached_docs(c: &Connection, sha: &str) -> Result<(), StoreError> {
    c.execute(
        "DELETE FROM attachment_docs WHERE blob_sha256 = ?1 AND deleted_at IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM chunks WHERE attachment_id = attachment_docs.id)",
        [sha],
    )?;
    Ok(())
}

/// `refcount` строки блоба (`None` — строки нет).
fn refcount(c: &Connection, sha: &str) -> Result<Option<i64>, StoreError> {
    Ok(
        c.query_row("SELECT refcount FROM blobs WHERE sha256 = ?1", [sha], |r| {
            r.get(0)
        })
        .optional()?,
    )
}

/// Первая фаза удаления строки с файлом (в транзакции писателя): повторная проверка, файл — в
/// `.gc/`, `DELETE`. `Some(байт)` — удалено (файл ждёт второй фазы), `None` — оставлено.
fn delete_row(
    c: &Connection,
    root: &Path,
    sha: &str,
    now: Timestamp,
    days: u32,
) -> Result<Option<u64>, StoreError> {
    if refcount(c, sha)? != Some(0) || referenced(c, sha)? {
        return Ok(None);
    }
    let path = blob_path(root, sha);
    let Ok(meta) = std::fs::metadata(&path) else {
        return Ok(None);
    };
    if !meta.is_file() || !aged(&meta, now, min_age(days)) {
        return Ok(None);
    }
    drop_detached_docs(c, sha)?;
    let gc = root.join(GC_DIR);
    std::fs::create_dir_all(&gc)?;
    std::fs::rename(&path, gc.join(sha))?;
    fail_point(root, "after_rename")?;
    c.execute(
        "DELETE FROM blobs WHERE sha256 = ?1 AND refcount = 0",
        [sha],
    )?;
    Ok(Some(meta.len()))
}

/// Файл без строки (в транзакции писателя: строку не вставит никто, пока она идёт): повторная
/// проверка и удаление. `Some(байт)` — удалён.
fn delete_orphan(
    c: &Connection,
    root: &Path,
    sha: &str,
    now: Timestamp,
    days: u32,
) -> Result<Option<u64>, StoreError> {
    if refcount(c, sha)?.is_some() {
        return Ok(None);
    }
    let path = blob_path(root, sha);
    let Ok(meta) = std::fs::metadata(&path) else {
        return Ok(None);
    };
    if !meta.is_file() || !aged(&meta, now, min_age(days)) {
        return Ok(None);
    }
    std::fs::remove_file(&path)?;
    Ok(Some(meta.len()))
}

/// Строка `refcount = 0` без файла (в транзакции писателя): повторная проверка и `DELETE`.
fn delete_dangling(c: &Connection, root: &Path, sha: &str) -> Result<bool, StoreError> {
    if refcount(c, sha)? != Some(0)
        || referenced(c, sha)?
        || blob_path(root, sha).exists()
        || root.join(GC_DIR).join(sha).exists()
    {
        return Ok(false);
    }
    drop_detached_docs(c, sha)?;
    Ok(c.execute(
        "DELETE FROM blobs WHERE sha256 = ?1 AND refcount = 0",
        [sha],
    )? == 1)
}

/// Разбор `.gc/` (под исключительным замком или на остановленном сервере): файл со строкой —
/// на место (место занято — копия из `.gc/` удаляется), без строки — удаляется. Возвращает
/// `(возвращено, удалено, байт удалено)`.
pub fn recover(c: &Connection, root: &Path) -> Result<(u64, u64, u64), StoreError> {
    let gc = root.join(GC_DIR);
    let (mut restored, mut removed, mut bytes) = (0, 0, 0);
    for (sha, meta) in files_in(&gc)? {
        if !is_sha256(&sha) {
            continue;
        }
        let parked = gc.join(&sha);
        if refcount(c, &sha)?.is_some() {
            let place = blob_path(root, &sha);
            if place.exists() {
                std::fs::remove_file(&parked)?;
            } else {
                if let Some(d) = place.parent() {
                    std::fs::create_dir_all(d)?;
                }
                std::fs::rename(&parked, &place)?;
            }
            restored += 1;
        } else {
            std::fs::remove_file(&parked)?;
            removed += 1;
            bytes += meta.len();
        }
    }
    Ok((restored, removed, bytes))
}

/// Удаляет `.incoming/*.part` старше суток к моменту `now` (под исключительным замком или на
/// остановленном сервере): `(файлов, байт)`.
pub fn remove_stale_parts(root: &Path, now: Timestamp) -> Result<(u64, u64), StoreError> {
    let (mut n, mut bytes) = (0, 0);
    let dir = root.join(INCOMING);
    for (name, meta) in files_in(&dir)? {
        if name.ends_with(".part") && aged(&meta, now, PART_MAX_AGE) {
            match std::fs::remove_file(dir.join(&name)) {
                Ok(()) => {
                    n += 1;
                    bytes += meta.len();
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok((n, bytes))
}

/// Разбор `.gc/` через читателя базы.
async fn recover_db(db: &Db, root: &Path, rep: &mut GcReport) -> Result<(), StoreError> {
    let r = root.to_path_buf();
    let (restored, removed, bytes) = db.read(move |c| recover(c, &r)).await?;
    rep.restored += restored;
    rep.files += removed;
    rep.bytes += bytes;
    Ok(())
}

/// Один блоб прохода: итог транзакции первой фазы.
enum Step {
    /// Строка удалена, файл — в `.gc/` (`байт`).
    Parked(u64),
    /// Файл удалён сразу (`байт`).
    Removed(u64),
    /// Строка без файла удалена.
    Row,
    Kept,
}

/// Проход сборщика по пользователю с корнем блобов `root` и базой `db` на момент `now` (см.
/// модуль). `days = 0` — ничего не делает. Замок блобов занят — `busy`, ничего не тронуто.
/// Ошибка отдельного блоба — WARN и `errors`, проход продолжается.
pub async fn sweep(
    db: &Db,
    root: &Path,
    now: Timestamp,
    days: u32,
) -> Result<GcReport, StoreError> {
    let mut rep = GcReport::default();
    if days == 0 {
        return Ok(rep);
    }
    let gate = BlobGate::of(root);
    {
        let Some(_x) = gate.try_exclusive() else {
            rep.busy = true;
            return Ok(rep);
        };
        recover_db(db, root, &mut rep).await?;
    }
    let p = {
        let r = root.to_path_buf();
        db.read(move |c| plan(c, &r, now, days)).await?
    };
    #[cfg(feature = "test-hooks")]
    crate::test_hooks::gc_after_plan(root).await;
    let Some(_x) = gate.try_exclusive() else {
        rep.busy = true;
        return Ok(rep);
    };
    let mut jobs: Vec<(u8, String)> = Vec::new();
    jobs.extend(p.rows.iter().map(|i| (0, i.sha256.clone())));
    jobs.extend(p.orphans.iter().map(|i| (1, i.sha256.clone())));
    jobs.extend(p.dangling.iter().map(|s| (2, s.clone())));
    for (kind, sha) in jobs {
        let (r, s) = (root.to_path_buf(), sha.clone());
        let step = db
            .write(move |t| match kind {
                0 => Ok(delete_row(t, &r, &s, now, days)?.map_or(Step::Kept, Step::Parked)),
                1 => Ok(delete_orphan(t, &r, &s, now, days)?.map_or(Step::Kept, Step::Removed)),
                _ => Ok(if delete_dangling(t, &r, &s)? {
                    Step::Row
                } else {
                    Step::Kept
                }),
            })
            .await;
        match step {
            Ok(Step::Parked(bytes)) => {
                rep.rows += 1;
                fail_point(root, "after_commit")?;
                match std::fs::remove_file(root.join(GC_DIR).join(&sha)) {
                    Ok(()) => {
                        rep.files += 1;
                        rep.bytes += bytes;
                    }
                    Err(e) => {
                        tracing::warn!(sha256 = %sha, error = %e, "blob gc: файл остался в .gc");
                        rep.errors += 1;
                    }
                }
            }
            Ok(Step::Removed(bytes)) => {
                rep.files += 1;
                rep.bytes += bytes;
            }
            Ok(Step::Row) => rep.rows += 1,
            Ok(Step::Kept) => rep.kept += 1,
            Err(e) if is_crash(&e) => return Err(e),
            Err(e) => {
                tracing::warn!(sha256 = %sha, error = %e, "blob gc: блоб не удалён");
                rep.errors += 1;
            }
        }
    }
    let (n, bytes) = remove_stale_parts(root, now)?;
    rep.parts += n;
    rep.files += n;
    rep.bytes += bytes;
    // транзакция, откаченная после переименования, оставила файл в `.gc/` — на место сейчас же
    recover_db(db, root, &mut rep).await?;
    Ok(rep)
}
