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
//!    известных текстов). Сбой одной базы не останавливает остальные: она пропускается
//!    и повторяется позже, уже обработанные базы повторно не перечанковываются;
//!    индексация идёт и во время повторов. База, повреждённая навсегда (файл не SQLite,
//!    схема новее) или не открывшаяся [`REBUILD_MAX_ATTEMPTS`] раз подряд, исключается из
//!    пересборки: WARN, аудит `index.rebuild_skipped`, видна в `/index/status`
//!    ([`WorkerStatus::rebuild_skipped`]). Исключение хранится в `system.db`
//!    (`index_skipped`) и переживает перезапуск; когда база снова откроется, в ней
//!    ставится отметка reindex из кэша, исключение снимается. Для `storage_rebuilt` аудит `index.storage_rebuilt` и
//!    `ack_storage_rebuilt()` — когда прошли (или исключены) все базы: флаг
//!    `.zam-reindex` не держит полную перечанковку на каждом рестарте из-за одной базы.
//! 3. **Reindex по запросу** ([`WorkerHandle::request_reindex`], `POST /admin/reindex`):
//!    [`crate::reindex`] — из кэша без инференса или полный; выполняется здесь же,
//!    последовательно с заданиями, в том числе при чужой модели коллекции (её он и
//!    пересоздаёт). Прерванный reindex базы (отметка `reindex_pending`) повторяется при
//!    первом открытии базы после запуска.
//! 4. **Задания.** Базы пользователей по кругу, пачками по `batch_jobs`: чанки узла →
//!    кэш `embeddings` по `chunks.embed_key` → инференс промахов (`spawn_blocking`, пачки
//!    не больше `batch_tokens`, уступка `yield_ms`) → одна запись в Qdrant на пачку:
//!    `delete_nodes` + `upsert` (`wait=true`; отказ пачки — по одному узлу) →
//!    `complete_job` → `index_state = 'indexed'` только при [`JobOutcome::Done`].
//!    Qdrant недоступен или коллекции нет — задания возвращаются [`release_job`] без
//!    траты попыток; прочие ошибки — [`fail_job`] (backoff, после 5 — `failed`).
//! 5. **L0** (§8.3): автоматический L0 заметки — предложение у центроида её чанков;
//!    папки-предки — свёртка L0 детей с дебаунсом `rollup_debounce`, папка индексируется
//!    точкой уровня `folder`.
//! 6. **Сборка мусора кэша** в простое: раз в `gc_interval` на базу пачка
//!    [`gc_embeddings`] (до [`GC_BATCH`] строк; полная пачка — следующая в ближайший простой).
//!
//! Текст эмбеддинга чанка — [`embed_input`]: `context_prefix + "\n" + text` (§8.2 п. 6);
//! ключ кэша — [`embed_key`]: blake3 ровно этой строки, формула одна
//! ([`zam_store::embed_key_of`]) и хранится в `chunks.embed_key`. Одинаковая
//! эмбеддируемая строка считается моделью один раз; тот же текст под другим префиксом
//! (другая заметка) — отдельный вектор. Ключи предложений L0 и L0 папки — со своим
//! доменом и владельцем-узлом (`embeddings.owner`): устаревшие снимаются при пересчёте L0.
//! Перенос узла: API сообщает старого родителя ([`WorkerHandle::rollup_hint`]), его
//! свёртка пересчитывается. В лог не попадают тексты, векторы и ключ Qdrant.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use tokio::sync::{Notify, watch};
use zam_core::Id;
use zam_markdown::ChunkLevel;
use zam_store::{
    Db, Embedding, IndexChunk, IndexFolder, IndexState, Job, JobOp, JobOutcome, NodeKind,
    SpaceKind, StoreError, SystemStore, UserDbs, adopt_owned_embeddings, chunks_for_index,
    claim_jobs, complete_job, enqueue_reindex, fail_job, gc_embeddings, get_embeddings, get_node,
    get_space, indexed_count, list_children, pending_reindex, prune_owned_embeddings,
    put_embeddings, put_owned_embeddings, release_job, requeue_running, set_auto_l0, set_folder_l0,
    set_index_state,
};

use crate::reindex::{GC_BATCH, ReindexCtx, ReindexReport, ReindexRequest, reindex};
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
    /// Суммарное время, которое эмбеддер уступил запросам внутри `embed_passages` (не
    /// инференс; вычитается из `embed_ms`). По умолчанию — уступки нет.
    fn yield_wait(&self) -> Duration {
        Duration::ZERO
    }
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
    /// Период сборки мусора кэша `embeddings` на базу (в простое).
    pub gc_interval: Duration,
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
            gc_interval: Duration::from_secs(3600),
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
    /// Папок, ждущих свёртки L0 (дебаунс или выполняется).
    pub rollups_pending: usize,
    /// Пачка заданий в обработке: от выдачи до конца пачки, включая auto-L0 и отметку
    /// `indexed` после закрытия заданий. Вместе с пустой очередью и `rollups_pending == 0`
    /// означает, что воркер в покое.
    pub batch_running: bool,
    /// Reindex по запросу выполняется сейчас.
    pub reindex_running: bool,
    /// Запросов reindex в очереди, включая выполняемый.
    pub reindex_queued: usize,
    /// Последний завершённый reindex по запросу.
    pub last_reindex: Option<ReindexOutcome>,
    /// Базы, исключённые из пересборки индекса (повреждены или не открываются).
    pub rebuild_skipped: Vec<SkippedDb>,
}

/// Итог reindex по запросу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReindexOutcome {
    pub request: ReindexRequest,
    pub finished_at: Timestamp,
    /// Отчёт или ошибка (без текста заметок).
    pub result: Result<ReindexReport, String>,
}

