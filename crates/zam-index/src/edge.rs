//! Прототип [`VectorIndex`] на встроенном Qdrant Edge (`qdrant-edge` 0.8, задача 16 M2,
//! `docs/adr/0001-qdrant-edge.md`). Только feature `edge`: в сервер и комплекты не входит.
//!
//! Схема та же, что у [`crate::QdrantIndex`] (§6.4), насколько её выражает Edge:
//! - шард `<dir>/zam_chunks/` — аналог коллекции; `dense` — Cosine, оригиналы на диске (mmap),
//!   int8 scalar quantization quantile 0.99 в RAM, поиск с rescoring; `sparse` — на диске (mmap);
//!   HNSW m=16, ef_construct=100, payload_m=16; payload на диске; 2 сегмента;
//! - payload-индексы те же (`user_id` с `is_tenant`, `space_id`, `node_id`, `path_ancestors`,
//!   `level`, `updated_at`);
//! - метаданных коллекции в Edge нет: `{model_id, schema, dim}` лежат рядом в `zam_meta.json`
//!   (атомарная запись); шард без файла — «не наш» (`model_id: None`).
//!
//! API Edge синхронный: каждый вызов идёт через `spawn_blocking`, паника внутри Edge
//! превращается в ошибку, а не роняет процесс. Индексация (HNSW) у Edge только по явному
//! `optimize()`, который блокирует до конца: его выполняет фоновый поток после паузы в
//! записи ([`EdgeOptions::optimize_debounce`]). До оптимизации новые точки ищутся полным
//! перебором в appendable-сегменте.
//!
//! **Долговечность не как у Qdrant.** `update` пишет WAL и применяет операцию к сегментам,
//! но Edge 0.8 WAL при `load` не воспроизводит (вызова `SerdeWal::read_all` в крейте нет) и
//! не усекает (`ack` не вызывается), а сегменты попадают на диск только в `flush` — после
//! оптимизации и при закрытии. Поэтому подтверждённый upsert теряется, если процесс завершится
//! до `flush` (тест `confirmed_upsert_survives_process_exit`, `#[ignore]`), а WAL растёт
//! без предела и целиком читается при открытии (ADR 0001). Для сервера это нарушило бы
//! правило «задание закрывается только после подтверждённого upsert»; прототип оставлен
//! как замерен, без `flush` на каждую запись.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use qdrant_edge::{
    Condition, CountRequest, Distance, EdgeConfig, EdgeOptimizersConfig, EdgeShard,
    EdgeSparseVectorParams, EdgeVectorParams, FieldCondition, FieldIndexOperations, Filter, Fusion,
    HnswIndexConfig, JsonPath, Match, NamedQuery, OperationError, PayloadFieldSchema,
    PayloadSelector, PayloadSelectorInclude, PointInsertOperations, PointOperations, PointStruct,
    Prefetch, QuantizationConfig, QueryEnum, QueryRequest, ScoringQuery, SearchParams,
    SparseVector, UpdateOperation, Vector, VectorInternal, Vectors, WithPayloadInterface,
};
use serde_json::{Value, json};
use zam_core::Id;

use crate::IndexError;
use crate::qdrant::{COLLECTION, DENSE, SCHEMA_VERSION, SPARSE};
use crate::vector::{
    IndexFuture, IndexMeta, IndexPoint, VectorFilter, VectorHit, VectorIndex, point_id,
};

/// Файл метаданных шарда (аналог `metadata` коллекции Qdrant).
const META_FILE: &str = "zam_meta.json";
/// Число сегментов (как у managed, см. `qdrant.rs`).
const SEGMENTS: usize = 2;
/// Размер пачки upsert (как у managed).
const UPSERT_BATCH: usize = 256;
const LEAF_PREFETCH: usize = 50;
const NOTE_PREFETCH: usize = 10;
const RRF_K: usize = 60;
const QUERY_LIMIT: usize = 60;
const MAX_ERR_MSG: usize = 200;
const LEVEL_CHUNK: &str = "chunk";
const LEVELS_NOTE: [&str; 2] = ["note", "section"];

