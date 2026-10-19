//! Воркер векторной индексации (§8.1, §8.3 ТЗ): одна tokio-задача на сервер.
//!
//! Цикл:
//! 1. **Ворота.** Qdrant готов (`Ready` / `External`), хранилище не требует пересборки,
//!    коллекция проверена [`VectorIndex::ensure`]: `model_id` и размерность совпадают с
//!    текущей моделью. Иначе задания не берутся — очередь копится (Review Focus 1, 3).
//!    Коллекция с чужим `model_id` не трогается: векторный канал выключен до reindex.
//! 2. **Пересборка.** `storage_rebuilt` супервизора, [`WorkerHandle::request_rebuild`]
//!    или коллекция, созданная заново при уже проиндексированных узлах →
//!    `enqueue_reindex` во всех базах (векторы из `embeddings`, без инференса для
//!    известных текстов); для `storage_rebuilt` — аудит `index.storage_rebuilt` и
//!    `ack_storage_rebuilt()`.
//! 3. **Задания.** Базы пользователей по кругу, пачками по `batch_jobs`: чанки узла →
//!    кэш `embeddings` по `text_hash` → инференс промахов (`spawn_blocking`, пачки не
//!    больше `batch_tokens`, уступка `yield_ms`) → `delete_node` + `upsert` (`wait=true`)
//!    → `complete_job` → `index_state = 'indexed'` только при [`JobOutcome::Done`].
//!    Qdrant недоступен или коллекции нет — задания возвращаются [`release_job`] без
//!    траты попыток; прочие ошибки — [`fail_job`] (backoff, после 5 — `failed`).
//! 4. **L0** (§8.3): автоматический L0 заметки — предложение у центроида её чанков;
//!    папки-предки — свёртка L0 детей с дебаунсом `rollup_debounce`, папка индексируется
//!    точкой уровня `folder`.
//!
//! Текст эмбеддинга чанка — `context_prefix + "\n" + text` (§8.2 п. 6), ключ кэша —
//! `text_hash` текста чанка (§6.3). В лог не попадают тексты, векторы и ключ Qdrant.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use tokio::sync::{Notify, watch};
use zam_core::Id;
use zam_markdown::ChunkLevel;
use zam_store::{
    Db, Embedding, IndexChunk, IndexState, Job, JobOp, JobOutcome, NodeKind, SpaceKind, StoreError,
    SystemStore, UserDbs, chunks_for_index, claim_jobs, complete_job, enqueue_reindex, fail_job,
    get_embeddings, get_node, get_space, indexed_count, list_children, put_embeddings, release_job,
    requeue_running, set_auto_l0, set_folder_l0, set_index_state,
};

use crate::rollup::{self, FolderChild};
use crate::supervisor::{QdrantState, Supervisor};
use crate::{IndexError, IndexPoint, PointPayload, VectorIndex};

/// Модель эмбеддингов для воркера. Отдельный трейт, чтобы `zam-index` не зависел от
/// ONNX Runtime: сервер оборачивает `zam_ml::Embedder`, тесты — подставляют свою модель.
pub trait PassageEmbedder: Send + Sync + 'static {
    /// `"<id>@<sha8>"`: ключ кэша `embeddings` и метаданные коллекции.
    fn model_id(&self) -> &str;
    /// Размерность dense.
    fn dim(&self) -> usize;
    /// Эмбеддинги текстов в порядке `texts` (блокирующий вызов: инференс).
    fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Embedding>, String>;
    /// Токены текста без спецтокенов (предел L0).
    fn count_tokens(&self, text: &str) -> usize;
}

/// Настройки воркера (`[index]` конфигурации сервера).
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Заданий в одной выдаче.
    pub batch_jobs: usize,
    /// Предел токенов одного вызова инференса.
    pub batch_tokens: usize,
    /// Уступка между пачками.
    pub yield_ms: u64,
    /// Дебаунс свёртки L0 папок.
    pub rollup_debounce: Duration,
    /// Опрос очереди без событий (повторы после backoff).
    pub idle_poll: Duration,
    /// Предел задержки повтора проверки Qdrant.
    pub retry_max: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        WorkerConfig {
            batch_jobs: 8,
            batch_tokens: 8192,
            yield_ms: 5,
            rollup_debounce: Duration::from_secs(2),
            idle_poll: Duration::from_secs(2),
            retry_max: Duration::from_secs(10),
        }
    }
}

/// Зависимости воркера.
pub struct WorkerDeps {
    pub system: SystemStore,
    pub users: UserDbs,
    pub embedder: Arc<dyn PassageEmbedder>,
    pub vector: Arc<dyn VectorIndex>,
    /// Управляемый или внешний Qdrant; `None` — состояние не отслеживается (тесты).
    pub supervisor: Option<Arc<Supervisor>>,
}