/// База пользователя, исключённая из пересборки индекса.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedDb {
    pub user: Id,
    pub error: String,
    pub attempts: u32,
}

struct Inner {
    status: Mutex<WorkerStatus>,
    notify: Notify,
    rebuild: AtomicBool,
    /// Запросы reindex по порядку поступления.
    reindex: Mutex<VecDeque<ReindexRequest>>,
    embedded: AtomicU64,
    jobs_done: AtomicU64,
    /// Заданий, возвращённых в очередь из-за недоступности Qdrant при записи пачки.
    jobs_requeued: AtomicU64,
    /// Суммарное время инференса и записи в Qdrant, мкс (диагностика, `/index/status`).
    embed_us: AtomicU64,
    /// Ожидание уступки запросам внутри инференса, мкс (в `embed_us` не входит).
    yield_us: AtomicU64,
    qdrant_us: AtomicU64,
    /// Папки, свёртку которых нужно пересчитать (старый родитель при переносе).
    /// Множество: без воркера (FTS-only) подсказки не забираются, а размер ограничен
    /// числом папок.
    hints: Mutex<HashSet<(Id, Id)>>,
    stop: watch::Sender<bool>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Точка остановки тестов перед записью пачки в Qdrant (фича `test-hooks`).
    #[cfg(feature = "test-hooks")]
    hold: crate::test_hooks::Hold,
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
                reindex: Mutex::default(),
                embedded: AtomicU64::new(0),
                jobs_done: AtomicU64::new(0),
                jobs_requeued: AtomicU64::new(0),
                embed_us: AtomicU64::new(0),
                yield_us: AtomicU64::new(0),
                qdrant_us: AtomicU64::new(0),
                hints: Mutex::default(),
                stop,
                task: Mutex::new(None),
                #[cfg(feature = "test-hooks")]
                hold: crate::test_hooks::Hold::default(),
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
            pending: None,
            last_q: None,
            bad_users: HashMap::new(),
            reindex_retry: None,
            skipped_dbs: HashSet::new(),
            pending_checked: HashSet::new(),
            scanned: false,
            gc_due: HashMap::new(),
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

    /// Сколько заданий возвращено в очередь из-за недоступности Qdrant (`Unavailable`
    /// или пропавшая коллекция при записи пачки): запись не подтверждена, задание не
    /// закрыто и не стало неудачным — повтор после возврата Qdrant.
    pub fn jobs_requeued(&self) -> u64 {
        self.inner.jobs_requeued.load(Ordering::SeqCst)
    }

    /// Суммарное ожидание уступки запросам внутри инференса с запуска (в [`Self::busy`] не
    /// входит).
    pub fn yield_wait(&self) -> Duration {
        Duration::from_micros(self.inner.yield_us.load(Ordering::SeqCst))
    }

    /// Суммарное время с запуска: `(инференс, запись в Qdrant)` — где воркер тратит время.
    pub fn busy(&self) -> (Duration, Duration) {
        (
            Duration::from_micros(self.inner.embed_us.load(Ordering::SeqCst)),
            Duration::from_micros(self.inner.qdrant_us.load(Ordering::SeqCst)),
        )
    }

    /// Пересчитать свёртку L0 папки `folder` пользователя `user` и её предков (с дебаунсом):
    /// API вызывает для старого родителя при переносе или переименовании узла — задание
    /// индексации знает только новое положение.
    pub fn rollup_hint(&self, user: Id, folder: Id) {
        lock(&self.inner.hints).insert((user, folder));
        self.notify();
    }

    /// Пересобрать индекс: `ensure` заново и `enqueue_reindex` во всех базах (после
    /// `drop_all` или смены модели). Выполняется воркером асинхронно.
    pub fn request_rebuild(&self) {
        self.inner.rebuild.store(true, Ordering::SeqCst);
        self.notify();
    }

    /// Пересобрать индекс области ([`crate::reindex`]): из кэша `embeddings` без
    /// инференса или полностью (`full`). Выполняется воркером асинхронно, по порядку
    /// запросов; итог — [`WorkerStatus::last_reindex`].
    pub fn request_reindex(&self, req: ReindexRequest) {
        let n = {
            let mut q = lock(&self.inner.reindex);
            q.push_back(req);
            q.len()
        };
        lock(&self.inner.status).reindex_queued = n;
        self.notify();
    }

    /// Останавливает воркер и ждёт задачу (не дольше минуты: пачка инференса
    /// не прерывается). Повторный вызов — без действия.
    pub async fn shutdown(&self) {
        let _ = self.inner.stop.send(true);
        self.inner.notify.notify_one();
        let task = lock(&self.inner.task).take();
        if let Some(mut t) = task {
            match tokio::time::timeout(STOP_TIMEOUT, &mut t).await {
                Err(_) => {
                    tracing::warn!("воркер индексации не остановился за минуту, задача прервана");
                    t.abort();
                }
                Ok(Err(e)) if e.is_panic() => {
                    tracing::error!(error = %e, "задача воркера индексации завершилась паникой");
                }
                Ok(_) => {}
            }
        }
        let mut st = lock(&self.inner.status);
        st.running = false;
        st.vector_ready = false;
    }
}