/// Payload-индексы: (поле, схема в JSON-форме Qdrant).
const PAYLOAD_INDEXES: [(&str, &str); 6] = [
    ("user_id", r#"{"type":"keyword","is_tenant":true}"#),
    ("space_id", r#""keyword""#),
    ("node_id", r#""keyword""#),
    ("path_ancestors", r#""keyword""#),
    ("level", r#""keyword""#),
    ("updated_at", r#""integer""#),
];

/// Параметры Edge-индекса.
#[derive(Debug, Clone)]
pub struct EdgeOptions {
    /// Потоков поиска шарда (как `max_search_threads` managed, 2).
    pub max_search_threads: usize,
    /// Потоков построения HNSW (как `optimizer_cpu_budget` managed, 2; 0 — авто 8–16).
    pub indexing_threads: usize,
    /// Пауза в записи, после которой фоновый поток запускает `optimize()`.
    pub optimize_debounce: Duration,
}

impl Default for EdgeOptions {
    fn default() -> Self {
        EdgeOptions {
            max_search_threads: 2,
            indexing_threads: 2,
            optimize_debounce: Duration::from_secs(2),
        }
    }
}

/// Метаданные шарда в `zam_meta.json`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ShardMeta {
    model_id: String,
    schema: i64,
    dim: usize,
}

/// Открытый шард и его метаданные (`None` — файла нет: шард не наш).
struct Open {
    shard: Arc<EdgeShard>,
    meta: Option<ShardMeta>,
}

/// Состояние, общее для вызовов и фонового оптимизатора.
struct Inner {
    dir: PathBuf,
    opts: EdgeOptions,
    /// `None` — коллекции нет. Замок короткий (взять или подменить `Arc`): его берут и из
    /// async-кода ([`Inner::shard`]), поэтому загрузка и удаление шарда идут вне его.
    slot: Mutex<Option<Open>>,
    /// Сериализует открытие, создание и удаление шарда (секунды на HDD). Берётся только в
    /// блокирующих потоках (`spawn_blocking`), никогда — из async-кода.
    lifecycle: Mutex<()>,
}

impl Inner {
    fn shard_dir(&self) -> PathBuf {
        self.dir.join(COLLECTION)
    }

    fn lock(&self) -> MutexGuard<'_, Option<Open>> {
        // Паника внутри Edge происходит вне замка (в spawn_blocking), отравление не ожидается;
        // если оно всё же случилось, состояние слота корректно — продолжаем.
        self.slot.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Шард или `NotFound` (как у Qdrant при отсутствии коллекции).
    fn shard(&self) -> Result<Arc<EdgeShard>, IndexError> {
        self.lock()
            .as_ref()
            .map(|o| o.shard.clone())
            .ok_or_else(|| IndexError::NotFound(format!("шард {COLLECTION}")))
    }
}

/// Векторный индекс на встроенном Qdrant Edge (прототип, feature `edge`).
pub struct EdgeIndex {
    inner: Arc<Inner>,
    /// Сигнал «была запись» фоновому оптимизатору; закрытие канала останавливает поток.
    dirty: Mutex<Option<Sender<()>>>,
    optimizer: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl std::fmt::Debug for EdgeIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeIndex")
            .field("dir", &self.inner.dir)
            .finish_non_exhaustive()
    }
}

impl EdgeIndex {
    /// Открывает каталог `dir` (`<data_dir>/edge/`): существующий шард загружается, иначе
    /// коллекции нет до [`VectorIndex::ensure`]. Блокирующий вызов (чтение сегментов).
    pub fn open(dir: &Path, opts: EdgeOptions) -> Result<Self, IndexError> {
        std::fs::create_dir_all(dir).map_err(|e| io_err("каталог Edge", e))?;
        let inner = Arc::new(Inner {
            dir: dir.to_path_buf(),
            opts,
            slot: Mutex::new(None),
            lifecycle: Mutex::new(()),
        });
        let sdir = inner.shard_dir();
        if sdir.exists() {
            let open = load_shard(&sdir, &inner.opts)?;
            *inner.lock() = Some(open);
        }
        let (tx, rx) = channel();
        let worker = inner.clone();
        let optimizer = std::thread::Builder::new()
            .name("zam-edge-optimize".into())
            .spawn(move || optimizer_loop(&worker, &rx))
            .map_err(|e| io_err("поток оптимизатора Edge", e))?;
        Ok(EdgeIndex {
            inner,
            dirty: Mutex::new(Some(tx)),
            optimizer: Mutex::new(Some(optimizer)),
        })
    }

    /// Оптимизация (построение HNSW, слияние сегментов) сейчас, с ожиданием конца, и сброс
    /// на диск. Для замеров и тестов; в работе её запускает фоновый поток.
    pub async fn optimize_now(&self) -> Result<(), IndexError> {
        let shard = self.inner.shard()?;
        blocking("optimize", move || {
            shard.optimize()?;
            shard.flush()
        })
        .await
    }

    /// Число сегментов и проиндексированных векторов (для замеров).
    pub async fn segments(&self) -> Result<(usize, usize), IndexError> {
        let shard = self.inner.shard()?;
        blocking("info", move || {
            let i = shard.info()?;
            Ok((i.segments_count, i.indexed_vectors_count))
        })
        .await
    }

    fn mark_dirty(&self) {
        if let Some(tx) = self
            .dirty
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            let _ = tx.send(());
        }
    }

    async fn ensure_impl(&self, model_id: &str, dim: usize) -> Result<IndexMeta, IndexError> {
        let inner = self.inner.clone();
        let model_id = model_id.to_string();
        run_blocking("ensure", move || ensure_blocking(&inner, &model_id, dim)).await
    }

    async fn upsert_impl(&self, pts: Vec<IndexPoint>) -> Result<(), IndexError> {
        // Неизменное ограничение §6.4: Human никогда не попадает в индекс.
        if pts
            .iter()
            .any(|p| p.payload.space_kind.eq_ignore_ascii_case("human"))
        {
            return Err(IndexError::Other(
                "пространство human не индексируется".into(),
            ));
        }
        if pts.iter().any(|p| p.dense.is_empty()) {
            return Err(IndexError::Other("точка без dense-вектора".into()));
        }
        let shard = self.inner.shard()?;
        if pts.is_empty() {
            return Ok(());
        }
        let r = run_blocking("upsert", move || {
            let mut it = pts.into_iter().peekable();
            while it.peek().is_some() {
                let batch = it
                    .by_ref()
                    .take(UPSERT_BATCH)
                    .map(to_point)
                    .collect::<Result<Vec<_>, _>>()?;
                shard
                    .update(UpdateOperation::PointOperation(
                        PointOperations::UpsertPoints(PointInsertOperations::PointsList(batch)),
                    ))
                    .map_err(|e| map_err("upsert", e))?;
            }
            Ok(())
        })
        .await;
        self.mark_dirty();
        r
    }

    async fn delete_by(&self, op: &'static str, filter: Filter) -> Result<(), IndexError> {
        let shard = self.inner.shard()?;
        let r = blocking(op, move || {
            shard.update(UpdateOperation::PointOperation(
                PointOperations::DeletePointsByFilter(filter),
            ))
        })
        .await;
        self.mark_dirty();
        r
    }

    async fn search_impl(
        &self,
        q_dense: &[f32],
        q_sparse: &[(u32, f32)],
        f: &VectorFilter,
    ) -> Result<Vec<VectorHit>, IndexError> {
        let leaf_levels = allowed_levels(&[LEVEL_CHUNK], &f.levels);
        let note_levels = allowed_levels(&LEVELS_NOTE, &f.levels);
        let mut prefetches = Vec::new();
        if !leaf_levels.is_empty() && !q_dense.is_empty() {
            prefetches.push(prefetch(
                dense_query(q_dense),
                base_filter(f, &leaf_levels),
                Some(rescore()),
                LEAF_PREFETCH,
            ));
        }
        if !leaf_levels.is_empty() && !q_sparse.is_empty() {
            prefetches.push(prefetch(
                sparse_query(q_sparse)?,
                base_filter(f, &leaf_levels),
                None,
                LEAF_PREFETCH,
            ));
        }
        if !note_levels.is_empty() && !q_dense.is_empty() {
            prefetches.push(prefetch(
                dense_query(q_dense),
                base_filter(f, &note_levels),
                Some(rescore()),
                NOTE_PREFETCH,
            ));
        }
        let shard = self.inner.shard()?;
        if prefetches.is_empty() {
            return Ok(Vec::new());
        }
        let mut req = QueryRequest::new(QUERY_LIMIT);
        req.prefetches = prefetches;
        req.query = Some(ScoringQuery::Fusion(Fusion::Rrf {
            k: RRF_K,
            weights: None,
        }));
        req.with_payload = WithPayloadInterface::Selector(PayloadSelector::Include(
            PayloadSelectorInclude::new(vec![jp("chunk_id"), jp("node_id"), jp("level")]),
        ));
        let points = blocking("query", move || shard.query(req)).await?;
        points
            .into_iter()
            .map(|p| {
                let payload = p.payload.unwrap_or_default();
                let get = |k: &str| payload.0.get(k).and_then(Value::as_str).map(str::to_string);
                let chunk_id = get("chunk_id")
                    .ok_or_else(|| IndexError::Corrupt("точка без chunk_id".into()))?;
                let node_id = get("node_id")
                    .and_then(|s| s.parse::<Id>().ok())
                    .ok_or_else(|| IndexError::Corrupt("точка без корректного node_id".into()))?;
                let level =
                    get("level").ok_or_else(|| IndexError::Corrupt("точка без level".into()))?;
                Ok(VectorHit {
                    chunk_id,
                    node_id,
                    score: p.score,
                    level,
                })
            })
            .collect()
    }

    async fn count_impl(&self, f: &VectorFilter) -> Result<u64, IndexError> {
        let shard = self.inner.shard()?;
        let filter = count_filter(f);
        let n = blocking("count", move || {
            let mut req = CountRequest::new();
            req.filter = Some(filter);
            shard.count(req)
        })
        .await?;
        Ok(n as u64)
    }

    async fn drop_all_impl(&self) -> Result<(), IndexError> {
        let inner = self.inner.clone();
        run_blocking("drop_all", move || {
            let _life = inner.lifecycle.lock().unwrap_or_else(|p| p.into_inner());
            let taken = inner.lock().take();
            if let Some(open) = taken {
                // Каталог удаляется только после закрытия шарда (на Windows открытые mmap
                // не дают удалить файлы): ждём завершения вызовов, держащих ссылку. Идущую
                // оптимизацию не прервать — в `EdgeShard::optimize` флаг остановки локальный,
                // отмены в API 0.8 нет; `drop_all` ждёт её конца (минуты на 1 млн точек).
                let mut shard = open.shard;
                loop {
                    match Arc::try_unwrap(shard) {
                        Ok(s) => {
                            drop(s);
                            break;
                        }
                        Err(still) => {
                            shard = still;
                            std::thread::sleep(Duration::from_millis(20));
                        }
                    }
                }
            }
            let sdir = inner.shard_dir();
            if sdir.exists() {
                std::fs::remove_dir_all(&sdir).map_err(|e| io_err("удаление шарда Edge", e))?;
                tracing::info!(collection = COLLECTION, "шард Edge удалён");
            }
            Ok(())
        })
        .await
    }
}

impl Drop for EdgeIndex {
    fn drop(&mut self) {
        // Закрытие канала останавливает оптимизатор; ждём его, чтобы шард закрылся до выхода.
        // Идущий `optimize()` отменить нельзя (в API Edge 0.8 нет отмены): `Drop` ждёт его
        // конца — на 1 млн точек это до десятков минут.
        self.dirty.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(h) = self
            .optimizer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = h.join();
        }
    }
}

impl VectorIndex for EdgeIndex {
    fn ensure<'a>(&'a self, model_id: &'a str, dim: usize) -> IndexFuture<'a, IndexMeta> {
        Box::pin(self.ensure_impl(model_id, dim))
    }

    fn upsert(&self, pts: Vec<IndexPoint>) -> IndexFuture<'_, ()> {
        Box::pin(self.upsert_impl(pts))
    }

    fn delete_nodes<'a>(&'a self, user: Id, nodes: &'a [Id]) -> IndexFuture<'a, ()> {
        Box::pin(async move {
            if nodes.is_empty() {
                return Ok(());
            }
            let filter = must(vec![
                kw("user_id", user.to_string()),
                kw_any("node_id", nodes.iter().map(Id::to_string).collect()),
            ]);
            self.delete_by("delete", filter).await
        })
    }

    fn delete_scope<'a>(&'a self, f: &'a VectorFilter) -> IndexFuture<'a, ()> {
        Box::pin(self.delete_by("delete", count_filter(f)))
    }

    fn search<'a>(
        &'a self,
        q_dense: &'a [f32],
        q_sparse: &'a [(u32, f32)],
        f: &'a VectorFilter,
    ) -> IndexFuture<'a, Vec<VectorHit>> {
        Box::pin(self.search_impl(q_dense, q_sparse, f))
    }

    fn count<'a>(&'a self, f: &'a VectorFilter) -> IndexFuture<'a, u64> {
        Box::pin(self.count_impl(f))
    }

    fn drop_all(&self) -> IndexFuture<'_, ()> {
        Box::pin(self.drop_all_impl())
    }

    fn health(&self) -> IndexFuture<'_, ()> {
        // Встроенный индекс доступен, пока доступен каталог.
        Box::pin(async move {
            if self.inner.dir.is_dir() {
                Ok(())
            } else {
                Err(IndexError::Unavailable("каталог Edge недоступен".into()))
            }
        })
    }
}