/// Снимок состояния воркера для деградации поиска и `/index/status`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkerStatus {
    /// Задача воркера запущена.
    pub running: bool,
    /// Коллекция проверена, последняя операция с Qdrant успешна: векторный канал включён.
    pub vector_ready: bool,
    /// `model_id` коллекции (последний `ensure`).
    pub collection_model: Option<String>,
    /// `(коллекция, текущая модель)`: модели не совпадают, канал выключен до reindex.
    pub model_mismatch: Option<(String, String)>,
    /// Последняя ошибка Qdrant или инференса (без текста заметок).
    pub last_error: Option<String>,
}

struct Inner {
    status: Mutex<WorkerStatus>,
    notify: Notify,
    rebuild: AtomicBool,
    embedded: AtomicU64,
    jobs_done: AtomicU64,
    stop: watch::Sender<bool>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// Управление воркером; дёшево клонируется.
#[derive(Clone)]
pub struct WorkerHandle {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for WorkerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerHandle")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

/// Сколько ждать остановки задачи воркера (инференс пачки не прерывается).
const STOP_TIMEOUT: Duration = Duration::from_secs(60);

impl WorkerHandle {
    /// Воркер не запущен (нет модели или Qdrant): очередь копится.
    pub fn idle() -> WorkerHandle {
        let (stop, _) = watch::channel(false);
        WorkerHandle {
            inner: Arc::new(Inner {
                status: Mutex::default(),
                notify: Notify::new(),
                rebuild: AtomicBool::new(false),
                embedded: AtomicU64::new(0),
                jobs_done: AtomicU64::new(0),
                stop,
                task: Mutex::new(None),
            }),
        }
    }

    /// Запускает воркер на текущем runtime tokio.
    pub fn spawn(deps: WorkerDeps, cfg: WorkerConfig) -> WorkerHandle {
        let h = WorkerHandle::idle();
        lock(&h.inner.status).running = true;
        let w = Worker {
            stop: h.inner.stop.subscribe(),
            inner: h.inner.clone(),
            qstate: deps.supervisor.as_ref().map(|s| s.state()),
            deps,
            cfg,
            rollups: Rollups::default(),
            opened: HashSet::new(),
        };
        let task = tokio::spawn(w.run());
        *lock(&h.inner.task) = Some(task);
        h
    }

    /// Будит воркер (после записи узла).
    pub fn notify(&self) {
        self.inner.notify.notify_one();
    }

    /// Снимок состояния.
    pub fn status(&self) -> WorkerStatus {
        lock(&self.inner.status).clone()
    }

    /// Сколько текстов прошло через модель (промахи кэша `embeddings`).
    pub fn embedded_texts(&self) -> u64 {
        self.inner.embedded.load(Ordering::SeqCst)
    }

    /// Сколько заданий закрыто (`Done`).
    pub fn jobs_done(&self) -> u64 {
        self.inner.jobs_done.load(Ordering::SeqCst)
    }

    /// Пересобрать индекс: `ensure` заново и `enqueue_reindex` во всех базах (после
    /// `drop_all` или смены модели). Выполняется воркером асинхронно.
    pub fn request_rebuild(&self) {
        self.inner.rebuild.store(true, Ordering::SeqCst);
        self.notify();
    }