/// Прибавляет к счётчику время с `t`, мкс.
fn add_us(c: &AtomicU64, t: Instant) {
    c.fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
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
    /// Незавершённая пересборка: базы, которые ещё не прошли `enqueue_reindex`.
    pending: Option<Rebuild>,
    /// Последнее увиденное состояние Qdrant (смена — повод проверить ворота сразу).
    last_q: Option<QdrantState>,
    /// Базы, которые не открылись: следующая попытка не раньше срока (без шума в журнале).
    bad_users: HashMap<Id, Instant>,
    /// Повтор reindex по запросу после недоступности Qdrant не раньше срока.
    reindex_retry: Option<Instant>,
    /// Базы, исключённые из пересборки (`system.db`): при успешном открытии — их reindex.
    skipped_dbs: HashSet<Id>,
    /// Базы, в которых после запуска проверена отметка незавершённого reindex.
    pending_checked: HashSet<Id>,
    /// Все базы открыты (проверены отметки) хотя бы раз после запуска.
    scanned: bool,
    /// Следующая сборка мусора кэша базы.
    gc_due: HashMap<Id, Instant>,
}

/// Пересборка, идущая по базам.
struct Rebuild {
    /// Хранилище Qdrant стёрто: по завершении — аудит и `ack_storage_rebuilt`.
    storage: bool,
    /// Базы, уже поставленные в очередь.
    done: HashSet<Id>,
    /// Поставлено заданий.
    jobs: u64,
    started: Instant,
    /// Следующая попытка для неудавшихся баз.
    next_try: Instant,
    /// Неудачных попыток по базам.
    attempts: HashMap<Id, u32>,
    /// Исключены из пересборки (повреждены или не открылись [`REBUILD_MAX_ATTEMPTS`] раз).
    skipped: Vec<Id>,
}

/// Задача воркера завершилась (штатно или паникой): векторный канал выключен, иначе
/// `vector_available` продолжал бы отвечать «готов».
struct ExitGuard(Arc<Inner>);

impl Drop for ExitGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            tracing::error!("воркер индексации завершился паникой: векторный поиск выключен");
        }
        let mut st = lock(&self.0.status);
        st.running = false;
        st.vector_ready = false;
    }
}

/// Пауза между попытками для базы, которая не открылась.
const BAD_USER_RETRY: Duration = Duration::from_secs(60);