/// Фоновый оптимизатор: после записи ждёт паузу `optimize_debounce` и запускает
/// `optimize()`; запись во время оптимизации ставит следующий проход.
fn optimizer_loop(inner: &Inner, rx: &Receiver<()>) {
    let debounce = inner.opts.optimize_debounce;
    // Ждём первую запись; закрытый канал — выход.
    while rx.recv().is_ok() {
        // Дебаунс: каждая новая запись откладывает проход.
        loop {
            match rx.recv_timeout(debounce) {
                Ok(()) => continue,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let Some(shard) = inner.lock().as_ref().map(|o| o.shard.clone()) else {
            continue;
        };
        let t = std::time::Instant::now();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            shard.optimize().and_then(|any| shard.flush().map(|()| any))
        }));
        match r {
            Ok(Ok(any)) => tracing::debug!(
                optimized = any,
                ms = t.elapsed().as_millis() as u64,
                "оптимизация Edge"
            ),
            Ok(Err(e)) => tracing::warn!(error = %truncate(&e.to_string()), "оптимизация Edge"),
            Err(_) => tracing::error!("паника в оптимизации Edge"),
        }
    }
}

/// Открытие существующего шарда (параметры поиска и индексации — из `opts`).
fn load_shard(sdir: &Path, opts: &EdgeOptions) -> Result<Open, IndexError> {
    let cfg = EdgeConfig {
        max_search_threads: Some(opts.max_search_threads),
        ..EdgeConfig::default()
    };
    let shard = std::panic::catch_unwind(|| EdgeShard::load(sdir, Some(cfg)))
        .map_err(|_| IndexError::Corrupt("паника Edge при загрузке шарда".into()))?
        .map_err(|e| IndexError::Corrupt(format!("загрузка шарда Edge: {}", short(&e))))?;
    Ok(Open {
        shard: Arc::new(shard),
        meta: read_meta(sdir),
    })
}

