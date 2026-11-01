//! Конвейер индексации вложений (спецификация M5 §3, задача 5): задания извлечения
//! (`upsert` документа вложения в `extract_state = 'pending'`, [`zam_store::claim_extract_jobs`])
//! → вид по содержимому (`zam_docs::sniff_file`) и размер против `docs.max_bytes` → кэш
//! `blob_extract` по (`sha256`, `EXTRACTOR_VERSION`) или разбор в рабочем процессе
//! ([`DocPool`]) → [`zam_store::put_extract`] → нарезка и чанки
//! ([`zam_store::replace_attachment_chunks`]: сканер секретов, `done`, `upsert` ради точек).
//! Точки строит воркер векторного индекса; Human — только FTS.
//!
//! **Отдельно от воркера векторного индекса.** Конвейер работает и без модели и Qdrant (режим
//! FTS-only): текст вложений ищется полнотекстово всегда. Разбор не задерживает индексацию
//! заметок: у воркера свой цикл, а задания точек вложений идут с приоритетом ниже записей
//! заметок (`PRIORITY_ATTACHMENT`).
//!
//! **Все виды — в рабочем процессе (решение исполнителя, задача 5).** Не только PDF, но и OOXML,
//! таблицы, CSV и текст разбираются в `zam-server doc-worker`: пик памяти разбора (до
//! `max_unpacked` + 8 × `max_chars`, ≈ 650 МиБ при умолчаниях) ложится на процесс с лимитом
//! памяти (Job Object / `RLIMIT_AS`), а не на сервер; паника или зависание разборщика убивает
//! процесс, а не поток сервера (поток `spawn_blocking` прервать нельзя); срок — жёсткий. Цена —
//! копия файла в кадр запроса (base64) и текста в кадр ответа и запуск процесса раз в
//! `docs.worker_jobs` заданий; в сервере остаются файл и кадры (≤ `max_bytes` и выдача).
//!
//! **Одновременность.** Разборов одновременно — не больше `docs.workers` (слотов пула):
//! задания берутся по одному на пользователя за круг, пока есть свободный слот, — пользователи
//! делят слоты по очереди, один большой архив вложений не занимает конвейер целиком.
//!
//! **Неудачи.** Разбор не удался — документ `failed` с причиной (`parse_crash`,
//! `parse_timeout`, `parse_oom`, `parse_error: <область>`, `pdfium_unavailable`, `too_large`,
//! `blob_missing`; без подробностей разборщика и путей сервера), задание — сразу `failed`
//! (одной транзакцией с состоянием документа; одна строка `failed_items` на документ), повтор —
//! reindex. Детерминированные неудачи (`parse_error`, вид не поддерживается разборщиком)
//! кэшируются по (`sha256`, версия извлечения): падающий файл в N заметках разбирается один
//! раз; документы одного блоба разбираются по очереди (захват блоба). PDF без pdfium — `failed`, а не `unsupported`: вид поддерживается, недоступна
//! библиотека; при запуске с pdfium такие документы извлекаются снова. Временные ошибки (база,
//! чтение файла) — повтор с backoff. Неподдерживаемый вид — `unsupported`, без неудачи.
//!
//! **Запуск.** При первом открытии базы: задания извлечения, оставшиеся `running` после аварии, —
//! снова в очередь; документам `pending` без задания (дозаполнение миграции 0019, задания,
//! закрытые воркером до задачи 5) — задание.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use jiff::Timestamp;
use tokio::sync::{Notify, Semaphore, watch};
use tokio::task::JoinSet;
use zam_core::Id;
use zam_docs::{DocError, DocKind, Limits};
use zam_store::{
    BLOB_MISSING, BlobStore, CachedExtract, Db, ExtractEnd, ExtractState, Job, JobOutcome,
    PDFIUM_UNAVAILABLE, StoreError, SystemStore, TOO_LARGE, UserDbs, cached_extract,
    claim_extract_jobs, complete_job, defer_job, ensure_extract_jobs, extract_target, fail_job,
    finish_extract, has_failed_with, put_extract, put_extract_failure, requeue_failed_with,
    requeue_running_extract,
};

use super::pool::{DocPool, ParseFailure};
use crate::config::DocsSection;