/// Неудачных попыток базы в пересборке, после которых она исключается (повреждённая
/// навсегда исключается сразу).
pub const REBUILD_MAX_ATTEMPTS: u32 = 5;

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
        // состояние сменилось, пока воркер был занят (сигнал `changed` уже мог быть
        // поглощён другим ожиданием): сразу назад, к воротам
        if self.q_changed() {
            return Wake::State;
        }
        let mut stop = self.stop.clone();
        let mut q = self.qstate.clone();
        let inner = self.inner.clone();
        let reason = tokio::select! {
            _ = stop.changed() => Wake::Stop,
            () = state_changed(&mut q) => Wake::State,
            () = inner.notify.notified(), if wake => Wake::Notify,
            () = tokio::time::sleep_until(until.into()) => Wake::Timer,
        };
        if reason == Wake::Stop {
            return reason;
        }
        // сравнение явное: какая бы ветка ни сработала, смена состояния не теряется
        if self.q_changed() {
            Wake::State
        } else {
            reason
        }
    }

    /// Состояние Qdrant отличается от последнего увиденного (и отмечается увиденным).
    fn q_changed(&mut self) -> bool {
        let Some(rx) = &mut self.qstate else {
            return false;
        };
        let cur = rx.borrow_and_update().clone();
        if self.last_q.as_ref() == Some(&cur) {
            return false;
        }
        let first = self.last_q.is_none();
        self.last_q = Some(cur);
        !first
    }

    async fn run(mut self) {
        let _guard = ExitGuard(self.inner.clone());
        self.q_changed();
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
            if requested {
                // новый запрос — заново по всем базам
                self.start_rebuild(storage);
                ensured = false;
            } else if storage && self.pending.is_none() {
                self.start_rebuild(true);
                ensured = false;
            }
            if self
                .pending
                .as_ref()
                .is_some_and(|p| Instant::now() >= p.next_try)
            {
                self.rebuild_step().await;
            }
            // 2. При запуске: исключённые базы из system.db и обход всех баз — отметки
            // незавершённого reindex (в том числе при чужой модели коллекции, когда до
            // заданий дело не доходит) и восстановленные исключённые базы
            if !self.scanned {
                self.scanned = true;
                self.load_skipped().await;
                for u in self.users().await {
                    let _ = self.open_db(u).await;
                }
            }
            // 2a. Reindex по запросу (и при чужой модели коллекции: он её пересоздаёт)
            if self.reindex_retry.is_none_or(|t| Instant::now() >= t)
                && let Some(req) = self.next_reindex()
            {
                match self.run_reindex(req).await {
                    Ok(()) => {
                        self.reindex_retry = None;
                        // статус коллекции (модель, готовность) — заново через ensure
                        ensured = false;
                        retry_at = None;
                        backoff = Duration::from_millis(500);
                    }
                    Err(e) => {
                        ensured = false;
                        self.vector_down(&e);
                        let t = Instant::now() + backoff;
                        retry_at = Some(t);
                        self.reindex_retry = Some(t);
                        backoff = (backoff * 2).min(self.cfg.retry_max);
                        continue;
                    }
                }
            }
            // 3. Коллекция
            if !ensured {
                if let Some(t) = retry_at
                    && Instant::now() < t
                {
                    // смена состояния Qdrant (перезапуск, готовность) или запрос пересборки
                    // (он приходит через notify) — проверить сразу; запись узла — ждать срока
                    let woke = self.wait(t, true).await;
                    if self.has_reindex() && self.reindex_retry.is_none() {
                        continue;
                    }
                    if woke != Wake::State && !self.inner.rebuild.load(Ordering::SeqCst) {
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
            self.take_hints().await;
            let rolled = self.run_rollups().await;
            let left = self.rollups.due.len();
            self.status(|s| s.rollups_pending = left);
            match rolled {
                Ok(()) => {}
                Err(Halt::Stopped) => break,
                Err(Halt::Vector(e)) => {
                    ensured = false;
                    self.vector_down(&e);
                    retry_at = Some(Instant::now() + backoff);
                    backoff = (backoff * 2).min(self.cfg.retry_max);
                    continue;
                }
            }
            if did {
                tokio::time::sleep(Duration::from_millis(self.cfg.yield_ms)).await;
            } else {
                // простой: сборка мусора кэша
                if self.gc_tick().await {
                    continue;
                }
                let mut until = Instant::now() + self.cfg.idle_poll;
                if let Some(d) = self.rollups.next_due() {
                    until = until.min(d);
                }
                if let Some(p) = &self.pending {
                    until = until.min(p.next_try);
                }
                self.wait(until, true).await;
            }
        }
        tracing::info!("воркер индексации остановлен");
    }

    fn has_reindex(&self) -> bool {
        !lock(&self.inner.reindex).is_empty()
    }

    /// Следующий запрос reindex (остаётся в очереди до успешного выполнения).
    fn next_reindex(&self) -> Option<ReindexRequest> {
        lock(&self.inner.reindex).front().cloned()
    }

    /// Выполняет первый запрос reindex. Недоступность Qdrant — `Err` (запрос остаётся в
    /// очереди, повтор после backoff); прочие ошибки — в итог запроса.
    async fn run_reindex(&mut self, req: ReindexRequest) -> Result<(), IndexError> {
        self.status(|s| s.reindex_running = true);
        let model = self.model();
        let ctx = ReindexCtx {
            system: &self.deps.system,
            users: &self.deps.users,
            vector: self.deps.vector.as_ref(),
            model_id: &model,
            dim: self.deps.embedder.dim(),
            embedder: None,
        };
        let res = reindex(&ctx, &req).await;
        self.status(|s| s.reindex_running = false);
        let result = match res {
            Err(e @ (IndexError::Unavailable(_) | IndexError::NotFound(_))) => {
                tracing::warn!(error = %e, "reindex прерван: Qdrant недоступен, повтор позже");
                return Err(e);
            }
            Err(e) => {
                tracing::warn!(error = %e, "reindex не выполнен");
                Err(e.to_string())
            }
            Ok(rep) => {
                // L0 папок, которых нет в кэше, — свёрткой (она же построит точку)
                for (u, f) in &rep.missed_folders {
                    self.rollups.schedule(*u, *f, self.cfg.rollup_debounce);
                }
                Ok(rep)
            }
        };
        let left = {
            let mut q = lock(&self.inner.reindex);
            q.pop_front();
            q.len()
        };
        let outcome = ReindexOutcome {
            request: req,
            finished_at: Timestamp::now(),
            result,
        };
        if let Ok(rep) = &outcome.result {
            let audit = self
                .deps
                .system
                .audit(
                    "system",
                    outcome.request.user,
                    "index.reindex_done",
                    None,
                    None,
                    serde_json::json!({
                        "full": rep.full, "users": rep.users, "nodes": rep.nodes,
                        "points": rep.points, "misses": rep.misses, "jobs": rep.jobs,
                        "gc_deleted": rep.gc_deleted, "recreated": rep.recreated,
                        "failed_users": rep.failed_users.len(), "elapsed_ms": rep.elapsed_ms,
                    }),
                )
                .await;
            if let Err(e) = audit {
                tracing::warn!(error = %e, "аудит index.reindex_done не записан");
            }
        }
        self.status(|s| {
            s.reindex_queued = left;
            s.last_reindex = Some(outcome);
        });
        // базы, которые reindex не открыл, он отметил в system.db
        self.load_skipped().await;
        Ok(())
    }

    /// Сборка мусора кэша в простое: одна пачка на базу, у которой подошёл срок.
    /// `true` — пачка была полной (мусор ещё есть: продолжить без ожидания).
    async fn gc_tick(&mut self) -> bool {
        let now = Instant::now();
        let mut more = false;
        for u in self.users().await {
            if self.stopped() || self.gc_due.get(&u).is_some_and(|t| now < *t) {
                continue;
            }
            let Some(db) = self.open_db(u).await else {
                continue;
            };
            let next = match gc_embeddings(&db, GC_BATCH).await {
                Ok(n) if n >= GC_BATCH as u64 => {
                    more = true;
                    tracing::debug!(user = %u, deleted = n, "сборка мусора кэша эмбеддингов");
                    now
                }
                Ok(n) => {
                    if n > 0 {
                        tracing::debug!(user = %u, deleted = n, "сборка мусора кэша эмбеддингов");
                    }
                    now + self.cfg.gc_interval
                }
                Err(e) => {
                    tracing::warn!(user = %u, error = %e, "сборка мусора кэша эмбеддингов");
                    now + self.cfg.gc_interval
                }
            };
            self.gc_due.insert(u, next);
        }
        more
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
                if let Some(db) = self.open_db(u).await {
                    lost += indexed_count(&db).await.unwrap_or(0);
                }
            }
            if lost > 0 && self.pending.is_none() {
                tracing::warn!(
                    nodes = lost,
                    "коллекция Qdrant создана заново: пересборка из embeddings"
                );
                self.start_rebuild(false);
                self.rebuild_step().await;
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

    /// Начинает пересборку по всем базам (прежняя незавершённая заменяется; флаг
    /// `storage` сохраняется, пока стирание хранилища не подтверждено).
    fn start_rebuild(&mut self, storage: bool) {
        let storage = storage || self.pending.as_ref().is_some_and(|p| p.storage);
        self.pending = Some(Rebuild {
            storage,
            done: HashSet::new(),
            jobs: 0,
            started: Instant::now(),
            next_try: Instant::now(),
            attempts: HashMap::new(),
            skipped: Vec::new(),
        });
    }

    /// Шаг пересборки: `enqueue_reindex` в каждой ещё не обработанной базе. Сбой базы —
    /// WARN и повтор позже только для неё. Когда прошли все базы: для стёртого хранилища
    /// аудит `index.storage_rebuilt` и `ack_storage_rebuilt()`.
    async fn rebuild_step(&mut self) {
        let Some(mut p) = self.pending.take() else {
            return;
        };
        let users = match self.deps.system.list_users().await {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(error = %e, "пересборка: список пользователей недоступен");
                p.next_try = Instant::now() + self.cfg.retry_max;
                self.pending = Some(p);
                return;
            }
        };
        let mut failed = 0usize;
        for u in &users {
            if p.done.contains(&u.id) || self.stopped() {
                continue;
            }
            let res = match self.deps.users.get(u.id).await {
                Ok(db) => enqueue_reindex(&db, None).await,
                Err(e) => Err(e),
            };
            match res {
                Ok(n) => {
                    p.jobs += n;
                    p.done.insert(u.id);
                }
                Err(e) => {
                    let n = p.attempts.entry(u.id).or_insert(0);
                    *n += 1;
                    let attempts = *n;
                    if e.is_permanent() || attempts >= REBUILD_MAX_ATTEMPTS {
                        p.done.insert(u.id);
                        p.skipped.push(u.id);
                        self.skip_db(u.id, &e, attempts, p.storage).await;
                    } else {
                        failed += 1;
                        tracing::warn!(
                            user = %u.id,
                            error = %e,
                            attempts,
                            "пересборка: база пользователя пропущена, повтор позже"
                        );
                    }
                }
            }
        }
        if failed > 0 || self.stopped() {
            p.next_try = Instant::now() + self.cfg.retry_max;
            self.pending = Some(p);
            return;
        }
        tracing::info!(
            users = users.len(),
            skipped = p.skipped.len(),
            jobs = p.jobs,
            elapsed_ms = p.started.elapsed().as_millis() as u64,
            "пересборка индекса поставлена в очередь"
        );
        if p.storage {
            let audit = self
                .deps
                .system
                .audit(
                    "system",
                    None,
                    "index.storage_rebuilt",
                    None,
                    None,
                    serde_json::json!({
                        "users": users.len(),
                        "jobs": p.jobs,
                        "skipped": p.skipped.iter().map(Id::to_string).collect::<Vec<_>>(),
                    }),
                )
                .await;
            if let Err(e) = audit {
                tracing::warn!(error = %e, "аудит index.storage_rebuilt не записан, повтор");
                p.next_try = Instant::now() + self.cfg.retry_max;
                self.pending = Some(p);
                return;
            }
            if let Some(s) = &self.deps.supervisor
                && let Err(e) = s.ack_storage_rebuilt()
            {
                tracing::warn!(error = %e, "флаг пересборки хранилища не снят");
            }
        }
    }

    /// База исключена из пересборки: WARN, аудит `index.rebuild_skipped`, статус. Когда
    /// она снова откроется ([`Self::open_db`]), её reindex поставится сам.
    async fn skip_db(&mut self, user: Id, e: &StoreError, attempts: u32, storage: bool) {
        tracing::warn!(
            user = %user,
            error = %e,
            attempts,
            "пересборка индекса: база пользователя исключена (повреждена или не открывается); \
             после восстановления базы выполните `zam-server reindex --user {user}`"
        );
        self.skipped_dbs.insert(user);
        let err = e.to_string();
        if let Err(se) = self.deps.system.index_skip(user, &err, attempts).await {
            tracing::warn!(user = %user, error = %se, "отметка исключённой базы не записана");
        }
        let audit = self
            .deps
            .system
            .audit(
                "system",
                Some(user),
                "index.rebuild_skipped",
                Some(&user.to_string()),
                None,
                serde_json::json!({ "error": err, "attempts": attempts, "storage": storage }),
            )
            .await;
        if let Err(a) = audit {
            tracing::warn!(error = %a, "аудит index.rebuild_skipped не записан");
        }
        self.status(|s| {
            s.rebuild_skipped.retain(|d| d.user != user);
            s.rebuild_skipped.push(SkippedDb {
                user,
                error: err,
                attempts,
            });
        });
    }

    /// Исключённые из пересборки базы из `system.db` (переживают перезапуск) — в память
    /// воркера и статус.
    async fn load_skipped(&mut self) {
        match self.deps.system.index_skipped().await {
            Ok(list) => {
                self.skipped_dbs = list.iter().map(|s| s.user_id).collect();
                self.status(|s| {
                    s.rebuild_skipped = list
                        .into_iter()
                        .map(|d| SkippedDb {
                            user: d.user_id,
                            error: d.error,
                            attempts: d.attempts,
                        })
                        .collect();
                });
            }
            Err(e) => tracing::warn!(error = %e, "список исключённых из пересборки баз"),
        }
    }

    /// Исключённая база снова открылась: в ней ставится отметка reindex из кэша (её
    /// подхватит проверка незавершённого reindex, после перезапуска тоже), затем отметка
    /// исключения в `system.db` снимается. Сбой — повтор при следующем открытии.
    async fn recover_skipped(&mut self, user: Id, db: &Db) {
        let req = ReindexRequest {
            user: Some(user),
            space: None,
            full: false,
        };
        let res = match zam_store::mark_reindex(db, Some(req.spec(None))).await {
            Ok(()) => self.deps.system.index_unskip(user).await,
            Err(e) => Err(e),
        };
        match res {
            Ok(()) => {
                tracing::info!(
                    user = %user,
                    "база, исключённая из пересборки, снова открылась: reindex"
                );
                self.skipped_dbs.remove(&user);
                // отметку reindex прочитать заново
                self.pending_checked.remove(&user);
                self.status(|s| s.rebuild_skipped.retain(|d| d.user != user));
            }
            Err(e) => tracing::warn!(user = %user, error = %e, "reindex исключённой базы"),
        }
    }

    /// База пользователя; не открывшаяся — WARN не чаще раза в минуту и пропуск.
    /// Открывшаяся база, исключённая ранее из пересборки, получает свой reindex.
    async fn open_db(&mut self, user: Id) -> Option<Db> {
        if self
            .bad_users
            .get(&user)
            .is_some_and(|t| Instant::now() < *t)
        {
            return None;
        }
        match self.deps.users.get(user).await {
            Ok(db) => {
                self.bad_users.remove(&user);
                if self.skipped_dbs.contains(&user) {
                    self.recover_skipped(user, &db).await;
                }
                // незавершённый reindex базы (отметка в ней самой) — в очередь; проверяется
                // при первом открытии после запуска, до ворот коллекции: так возобновляется
                // и reindex, прерванный при чужой модели коллекции
                if self.pending_checked.insert(user) {
                    match pending_reindex(&db).await {
                        Ok(Some(spec)) => {
                            tracing::info!(user = %user, "незавершённый reindex базы: повтор");
                            let req = ReindexRequest::from_spec(user, &spec);
                            let n = {
                                let mut q = lock(&self.inner.reindex);
                                q.push_back(req);
                                q.len()
                            };
                            self.status(|s| s.reindex_queued = n);
                        }
                        Ok(None) => {}
                        Err(e) => {
                            self.pending_checked.remove(&user);
                            tracing::warn!(user = %user, error = %e, "отметка reindex");
                        }
                    }
                }
                Some(db)
            }
            Err(e) => {
                tracing::warn!(user = %user, error = %e, "база пользователя не открылась, пропуск");
                self.bad_users.insert(user, Instant::now() + BAD_USER_RETRY);
                None
            }
        }
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
            let Some(db) = self.open_db(user).await else {
                continue;
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
            self.status(|s| s.batch_running = true);
            let res = self.batch(user, &db, jobs).await;
            self.status(|s| s.batch_running = false);
            res?;
            tokio::time::sleep(Duration::from_millis(self.cfg.yield_ms)).await;
        }
        *rr = rr.wrapping_add(1);
        Ok(did)
    }

    /// Возвращает задания в очередь; итог — сколько возвращено (устаревший захват —
    /// задание уже перехвачено — не считается).
    async fn release_all(&self, db: &Db, jobs: &[Job]) -> u64 {
        let mut n = 0;
        for j in jobs {
            match release_job(db, j.id, j.claim_gen).await {
                Ok(JobOutcome::Requeued) => n += 1,
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(job = j.id, error = %e, "задание не возвращено в очередь")
                }
            }
        }
        n
    }

    /// Возвращает задания пачки в очередь из-за недоступности Qdrant (счётчик
    /// [`WorkerHandle::jobs_requeued`]).
    async fn requeue_unavailable(&self, db: &Db, jobs: &[Job]) {
        let n = self.release_all(db, jobs).await;
        self.inner.jobs_requeued.fetch_add(n, Ordering::SeqCst);
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
                key: c.embed_key.clone(),
                input: embed_input(c),
                tokens: c.token_count.max(1) as usize + c.context_prefix.len() / 3,
            })
            .collect();
        let embs = match self.embed_cached(db, items, None).await {
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
        // точки пачки (считаются один раз); чанк без эмбеддинга — неудача только своего
        // задания
        let mut ready: Vec<(Job, Plan)> = Vec::with_capacity(plans.len());
        let mut pts: Vec<IndexPoint> = Vec::new();
        for (j, plan) in plans {
            match plan_points(user, &plan, &embs) {
                Ok(p) => {
                    pts.extend(p);
                    ready.push((j, plan));
                }
                Err(e) => self.fail(db, &j, &e.to_string()).await,
            }
        }
        if self.stopped() {
            let back: Vec<Job> = ready.into_iter().map(|(j, _)| j).collect();
            self.release_all(db, &back).await;
            return Err(Halt::Stopped);
        }
        // Qdrant: две записи на пачку (удалить старые точки узлов, записать новые), а не
        // две на задание — на HDD каждая запись `wait=true` стоит десятки миллисекунд
        let nodes: Vec<Id> = ready.iter().map(|(j, _)| j.target_id).collect();
        #[cfg(feature = "test-hooks")]
        self.inner.hold.park().await;
        match self.write_points(user, &nodes, pts).await {
            Ok(()) => {}
            Err(e @ (IndexError::Unavailable(_) | IndexError::NotFound(_))) => {
                let back: Vec<Job> = ready.into_iter().map(|(j, _)| j).collect();
                self.requeue_unavailable(db, &back).await;
                return Err(Halt::Vector(e));
            }
            Err(e) => {
                // пачка отклонена: по одному, чтобы неудачным стало только виновное задание
                // (точки узлов строятся заново только на этом редком пути)
                tracing::debug!(error = %e, "запись пачки в Qdrant отклонена, по одному узлу");
                let mut keep = Vec::with_capacity(ready.len());
                let mut rest = ready.into_iter();
                while let Some((j, plan)) = rest.next() {
                    let pts = plan_points(user, &plan, &embs).unwrap_or_default();
                    match self.write_points(user, &[j.target_id], pts).await {
                        Ok(()) => keep.push((j, plan)),
                        Err(e @ (IndexError::Unavailable(_) | IndexError::NotFound(_))) => {
                            // записанные уже подтверждены, но их задания тоже
                            // возвращаются в очередь: повтор идемпотентен
                            let mut back: Vec<Job> = keep.into_iter().map(|(j, _)| j).collect();
                            back.push(j);
                            back.extend(rest.map(|(j, _)| j));
                            self.requeue_unavailable(db, &back).await;
                            return Err(Halt::Vector(e));
                        }
                        Err(e) => self.fail(db, &j, &e.to_string()).await,
                    }
                }
                ready = keep;
            }
        }
        // завершение заданий: только после подтверждённой записи
        for (j, plan) in ready {
            // свёртка предков ставится до закрытия задания, auto-L0 идёт после него: покой
            // воркера — пустая очередь, `batch_running == false` и `rollups_pending == 0`
            let parent = get_node(db, j.target_id)
                .await
                .ok()
                .and_then(|n| n.parent_id);
            self.schedule_chain(user, db, parent).await;
            match complete_job(db, j.id, j.claim_gen).await {
                Ok(JobOutcome::Done) => {
                    self.inner.jobs_done.fetch_add(1, Ordering::SeqCst);
                    if let Plan::Upsert(chunks) = &plan {
                        // L0 до отметки: `indexed` означает, что узел обработан целиком
                        self.auto_l0(db, j.target_id, j.rev, chunks, &embs).await;
                    }
                    // upsert без чанков (пустая заметка) тоже завершён: иначе вечный pending;
                    // исключённый и удалённый узел set_index_state не меняет
                    if j.op == JobOp::Upsert
                        && let Err(e) =
                            set_index_state(db, j.target_id, j.rev, IndexState::Indexed).await
                    {
                        tracing::warn!(node = %j.target_id, error = %e, "index_state");
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(job = j.id, error = %e, "complete_job"),
            }
        }
        Ok(())
    }

    /// Точки узлов в Qdrant: удалить старые точки `nodes`, записать `pts` (`wait=true`).
    async fn write_points(
        &self,
        user: Id,
        nodes: &[Id],
        pts: Vec<IndexPoint>,
    ) -> Result<(), IndexError> {
        if nodes.is_empty() {
            return Ok(());
        }
        let t = Instant::now();
        let res = async {
            self.deps.vector.delete_nodes(user, nodes).await?;
            if !pts.is_empty() {
                self.deps.vector.upsert(pts).await?;
            }
            Ok(())
        }
        .await;
        add_us(&self.inner.qdrant_us, t);
        res
    }

    /// Эмбеддинги по ключам: кэш `embeddings`, промахи — через модель пачками по
    /// `batch_tokens` с уступкой между ними, результат сразу в кэш (`owner` — узел
    /// служебных эмбеддингов L0; `None` — эмбеддинги чанков).
    async fn embed_cached(
        &self,
        db: &Db,
        items: Vec<EmbedItem>,
        owner: Option<Id>,
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
        // служебные строки до v5 (без владельца) — узлу: иначе их снимет сборка мусора
        if let Some(o) = owner
            && !found.is_empty()
        {
            let hit: Vec<String> = found.keys().cloned().collect();
            if let Err(e) = adopt_owned_embeddings(db, &model, o, &hit).await {
                tracing::debug!(node = %o, error = %e, "владелец служебных эмбеддингов");
            }
        }
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
            let t = Instant::now();
            let y0 = self.deps.embedder.yield_wait();
            let out = tokio::task::spawn_blocking(move || {
                let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
                emb.embed_passages(&refs)
            })
            .await;
            // уступка запросам — не инференс: отдельный счётчик
            let waited = self.deps.embedder.yield_wait().saturating_sub(y0);
            let busy = t.elapsed().saturating_sub(waited);
            self.inner
                .embed_us
                .fetch_add(busy.as_micros() as u64, Ordering::SeqCst);
            self.inner
                .yield_us
                .fetch_add(waited.as_micros() as u64, Ordering::SeqCst);
            let out = out
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
            match owner {
                None => put_embeddings(db, &model, batch.clone()).await,
                Some(o) => put_owned_embeddings(db, &model, o, batch.clone()).await,
            }
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
                embs.get(&c.embed_key)
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
        // ключи кандидатов — текущий набор служебных эмбеддингов заметки
        let keys: Vec<String> = if cands.len() > 1 {
            cands.iter().map(|s| keyed_hash("l0-sentence", s)).collect()
        } else {
            Vec::new()
        };
        if let Err(e) = prune_owned_embeddings(db, &self.model(), node, &keys).await {
            tracing::debug!(node = %node, error = %e, "устаревшие эмбеддинги L0");
        }
        let pick = match cands.len() {
            0 => return,
            1 => 0,
            _ => {
                let items: Vec<EmbedItem> = cands
                    .iter()
                    .zip(&keys)
                    .map(|(s, k)| EmbedItem {
                        key: k.clone(),
                        input: s.clone(),
                        tokens: s.len() / 3 + 1,
                    })
                    .collect();
                let Ok(m) = self.embed_cached(db, items, Some(node)).await else {
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

    /// Подсказки API (старый родитель перенесённого узла) — в очередь свёртки.
    async fn take_hints(&mut self) {
        let hints = std::mem::take(&mut *lock(&self.inner.hints));
        for (user, folder) in hints {
            if let Some(db) = self.open_db(user).await {
                self.schedule_chain(user, &db, Some(folder)).await;
            }
        }
    }

    /// Папка `start` и её предки (включая удалённые) — в очередь свёртки.
    async fn schedule_chain(&mut self, user: Id, db: &Db, start: Option<Id>) {
        let mut cur = start;
        // глубина дерева ограничена здравым смыслом; защита от цикла в повреждённых данных
        for _ in 0..256 {
            let Some(id) = cur else { break };
            self.rollups.schedule(user, id, self.cfg.rollup_debounce);
            cur = match get_node(db, id).await {
                Ok(n) => n.parent_id,
                Err(_) => break,
            };
        }
        let n = self.rollups.due.len();
        self.status(|s| s.rollups_pending = n);
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
            if let Err(e) = prune_owned_embeddings(db, &self.model(), folder, &[]).await {
                tracing::debug!(folder = %folder, error = %e, "устаревшие эмбеддинги L0 папки");
            }
            return ok_or_vector(self.deps.vector.delete_node(user, folder).await);
        };
        let (input, key) = folder_embed(&space.name, &f.path, &f.name, &text);
        if let Err(e) =
            prune_owned_embeddings(db, &self.model(), folder, std::slice::from_ref(&key)).await
        {
            tracing::debug!(folder = %folder, error = %e, "устаревшие эмбеддинги L0 папки");
        }
        let item = EmbedItem {
            key: key.clone(),
            input,
            tokens: text.len() / 3 + 1,
        };
        let e = match self.embed_cached(db, vec![item], Some(folder)).await {
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
        let info = IndexFolder {
            node_id: folder,
            space_id: f.space_id,
            space_kind: space.kind,
            space_name: space.name,
            solution_guid: space.solution_guid,
            path: f.path,
            name: f.name,
            l0: text,
            updated_at: f.updated_at,
        };
        let point = folder_point(user, &info, e, key);
        let t = Instant::now();
        let res = self.deps.vector.upsert(vec![point]).await;
        add_us(&self.inner.qdrant_us, t);
        ok_or_vector(res)
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

pub use zam_store::{embed_input_of, embed_key_of};

/// Текст для модели: `context_prefix` участвует в эмбеддинге (§8.2 п. 6).
pub fn embed_input(c: &IndexChunk) -> String {
    embed_input_of(&c.context_prefix, &c.text)
}

/// Ключ кэша `embeddings` чанка: blake3 (hex) ровно той строки, что уходит в модель
/// ([`embed_input`]), по формуле [`embed_key_of`] — той же, что пишет `chunks.embed_key`.
/// Воркер, пересборка из кэша и поиск берут готовый ключ из `chunks.embed_key`
/// ([`IndexChunk::embed_key`]); эта функция — для проверки и чанков вне базы.
pub fn embed_key(c: &IndexChunk) -> String {
    embed_key_of(&c.context_prefix, &c.text)
}

/// Текст эмбеддинга L0 папки и его ключ кэша (домен `folder`).
pub(crate) fn folder_embed(space: &str, path: &str, name: &str, l0: &str) -> (String, String) {
    let input = format!("[{space}/{path}] {name}\n{l0}");
    let key = keyed_hash("folder", &input);
    (input, key)
}

/// Ключ кэша `embeddings` для текста, которого нет среди чанков (предложение L0,
/// L0 папки): домен отделяет его от ключей чанков ([`embed_key`]).
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

/// Точки узла по плану задания; нет эмбеддинга какого-либо чанка — ошибка.
fn plan_points(
    user: Id,
    plan: &Plan,
    embs: &HashMap<String, Embedding>,
) -> Result<Vec<IndexPoint>, IndexError> {
    match plan {
        Plan::Delete => Ok(Vec::new()),
        Plan::Upsert(chunks) => chunks
            .iter()
            .map(|c| {
                embs.get(&c.embed_key)
                    .map(|e| chunk_point(user, c, e))
                    .ok_or_else(|| IndexError::Other(format!("нет эмбеддинга чанка {}", c.id)))
            })
            .collect(),
    }
}

/// Точка уровня `folder` (одна на папку; `text_hash` — ключ кэша её L0).
pub(crate) fn folder_point(user: Id, f: &IndexFolder, e: Embedding, key: String) -> IndexPoint {
    IndexPoint {
        chunk_id: folder_chunk_id(f.node_id),
        dense: e.dense,
        sparse: e.sparse,
        payload: PointPayload {
            user_id: user,
            space_id: f.space_id,
            space_kind: f.space_kind.as_str().to_string(),
            solution_guid: f.solution_guid.clone(),
            node_id: f.node_id,
            path_ancestors: path_ancestors(&f.path),
            area: None,
            level: "folder".into(),
            kind: "folder".into(),
            tags: Vec::new(),
            updated_at: f.updated_at.as_second(),
            rev: 0,
            text_hash: key,
        },
    }
}

/// Точка чанка заметки.
pub(crate) fn chunk_point(user: Id, c: &IndexChunk, e: &Embedding) -> IndexPoint {
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

/// Точка остановки перед записью пачки в Qdrant (фича `test-hooks`, см.
/// [`crate::test_hooks`]): эмбеддинги пачки посчитаны, задания захвачены.
#[cfg(feature = "test-hooks")]
impl WorkerHandle {
    /// Взвести: воркер встанет перед записью следующей пачки.
    #[doc(hidden)]
    pub fn hold_next_write(&self) {
        self.inner.hold.arm();
    }

    /// Ждёт, пока воркер встанет на точке; `false` — не встал за `limit`.
    #[doc(hidden)]
    pub async fn wait_write_held(&self, limit: Duration) -> bool {
        self.inner.hold.wait_parked(limit).await
    }

    /// Отпустить воркер (и снять взвод — на случай паники теста).
    #[doc(hidden)]
    pub fn release_write(&self) {
        self.inner.hold.release();
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