fn read_meta(sdir: &Path) -> Option<ShardMeta> {
    let raw = std::fs::read(sdir.join(META_FILE)).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Атомарная запись `zam_meta.json` с `fsync` (как метка хранилища Qdrant).
fn write_meta(sdir: &Path, meta: &ShardMeta) -> Result<(), IndexError> {
    let body = serde_json::to_vec(meta).map_err(|e| IndexError::Other(e.to_string()))?;
    crate::supervisor::write_atomic(sdir, META_FILE, &body)
        .map_err(|e| io_err("метаданные Edge", e))
}

fn edge_config(dim: usize, opts: &EdgeOptions) -> Result<EdgeConfig, IndexError> {
    let quant: QuantizationConfig = serde_json::from_value(
        json!({"scalar": {"type": "int8", "quantile": 0.99, "always_ram": true}}),
    )
    .map_err(|e| IndexError::Other(format!("конфигурация квантования: {e}")))?;
    let hnsw = HnswIndexConfig {
        m: 16,
        ef_construct: 100,
        payload_m: Some(16),
        max_indexing_threads: opts.indexing_threads,
        ..HnswIndexConfig::default()
    };
    let optimizers = EdgeOptimizersConfig {
        default_segment_number: Some(SEGMENTS),
        ..EdgeOptimizersConfig::default()
    };
    let dense = EdgeVectorParams::builder(dim, Distance::Cosine)
        .on_disk(true)
        .quantization_config(quant)
        .build();
    let sparse = EdgeSparseVectorParams {
        full_scan_threshold: None,
        on_disk: Some(true),
        modifier: None,
        datatype: None,
    };
    Ok(EdgeConfig::builder()
        .vector(DENSE, dense)
        .sparse_vector(SPARSE, sparse)
        .on_disk_payload(true)
        .hnsw_config(hnsw)
        .optimizers(optimizers)
        .max_search_threads(opts.max_search_threads)
        .build())
}

fn ensure_blocking(inner: &Inner, model_id: &str, dim: usize) -> Result<IndexMeta, IndexError> {
    // Открытие и создание — под замком жизненного цикла, слот при этом свободен: поиск и
    // запись (`NotFound` до открытия) не ждут загрузки сегментов.
    let _life = inner.lifecycle.lock().unwrap_or_else(|p| p.into_inner());
    let mut created = false;
    if inner.lock().is_none() {
        let sdir = inner.shard_dir();
        let open = if sdir.exists() {
            load_shard(&sdir, &inner.opts)?
        } else {
            std::fs::create_dir_all(&sdir).map_err(|e| io_err("каталог шарда Edge", e))?;
            let cfg = edge_config(dim, &inner.opts)?;
            let shard = EdgeShard::new(&sdir, cfg).map_err(|e| map_err("create", e))?;
            let meta = ShardMeta {
                model_id: model_id.to_string(),
                schema: SCHEMA_VERSION,
                dim,
            };
            write_meta(&sdir, &meta)?;
            created = true;
            tracing::info!(collection = COLLECTION, model_id, dim, "шард Edge создан");
            Open {
                shard: Arc::new(shard),
                meta: Some(meta),
            }
        };
        *inner.lock() = Some(open);
    }
    let (shard, meta) = {
        let slot = inner.lock();
        let open = slot.as_ref().expect("шард открыт выше");
        (open.shard.clone(), open.meta.clone())
    };
    let info = shard.info().map_err(|e| map_err("info", e))?;
    let own = meta.as_ref().is_some_and(|m| m.model_id == model_id);
    // Недостающие payload-индексы досоздаём только в «своём» шарде (как у managed).
    if own {
        for (field, schema) in PAYLOAD_INDEXES {
            if !info.payload_schema.keys().any(|k| k.to_string() == field) {
                create_index(&shard, field, schema)?;
            }
        }
    }
    let dim_now = match &meta {
        Some(m) => m.dim,
        None => shard
            .config()
            .vectors
            .get(DENSE)
            .map(|v| v.size)
            .ok_or_else(|| IndexError::Corrupt("в шарде нет вектора dense".into()))?,
    };
    let points = shard
        .count(CountRequest::new())
        .map_err(|e| map_err("count", e))? as u64;
    Ok(IndexMeta {
        model_id: meta.map(|m| m.model_id),
        dim: dim_now,
        points,
        created,
    })
}

fn create_index(shard: &EdgeShard, field: &str, schema: &str) -> Result<(), IndexError> {
    let schema: PayloadFieldSchema = serde_json::from_str(schema)
        .map_err(|e| IndexError::Other(format!("схема индекса {field}: {e}")))?;
    shard
        .update(UpdateOperation::FieldIndexOperation(
            FieldIndexOperations::CreateIndex(qdrant_edge::CreateIndex {
                field_name: jp(field),
                field_schema: Some(schema),
            }),
        ))
        .map_err(|e| map_err("create_index", e))
}

/// Точка Edge: id — UUIDv5 от `chunk_id`, payload — как у managed.
fn to_point(p: IndexPoint) -> Result<qdrant_edge::PointStructPersisted, IndexError> {
    let mut named: Vec<(&str, Vector)> = vec![(DENSE, Vector::new_dense(p.dense))];
    if !p.sparse.is_empty() {
        named.push((SPARSE, sparse_vector(&p.sparse)?));
    }
    let pl = p.payload;
    let mut payload = serde_json::Map::new();
    payload.insert("chunk_id".into(), p.chunk_id.clone().into());
    payload.insert("user_id".into(), pl.user_id.to_string().into());
    payload.insert("space_id".into(), pl.space_id.to_string().into());
    payload.insert("space_kind".into(), pl.space_kind.into());
    if let Some(g) = pl.solution_guid {
        payload.insert("solution_guid".into(), g.into());
    }
    payload.insert("node_id".into(), pl.node_id.to_string().into());
    payload.insert("path_ancestors".into(), pl.path_ancestors.into());
    if let Some(a) = pl.area {
        payload.insert("area".into(), a.into());
    }
    payload.insert("level".into(), pl.level.into());
    payload.insert("kind".into(), pl.kind.into());
    payload.insert("tags".into(), pl.tags.into());
    payload.insert("updated_at".into(), pl.updated_at.into());
    payload.insert("rev".into(), pl.rev.into());
    payload.insert("text_hash".into(), pl.text_hash.into());
    let id = qdrant_edge::PointId::Uuid(point_id(&p.chunk_id));
    Ok(PointStruct::new(id, Vectors::new_named(named), Value::Object(payload)).0)
}

/// Sparse-вектор с индексами по возрастанию (требование Edge к скорингу).
fn sparse_vector(pairs: &[(u32, f32)]) -> Result<Vector, IndexError> {
    let mut v = SparseVector::new(
        pairs.iter().map(|p| p.0).collect(),
        pairs.iter().map(|p| p.1).collect(),
    )
    .map_err(|e| IndexError::Other(format!("sparse-вектор: {e}")))?;
    v.sort_by_indices();
    Ok(Vector::from(v))
}

fn dense_query(q: &[f32]) -> ScoringQuery {
    ScoringQuery::Vector(QueryEnum::Nearest(NamedQuery {
        query: VectorInternal::Dense(q.to_vec()),
        using: Some(DENSE.to_string()),
    }))
}

fn sparse_query(q: &[(u32, f32)]) -> Result<ScoringQuery, IndexError> {
    Ok(ScoringQuery::Vector(QueryEnum::Nearest(NamedQuery {
        query: sparse_vector(q)?.0,
        using: Some(SPARSE.to_string()),
    })))
}

fn rescore() -> SearchParams {
    // Поиск по int8 с rescoring по оригиналам, как у managed.
    serde_json::from_value(json!({"quantization": {"rescore": true}}))
        .expect("корректные параметры поиска")
}

fn prefetch(
    query: ScoringQuery,
    filter: Filter,
    params: Option<SearchParams>,
    limit: usize,
) -> Prefetch {
    let mut p = Prefetch::new(limit);
    p.query = Some(query);
    p.filter = Some(filter);
    p.params = params;
    p
}

/// Путь поля payload. Имена полей — константы этого модуля, разбор не падает.
fn jp(field: &str) -> JsonPath {
    field.parse().expect("корректное имя поля payload")
}

fn must(conditions: Vec<Condition>) -> Filter {
    Filter {
        must: Some(conditions),
        ..Filter::default()
    }
}

/// Точное совпадение keyword.
fn kw(field: &str, value: String) -> Condition {
    Condition::Field(FieldCondition::new_match(jp(field), Match::from(value)))
}

/// Совпадение с любым из значений keyword.
fn kw_any(field: &str, values: Vec<String>) -> Condition {
    Condition::Field(FieldCondition::new_match(jp(field), Match::from(values)))
}

/// Тенант, пространства и префикс пути.
fn scope_conditions(f: &VectorFilter) -> Vec<Condition> {
    let mut c = vec![kw("user_id", f.user_id.to_string())];
    if !f.space_ids.is_empty() {
        c.push(kw_any(
            "space_id",
            f.space_ids.iter().map(Id::to_string).collect(),
        ));
    }
    if let Some(prefix) = &f.path_prefix {
        c.push(kw("path_ancestors", prefix.clone()));
    }
    c
}

fn count_filter(f: &VectorFilter) -> Filter {
    let mut c = scope_conditions(f);
    if !f.levels.is_empty() {
        c.push(kw_any("level", f.levels.clone()));
    }
    must(c)
}

fn base_filter(f: &VectorFilter, levels: &[String]) -> Filter {
    let mut c = scope_conditions(f);
    c.push(kw_any("level", levels.to_vec()));
    must(c)
}

fn allowed_levels(prefetch: &[&str], requested: &[String]) -> Vec<String> {
    prefetch
        .iter()
        .filter(|l| requested.is_empty() || requested.iter().any(|r| r == *l))
        .map(|l| (*l).to_string())
        .collect()
}

/// Блокирующий вызов Edge в `spawn_blocking`: ошибка Edge — [`map_err`], паника — ошибка.
async fn blocking<T, F>(op: &'static str, f: F) -> Result<T, IndexError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, OperationError> + Send + 'static,
{
    run_blocking(op, move || f().map_err(|e| map_err(op, e))).await
}

/// То же для замыканий, возвращающих [`IndexError`].
async fn run_blocking<T, F>(op: &'static str, f: F) -> Result<T, IndexError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, IndexError> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r,
        Err(e) if e.is_panic() => {
            tracing::error!(op, "паника внутри Qdrant Edge");
            Err(IndexError::Other(format!(
                "{op}: сбой Qdrant Edge (паника)"
            )))
        }
        Err(_) => Err(IndexError::Other(format!("{op}: задача Edge отменена"))),
    }
}

/// Ошибка Edge → [`IndexError`]. Текст усечён; векторов и payload в нём нет.
fn map_err(op: &'static str, e: OperationError) -> IndexError {
    let msg = short(&e);
    match e {
        OperationError::InconsistentStorage { .. } | OperationError::FileNotFound { .. } => {
            IndexError::Corrupt(format!("{op}: {msg}"))
        }
        OperationError::ServiceError { .. } => IndexError::Unavailable(format!("{op}: {msg}")),
        _ => IndexError::Other(format!("{op}: {msg}")),
    }
}

fn short(e: &OperationError) -> String {
    truncate(&e.to_string())
}

fn io_err(what: &str, e: std::io::Error) -> IndexError {
    IndexError::Process(format!("{what}: {e}"))
}

fn truncate(s: &str) -> String {
    match s.char_indices().nth(MAX_ERR_MSG) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_index_schemas_parse() {
        for (field, schema) in PAYLOAD_INDEXES {
            let s: Result<PayloadFieldSchema, _> = serde_json::from_str(schema);
            assert!(s.is_ok(), "{field}: {s:?}");
        }
    }

    #[test]
    fn config_mirrors_managed_collection() {
        let cfg = edge_config(1024, &EdgeOptions::default()).unwrap();
        let d = &cfg.vectors[DENSE];
        assert_eq!(d.size, 1024);
        assert_eq!(d.on_disk, Some(true));
        assert!(d.quantization_config.is_some());
        assert_eq!(cfg.on_disk_payload, Some(true));
        assert_eq!(cfg.max_search_threads, Some(2));
        assert_eq!(cfg.hnsw_config.unwrap().m, 16);
        let _ = rescore();
    }

    #[test]
    fn sparse_is_sorted_by_index() {
        let v = sparse_vector(&[(9, 0.1), (2, 0.5)]).unwrap();
        match v.0 {
            VectorInternal::Sparse(s) => assert_eq!(s.indices, [2, 9]),
            other => panic!("{other:?}"),
        }
    }
}