/// Опрос очереди без событий (повторы после backoff, задания от переноса и восстановления).
const IDLE_POLL: Duration = Duration::from_secs(2);
/// Пауза между попытками открыть базу, которая не открылась.
const BAD_USER_RETRY: Duration = Duration::from_secs(60);
/// Сколько ждать незаконченные разборы при остановке (затем задания остаются `running` и
/// возвращаются в очередь при следующем запуске).
const STOP_WAIT: Duration = Duration::from_secs(5);
/// Через сколько снова выдать задание документа, блоб которого разбирается в другом слоте.
const BLOB_BUSY_RETRY: Duration = Duration::from_secs(1);

/// Блоб пользователя: `(пользователь, sha256)`.
type BlobKey = (Id, String);

/// Снимок конвейера для `/index/status`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocsStatus {
    /// Причина недоступности pdfium по последнему разбору PDF или проверке (`None` — pdfium
    /// загружен или ещё не требовался).
    pub pdfium_error: Option<String>,
    /// Слотов разбора (`docs.workers`).
    pub workers: usize,
    /// Документов в работе.
    pub running: usize,
    /// Разборов в рабочем процессе с запуска.
    pub extracted: u64,
    /// Извлечений из кэша `blob_extract` с запуска.
    pub cache_hits: u64,
    /// Документов, ушедших в `failed`, с запуска.
    pub failed: u64,
    /// Рабочих процессов запущено с запуска.
    pub spawned: u64,
}

struct Inner {
    pool: DocPool,
    limits: Limits,
    workers: usize,
    system: SystemStore,
    users: UserDbs,
    blobs_dir: PathBuf,
    notify: Notify,
    stop: watch::Sender<bool>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    extracted: AtomicU64,
    cache_hits: AtomicU64,
    failed: AtomicU64,
    running: AtomicUsize,
    /// pdfium загружается (проверка `ping` при запуске — один раз, только если есть документы,
    /// не извлечённые из-за его отсутствия).
    pdfium_ok: tokio::sync::OnceCell<bool>,
    /// Блобы, которые сейчас разбираются ([`try_lock_blob`]).
    blob_locks: Mutex<HashSet<BlobKey>>,
}

/// Конвейер индексации вложений сервера; дёшево клонируется через `Arc`.
pub struct DocIndexer {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for DocIndexer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocIndexer")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl DocIndexer {
    /// Запускает конвейер на текущем runtime tokio: пул рабочих процессов по `[docs]` (процессы
    /// — по требованию) и задача цикла.
    pub fn start(
        cfg: &DocsSection,
        system: SystemStore,
        users: UserDbs,
        blobs_dir: PathBuf,
    ) -> Self {
        let pool = match &cfg.worker_program {
            Some(p) => DocPool::with_program(cfg, p.clone()),
            None => DocPool::new(cfg),
        };
        let (stop, rx) = watch::channel(false);
        let inner = Arc::new(Inner {
            pool,
            limits: cfg.limits(),
            workers: cfg.workers.max(1),
            system,
            users,
            blobs_dir,
            notify: Notify::new(),
            stop,
            task: Mutex::new(None),
            extracted: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            running: AtomicUsize::new(0),
            pdfium_ok: tokio::sync::OnceCell::new(),
            blob_locks: Mutex::default(),
        });
        let task = tokio::spawn(run(inner.clone(), rx));
        *lock(&inner.task) = Some(task);
        DocIndexer { inner }
    }

    /// Будит конвейер (вложение загружено, документ восстановлен или перенесён).
    pub fn notify(&self) {
        self.inner.notify.notify_one();
    }

    /// Разборов в рабочем процессе с запуска (без извлечений из кэша).
    pub fn extracted(&self) -> u64 {
        self.inner.extracted.load(Ordering::SeqCst)
    }

    /// Пул рабочих процессов (миниатюры, превью — следующие задачи M5).
    pub fn pool(&self) -> &DocPool {
        &self.inner.pool
    }

    pub fn status(&self) -> DocsStatus {
        let i = &self.inner;
        DocsStatus {
            pdfium_error: i.pool.pdfium_error(),
            workers: i.workers,
            running: i.running.load(Ordering::SeqCst),
            extracted: i.extracted.load(Ordering::SeqCst),
            cache_hits: i.cache_hits.load(Ordering::SeqCst),
            failed: i.failed.load(Ordering::SeqCst),
            spawned: i.pool.spawned(),
        }
    }