    /// Останавливает воркер и ждёт задачу (не дольше минуты: пачка инференса
    /// не прерывается). Повторный вызов — без действия.
    pub async fn shutdown(&self) {
        let _ = self.inner.stop.send(true);
        self.inner.notify.notify_one();
        let task = lock(&self.inner.task).take();
        if let Some(mut t) = task
            && tokio::time::timeout(STOP_TIMEOUT, &mut t).await.is_err()
        {
            tracing::warn!("воркер индексации не остановился за минуту, задача прервана");
            t.abort();
        }
        lock(&self.inner.status).running = false;
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Чем закончилось ожидание.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wake {
    Stop,
    State,
    Notify,
    Timer,
}

/// Почему шаг прерван.
enum Halt {
    /// Qdrant недоступен или коллекции нет: задания возвращены, нужна проверка ворот.
    Vector(IndexError),
    /// Остановка сервера.
    Stopped,
}

/// Отложенные свёртки папок: `(пользователь, папка) → (первое событие, срок)`.
#[derive(Default)]
struct Rollups {
    due: HashMap<(Id, Id), (Instant, Instant)>,
}

impl Rollups {
    /// Хвостовой дебаунс, но не дольше пяти интервалов от первого события.
    fn schedule(&mut self, user: Id, folder: Id, debounce: Duration) {
        let now = Instant::now();
        let e = self
            .due
            .entry((user, folder))
            .or_insert((now, now + debounce));
        e.1 = (now + debounce).min(e.0 + debounce * 5);
    }

    fn next_due(&self) -> Option<Instant> {
        self.due.values().map(|v| v.1).min()
    }

    fn take_due(&mut self) -> Vec<(Id, Id)> {
        let now = Instant::now();
        let ready: Vec<(Id, Id)> = self
            .due
            .iter()
            .filter(|(_, v)| v.1 <= now)
            .map(|(k, _)| *k)
            .collect();
        for k in &ready {
            self.due.remove(k);
        }
        ready
    }
}

/// Точка ещё не построена: чанк и ключ кэша.
struct EmbedItem {
    key: String,
    input: String,
    tokens: usize,
}

/// Что делать с узлом задания.
enum Plan {
    Delete,
    Upsert(Vec<IndexChunk>),
}

struct Worker {
    deps: WorkerDeps,
    cfg: WorkerConfig,
    inner: Arc<Inner>,
    stop: watch::Receiver<bool>,
    qstate: Option<watch::Receiver<QdrantState>>,
    rollups: Rollups,
    /// Базы, для которых выполнен `requeue_running` (задания после аварии).
    opened: HashSet<Id>,
}

impl Worker {
    fn stopped(&self) -> bool {
        *self.stop.borrow()
    }

    fn status(&self, f: impl FnOnce(&mut WorkerStatus)) {
        f(&mut lock(&self.inner.status));
    }

    fn model(&self) -> String {
        self.deps.embedder.model_id().to_string()
    }

    /// Qdrant в состоянии, в котором с ним можно работать.
    fn qdrant_up(&self) -> bool {
        match &self.qstate {
            None => true,
            Some(rx) => matches!(
                *rx.borrow(),
                QdrantState::Ready { .. } | QdrantState::External
            ),
        }
    }

    /// Ждёт события: остановка, смена состояния Qdrant, `notify` (если `wake`) или срок.
    async fn wait(&mut self, until: Instant, wake: bool) -> Wake {
        let mut stop = self.stop.clone();
        let mut q = self.qstate.clone();
        let inner = self.inner.clone();
        let reason = tokio::select! {
            _ = stop.changed() => Wake::Stop,
            () = state_changed(&mut q) => Wake::State,
            () = inner.notify.notified(), if wake => Wake::Notify,
            () = tokio::time::sleep_until(until.into()) => Wake::Timer,
        };
        if let (Some(mine), Some(mut theirs)) = (&mut self.qstate, q) {
            // отметить увиденное значение: следующий `changed` — только на новое
            theirs.borrow_and_update();
            *mine = theirs;
        }
        reason
    }

    async fn run(mut self) {
        tracing::info!(model = %self.model(), "воркер индексации запущен");
        let mut ensured = false;
        let mut retry_at: Option<Instant> = None;
        let mut backoff = Duration::from_millis(500);
        let mut rr = 0usize;
        loop {
            if self.stopped() {
                break;
            }
            // 1. Qdrant
            if !self.qdrant_up() {
                if ensured {
                    tracing::info!("Qdrant недоступен: индексация приостановлена");
                }
                ensured = false;
                self.status(|s| s.vector_ready = false);
                self.wait(Instant::now() + self.cfg.idle_poll * 15, true)
                    .await;
                continue;
            }
            // 2. Пересборка хранилища или запрос reindex
            let storage = self
                .deps
                .supervisor
                .as_ref()
                .is_some_and(|s| s.storage_rebuilt());
            let requested = self.inner.rebuild.swap(false, Ordering::SeqCst);
            if storage || requested {
                ensured = false;
                if let Err(e) = self.rebuild(storage).await {
                    tracing::warn!(error = %e, "пересборка индекса не поставлена, повтор");
                    self.inner.rebuild.store(requested, Ordering::SeqCst);
                    self.wait(Instant::now() + self.cfg.retry_max, false).await;
                    continue;
                }
            }
            // 3. Коллекция
            if !ensured {
                if let Some(t) = retry_at
                    && Instant::now() < t
                {
                    // смена состояния Qdrant (перезапуск, готовность) или запрос пересборки
                    // (он приходит через notify) — проверить сразу; запись узла — ждать срока
                    if self.wait(t, true).await != Wake::State
                        && !self.inner.rebuild.load(Ordering::SeqCst)
                    {
                        continue;
                    }
                }
                retry_at = None;
                match self.ensure().await {
                    Ok(true) => {
                        ensured = true;
                        backoff = Duration::from_millis(500);
                    }
                    Ok(false) => {
                        // чужая модель: повторная проверка изредка (reindex — задача 10)
                        retry_at = Some(Instant::now() + self.cfg.idle_poll * 15);
                        continue;
                    }
                    Err(e) => {
                        self.vector_down(&e);
                        retry_at = Some(Instant::now() + backoff);
                        backoff = (backoff * 2).min(self.cfg.retry_max);
                        continue;
                    }
                }
            }
            // 4. Задания
            let did = match self.round(&mut rr).await {
                Ok(did) => did,
                Err(Halt::Stopped) => break,
                Err(Halt::Vector(e)) => {
                    ensured = false;
                    self.vector_down(&e);
                    retry_at = Some(Instant::now() + backoff);
                    backoff = (backoff * 2).min(self.cfg.retry_max);
                    continue;
                }
            };
            // 5. Свёртки L0
            match self.run_rollups().await {
                Ok(()) => {}
                Err(Halt::Stopped) => break,
                Err(Halt::Vector(e)) => {
                    ensured = false;
                    self.vector_down(&e);
                    retry_at = Some(Instant::now() + backoff);
                    continue;
                }
            }
            if did {
                tokio::time::sleep(Duration::from_millis(self.cfg.yield_ms)).await;
            } else {
                let mut until = Instant::now() + self.cfg.idle_poll;
                if let Some(d) = self.rollups.next_due() {
                    until = until.min(d);
                }
                self.wait(until, true).await;
            }
        }
        tracing::info!("воркер индексации остановлен");
    }

    fn vector_down(&self, e: &IndexError) {
        let was = lock(&self.inner.status).vector_ready;
        if was {
            tracing::warn!(error = %e, "векторный индекс недоступен: задания копятся до его возврата");
        } else {
            tracing::debug!(error = %e, "векторный индекс всё ещё недоступен");
        }
        self.status(|s| {
            s.vector_ready = false;
            s.last_error = Some(e.to_string());
        });
    }

    /// `ensure` коллекции. `Ok(true)` — можно индексировать, `Ok(false)` — чужая модель.
    async fn ensure(&mut self) -> Result<bool, IndexError> {
        let model = self.model();
        let dim = self.deps.embedder.dim();
        let meta = self.deps.vector.ensure(&model, dim).await?;
        let ours = meta.model_id.as_deref() == Some(model.as_str()) && meta.dim == dim;
        if !ours {
            let coll = match &meta.model_id {
                Some(m) if meta.dim != dim => format!("{m} (dim {})", meta.dim),
                Some(m) => m.clone(),
                None => "(без model_id)".to_string(),
            };
            let first = lock(&self.inner.status).model_mismatch.is_none();
            if first {
                tracing::warn!(
                    collection_model = %coll,
                    model = %model,
                    "модель коллекции Qdrant не совпадает с текущей: векторный поиск выключен до reindex"
                );
            }
            self.status(|s| {
                s.vector_ready = false;
                s.collection_model = meta.model_id.clone();
                s.model_mismatch = Some((coll, model));
            });
            return Ok(false);
        }
        self.status(|s| {
            s.vector_ready = true;
            s.collection_model = meta.model_id.clone();
            s.model_mismatch = None;
            s.last_error = None;
        });
        if meta.created {
            // коллекция новая: если узлы уже были проиндексированы (коллекцию удалили),
            // их нужно вернуть из `embeddings`
            let mut lost = 0i64;
            for u in self.users().await {
                if let Ok(db) = self.deps.users.get(u).await {
                    lost += indexed_count(&db).await.unwrap_or(0);
                }
            }
            if lost > 0 {
                tracing::warn!(
                    nodes = lost,
                    "коллекция Qdrant создана заново: пересборка из embeddings"
                );
                if let Err(e) = self.rebuild(false).await {
                    tracing::warn!(error = %e, "пересборка не поставлена, повтор");
                    self.inner.rebuild.store(true, Ordering::SeqCst);
                }
            }
        }
        Ok(true)
    }

    async fn users(&self) -> Vec<Id> {
        match self.deps.system.list_users().await {
            Ok(v) => v.into_iter().map(|u| u.id).collect(),
            Err(e) => {
                tracing::warn!(error = %e, "список пользователей недоступен");
                Vec::new()
            }
        }
    }

    /// `enqueue_reindex` во всех базах; `storage` — хранилище Qdrant стёрто: аудит и ack.
    async fn rebuild(&mut self, storage: bool) -> Result<(), StoreError> {
        let started = Instant::now();
        let mut jobs = 0u64;
        let users = self.deps.system.list_users().await?;
        for u in &users {
            let db = self.deps.users.get(u.id).await?;
            jobs += enqueue_reindex(&db, None).await?;
        }
        tracing::info!(
            users = users.len(),
            jobs,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "пересборка индекса поставлена в очередь"
        );
        if storage {
            self.deps
                .system
                .audit(
                    "system",
                    None,
                    "index.storage_rebuilt",
                    None,
                    None,
                    serde_json::json!({ "users": users.len(), "jobs": jobs }),
                )
                .await?;
            if let Some(s) = &self.deps.supervisor
                && let Err(e) = s.ack_storage_rebuilt()
            {
                tracing::warn!(error = %e, "флаг пересборки хранилища не снят");
            }
        }
        Ok(())
    }

    /// Один круг по базам: не больше одной пачки на пользователя. `Ok(true)` — работа была.
    async fn round(&mut self, rr: &mut usize) -> Result<bool, Halt> {
        let users = self.users().await;
        if users.is_empty() {
            return Ok(false);
        }
        let mut did = false;
        for k in 0..users.len() {
            if self.stopped() {
                return Err(Halt::Stopped);
            }
            let user = users[(*rr + k) % users.len()];
            let db = match self.deps.users.get(user).await {
                Ok(db) => db,
                Err(e) => {
                    tracing::warn!(user = %user, error = %e, "база пользователя не открылась");
                    continue;
                }
            };
            if self.opened.insert(user) {
                match requeue_running(&db).await {
                    Ok(n) if n > 0 => {
                        tracing::info!(user = %user, jobs = n, "задания после аварийной остановки снова в очереди")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(user = %user, error = %e, "requeue_running"),
                }
            }
            let jobs = match claim_jobs(&db, self.cfg.batch_jobs, Timestamp::now()).await {
                Ok(j) => j,
                Err(e) => {
                    tracing::warn!(user = %user, error = %e, "выдача заданий");
                    continue;
                }
            };
            if jobs.is_empty() {
                continue;
            }
            did = true;
            self.batch(user, &db, jobs).await?;
            tokio::time::sleep(Duration::from_millis(self.cfg.yield_ms)).await;
        }
        *rr = rr.wrapping_add(1);
        Ok(did)
    }

    async fn release_all(&self, db: &Db, jobs: &[Job]) {
        for j in jobs {
            if let Err(e) = release_job(db, j.id, j.claim_gen).await {
                tracing::warn!(job = j.id, error = %e, "задание не возвращено в очередь");
            }
        }
    }

    async fn fail(&self, db: &Db, j: &Job, err: &str) {
        match fail_job(db, j.id, j.claim_gen, err, Timestamp::now()).await {
            Ok(JobOutcome::Failed) if j.op == JobOp::Delete => tracing::warn!(
                node = %j.target_id,
                error = err,
                "задание delete завершилось неудачей: точки узла остаются в Qdrant (поиск их отфильтрует)"
            ),
            Ok(JobOutcome::Failed) => {
                tracing::warn!(node = %j.target_id, error = err, "задание индексации failed")
            }
            Ok(_) => {
                tracing::debug!(node = %j.target_id, error = err, "задание индексации: повтор")
            }
            Err(e) => tracing::warn!(job = j.id, error = %e, "fail_job"),
        }
    }

    /// Пачка заданий одной базы.
    async fn batch(&mut self, user: Id, db: &Db, jobs: Vec<Job>) -> Result<(), Halt> {
        // планы
        let mut plans: Vec<(Job, Plan)> = Vec::with_capacity(jobs.len());
        for j in jobs {
            let plan = match j.op {
                JobOp::Delete => Plan::Delete,
                JobOp::Upsert => match chunks_for_index(db, j.target_id).await {
                    Ok(c) if c.is_empty() => Plan::Delete,
                    Ok(c) => Plan::Upsert(c),
                    // узла нет: убрать возможные точки
                    Err(StoreError::NotFound) => Plan::Delete,
                    Err(e) => {
                        self.fail(db, &j, &format!("чанки узла: {e}")).await;
                        continue;
                    }
                },
            };
            plans.push((j, plan));
        }
        // эмбеддинги всех чанков пачки (общий кэш, повторы текстов — один раз)
        let items: Vec<EmbedItem> = plans
            .iter()
            .filter_map(|(_, p)| match p {
                Plan::Upsert(c) => Some(c),
                Plan::Delete => None,
            })
            .flatten()
            .map(|c| EmbedItem {
                key: c.text_hash.clone(),
                input: embed_input(c),
                tokens: c.token_count.max(1) as usize + c.context_prefix.len() / 3,
            })
            .collect();
        let embs = match self.embed_cached(db, items).await {
            Ok(m) => m,
            Err(EmbedFail::Stopped) => {
                let all: Vec<Job> = plans.into_iter().map(|(j, _)| j).collect();
                self.release_all(db, &all).await;
                return Err(Halt::Stopped);
            }
            Err(EmbedFail::Other(e)) => {
                tracing::warn!(error = %e, "эмбеддинги пачки не получены");
                self.status(|s| s.last_error = Some(e.clone()));
                let mut keep = Vec::new();
                for (j, p) in plans {
                    match p {
                        Plan::Upsert(_) => self.fail(db, &j, &e).await,
                        Plan::Delete => keep.push((j, p)),
                    }
                }
                plans = keep;
                HashMap::new()
            }
        };
        // Qdrant и завершение
        let mut rest = plans.into_iter();
        while let Some((j, plan)) = rest.next() {
            if self.stopped() {
                let mut back = vec![j];
                back.extend(rest.map(|(j, _)| j));
                self.release_all(db, &back).await;
                return Err(Halt::Stopped);
            }
            let res = self.apply(user, &j, &plan, &embs).await;
            match res {
                Ok(()) => {}
                Err(e @ (IndexError::Unavailable(_) | IndexError::NotFound(_))) => {
                    let mut back = vec![j];
                    back.extend(rest.map(|(j, _)| j));
                    self.release_all(db, &back).await;
                    return Err(Halt::Vector(e));
                }
                Err(e) => {
                    self.fail(db, &j, &e.to_string()).await;
                    continue;
                }
            }
            match complete_job(db, j.id, j.claim_gen).await {
                Ok(JobOutcome::Done) => {
                    self.inner.jobs_done.fetch_add(1, Ordering::SeqCst);
                    if let Plan::Upsert(chunks) = &plan {
                        // L0 до отметки: `indexed` означает, что узел обработан целиком
                        self.auto_l0(db, j.target_id, j.rev, chunks, &embs).await;
                        if let Err(e) =
                            set_index_state(db, j.target_id, j.rev, IndexState::Indexed).await
                        {
                            tracing::warn!(node = %j.target_id, error = %e, "index_state");
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(job = j.id, error = %e, "complete_job"),
            }
            self.schedule_ancestors(user, db, j.target_id).await;
        }
        Ok(())
    }

    /// Точки узла в Qdrant: удалить старые, записать текущие.
    async fn apply(
        &self,
        user: Id,
        j: &Job,
        plan: &Plan,
        embs: &HashMap<String, Embedding>,
    ) -> Result<(), IndexError> {
        let pts = match plan {
            Plan::Delete => Vec::new(),
            Plan::Upsert(chunks) => {
                let mut pts = Vec::with_capacity(chunks.len());
                for c in chunks {
                    let e = embs.get(&c.text_hash).ok_or_else(|| {
                        IndexError::Other(format!("нет эмбеддинга чанка {}", c.id))
                    })?;
                    pts.push(chunk_point(user, c, e));
                }
                pts
            }
        };
        self.deps.vector.delete_node(user, j.target_id).await?;
        if !pts.is_empty() {
            self.deps.vector.upsert(pts).await?;
        }
        Ok(())
    }

    /// Эмбеддинги по ключам: кэш `embeddings`, промахи — через модель пачками по
    /// `batch_tokens` с уступкой между ними, результат сразу в кэш.
    async fn embed_cached(
        &self,
        db: &Db,
        items: Vec<EmbedItem>,
    ) -> Result<HashMap<String, Embedding>, EmbedFail> {
        if items.is_empty() {
            return Ok(HashMap::new());
        }
        let model = self.model();
        let mut keys: Vec<String> = items.iter().map(|i| i.key.clone()).collect();
        keys.sort();
        keys.dedup();
        let mut found = get_embeddings(db, &model, &keys)
            .await
            .map_err(|e| EmbedFail::Other(format!("кэш эмбеддингов: {e}")))?;
        let mut seen = HashSet::new();
        let misses: Vec<EmbedItem> = items
            .into_iter()
            .filter(|i| !found.contains_key(&i.key) && seen.insert(i.key.clone()))
            .collect();
        let mut start = 0;
        while start < misses.len() {
            if self.stopped() {
                return Err(EmbedFail::Stopped);
            }
            let mut end = start;
            let mut tokens = 0usize;
            while end < misses.len()
                && (end == start || tokens + misses[end].tokens <= self.cfg.batch_tokens)
            {
                tokens += misses[end].tokens;
                end += 1;
            }
            let inputs: Vec<String> = misses[start..end].iter().map(|i| i.input.clone()).collect();
            let emb = self.deps.embedder.clone();
            let n = inputs.len();
            let out = tokio::task::spawn_blocking(move || {
                let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
                emb.embed_passages(&refs)
            })
            .await
            .map_err(|e| EmbedFail::Other(format!("поток инференса: {e}")))?
            .map_err(EmbedFail::Other)?;
            if out.len() != n {
                return Err(EmbedFail::Other(format!(
                    "модель вернула {} векторов на {n} текстов",
                    out.len()
                )));
            }
            self.inner.embedded.fetch_add(n as u64, Ordering::SeqCst);
            tracing::debug!(texts = n, tokens, "инференс эмбеддингов");
            let batch: Vec<(String, Embedding)> = misses[start..end]
                .iter()
                .map(|i| i.key.clone())
                .zip(out)
                .collect();
            put_embeddings(db, &model, batch.clone())
                .await
                .map_err(|e| EmbedFail::Other(format!("кэш эмбеддингов: {e}")))?;
            found.extend(batch);
            start = end;
            if start < misses.len() {
                tokio::time::sleep(Duration::from_millis(self.cfg.yield_ms)).await;
            }
        }
        Ok(found)
    }

    /// Автоматический L0 заметки по центроиду её листовых чанков (§8.3). Ошибки не
    /// критичны: L0 остаётся первым абзацем.
    async fn auto_l0(
        &self,
        db: &Db,
        node: Id,
        rev: i64,
        chunks: &[IndexChunk],
        embs: &HashMap<String, Embedding>,
    ) {
        let leaves: Vec<(&str, &[f32])> = chunks
            .iter()
            .filter(|c| c.level == ChunkLevel::Chunk)
            .filter_map(|c| {
                embs.get(&c.text_hash)
                    .map(|e| (c.text.as_str(), &e.dense[..]))
            })
            .collect();
        if leaves.is_empty() {
            return;
        }
        match get_node(db, node).await {
            Ok(n) if n.l0_source.as_deref() == Some("auto") && n.current_rev == rev => {}
            _ => return,
        }
        let vecs: Vec<&[f32]> = leaves.iter().map(|l| l.1).collect();
        let Some(center) = rollup::centroid(&vecs) else {
            return;
        };
        let cands = rollup::candidate_sentences(&leaves, &center);
        let pick = match cands.len() {
            0 => return,
            1 => 0,
            _ => {
                let items: Vec<EmbedItem> = cands
                    .iter()
                    .map(|s| EmbedItem {
                        key: keyed_hash("l0-sentence", s),
                        input: s.clone(),
                        tokens: s.len() / 3 + 1,
                    })
                    .collect();
                let keys: Vec<String> = items.iter().map(|i| i.key.clone()).collect();
                let Ok(m) = self.embed_cached(db, items).await else {
                    return;
                };
                let vs: Vec<&[f32]> = keys
                    .iter()
                    .filter_map(|k| m.get(k).map(|e| &e.dense[..]))
                    .collect();
                if vs.len() != cands.len() {
                    return;
                }
                match rollup::best_candidate(&center, &vs) {
                    Some(i) => i,
                    None => return,
                }
            }
        };
        let emb = self.deps.embedder.clone();
        let l0 = rollup::truncate_tokens(&cands[pick], rollup::L0_MAX_TOKENS, &|s| {
            emb.count_tokens(s)
        });
        if let Err(e) = set_auto_l0(db, node, rev, &l0).await {
            tracing::warn!(node = %node, error = %e, "автоматический L0");
        }
    }

    /// Папки-предки узла (включая удалённые) — в очередь свёртки.
    async fn schedule_ancestors(&mut self, user: Id, db: &Db, node: Id) {
        let mut cur = match get_node(db, node).await {
            Ok(n) => n.parent_id,
            Err(_) => return,
        };
        // глубина дерева ограничена здравым смыслом; защита от цикла в повреждённых данных
        for _ in 0..256 {
            let Some(id) = cur else { break };
            self.rollups.schedule(user, id, self.cfg.rollup_debounce);
            cur = match get_node(db, id).await {
                Ok(n) => n.parent_id,
                Err(_) => break,
            };
        }
    }

    /// Созревшие свёртки: глубокие папки раньше (L0 родителя строится из L0 детей).
    async fn run_rollups(&mut self) -> Result<(), Halt> {
        let due = self.rollups.take_due();
        if due.is_empty() {
            return Ok(());
        }
        let mut by_user: HashMap<Id, Vec<Id>> = HashMap::new();
        for (u, f) in due {
            by_user.entry(u).or_default().push(f);
        }
        for (user, folders) in by_user {
            let Ok(db) = self.deps.users.get(user).await else {
                continue;
            };
            let mut depth: Vec<(usize, Id)> = Vec::new();
            for f in folders {
                if let Ok(n) = get_node(&db, f).await {
                    depth.push((n.path.matches('/').count(), f));
                }
            }
            depth.sort_by_key(|a| std::cmp::Reverse(a.0));
            let mut rest = depth.into_iter();
            while let Some((_, f)) = rest.next() {
                if self.stopped() {
                    return Err(Halt::Stopped);
                }
                if let Err(e) = self.rollup_folder(user, &db, f).await {
                    // папка и все несделанные — снова в очередь
                    self.rollups.schedule(user, f, self.cfg.rollup_debounce);
                    for (_, g) in rest {
                        self.rollups.schedule(user, g, self.cfg.rollup_debounce);
                    }
                    return Err(Halt::Vector(e));
                }
            }
        }
        Ok(())
    }

    /// L0 папки из L0 детей; точка папки в Qdrant (кроме Human). Ошибки хранилища
    /// пишутся в лог; ошибка доступности Qdrant возвращается.
    async fn rollup_folder(&self, user: Id, db: &Db, folder: Id) -> Result<(), IndexError> {
        let Ok(f) = get_node(db, folder).await else {
            return Ok(());
        };
        if f.kind != NodeKind::Folder {
            return Ok(());
        }
        let space = match get_space(db, f.space_id).await {
            Ok(Some(s)) => s,
            _ => return Ok(()),
        };
        let human = space.kind == SpaceKind::Human;
        if f.deleted_at.is_some() {
            if !human {
                return ok_or_vector(self.deps.vector.delete_node(user, folder).await);
            }
            return Ok(());
        }
        let children = match list_children(db, f.space_id, Some(folder), 1).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(folder = %folder, error = %e, "дети папки");
                return Ok(());
            }
        };
        let kids: Vec<FolderChild> = children
            .into_iter()
            // `index: false` не попадает и в свёртку: иначе его L0 ушёл бы в вектор папки
            .filter(|c| {
                !(c.kind == NodeKind::Note && c.index_state == IndexState::Excluded.as_str())
            })
            .map(|c| FolderChild {
                folder: c.kind == NodeKind::Folder,
                name: c.name,
                l0: c.l0,
                updated_at: c.updated_at.as_second(),
            })
            .collect();
        let l0 = rollup::folder_l0(&kids);
        if let Err(e) = set_folder_l0(db, folder, l0.as_deref()).await {
            tracing::warn!(folder = %folder, error = %e, "L0 папки");
            return Ok(());
        }
        if human {
            return Ok(());
        }
        let Some(text) = l0 else {
            return ok_or_vector(self.deps.vector.delete_node(user, folder).await);
        };
        let input = format!("[{}/{}] {}\n{text}", space.name, f.path, f.name);
        let key = keyed_hash("folder", &input);
        let item = EmbedItem {
            key: key.clone(),
            input,
            tokens: text.len() / 3 + 1,
        };
        let e = match self.embed_cached(db, vec![item]).await {
            Ok(mut m) => match m.remove(&key) {
                Some(e) => e,
                None => return Ok(()),
            },
            Err(EmbedFail::Stopped) => return Ok(()),
            Err(EmbedFail::Other(e)) => {
                tracing::warn!(folder = %folder, error = %e, "эмбеддинг L0 папки");
                return Ok(());
            }
        };
        let point = IndexPoint {
            chunk_id: folder_chunk_id(folder),
            dense: e.dense,
            sparse: e.sparse,
            payload: PointPayload {
                user_id: user,
                space_id: f.space_id,
                space_kind: space.kind.as_str().to_string(),
                solution_guid: space.solution_guid.clone(),
                node_id: folder,
                path_ancestors: path_ancestors(&f.path),
                area: None,
                level: "folder".into(),
                kind: "folder".into(),
                tags: Vec::new(),
                updated_at: f.updated_at.as_second(),
                rev: 0,
                text_hash: key,
            },
        };
        ok_or_vector(self.deps.vector.upsert(vec![point]).await)
    }
}

/// Ошибка инференса или кэша.
enum EmbedFail {
    Stopped,
    Other(String),
}

/// Недоступность Qdrant — наверх (ворота), прочее — в лог.
fn ok_or_vector(r: Result<(), IndexError>) -> Result<(), IndexError> {
    match r {
        Err(e @ (IndexError::Unavailable(_) | IndexError::NotFound(_))) => Err(e),
        Err(e) => {
            tracing::warn!(error = %e, "точка папки в Qdrant");
            Ok(())
        }
        Ok(()) => Ok(()),
    }
}

async fn state_changed(rx: &mut Option<watch::Receiver<QdrantState>>) {
    match rx {
        Some(r) => {
            if r.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
        None => std::future::pending::<()>().await,
    }
}

/// Текст для модели: `context_prefix` участвует в эмбеддинге (§8.2 п. 6).
pub fn embed_input(c: &IndexChunk) -> String {
    if c.context_prefix.is_empty() {
        c.text.clone()
    } else {
        format!("{}\n{}", c.context_prefix, c.text)
    }
}

/// Ключ кэша `embeddings` для текста, которого нет среди чанков (предложение L0,
/// L0 папки): домен отделяет его от `text_hash` чанков.
fn keyed_hash(domain: &str, text: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(domain.as_bytes());
    h.update(b"\n");
    h.update(text.as_bytes());
    h.finalize().to_hex().to_string()
}

/// Детерминированный id чанка папки: одна точка на папку.
pub fn folder_chunk_id(folder: Id) -> String {
    let mut h = blake3::Hasher::new();
    h.update(folder.as_uuid().as_bytes());
    h.update(b"folder");
    h.finalize().as_bytes()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn path_ancestors(path: &str) -> Vec<String> {
    path.match_indices('/')
        .map(|(i, _)| path[..i].to_string())
        .chain(std::iter::once(path.to_string()))
        .collect()
}

/// Точка чанка заметки.
fn chunk_point(user: Id, c: &IndexChunk, e: &Embedding) -> IndexPoint {
    IndexPoint {
        chunk_id: c.id.clone(),
        dense: e.dense.clone(),
        sparse: e.sparse.clone(),
        payload: PointPayload {
            user_id: user,
            space_id: c.node.space_id,
            space_kind: c.node.space_kind.as_str().to_string(),
            solution_guid: c.node.solution_guid.clone(),
            node_id: c.node.node_id,
            path_ancestors: c.node.path_ancestors.clone(),
            area: None,
            level: c.level.as_str().to_string(),
            kind: "note".into(),
            tags: c.node.tags.clone(),
            updated_at: c.node.updated_at.as_second(),
            rev: c.rev,
            text_hash: c.text_hash.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollup_debounce_is_trailing_and_capped() {
        let mut r = Rollups::default();
        let (u, f) = (Id::new(), Id::new());
        r.schedule(u, f, Duration::from_millis(0));
        assert_eq!(r.take_due(), vec![(u, f)]);
        assert!(r.take_due().is_empty());
        r.schedule(u, f, Duration::from_secs(60));
        assert!(r.take_due().is_empty());
        assert!(r.next_due().unwrap() > Instant::now());
    }

    #[test]
    fn folder_chunk_id_is_stable_hex() {
        let f = Id::new();
        assert_eq!(folder_chunk_id(f), folder_chunk_id(f));
        assert_eq!(folder_chunk_id(f).len(), 32);
        assert_ne!(folder_chunk_id(f), folder_chunk_id(Id::new()));
        assert_ne!(keyed_hash("a", "x"), keyed_hash("b", "x"));
    }
}