    /// Останавливает цикл: новые задания не берутся, незаконченные разборы ждут не дольше
    /// [`STOP_WAIT`]. Повторный вызов — без действия.
    pub async fn shutdown(&self) {
        let _ = self.inner.stop.send(true);
        self.inner.notify.notify_one();
        let task = lock(&self.inner.task).take();
        if let Some(t) = task
            && let Err(e) = t.await
            && e.is_panic()
        {
            tracing::error!(error = %e, "конвейер документов завершился паникой");
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Цикл конвейера: базы по кругу, по заданию на пользователя за круг, пока есть свободный слот.
async fn run(inner: Arc<Inner>, mut stop: watch::Receiver<bool>) {
    tracing::info!(workers = inner.workers, "конвейер документов запущен");
    let slots = Arc::new(Semaphore::new(inner.workers));
    let mut tasks: JoinSet<()> = JoinSet::new();
    let mut opened: HashSet<Id> = HashSet::new();
    let mut bad: HashMap<Id, Instant> = HashMap::new();
    let mut rr = 0usize;
    loop {
        if *stop.borrow() {
            break;
        }
        while let Some(r) = tasks.try_join_next() {
            if let Err(e) = r
                && e.is_panic()
            {
                tracing::error!(error = %e, "задача извлечения вложения завершилась паникой");
            }
        }
        let users: Vec<Id> = match inner.system.list_users().await {
            Ok(v) => v.into_iter().map(|u| u.id).collect(),
            Err(e) => {
                tracing::warn!(error = %e, "конвейер документов: список пользователей");
                Vec::new()
            }
        };
        let mut claimed = false;
        for k in 0..users.len() {
            if slots.available_permits() == 0 || *stop.borrow() {
                break;
            }
            let user = users[(rr + k) % users.len()];
            if bad.get(&user).is_some_and(|t| Instant::now() < *t) {
                continue;
            }
            let db = match inner.users.get(user).await {
                Ok(db) => {
                    bad.remove(&user);
                    db
                }
                Err(e) => {
                    tracing::warn!(user = %user, error = %e, "конвейер документов: база не открылась");
                    bad.insert(user, Instant::now() + BAD_USER_RETRY);
                    continue;
                }
            };
            if opened.insert(user) {
                startup(&inner, user, &db).await;
            }
            let jobs = match claim_extract_jobs(&db, 1, Timestamp::now()).await {
                Ok(j) => j,
                Err(e) => {
                    tracing::warn!(user = %user, error = %e, "выдача заданий извлечения");
                    continue;
                }
            };
            for job in jobs {
                // блоб уже разбирается в другом слоте — задание обратно в очередь, слот не занят
                let sha = match extract_target(&db, job.target_id).await {
                    Ok(Some(t)) => Some(t.blob_sha256),
                    _ => None,
                };
                let blob = match sha {
                    Some(sha) => match try_lock_blob(&inner, user, &sha) {
                        Some(g) => Some(g),
                        None => {
                            if let Err(e) =
                                defer_job(&db, job.id, job.claim_gen, BLOB_BUSY_RETRY).await
                            {
                                tracing::warn!(job = job.id, error = %e, "задание извлечения не отложено");
                            }
                            continue;
                        }
                    },
                    None => None,
                };
                let Ok(permit) = slots.clone().acquire_owned().await else {
                    break;
                };
                claimed = true;
                let (inner, db) = (inner.clone(), db.clone());
                tasks.spawn(async move {
                    inner.running.fetch_add(1, Ordering::SeqCst);
                    process(&inner, user, &db, job).await;
                    inner.running.fetch_sub(1, Ordering::SeqCst);
                    drop(blob);
                    drop(permit);
                });
            }
        }
        rr = rr.wrapping_add(1);
        // задание взято, слот ещё есть — следующий круг сразу
        if claimed && slots.available_permits() > 0 {
            continue;
        }
        let notify = inner.notify.notified();
        tokio::select! {
            _ = stop.changed() => break,
            () = notify => {}
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
            () = tokio::time::sleep(IDLE_POLL) => {}
        }
    }
    // незаконченные разборы: недолго ждём, затем бросаем (задания `running` вернутся в
    // очередь при следующем запуске)
    let wait = async { while tasks.join_next().await.is_some() {} };
    if tokio::time::timeout(STOP_WAIT, wait).await.is_err() {
        tracing::info!(
            "конвейер документов: незаконченные разборы прерваны, продолжатся при запуске"
        );
        tasks.abort_all();
    }
    tracing::info!("конвейер документов остановлен");
}

/// Первое открытие базы после запуска: задания после аварии, `pending` без задания, документы,
/// не извлечённые без pdfium (если pdfium теперь загружается).
async fn startup(inner: &Inner, user: Id, db: &Db) {
    match requeue_running_extract(db).await {
        Ok(n) if n > 0 => {
            tracing::info!(user = %user, jobs = n, "задания извлечения после остановки снова в очереди")
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(user = %user, error = %e, "сброс заданий извлечения"),
    }
    match ensure_extract_jobs(db).await {
        Ok(n) if n > 0 => {
            tracing::info!(user = %user, docs = n, "документы вложений без задания: задания поставлены")
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(user = %user, error = %e, "задания документов без задания"),
    }
    if !has_failed_with(db, PDFIUM_UNAVAILABLE)
        .await
        .unwrap_or(false)
    {
        return;
    }
    let ok = *inner
        .pdfium_ok
        .get_or_init(|| async { matches!(inner.pool.ping().await, Ok(None)) })
        .await;
    if ok {
        match requeue_failed_with(db, PDFIUM_UNAVAILABLE).await {
            Ok(n) => tracing::info!(
                user = %user,
                docs = n,
                "pdfium загружается: PDF, не извлечённые без него, извлекаются снова"
            ),
            Err(e) => tracing::warn!(user = %user, error = %e, "повтор PDF без pdfium"),
        }
    }
}

/// Чем закончилось извлечение документа.
enum Step {
    /// Извлечению конец без записи (документ скрыт, откреплён, уже не `pending`): задание
    /// закрывается.
    Skip,
    /// Итог для базы: чанки, `unsupported` или `failed` — одной транзакцией с заданием.
    End(ExtractEnd),
    /// Временная ошибка (база, чтение файла): повтор с backoff.
    Retry(String),
}

impl From<StoreError> for Step {
    fn from(e: StoreError) -> Self {
        Step::Retry(e.to_string())
    }
}

/// Задание извлечения: шаг и его итог в базе (состояние документа и задание — одной
/// транзакцией, [`zam_store::finish_extract`]).
async fn process(inner: &Inner, user: Id, db: &Db, job: Job) {
    let att = job.target_id;
    let outcome = match extract_doc(inner, user, db, att).await {
        Step::Skip => complete_job(db, job.id, job.claim_gen).await,
        Step::End(end) => {
            if let ExtractEnd::Failed(reason) = &end {
                inner.failed.fetch_add(1, Ordering::SeqCst);
                tracing::warn!(user = %user, attachment = %att, reason = %reason, "вложение не извлечено");
            }
            match finish_extract(db, job.id, job.claim_gen, att, end).await {
                Ok(o) => Ok(o),
                Err(e) => {
                    fail_job(db, job.id, job.claim_gen, &e.to_string(), Timestamp::now()).await
                }
            }
        }
        Step::Retry(e) => {
            tracing::debug!(user = %user, attachment = %att, error = %e, "извлечение: повтор");
            fail_job(db, job.id, job.claim_gen, &e, Timestamp::now()).await
        }
    };
    match outcome {
        Ok(JobOutcome::Failed) => {
            tracing::debug!(attachment = %att, "задание извлечения failed")
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(job = job.id, error = %e, "итог задания извлечения"),
    }
}

/// Вид по содержимому и размер файла блоба (в пуле блокирующих задач: чтение файла).
async fn sniff(
    path: PathBuf,
    filename: String,
    limits: Limits,
) -> std::io::Result<(Option<DocKind>, u64)> {
    tokio::task::spawn_blocking(move || {
        let f = std::fs::File::open(&path)?;
        let size = f.metadata()?.len();
        let kind = zam_docs::sniff_file(std::io::BufReader::new(f), &filename, &limits)?;
        Ok((kind, size))
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Захват блоба `sha` пользователя `user` на время разбора без ожидания: `None` — блоб уже
/// разбирается. Второй документ того же файла не ждёт в слоте, а возвращается в очередь
/// ([`BLOB_BUSY_RETRY`]) и потом берёт результат из кэша — слот достаётся другому файлу.
fn try_lock_blob(inner: &Arc<Inner>, user: Id, sha: &str) -> Option<BlobGuard> {
    let key = (user, sha.to_string());
    lock(&inner.blob_locks)
        .insert(key.clone())
        .then(|| BlobGuard {
            inner: inner.clone(),
            key,
        })
}

/// Захват блоба ([`try_lock_blob`]); освобождается при удалении.
pub struct BlobGuard {
    inner: Arc<Inner>,
    key: BlobKey,
}

impl Drop for BlobGuard {
    fn drop(&mut self) {
        lock(&self.inner.blob_locks).remove(&self.key);
    }
}

#[cfg(feature = "test-hooks")]
impl DocIndexer {
    /// Захватить блоб, как его захватывает разбор (тест: документ этого файла не должен
    /// занимать слот, пока блоб занят). `None` — уже захвачен.
    #[doc(hidden)]
    pub fn hold_blob(&self, user: Id, sha: &str) -> Option<BlobGuard> {
        try_lock_blob(&self.inner, user, sha)
    }
}

/// Извлечение документа `att` пользователя `user`.
async fn extract_doc(inner: &Inner, user: Id, db: &Db, att: Id) -> Step {
    let t = match extract_target(db, att).await {
        Ok(Some(t)) => t,
        Ok(None) => return Step::Skip,
        Err(e) => return e.into(),
    };
    // скрыт, откреплён или уже обработан: восстановление поставит задание заново
    if !t.live || t.state != ExtractState::Pending {
        return Step::Skip;
    }
    match cached_extract(db, &t.blob_sha256).await {
        Ok(Some(c)) => {
            inner.cache_hits.fetch_add(1, Ordering::SeqCst);
            return Step::End(match c {
                CachedExtract::Extracted(x) => ExtractEnd::Units(x.units),
                CachedExtract::Failed(reason) => ExtractEnd::Failed(reason),
                CachedExtract::Unsupported => ExtractEnd::Unsupported,
            });
        }
        Ok(None) => {}
        Err(e) => return e.into(),
    }
    let path = BlobStore::new(&inner.blobs_dir, user).path_of(&t.blob_sha256);
    let (kind, size) = match sniff(path.clone(), t.filename.clone(), inner.limits.clone()).await {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Step::End(ExtractEnd::Failed(BLOB_MISSING.into()));
        }
        Err(e) => return Step::Retry(format!("файл вложения: {e}")),
    };
    // вид и размер — по файлу и `[docs]` (не кэшируются: дёшевы и зависят от конфигурации)
    let Some(kind) = kind else {
        return Step::End(ExtractEnd::Unsupported);
    };
    if size > inner.limits.max_bytes {
        return Step::End(ExtractEnd::Failed(TOO_LARGE.into()));
    }
    let bytes = match read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Step::End(ExtractEnd::Failed(BLOB_MISSING.into()));
        }
        Err(e) => return Step::Retry(format!("файл вложения: {e}")),
    };
    inner.extracted.fetch_add(1, Ordering::SeqCst);
    let started = Instant::now();
    let x = match inner.pool.extract(kind, bytes).await {
        Ok(x) => x,
        Err(f) => {
            let (end, cache) = failure(inner, kind, f);
            if cache {
                let reason = match &end {
                    ExtractEnd::Failed(r) => Some(r.as_str()),
                    _ => None,
                };
                if let Err(e) = put_extract_failure(db, &t.blob_sha256, reason).await {
                    tracing::warn!(error = %e, "неудача разбора не записана в кэш");
                }
            }
            return Step::End(end);
        }
    };
    tracing::debug!(
        user = %user,
        attachment = %att,
        kind = kind.as_str(),
        units = x.units.len(),
        warnings = x.warnings.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "вложение разобрано"
    );
    if let Err(e) = put_extract(db, &t.blob_sha256, &x).await {
        return e.into();
    }
    Step::End(ExtractEnd::Units(x.units))
}

/// Чтение файла блоба (не больше `max_bytes`: размер проверен до чтения).
async fn read(path: &Path) -> std::io::Result<Bytes> {
    tokio::fs::read(path).await.map(Bytes::from)
}

/// Итог неудачного разбора для базы и признак «кэшировать» (неудача — свойство файла этой
/// версии извлечения). В `extract_error` — код и область ([`DocError::area`]), без
/// подробностей разборщика (имён тегов, сущностей) и без путей сервера: причина
/// недоступности pdfium (путь библиотеки) — только в журнал и администратору в `/index/status`.
fn failure(inner: &Inner, kind: DocKind, f: ParseFailure) -> (ExtractEnd, bool) {
    match f {
        // PDF без pdfium: вид поддерживается, библиотеки нет (не кэшируется: не свойство файла)
        ParseFailure::Doc(DocError::Unsupported) if kind == DocKind::Pdf => {
            tracing::warn!(
                pdfium = inner
                    .pool
                    .pdfium_error()
                    .as_deref()
                    .unwrap_or("pdfium не загружен"),
                "PDF не разобран: pdfium недоступен"
            );
            (ExtractEnd::Failed(PDFIUM_UNAVAILABLE.into()), false)
        }
        other => classify(other),
    }
}

/// Итог неудачного разбора (кроме PDF без pdfium) и признак «кэшировать». Кэшируются только
/// свойства файла: `Malformed` (`parse_error: <область>`) и вид, который разборщик не
/// поддерживает. `Internal` — паника разборщика или ошибка кадра в рабочем процессе — сбой
/// процесса (`parse_crash`), как и падение, срок и нехватка памяти: не кэшируются.
fn classify(f: ParseFailure) -> (ExtractEnd, bool) {
    match f {
        ParseFailure::Doc(DocError::Unsupported) => (ExtractEnd::Unsupported, true),
        ParseFailure::Doc(e @ DocError::Malformed(_)) => (
            ExtractEnd::Failed(format!("{}: {}", PARSE_ERROR, e.area())),
            true,
        ),
        ParseFailure::Doc(DocError::TooLarge) => (ExtractEnd::Failed(TOO_LARGE.into()), false),
        // срок истёк без частичного результата — сбой, а не свойство файла
        ParseFailure::Doc(DocError::Timeout) => (ExtractEnd::Failed("parse_timeout".into()), false),
        ParseFailure::Doc(DocError::Internal(_)) => (
            ExtractEnd::Failed(ParseFailure::Crash.reason().into()),
            false,
        ),
        other => (ExtractEnd::Failed(other.reason().into()), false),
    }
}

/// Код ошибки документа (`DocError::Malformed`).
const PARSE_ERROR: &str = "parse_error";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_error_reason_has_area_only() {
        let e =
            DocError::Malformed("xml (word/document.xml): неизвестная сущность &секрет;".into());
        assert_eq!(e.area(), "xml (word/document.xml)");
        assert_eq!(
            format!("{PARSE_ERROR}: {}", e.area()),
            "parse_error: xml (word/document.xml)"
        );
        assert_eq!(DocError::Internal("тег <w:p>".into()).area(), "internal");
        // кэшируются только свойства файла; паника разборщика (`Internal`) — сбой процесса
        let end = |f| classify(f);
        assert_eq!(
            end(ParseFailure::Doc(e.clone())),
            (
                ExtractEnd::Failed("parse_error: xml (word/document.xml)".into()),
                true
            )
        );
        assert_eq!(
            end(ParseFailure::Doc(DocError::Unsupported)),
            (ExtractEnd::Unsupported, true)
        );
        assert_eq!(
            end(ParseFailure::Doc(DocError::Internal("паника".into()))),
            (ExtractEnd::Failed("parse_crash".into()), false)
        );
        for (f, code) in [
            (ParseFailure::Crash, "parse_crash"),
            (ParseFailure::Timeout, "parse_timeout"),
            (ParseFailure::Oom, "parse_oom"),
            (ParseFailure::Doc(DocError::Timeout), "parse_timeout"),
        ] {
            assert_eq!(end(f), (ExtractEnd::Failed(code.into()), false));
        }
        assert_eq!(
            zam_store::error_code("parse_error: xml (word/document.xml)"),
            "parse_error"
        );
        assert_eq!(zam_store::error_code("parse_crash"), "parse_crash");
    }
}
